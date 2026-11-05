use crate::ansi::{DIM, GREEN, RESET};
use crate::config::{spavn_dir, tr_en};
use crate::hud::{self, LiveSpinner};
use crate::router::{stream_chat_completion, ChatMessage, ModelEndpoint};
use crate::state::{list_live_tasks, LiveTaskStatus};
use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;

fn secure_file(path: &Path) {
    #[cfg(unix)]
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE));
    #[cfg(not(unix))]
    let _ = path;
}

fn harden_sessions_dir(dir: &Path) {
    #[cfg(unix)]
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(PRIVATE_DIR_MODE));
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                secure_file(&path);
            }
        }
    }
}

static SESSIONS_HARDENED: std::sync::Once = std::sync::Once::new();

pub fn sessions_dir() -> PathBuf {
    let dir = spavn_dir().join("sessions");
    let _ = fs::create_dir_all(&dir);
    SESSIONS_HARDENED.call_once(|| harden_sessions_dir(&dir));
    dir
}

pub fn new_session_path() -> PathBuf {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ts = dur.as_secs();
    let dir = sessions_dir();
    let mut path = dir.join(format!("session-{}.jsonl", ts));
    if path.exists() && fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
        path = dir.join(format!("session-{}-{}.jsonl", ts, dur.subsec_millis()));
    }
    if let Ok(file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        drop(file);
    }
    secure_file(&path);
    path
}

pub fn latest_session_path() -> Option<PathBuf> {
    let dir = sessions_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    entries.sort();
    entries.pop()
}

pub fn load_session(path: &PathBuf) -> Vec<ChatMessage> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        if let Ok(msg) = serde_json::from_str::<ChatMessage>(&line) {
            if !messages.is_empty() && msg.role == "system" {
                continue;
            }
            messages.push(msg);
        }
    }
    messages
}

pub fn append_message(path: &PathBuf, msg: &ChatMessage) {
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        secure_file(path);
        if let Ok(line) = serde_json::to_string(msg) {
            let _ = writeln!(file, "{}", line);
        }
    }
}

pub fn rewrite_session(path: &PathBuf, messages: &[ChatMessage]) {
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        secure_file(path);
        for msg in messages {
            if let Ok(line) = serde_json::to_string(msg) {
                let _ = writeln!(file, "{}", line);
            }
        }
    }
}

pub fn fork_session(messages: &[ChatMessage]) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let forked_path = sessions_dir().join(format!("session-{}-fork.jsonl", ts));
    rewrite_session(&forked_path, messages);
    forked_path
}

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub path: PathBuf,
    pub filename: String,
    pub age_label: String,
    pub message_count: usize,
    pub preview: String,
}

fn clean_user_preview(raw: &str) -> String {
    let mut s = raw;
    while let Some(start) = s.find("<active_skill") {
        if let Some(end) = s[start..].find("</active_skill>") {
            s = &s[start + end + "</active_skill>".len()..];
        } else {
            break;
        }
    }
    let collapsed: String = s
        .lines()
        .map(|l| l.trim())
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("[SPAVN")
                && !l.starts_with("<spavn_knowledge")
                && !l.starts_with("</spavn_knowledge")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut chars = trimmed.chars();
    let head: String = chars.by_ref().take(42).collect();
    if chars.next().is_some() {
        format!("{}…", head)
    } else {
        head
    }
}

fn format_relative_age(now_secs: u64, mtime_secs: u64) -> String {
    let diff = now_secs.saturating_sub(mtime_secs);
    if diff < 60 {
        format!("{}sn önce", diff.max(1))
    } else if diff < 3600 {
        format!("{}dk önce", diff / 60)
    } else if diff < 86400 {
        format!("{}sa {}dk önce", diff / 3600, (diff % 3600) / 60)
    } else {
        format!("{} gün önce", diff / 86400)
    }
}

pub fn list_recent_sessions(limit: usize) -> Vec<SessionSummary> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    if let Ok(rd) = fs::read_dir(sessions_dir()) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                files.push((p, mtime));
            }
        }
    }

    files.sort_by_key(|a| std::cmp::Reverse(a.1));

    let mut summaries = Vec::new();
    for (path, mtime_secs) in files.into_iter().take(limit) {
        let msgs = load_session(&path);
        let mut msg_count = 0usize;
        let mut first_user = String::new();
        let mut last_user = String::new();
        for m in &msgs {
            if m.role == "user" || m.role == "assistant" {
                msg_count += 1;
            }
            if m.role == "user" {
                if let Some(ref c) = m.content {
                    let cleaned = clean_user_preview(c);
                    if !cleaned.is_empty() {
                        if first_user.is_empty() {
                            first_user = cleaned.clone();
                        }
                        last_user = cleaned;
                    }
                }
            }
        }
        let preview = if !last_user.is_empty() {
            last_user
        } else if !first_user.is_empty() {
            first_user
        } else {
            "(Boş oturum)".to_string()
        };
        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let age_label = format_relative_age(now_secs, mtime_secs);
        summaries.push(SessionSummary {
            path,
            filename,
            age_label,
            message_count: msg_count,
            preview,
        });
    }
    summaries
}

/// Headroom for the tool-card header summary (`╭─ ⚡ bash [local] …`).
pub const SUMMARY_MAX_CHARS: usize = 80;

/// Squeezes one summary into a single display line.
///
/// Heredocs, `&&` chains and multi-line regexes all reach us as one argument string, so a
/// raw newline would render a second physical row while the card header believes it printed
/// one — the inline dock then erases with the wrong row count and eats the lines above it
/// (`cd … <<'EOF'` cards appeared as a bare `cd` followed by an empty header). Whitespace
/// runs collapse to one space, ANSI sequences drop out (shared `ansi::skip_escape`) and control
/// bytes are whitespace, so an argument can never repaint or wrap the header.
///
/// Overlong text keeps both ends and loses its middle: `cd <path> && <real command>` is the
/// common shape, so cutting the tail turned the header into a bare `cd` — the command is the
/// half a reader needs.
pub fn one_line(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars * 2));
    let mut gap = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            crate::ansi::skip_escape(&mut chars);
            continue;
        }
        if ch.is_whitespace() || ch.is_control() {
            gap = !out.is_empty();
            continue;
        }
        if gap {
            out.push(' ');
            gap = false;
        }
        out.push(ch);
    }
    if out.chars().count() <= max_chars {
        return out;
    }
    let (cut, _) = crate::ansi::truncate_ansi_to_width_middle(&out, max_chars);
    cut.trim_end_matches(crate::ansi::RESET).to_string()
}

/// Mirrors the executor's argument aliases exactly.
///
/// The header used to read only the canonical key while `execute_tool` accepts aliases, so a
/// call spelled `{"cmd": …}`, `{"file_path": …}` or `{"target": "host", …}` rendered an empty
/// or lying header (`╭─ ⚡ bash [local]` with no command, or `[local]` for a remote run).
/// The alias table lives in `tools::get_str_alias`, so both sides now agree by construction.
pub fn summarize_tool_args(name: &str, args_json: &str, default_host: &str) -> String {
    use crate::tools::get_str_alias;

    let Ok(val) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return String::new();
    };
    let raw = match name {
        "read" | "edit" | "write" | "read_file" | "edit_file" | "write_file" => {
            get_str_alias(&val, &["path", "file_path", "file", "filepath"]).to_string()
        }
        // Heredoc bodies and multi-line scripts arrive as a single argument, but only the first
        // physical line names what the call does. Keeping the body let it swallow the whole
        // header, so the leading `cd <path>` — the part the reader actually needs — got cut.
        "bash" => {
            let explicit_host = get_str_alias(&val, &["host", "target"]);
            let host = if explicit_host.is_empty() {
                default_host
            } else {
                explicit_host
            };
            let command = get_str_alias(&val, &["command", "cmd", "script", "shell"]);
            let first_line = command.lines().next().unwrap_or("");
            format!("[{}] {}", host, first_line)
        }
        "search" => get_str_alias(&val, &["pattern", "query", "regex", "search"]).to_string(),
        "glob" | "find_files" | "list_files" | "file_glob" => {
            get_str_alias(&val, &["pattern", "glob", "query", "name", "match"]).to_string()
        }
        "memory" => {
            let op = get_str_alias(&val, &["op", "action", "mode"]);
            let op = if op.is_empty() { "recall" } else { op };
            format!(
                "{}: {}",
                op,
                get_str_alias(&val, &["query", "text", "content", "note"])
            )
        }
        "skill" => get_str_alias(&val, &["name", "skill", "id", "query"]).to_string(),
        "web" => {
            let url = get_str_alias(&val, &["url", "href", "link"]);
            if url.is_empty() {
                get_str_alias(&val, &["query", "q", "search"]).to_string()
            } else {
                url.to_string()
            }
        }
        "subagent" | "spawn_submodels" | "task" | "delegate" => {
            get_str_alias(&val, &["task", "prompt", "query", "objective"]).to_string()
        }
        _ => String::new(),
    };
    one_line(&raw, SUMMARY_MAX_CHARS)
}

/// What a call actually acts on, for matching against live task titles.
///
/// The header summary is a *display* string (`[local] cd ~/x && cargo test`), so deriving a
/// path stem from it produced nonsense and no task ever advanced from a shell step. This keeps
/// the display and the matcher on one alias table but two different outputs.
pub fn tool_progress_target(name: &str, args_json: &str) -> String {
    use crate::tools::get_str_alias;

    let Ok(val) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return String::new();
    };
    match name {
        "read" | "edit" | "write" | "read_file" | "edit_file" | "write_file" => {
            get_str_alias(&val, &["path", "file_path", "file", "filepath"]).to_string()
        }
        "bash" => {
            let command = get_str_alias(&val, &["command", "cmd", "script", "shell"]);
            // `cd <dir> && <real command>` is the common shape; the leading segment names a
            // directory, so the actionable half is the last one.
            command
                .lines()
                .next()
                .unwrap_or("")
                .rsplit("&&")
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        }
        "search" => get_str_alias(&val, &["pattern", "query", "regex", "search"]).to_string(),
        _ => String::new(),
    }
}

pub fn safe_utf8_prefix(s: &str, max_bytes: usize) -> &str {
    if max_bytes >= s.len() {
        return s;
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

pub fn safe_utf8_suffix(s: &str, tail_bytes: usize) -> &str {
    if tail_bytes >= s.len() {
        return s;
    }
    let mut idx = s.len() - tail_bytes;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    &s[idx..]
}

pub fn evict_old_skill_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<active_skill") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start..];
        if let Some(end) = after_start.find("</active_skill>") {
            out.push_str(
                "[SPAVN Skill Evicted: 1-turn skill instructions auto-removed to save context]",
            );
            rest = &after_start[end + "</active_skill>".len()..];
        } else {
            out.push_str(after_start);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| {
            let c_len = m.content.as_ref().map(|s| s.len()).unwrap_or(0);
            let r_len = m.reasoning_content.as_ref().map(|s| s.len()).unwrap_or(0);
            (c_len + r_len) / 4 + 12
        })
        .sum()
}

pub fn perform_manual_compact(
    endpoint: &ModelEndpoint,
    messages: &mut Vec<ChatMessage>,
    focus: Option<&str>,
) {
    if messages.len() <= 3 {
        println!(
            "\n{}🗜️  Bağlam zaten çok kısa ({} mesaj, ~{} token).{}",
            DIM,
            messages.len(),
            estimate_tokens(messages),
            RESET
        );
        return;
    }

    let before_tok = estimate_tokens(messages);
    let mut touched_tools: Vec<String> = Vec::new();
    for m in messages.iter_mut().skip(1) {
        if let Some(ref c) = m.content {
            if c.contains("<active_skill") {
                m.content = Some(evict_old_skill_blocks(c));
            }
        }
        if let Some(ref tcs) = m.tool_calls {
            for tc in tcs {
                let summary =
                    summarize_tool_args(&tc.function.name, &tc.function.arguments, "local");
                let item = if summary.is_empty() {
                    tc.function.name.clone()
                } else {
                    format!("{}({})", tc.function.name, summary)
                };
                if !touched_tools.contains(&item) && touched_tools.len() < 24 {
                    touched_tools.push(item);
                }
            }
        }
        if m.role == "tool" {
            if let Some(ref c) = m.content {
                if c.len() > 350 {
                    let head = safe_utf8_prefix(c, 180);
                    m.content = Some(format!(
                        "{}\n...[Compacted {} bytes]...",
                        head,
                        c.len().saturating_sub(180)
                    ));
                }
            }
        }
    }

    if messages.len() > 6 {
        let sys = messages[0].clone();
        let active_user_idx = messages.iter().rposition(|m| {
            m.role == "user" && !m.content.as_deref().unwrap_or("").starts_with("[SPAVN ")
        });
        let active_user_prompt = active_user_idx
            .and_then(|idx| messages[idx].content.clone())
            .map(|c| hud::strip_internal_tags_for_display(&c).trim().to_string())
            .filter(|s| !s.is_empty());
        let mut tail_start = messages.len().saturating_sub(4);
        if let Some(u_idx) = active_user_idx {
            if messages.len().saturating_sub(u_idx) <= 8 {
                tail_start = tail_start.min(u_idx);
            }
        }
        while tail_start > 1 && messages[tail_start].role == "tool" {
            tail_start -= 1;
        }
        let mut transcript_lines = Vec::new();
        for m in &messages[1..tail_start] {
            if m.role == "user" || m.role == "assistant" {
                if let Some(c) = &m.content {
                    let clean = hud::strip_internal_tags_for_display(c);
                    let snippet: String = clean
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .take(4)
                        .collect::<Vec<_>>()
                        .join(" / ")
                        .chars()
                        .take(260)
                        .collect();
                    if !snippet.is_empty() {
                        transcript_lines.push(format!("- [{}]: {}", m.role, snippet));
                    }
                }
            }
        }
        let focus_note = focus.map(|f| format!(" (Odak: {})", f)).unwrap_or_default();
        // Try LLM-powered semantic compaction first (OMP-style structured handoff summary),
        // with instant fallback to algorithmic digest if offline/interrupted.
        let mut llm_summary: Option<String> = None;
        if !transcript_lines.is_empty() {
            let mut spin = LiveSpinner::start(tr_en(
                "🗜️  Bağlam LLM ile anlamsal olarak özetleniyor",
                "🗜️  Summarizing context semantically with LLM",
            ));
            let focus_instruction = focus
                .map(|f| format!("\nFocus especially on: {}", f))
                .unwrap_or_default();
            let prompt_body = format!(
                "Summarize the following coding session history into a dense, high-signal technical handoff (<300 words) preserving:\n\
                 1. Goal & User Preferences\n\
                 2. Files Modified & Key Architectural Decisions\n\
                 3. Current State & Pending Next Steps{}\n\n\
                 Touched tools/files: {}\n\n\
                 Conversation Transcript:\n{}",
                focus_instruction,
                if touched_tools.is_empty() {
                    "(none)".to_string()
                } else {
                    touched_tools.join(", ")
                },
                transcript_lines.join("\n")
            );
            let compact_msgs = vec![
                ChatMessage::system(
                    "You are SPAVN's Context Compactor. Output ONLY the structured technical handoff summary in concise bullet points. No preamble.",
                ),
                ChatMessage::user(prompt_body),
            ];
            let empty_tools = serde_json::json!([]);
            if let Ok(res) =
                stream_chat_completion(endpoint, &compact_msgs, &empty_tools, |_| {}, |_| {})
            {
                let text = res
                    .message
                    .content
                    .or(res.message.reasoning_content)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    llm_summary = Some(text);
                }
            }
            spin.stop();
        }

        let mut new_msgs = vec![sys];
        let active_tasks = list_live_tasks();
        let tasks_handoff_suffix = if active_tasks.is_empty() {
            String::new()
        } else {
            let lines: Vec<String> = active_tasks
                .iter()
                .map(|t| {
                    let box_str = match t.status {
                        LiveTaskStatus::Done => "[x]",
                        LiveTaskStatus::InProgress => "[~]",
                        LiveTaskStatus::Pending => "[ ]",
                    };
                    format!("- {} {}", box_str, t.title)
                })
                .collect();
            format!("\n\n[Active Live Task Checklist]:\n{}", lines.join("\n"))
        };
        let preserved_user_suffix = match (active_user_idx, active_user_prompt.as_deref()) {
            (Some(u_idx), Some(prompt)) if u_idx < tail_start => {
                format!(
                    "\n\n[Active User Request — MUST complete all items]:\n{}{}",
                    prompt, tasks_handoff_suffix
                )
            }
            _ => tasks_handoff_suffix,
        };
        if let Some(semantic_summary) = llm_summary {
            new_msgs.push(ChatMessage::user(format!(
                "[SPAVN Semantic Context Handoff{}]:\n{}{}",
                focus_note, semantic_summary, preserved_user_suffix
            )));
        } else if !transcript_lines.is_empty() {
            let tools_note = if touched_tools.is_empty() {
                String::new()
            } else {
                format!("\n- [tools/files]: {}", touched_tools.join(", "))
            };
            new_msgs.push(ChatMessage::user(format!(
                "[SPAVN Auto-Compact Özet{}]:\n{}{}{}",
                focus_note,
                transcript_lines.join("\n"),
                tools_note,
                preserved_user_suffix
            )));
        }
        new_msgs.extend_from_slice(&messages[tail_start..]);
        *messages = new_msgs;
    }

    let after_tok = estimate_tokens(messages);
    let saved_pct = ((before_tok.saturating_sub(after_tok)) * 100) / before_tok.max(1);
    println!(
        "\n{}🗜️  [Compact Tamamlandı]: ~{} tok -> ~{} tok (%{} bağlam tasarrufu){}",
        GREEN, before_tok, after_tok, saved_pct, RESET
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn session_files_and_dir_are_private() {
        let dir = std::env::temp_dir().join(format!("spavn-session-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("SPAVN_HOME", &dir);

        let path = new_session_path();
        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(sessions_dir()).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "session file must be 0600");
        assert_eq!(dir_mode, 0o700, "sessions dir must be 0700");

        let _ = fs::remove_file(&path);
        std::env::remove_var("SPAVN_HOME");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_line_flattens_heredocs() {
        let raw = "cd ~/apps/spavn-cli\ncat > /tmp/x.sh <<'EOF'\n#!/bin/sh\necho hi\nEOF";
        assert_eq!(
            one_line(raw, SUMMARY_MAX_CHARS),
            "cd ~/apps/spavn-cli cat > /tmp/x.sh <<'EOF' #!/bin/sh echo hi EOF"
        );
    }

    #[test]
    fn one_line_strips_escapes_and_caps_width() {
        let poisoned = "\x1b[31mabcdefghij\x1b[0m";
        assert_eq!(one_line(poisoned, 4), "a…ij");
        assert_eq!(
            one_line(&"x".repeat(500), SUMMARY_MAX_CHARS)
                .chars()
                .count(),
            SUMMARY_MAX_CHARS
        );
        let cmd = "cd ~/apps/spavn-cli && cargo test memory::";
        let squeezed = one_line(cmd, 24);
        assert_eq!(squeezed.chars().count(), 24);
        assert!(squeezed.starts_with("cd ~/apps"), "{squeezed:?}");
        assert!(squeezed.ends_with("memory::"), "{squeezed:?}");
    }

    #[test]
    fn bash_summary_is_a_single_line() {
        let args = serde_json::json!({
            "command": "cd ~/apps/spavn-cli && cargo test\nprintf 'done\\n'",
            "host": "local"
        })
        .to_string();
        let summary = summarize_tool_args("bash", &args, "local");
        assert_eq!(summary, "[local] cd ~/apps/spavn-cli && cargo test");
    }

    #[test]
    fn bash_summary_drops_the_heredoc_body_but_keeps_the_path() {
        let args = serde_json::json!({
            "command": "cd ~/apps/spavn-cli && cat > /tmp/probe2.sh <<'EOF'\n#!/bin/sh\nM=\"$1\"\nEOF",
            "host": "local"
        })
        .to_string();
        let summary = summarize_tool_args("bash", &args, "local");
        assert_eq!(
            summary,
            "[local] cd ~/apps/spavn-cli && cat > /tmp/probe2.sh <<'EOF'"
        );
    }

    #[test]
    fn path_summary_is_unchanged() {
        let args = serde_json::json!({ "path": "src/hud/menus.rs" }).to_string();
        assert_eq!(
            summarize_tool_args("read", &args, "local"),
            "src/hud/menus.rs"
        );
    }

    #[test]
    fn summary_honours_the_executor_aliases() {
        let alias_path = serde_json::json!({ "file_path": "src/tools.rs" }).to_string();
        assert_eq!(
            summarize_tool_args("read", &alias_path, "local"),
            "src/tools.rs"
        );
        let alias_cmd = serde_json::json!({ "cmd": "cargo test" }).to_string();
        assert_eq!(
            summarize_tool_args("bash", &alias_cmd, "local"),
            "[local] cargo test"
        );
        let alias_host =
            serde_json::json!({ "target": "telya-server", "command": "uptime" }).to_string();
        assert_eq!(
            summarize_tool_args("bash", &alias_host, "local"),
            "[telya-server] uptime"
        );
    }

    #[test]
    fn empty_host_falls_back_to_the_default_instead_of_rendering_brackets() {
        let args = serde_json::json!({ "host": "", "command": "uptime" }).to_string();
        assert_eq!(
            summarize_tool_args("bash", &args, "local"),
            "[local] uptime"
        );
    }

    #[test]
    fn progress_target_is_structural_not_the_display_summary() {
        let bash = serde_json::json!({ "command": "cd ~/x && cargo test" }).to_string();
        assert_eq!(tool_progress_target("bash", &bash), "cargo test");
        let edit = serde_json::json!({ "file_path": "src/hud/menus.rs" }).to_string();
        assert_eq!(tool_progress_target("edit", &edit), "src/hud/menus.rs");
        assert_eq!(tool_progress_target("web", &edit), "");
    }
}
