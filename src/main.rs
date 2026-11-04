mod config;
mod guard;
mod http;
mod hud;
mod markdown;
mod memory;
mod router;
mod session;
mod skills;
mod slash_commands;
mod spavn_engine;
mod tools;

use config::{load_user_config, tr_en};
use hud::{
    draw_bottom_dock, fetch_quick_quota_line, interactive_select_session,
    interactive_select_slash_command, read_bottom_dock_input, render_assistant_footer,
    render_assistant_header, render_welcome_header, reset_scroll_region, setup_scroll_region,
    term_size, truncate_ansi_to_width, LiveSpinner, MarkdownStreamRenderer, PromptEvent,
};
use memory::{build_system_prompt, current_workdir};
use router::{resolve_endpoint, stream_chat_completion, ChatMessage, ModelEndpoint};
use session::{
    append_message, fork_session, latest_session_path, load_session, new_session_path,
    rewrite_session, sessions_dir,
};
use skills::expand_inline_skills;
use slash_commands::{dispatch_slash_command, SlashOutcome};
use spavn_engine::{
    dispatch_role_from_prompt, record_knowledge_artifact, retrieve_relevant_artifacts, AgentPhase,
    AgentRoleKey,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tools::{execute_tool, tool_definitions_for_phase, AccessMode, ToolContext};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static RESIZED: AtomicBool = AtomicBool::new(false);
static STEER_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static PROMPT_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

pub fn push_steer_message(msg: String) {
    if let Ok(mut q) = STEER_QUEUE.lock() {
        q.push_back(msg);
    }
}

pub fn pop_steer_messages() -> Vec<String> {
    if let Ok(mut q) = STEER_QUEUE.lock() {
        q.drain(..).collect()
    } else {
        Vec::new()
    }
}

pub fn steer_message_count() -> usize {
    STEER_QUEUE.lock().map(|q| q.len()).unwrap_or(0)
}

pub fn pop_last_steer_message() -> Option<String> {
    STEER_QUEUE.lock().ok()?.pop_back()
}

pub fn clear_steer_messages() {
    if let Ok(mut q) = STEER_QUEUE.lock() {
        q.clear();
    }
}

pub fn push_queued_message(msg: String) {
    if let Ok(mut q) = PROMPT_QUEUE.lock() {
        q.push_back(msg);
    }
}

pub fn pop_next_queued_message() -> Option<String> {
    PROMPT_QUEUE.lock().ok()?.pop_front()
}

pub fn pop_last_queued_message() -> Option<String> {
    PROMPT_QUEUE.lock().ok()?.pop_back()
}

pub fn peek_last_pending_message() -> Option<(bool, String)> {
    if let Ok(sq) = STEER_QUEUE.lock() {
        if let Some(last) = sq.back() {
            return Some((true, last.clone()));
        }
    }
    if let Ok(pq) = PROMPT_QUEUE.lock() {
        if let Some(last) = pq.back() {
            return Some((false, last.clone()));
        }
    }
    None
}

pub fn pop_last_pending_message() -> Option<(bool, String)> {
    if let Some(s) = pop_last_steer_message() {
        return Some((true, s));
    }
    if let Some(q) = pop_last_queued_message() {
        return Some((false, q));
    }
    None
}

pub fn list_pending_messages() -> Vec<(bool, String)> {
    let mut out = Vec::new();
    if let Ok(sq) = STEER_QUEUE.lock() {
        for m in sq.iter() {
            out.push((true, m.clone()));
        }
    }
    if let Ok(pq) = PROMPT_QUEUE.lock() {
        for m in pq.iter() {
            out.push((false, m.clone()));
        }
    }
    out
}

pub fn list_queued_messages() -> Vec<String> {
    PROMPT_QUEUE
        .lock()
        .map(|q| q.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn remove_queued_message(idx: usize) {
    if let Ok(mut q) = PROMPT_QUEUE.lock() {
        if idx < q.len() {
            q.remove(idx);
        }
    }
}

pub fn promote_queued_message(idx: usize) {
    if let Ok(mut q) = PROMPT_QUEUE.lock() {
        if idx > 0 && idx < q.len() {
            if let Some(item) = q.remove(idx) {
                q.push_front(item);
            }
        }
    }
}

pub fn clear_queued_messages() {
    if let Ok(mut q) = PROMPT_QUEUE.lock() {
        q.clear();
    }
}

pub fn queued_message_count() -> usize {
    PROMPT_QUEUE.lock().map(|q| q.len()).unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveTaskStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Debug, Clone)]
pub struct LiveTaskItem {
    pub title: String,
    pub status: LiveTaskStatus,
}

static LIVE_TASKS: std::sync::LazyLock<Mutex<Vec<LiveTaskItem>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

pub fn list_live_tasks() -> Vec<LiveTaskItem> {
    LIVE_TASKS.lock().map(|t| t.clone()).unwrap_or_default()
}

pub fn clear_live_tasks() {
    if let Ok(mut t) = LIVE_TASKS.lock() {
        t.clear();
    }
}

pub fn add_live_task(title: String) {
    let clean = title.trim().to_string();
    if clean.is_empty() {
        return;
    }
    if let Ok(mut t) = LIVE_TASKS.lock() {
        t.push(LiveTaskItem {
            title: clean,
            status: LiveTaskStatus::Pending,
        });
    }
}

pub fn toggle_live_task(idx_1based: usize) -> bool {
    if idx_1based == 0 {
        return false;
    }
    if let Ok(mut t) = LIVE_TASKS.lock() {
        if let Some(item) = t.get_mut(idx_1based - 1) {
            item.status = match item.status {
                LiveTaskStatus::Done => LiveTaskStatus::Pending,
                _ => LiveTaskStatus::Done,
            };
            return true;
        }
    }
    false
}

/// Zero-roundtrip task extractor: parses markdown checkboxes (`- [ ]`, `- [x]`) or
/// updates active task progress based on assistant output and tool calls without extra LLM turns.
pub fn sync_live_tasks_from_text(text: &str) {
    let mut parsed = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let rest = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| {
                let bytes = trimmed.as_bytes();
                if bytes.len() >= 3
                    && bytes[0].is_ascii_digit()
                    && bytes[1] == b'.'
                    && bytes[2] == b' '
                {
                    Some(&trimmed[3..])
                } else {
                    None
                }
            });
        if let Some(r) = rest {
            let r = r.trim_start();
            if let Some(title) = r.strip_prefix("[ ] ").or_else(|| r.strip_prefix("[] ")) {
                let clean = title.trim().trim_matches('*').trim_matches('`').trim();
                if !clean.is_empty() && clean.len() <= 120 {
                    parsed.push(LiveTaskItem {
                        title: clean.to_string(),
                        status: LiveTaskStatus::Pending,
                    });
                }
            } else if let Some(title) = r
                .strip_prefix("[x] ")
                .or_else(|| r.strip_prefix("[X] "))
                .or_else(|| r.strip_prefix("[✓] "))
            {
                let clean = title.trim().trim_matches('*').trim_matches('`').trim();
                if !clean.is_empty() && clean.len() <= 120 {
                    parsed.push(LiveTaskItem {
                        title: clean.to_string(),
                        status: LiveTaskStatus::Done,
                    });
                }
            } else if let Some(title) = r.strip_prefix("[~] ").or_else(|| r.strip_prefix("[>] ")) {
                let clean = title.trim().trim_matches('*').trim_matches('`').trim();
                if !clean.is_empty() && clean.len() <= 120 {
                    parsed.push(LiveTaskItem {
                        title: clean.to_string(),
                        status: LiveTaskStatus::InProgress,
                    });
                }
            }
        }
    }
    if parsed.len() >= 2 {
        if !parsed
            .iter()
            .any(|i| i.status == LiveTaskStatus::InProgress)
        {
            if let Some(first_pending) = parsed
                .iter_mut()
                .find(|i| i.status == LiveTaskStatus::Pending)
            {
                first_pending.status = LiveTaskStatus::InProgress;
            }
        }
        if let Ok(mut t) = LIVE_TASKS.lock() {
            *t = parsed;
        }
    }
}

/// Automatically advances task progress when tools touch files mentioned in a task item
/// or when all edits are verified. Zero LLM roundtrip cost.
pub fn advance_live_tasks_on_tool(tool_name: &str, args_summary: &str) {
    if let Ok(mut t) = LIVE_TASKS.lock() {
        if t.is_empty() {
            return;
        }
        let summary_lower = args_summary.to_lowercase();
        let file_stem = std::path::Path::new(args_summary.trim())
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        let is_mutating = matches!(
            tool_name,
            "edit" | "edit_file" | "write" | "write_file" | "bash"
        );
        if is_mutating && !file_stem.is_empty() && file_stem.len() >= 3 {
            let mut matched_idx = None;
            for (idx, item) in t.iter().enumerate() {
                if item.status != LiveTaskStatus::Done
                    && (item.title.to_lowercase().contains(&file_stem)
                        || (!summary_lower.is_empty()
                            && item.title.to_lowercase().contains(&summary_lower)))
                {
                    matched_idx = Some(idx);
                    break;
                }
            }
            if let Some(idx) = matched_idx {
                t[idx].status = LiveTaskStatus::Done;
            }
        }
        if !t.iter().any(|i| i.status == LiveTaskStatus::InProgress) {
            if let Some(next_p) = t.iter_mut().find(|i| i.status == LiveTaskStatus::Pending) {
                next_p.status = LiveTaskStatus::InProgress;
            }
        }
    }
}

pub fn format_live_tasks_compact_bar(max_w: usize) -> Option<String> {
    let tasks = list_live_tasks();
    if tasks.is_empty() {
        return None;
    }
    let total = tasks.len();
    let done = tasks
        .iter()
        .filter(|t| t.status == LiveTaskStatus::Done)
        .count();
    let mut parts = Vec::with_capacity(tasks.len().min(4));
    for item in tasks.iter().take(4) {
        let short_t: String = item.title.chars().take(26).collect();
        let s_str = match item.status {
            LiveTaskStatus::Done => format!("{}✓ {}{}", GREEN, short_t, RESET),
            LiveTaskStatus::InProgress => format!("{}{}▶ {}{}", CYAN, BOLD, short_t, RESET),
            LiveTaskStatus::Pending => format!("{}○ {}{}", DIM, short_t, RESET),
        };
        parts.push(s_str);
    }
    if total > 4 {
        parts.push(format!("{}+{}{}", DIM, total - 4, RESET));
    }
    let bar_color = if done == total { GREEN } else { YELLOW };
    let raw = format!(
        "{}│{}  {}📋 [{}/{}]{} {}",
        CYAN,
        RESET,
        bar_color,
        done,
        total,
        RESET,
        parts.join(&format!(" {}·{} ", DIM, RESET))
    );
    let (trunc, _) = hud::truncate_ansi_to_width(&raw, max_w);
    Some(trunc)
}

#[cfg(unix)]
extern "C" fn handle_sigint(_sig: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
extern "C" fn handle_sigwinch(_sig: i32) {
    RESIZED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
#[link(name = "c")]
unsafe extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
}

#[cfg(windows)]
extern "system" fn handle_win_ctrl(ctrl_type: u32) -> i32 {
    if ctrl_type == 0 || ctrl_type == 1 {
        INTERRUPTED.store(true, Ordering::SeqCst);
        return 1;
    }
    0
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

pub fn install_sigint_handler() {
    #[cfg(unix)]
    unsafe {
        let _ = signal(2, handle_sigint);
        let _ = signal(28, handle_sigwinch);
    }
    #[cfg(windows)]
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handle_win_ctrl), 1);
    }
}

pub fn take_resized() -> bool {
    RESIZED.swap(false, Ordering::SeqCst)
}

pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

pub fn trigger_interrupt() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub fn clear_interrupted() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}

const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

/// Local model context guard: max read-only file reads executed per agent turn.
/// Beyond this budget the turn stops dispatching reads and asks the model to analyze
/// what it already gathered, avoiding local KV-cache prefill stalls on huge parallel reads.
const LOCAL_READ_BUDGET: usize = 5;

fn is_read_tool_name(name: &str) -> bool {
    matches!(
        name.trim().to_lowercase().as_str(),
        "read" | "read_file" | "cat" | "view"
    )
}

fn is_fast_inline_tool(name: &str) -> bool {
    matches!(
        name.trim().to_lowercase().as_str(),
        "read"
            | "read_file"
            | "cat"
            | "view"
            | "edit"
            | "edit_file"
            | "replace"
            | "patch"
            | "write"
            | "write_file"
            | "search"
            | "grep"
            | "rg"
            | "lsp"
    )
}

fn summarize_tool_args(name: &str, args_json: &str, default_host: &str) -> String {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return String::new();
    };
    match name {
        "read" | "edit" | "write" => val
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "bash" => {
            let host = val
                .get("host")
                .and_then(|v| v.as_str())
                .unwrap_or(default_host);
            let cmd = val.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let short_cmd: String = cmd.chars().take(68).collect();
            format!("[{}] {}", host, short_cmd)
        }
        "search" => val
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "memory" => {
            let op = val.get("op").and_then(|v| v.as_str()).unwrap_or("recall");
            let q = val.get("query").and_then(|v| v.as_str()).unwrap_or("");
            format!("{}: {}", op, q)
        }
        "skill" => val
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "subagent" | "spawn_submodels" | "task" => val
            .get("task")
            .or_else(|| val.get("prompt"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(64)
            .collect(),
        _ => String::new(),
    }
}

fn safe_utf8_prefix(s: &str, max_bytes: usize) -> &str {
    if max_bytes >= s.len() {
        return s;
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

fn safe_utf8_suffix(s: &str, tail_bytes: usize) -> &str {
    if tail_bytes >= s.len() {
        return s;
    }
    let mut idx = s.len() - tail_bytes;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    &s[idx..]
}

fn evict_old_skill_blocks(text: &str) -> String {
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

fn compact_messages_for_model(
    messages: &[ChatMessage],
    is_local: bool,
    max_ctx_tokens: usize,
) -> Vec<ChatMessage> {
    let raw_tok = hud::estimate_messages_tokens(messages);
    let prune_old_tools = if is_local {
        raw_tok >= 16_000
    } else {
        raw_tok >= 48_000.min((max_ctx_tokens * 40) / 100)
    };
    let len = messages.len();
    let last_user_idx = messages
        .iter()
        .rposition(|m| {
            m.role == "user"
                && !m
                    .content
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("[SPAVN VERIFICATION GATE]")
        })
        .unwrap_or_else(|| {
            messages
                .iter()
                .rposition(|m| m.role == "user")
                .unwrap_or(len)
        });
    // Anchor pruning & reasoning eviction to `last_user_idx` (stepping only every 16 messages
    // within marathon turns) so the prompt prefix stays 100% byte-identical across consecutive
    // tool calls and `llama-server` KV-cache never re-evaluates the active turn prefix.
    let turn_anchor = last_user_idx + ((len.saturating_sub(last_user_idx)) / 16) * 12;
    let cutoff = if prune_old_tools {
        if is_local {
            turn_anchor
        } else {
            len.saturating_sub(20).min(turn_anchor)
        }
    } else {
        0
    };
    // Keep `messages[0]` static for local models so KV-cache from token 0 is never invalidated.
    let live_sigs = if !is_local && prune_old_tools {
        tools::build_live_project_signatures(".")
    } else {
        String::new()
    };
    let keep_reasoning_from = if is_local { turn_anchor } else { last_user_idx };
    messages
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let mut cloned = m.clone();
            if idx == 0 && m.role == "system" && !live_sigs.is_empty() {
                if let Some(ref sys_text) = cloned.content {
                    if !sys_text.contains("[SPAVN LIVE PROJECT SIGNATURES]") {
                        cloned.content = Some(format!(
                            "{}\n\n[SPAVN LIVE PROJECT SIGNATURES — Use exact symbol/type names below across files]:\n{}",
                            sys_text, live_sigs
                        ));
                    }
                }
            }

            if idx < keep_reasoning_from {
                cloned.reasoning_content = None;
            }

            if m.role == "user" && idx != last_user_idx {
                if let Some(ref content) = cloned.content {
                    if content.contains("<active_skill") {
                        cloned.content = Some(evict_old_skill_blocks(content));
                    }
                }
            }
            if idx > 0 && idx < cutoff {
                if m.role == "tool" {
                    if let Some(content) = &m.content {
                        if content.len() > 600 {
                            let head = safe_utf8_prefix(content, 260);
                            let tail = safe_utf8_suffix(content, 160);
                            cloned.content = Some(format!(
                                "{}\n...[SPAVN Context Compactor: {} bytes pruned from older turn]...\n{}",
                                head,
                                content.len().saturating_sub(420),
                                tail
                            ));
                        }
                    }
                } else if m.role == "assistant" {
                    if let Some(tcs) = &mut cloned.tool_calls {
                        for tc in tcs.iter_mut() {
                            if tc.function.arguments.len() > 800 {
                                let path_hint = serde_json::from_str::<serde_json::Value>(
                                    &tc.function.arguments,
                                )
                                .ok()
                                .and_then(|v| {
                                    v.get("path")
                                        .or_else(|| v.get("command"))
                                        .and_then(|s| s.as_str())
                                        .map(|s| safe_utf8_prefix(s, 120).to_string())
                                })
                                .unwrap_or_default();
                                tc.function.arguments = match tc.function.name.as_str() {
                                    "edit" => serde_json::json!({
                                        "path": path_hint,
                                        "old_text": "/* compacted */",
                                        "new_text": "/* compacted */"
                                    }),
                                    "write" => serde_json::json!({
                                        "path": path_hint,
                                        "content": "/* compacted */"
                                    }),
                                    "bash" => serde_json::json!({
                                        "command": path_hint
                                    }),
                                    _ => serde_json::json!({
                                        "path": path_hint
                                    }),
                                }
                                .to_string();
                            }
                        }
                    }
                }
            }
            cloned
        })
        .collect()
}

fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| {
            let c_len = m.content.as_ref().map(|s| s.len()).unwrap_or(0);
            let r_len = m.reasoning_content.as_ref().map(|s| s.len()).unwrap_or(0);
            (c_len + r_len) / 4 + 12
        })
        .sum()
}

pub(crate) fn perform_manual_compact(
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

/// True when the reply announces an action ("let me read...", "şimdi ... okuyacağım", trailing `:`)
/// but stops without calling a tool. Questions and "let me know" sign-offs are final answers.
fn is_premature_stop(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('?') || trimmed.ends_with("```") {
        return false;
    }
    let tail_lines: Vec<&str> = trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let last_line = tail_lines.last().copied().unwrap_or("");
    let lower_tail = tail_lines.join(" ").to_lowercase();
    let announces_en = (lower_tail.contains("let me ") && !lower_tail.contains("let me know"))
        || lower_tail.contains("i will now ")
        || lower_tail.contains("i'll now ");
    let announces_tr = lower_tail.contains("şimdi ")
        && (lower_tail.contains("yapacağım")
            || lower_tail.contains("düzenleyeceğim")
            || lower_tail.contains("okuyacağım")
            || lower_tail.contains("inceleyeceğim"));
    last_line.ends_with(':') || announces_tr || announces_en
}

fn run_agent_turn(
    endpoint: &ModelEndpoint,
    ctx: &mut ToolContext,
    messages: &mut Vec<ChatMessage>,
    session_file: &PathBuf,
    show_thinking: bool,
    quota_badge: Option<&str>,
    quiet: bool,
) -> Result<(), String> {
    clear_interrupted();
    hud::set_sticky_dock_status(
        endpoint,
        ctx.access,
        &ctx.phase,
        &ctx.default_host,
        quota_badge.unwrap_or("aktif"),
    );
    let tools_spec = tool_definitions_for_phase(ctx.phase);
    let mut last_tool_sig = String::new();
    let mut repeat_count = 0usize;
    let mut unverified_edits = false;
    let mut compiler_verified_in_edit = false;
    let mut verification_nudged = false;
    let mut modified_files_in_turn: Vec<String> = Vec::new();
    let mut auto_verify_rounds = 0usize;
    let mut empty_stop_retries = 0usize;
    let mut length_stop_retries = 0usize;
    let mut premature_stop_retries = 0usize;
    let cfg = load_user_config();
    // OMP-style Unpaired Tool-Call Recovery: if the last message in history is an assistant
    // message with tool_calls (e.g. from an interrupted session), execute/resolve them before calling LLM.
    if let Some(last_msg) = messages.last().cloned() {
        if last_msg.role == "assistant" {
            if let Some(pending_tcs) = last_msg.tool_calls {
                for tc in pending_tcs {
                    let recovery_msg = ChatMessage::tool_result(
                        tc.id,
                        tc.function.name,
                        "[SPAVN Turn Recovery: Previous tool execution was interrupted before completion.]".to_string(),
                    );
                    append_message(session_file, &recovery_msg);
                    messages.push(recovery_msg);
                }
            }
        }
    }

    // OMP-style Unbounded Turn Loop (`while (true)`): continues until all tool calls,
    // verification gates, recovery retries, and queued steering/follow-up messages settle.
    loop {
        if is_interrupted() {
            if !quiet {
                println!("\n{}⚡ [İşlem Esc/Ctrl+C ile durduruldu]{}", YELLOW, RESET);
            }
            return Ok(());
        }

        // Inject any live steering messages typed by the user while the turn was running!
        let steered = pop_steer_messages();
        if !steered.is_empty() {
            for sm in steered {
                println!(
                    "\n{}⚡ [Canlı Müdahale (Steer) Aktif Tura Enjekte Edildi]: {}{}{}",
                    YELLOW, BOLD, sm, RESET
                );
                let steer_msg = ChatMessage::user(format!(
                    "[LIVE USER STEERING INSTRUCTION — Prioritize this immediately]: {}",
                    sm
                ));
                append_message(session_file, &steer_msg);
                messages.push(steer_msg);
            }
        }

        let escalated_cloud = endpoint.is_local() && repeat_count >= 2;
        let step_endpoint = if escalated_cloud {
            if !quiet {
                println!(
                    "\n{}⚡ [SPAVN Hibrit Escalation: Yerel model döngüye girdi -> Bu adım {} ile çözülüyor...]{}",
                    MAGENTA, cfg.fallback_cloud_model, RESET
                );
            }
            resolve_endpoint(&cfg.fallback_cloud_model)
        } else {
            endpoint.clone()
        };

        let max_ctx_tokens = step_endpoint.context_window_tokens();
        let mut view_messages =
            compact_messages_for_model(messages, step_endpoint.is_local(), max_ctx_tokens);
        let mut view_tok = hud::estimate_messages_tokens(&view_messages);
        let compact_threshold = ((max_ctx_tokens * 80) / 100).min(120_000);
        if view_tok >= compact_threshold && messages.len() > 6 {
            let raw_before_tok = hud::estimate_messages_tokens(messages);
            perform_manual_compact(&step_endpoint, messages, None);
            rewrite_session(session_file, messages);
            view_messages =
                compact_messages_for_model(messages, step_endpoint.is_local(), max_ctx_tokens);
            view_tok = hud::estimate_messages_tokens(&view_messages);
            if !quiet {
                println!(
                    "{}╰─ ⚡ {}: {} → {}{}",
                    GREEN,
                    tr_en(
                        "[SPAVN Auto-Compact sonrası işlem otomatik devam ediyor]",
                        "[SPAVN Auto-Continuing after context compaction]"
                    ),
                    hud::format_token_short(raw_before_tok),
                    hud::format_token_short(view_tok),
                    RESET
                );
            }
        }
        hud::set_context_window_usage(view_tok, max_ctx_tokens);
        let mut spinner = if quiet {
            LiveSpinner::silent()
        } else {
            LiveSpinner::start(tr_en("Düşünülüyor", "Thinking"))
        };
        let content_started = std::cell::Cell::new(false);
        let content_chars = std::cell::Cell::new(0usize);
        let partial_content = RefCell::new(String::new());
        let md_renderer = RefCell::new(MarkdownStreamRenderer::new());

        let result = match stream_chat_completion(
            &step_endpoint,
            &view_messages,
            &tools_spec,
            |think_delta| {
                if !quiet {
                    hud::stream_thinking_delta(think_delta, show_thinking);
                }
            },
            |content_delta| {
                partial_content.borrow_mut().push_str(content_delta);
                if quiet {
                    return;
                }
                content_chars.set(content_chars.get() + content_delta.chars().count());
                hud::set_spinner_label(&format!(
                    "✍️ {} ({} {})",
                    tr_en("Yanıt yazılıyor", "Writing response"),
                    content_chars.get(),
                    tr_en("krk", "ch")
                ));
                if !content_started.get() {
                    content_started.set(true);
                    render_assistant_header(&step_endpoint.key);
                }
                md_renderer.borrow_mut().push(content_delta);
            },
        ) {
            Ok(r) => r,
            Err(err) => {
                if content_started.get() {
                    md_renderer.borrow_mut().finish();
                }
                hud::close_thinking_box_if_open();
                spinner.stop();
                if content_started.get() && !quiet {
                    println!("{}╰─{}", CYAN, RESET);
                }
                let partial_trimmed = partial_content.borrow().trim().to_string();
                if !partial_trimmed.is_empty() {
                    let saved_partial = ChatMessage {
                        role: "assistant".to_string(),
                        content: Some(format!("{}\n\n*[Esc ile durduruldu]*", partial_trimmed)),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                        thought_signature: None,
                        reasoning_content: None,
                    };
                    append_message(session_file, &saved_partial);
                    messages.push(saved_partial);
                }
                if is_interrupted() {
                    println!(
                        "\n{}⚡ {}{}",
                        YELLOW,
                        tr_en(
                            "[İşlem Esc/Ctrl+C ile iptal edildi]",
                            "[Operation cancelled via Esc/Ctrl+C]"
                        ),
                        RESET
                    );
                    return Ok(());
                }
                if step_endpoint.is_local() {
                    if !quiet {
                        println!(
                            "\n{}🔄 {} ({}) — {}{}",
                            CYAN,
                            tr_en(
                                "Yerel model bağlantısı yenileniyor",
                                "Reconnecting to local model"
                            ),
                            step_endpoint.key,
                            err,
                            RESET
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    let mut retry_local_spinner = if quiet {
                        LiveSpinner::silent()
                    } else {
                        LiveSpinner::start(tr_en(
                            "Yerel model yeniden deneniyor",
                            "Retrying local model",
                        ))
                    };
                    let local_retry_res = stream_chat_completion(
                        &step_endpoint,
                        &view_messages,
                        &tools_spec,
                        |think_delta| {
                            if !quiet {
                                hud::stream_thinking_delta(think_delta, show_thinking);
                            }
                        },
                        |c| {
                            partial_content.borrow_mut().push_str(c);
                            if quiet {
                                return;
                            }
                            content_chars.set(content_chars.get() + c.chars().count());
                            hud::set_spinner_label(&format!(
                                "✍️ {} ({} {})",
                                tr_en("Yanıt yazılıyor", "Writing response"),
                                content_chars.get(),
                                tr_en("krk", "ch")
                            ));
                            if !content_started.get() {
                                content_started.set(true);
                                render_assistant_header(&step_endpoint.key);
                            }
                            md_renderer.borrow_mut().push(c);
                        },
                    );
                    if content_started.get() {
                        md_renderer.borrow_mut().finish();
                    }
                    if !quiet {
                        hud::close_thinking_box_if_open();
                    }
                    retry_local_spinner.stop();

                    match local_retry_res {
                        Ok(ok_local) => ok_local,
                        Err(local_err2) => {
                            let fb_key = cfg.fallback_cloud_model.trim();
                            if fb_key.is_empty()
                                || fb_key.eq_ignore_ascii_case("none")
                                || fb_key == step_endpoint.key
                            {
                                return Err(local_err2);
                            }
                            if !quiet {
                                println!(
                                    "\n{}⚠️  {} ({}) -> {} {}{}",
                                    YELLOW,
                                    tr_en("Yerel model yanıt vermedi", "Local model unreachable"),
                                    local_err2,
                                    fb_key,
                                    tr_en(
                                        "modeline otomatik geçiliyor...",
                                        "failing over automatically..."
                                    ),
                                    RESET
                                );
                            }
                            let fallback_ep = resolve_endpoint(fb_key);
                            let mut fb_spinner = if quiet {
                                LiveSpinner::silent()
                            } else {
                                LiveSpinner::start(tr_en("Düşünülüyor", "Thinking"))
                            };
                            let fb_res = stream_chat_completion(
                                &fallback_ep,
                                &view_messages,
                                &tools_spec,
                                |think_delta| {
                                    if !quiet {
                                        hud::stream_thinking_delta(think_delta, show_thinking);
                                    }
                                },
                                |c| {
                                    partial_content.borrow_mut().push_str(c);
                                    if quiet {
                                        return;
                                    }
                                    content_chars.set(content_chars.get() + c.chars().count());
                                    hud::set_spinner_label(&format!(
                                        "✍️ {} ({} {})",
                                        tr_en("Yanıt yazılıyor", "Writing response"),
                                        content_chars.get(),
                                        tr_en("krk", "ch")
                                    ));
                                    if !content_started.get() {
                                        content_started.set(true);
                                        render_assistant_header(&fallback_ep.key);
                                    }
                                    md_renderer.borrow_mut().push(c);
                                },
                            );
                            if content_started.get() {
                                md_renderer.borrow_mut().finish();
                            }
                            if !quiet {
                                hud::close_thinking_box_if_open();
                            }
                            fb_spinner.stop();
                            fb_res?
                        }
                    }
                } else {
                    return Err(err);
                }
            }
        };

        if content_started.get() {
            md_renderer.borrow_mut().finish();
        }
        if !quiet {
            hud::close_thinking_box_if_open();
        }
        spinner.stop();

        let assistant_msg = result.message;
        if let Some(ref c_text) = assistant_msg.content {
            sync_live_tasks_from_text(c_text);
        }
        append_message(session_file, &assistant_msg);
        messages.push(assistant_msg.clone());
        let est_after = hud::estimate_messages_tokens(messages);
        let actual_used = (result.usage.total_tokens as usize).max(est_after);
        hud::set_context_window_usage(actual_used, step_endpoint.context_window_tokens());

        let secs = (result.elapsed_ms as f64) / 1000.0;
        let decode_secs = (result.elapsed_ms.saturating_sub(result.ttft_ms).max(1) as f64) / 1000.0;
        let tok_s = if decode_secs > 0.0 && result.usage.completion_tokens > 0 {
            (result.usage.completion_tokens as f64) / decode_secs
        } else {
            0.0
        };

        if let Some(tool_calls) = assistant_msg.tool_calls {
            let mut local_read_count = 0usize;
            for tc in tool_calls {
                let tool_name = tc.function.name.trim().to_string();
                if endpoint.is_local()
                    && is_read_tool_name(&tool_name)
                    && local_read_count >= LOCAL_READ_BUDGET
                {
                    let tool_msg = ChatMessage::tool_result(
                        tc.id,
                        tool_name,
                        format!(
                            "[SPAVN Local Context Budget]: {} files read in this step; analyze these first before reading more.",
                            LOCAL_READ_BUDGET
                        ),
                    );
                    append_message(session_file, &tool_msg);
                    messages.push(tool_msg);
                    continue;
                }
                if endpoint.is_local() && is_read_tool_name(&tool_name) {
                    local_read_count += 1;
                }
                let sig = format!("{}:{}", tool_name, tc.function.arguments.trim());
                if sig == last_tool_sig {
                    repeat_count += 1;
                } else {
                    last_tool_sig = sig;
                    repeat_count = 0;
                }

                let summary_for_task =
                    summarize_tool_args(&tool_name, &tc.function.arguments, &ctx.default_host);
                let (mut tool_output, tool_ms) = if quiet {
                    let t0 = std::time::Instant::now();
                    let out = execute_tool(ctx, &tool_name, &tc.function.arguments);
                    (out, t0.elapsed().as_millis())
                } else {
                    let summary = summary_for_task.clone();
                    println!(
                        "{}╭─ ⚡ {}{} {}{}{}",
                        YELLOW, BOLD, tool_name, RESET, DIM, summary
                    );
                    let tool_started = std::time::Instant::now();
                    let out = if is_fast_inline_tool(&tool_name) && ctx.default_host == "local" {
                        execute_tool(ctx, &tool_name, &tc.function.arguments)
                    } else {
                        let mut tool_spinner =
                            LiveSpinner::start(&format!("⚡ {} çalıştırılıyor", tool_name));
                        let res = execute_tool(ctx, &tool_name, &tc.function.arguments);
                        tool_spinner.stop();
                        res
                    };
                    (out, tool_started.elapsed().as_millis())
                };

                if ctx.phase == AgentPhase::Code
                    && matches!(
                        tool_name.as_str(),
                        "edit" | "edit_file" | "replace" | "write" | "write_file"
                    )
                {
                    if tool_output.starts_with("Successfully ") {
                        unverified_edits = true;
                        compiler_verified_in_edit = tool_output
                            .contains("[Compiler Check: cargo check OK]")
                            || tool_output.contains("[Compiler Check: tsc OK]");
                        if let Ok(v) =
                            serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                        {
                            if let Some(p) = v
                                .get("path")
                                .or_else(|| v.get("file_path"))
                                .or_else(|| v.get("file"))
                                .and_then(|s| s.as_str())
                            {
                                let p_str = p.trim().to_string();
                                if !p_str.is_empty() && !modified_files_in_turn.contains(&p_str) {
                                    modified_files_in_turn.push(p_str);
                                }
                            }
                        }
                    }
                } else if matches!(
                    tool_name.as_str(),
                    "bash" | "run_command" | "shell" | "exec"
                ) {
                    unverified_edits = false;
                }
                if repeat_count >= 2 || tool_output.starts_with("[EOF STOP:") {
                    tool_output.push_str(&format!(
                        "\n\n[SPAVN LOOP GUARD (count={})]: Stop repeating `{}` or reading past EOF. Proceed directly to your analysis or edit using the context already gathered.",
                        repeat_count + 1,
                        tool_name
                    ));
                }

                let is_err_tool = tool_output.starts_with("Edit error:")
                    || tool_output.starts_with("Read error")
                    || tool_output.starts_with("Write error")
                    || tool_output.starts_with("BLOCKED")
                    || tool_output.starts_with("Execution error:");
                if !is_err_tool {
                    advance_live_tasks_on_tool(&tool_name, &summary_for_task);
                }

                if !quiet {
                    let preview_line = tool_output
                        .lines()
                        .next()
                        .unwrap_or("(tamamlandı)")
                        .chars()
                        .take(80)
                        .collect::<String>();
                    let is_err = is_err_tool;
                    let status_mark = if is_err {
                        format!("{}✗{}", RED, YELLOW)
                    } else {
                        format!("{}✓{}", GREEN, YELLOW)
                    };
                    let (_, cols) = term_size();
                    let full_w = cols.saturating_sub(2).max(20);
                    let raw_footer = format!(
                        "{}╰─ {} {}{} ({} satır · {}ms){}",
                        YELLOW,
                        status_mark,
                        DIM,
                        preview_line,
                        tool_output.lines().count().max(1),
                        tool_ms,
                        RESET
                    );
                    let (trunc_footer, _) = truncate_ansi_to_width(&raw_footer, full_w);
                    println!("{}", trunc_footer);
                }

                let tool_msg = ChatMessage::tool_result(tc.id, tool_name, tool_output);
                append_message(session_file, &tool_msg);
                messages.push(tool_msg);
            }
            continue;
        }

        // If user injected a live steer message while the final text was streaming, continue the loop immediately!
        let post_steered = pop_steer_messages();
        if !post_steered.is_empty() {
            for sm in post_steered {
                println!(
                    "\n{}⚡ [Canlı Müdahale (Steer) Aktif Tura Enjekte Edildi]: {}{}{}",
                    YELLOW, BOLD, sm, RESET
                );
                let steer_msg = ChatMessage::user(format!(
                    "[LIVE USER STEERING INSTRUCTION — Prioritize this immediately]: {}",
                    sm
                ));
                append_message(session_file, &steer_msg);
                messages.push(steer_msg);
            }
            continue;
        }
        // 1. Max-Tokens (`finish_reason == "length"`) Auto-Continuation (OMP `length-stop-retry`)
        if result.finish_reason.as_deref() == Some("length") && length_stop_retries < 3 {
            length_stop_retries += 1;
            if !quiet {
                println!(
                    "\n{}⚡ [SPAVN Turn Recovery: Çıktı token sınırına ulaştı ({}/3) -> Kaldığı yerden devam ediliyor...]{}",
                    YELLOW, length_stop_retries, RESET
                );
            }
            let cont_msg = ChatMessage::user(
                "[SPAVN LENGTH-STOP RECOVERY]: Your previous response hit the output token limit (`finish_reason=length`). Continue immediately from the exact point where you stopped without repeating prior output.",
            );
            append_message(session_file, &cont_msg);
            messages.push(cont_msg);
            continue;
        }

        // 2. Empty Assistant Stop Recovery (OMP `empty-stop-retry`)
        let text_trimmed = assistant_msg.content.as_deref().unwrap_or("").trim();
        let reasoning_trimmed = assistant_msg
            .reasoning_content
            .as_deref()
            .unwrap_or("")
            .trim();
        if text_trimmed.is_empty() && !reasoning_trimmed.is_empty() {
            let recovered_from_reasoning = hud::strip_internal_tags_for_display(reasoning_trimmed);
            if !recovered_from_reasoning.is_empty() {
                if !quiet && !content_started.get() {
                    content_started.set(true);
                    render_assistant_header(&step_endpoint.key);
                    let mut r = MarkdownStreamRenderer::new();
                    r.push(&recovered_from_reasoning);
                    r.finish();
                }
                if let Some(last_m) = messages.last_mut() {
                    last_m.content = Some(recovered_from_reasoning.clone());
                }
                rewrite_session(session_file, messages);
                if !quiet {
                    render_assistant_footer(
                        &step_endpoint.key,
                        ctx.access.short_badge(),
                        result.ttft_ms,
                        secs,
                        tok_s,
                    );
                }
                break;
            }
        }
        if text_trimmed.is_empty() && empty_stop_retries < 2 {
            empty_stop_retries += 1;
            if !quiet {
                println!(
                    "\n{}⚡ [SPAVN Turn Recovery: Boş yanıt tespit edildi ({}/2) -> İşlem otomatik devam ediyor...]{}",
                    YELLOW, empty_stop_retries, RESET
                );
            }
            let empty_nudge = ChatMessage::user(
                "[SPAVN EMPTY-STOP RECOVERY]: You returned an empty response with no tool call and no text. Call the next required tool or provide the final answer now.",
            );
            append_message(session_file, &empty_nudge);
            messages.push(empty_nudge);
            continue;
        }

        // 3. Premature Action-Announcement Stop Recovery (OMP `unexpected-stop-retry`)
        let looks_like_premature_stop = is_premature_stop(text_trimmed);
        if looks_like_premature_stop && premature_stop_retries < 2 {
            premature_stop_retries += 1;
            if !quiet {
                println!(
                    "\n{}⚡ [SPAVN Turn Recovery: Yarım kalan aksiyon tespit edildi ({}/2) -> Araç çağrısı tetikleniyor...]{}",
                    YELLOW, premature_stop_retries, RESET
                );
            }
            let u_nudge = ChatMessage::user(
                "[SPAVN UNEXPECTED-STOP RECOVERY]: You announced an action you were about to take but stopped without calling a tool. Call the concrete tool now.",
            );
            append_message(session_file, &u_nudge);
            messages.push(u_nudge);
            continue;
        }

        // 4. Autonomous Zero-Stop Verification Gate after file edits (up to 4 auto-fix rounds)
        if !modified_files_in_turn.is_empty() && auto_verify_rounds < 4 {
            let cwd_str = ctx.policy.cwd.to_string_lossy();
            let gate_res = if compiler_verified_in_edit {
                tools::check_unfinished_placeholders(&modified_files_in_turn)
            } else {
                tools::run_project_verification_gate(&cwd_str, &modified_files_in_turn)
            };
            if let Some(gate_err) = gate_res {
                auto_verify_rounds += 1;
                unverified_edits = false;
                compiler_verified_in_edit = false;
                if !quiet {
                    println!(
                        "\n{}⚡ [SPAVN Zero-Stop Doğrulama Kapısı ({}/4): Derleme/eksik kod tespit edildi -> Otomatik düzeltme tetikleniyor...]{}",
                        YELLOW, auto_verify_rounds, RESET
                    );
                }
                let gate_msg = ChatMessage::user(gate_err);
                append_message(session_file, &gate_msg);
                messages.push(gate_msg);
                continue;
            } else if compiler_verified_in_edit
                || ctx.policy.cwd.join("Cargo.toml").is_file()
                || ctx.policy.cwd.join("tsconfig.json").is_file()
            {
                unverified_edits = false;
            }
        }
        if unverified_edits && !verification_nudged {
            verification_nudged = true;
            unverified_edits = false;
            if !quiet {
                println!(
                    "{}⚡ [SPAVN Otonom Doğrulama Kapısı: Dosya değişikliği sonrası derleme/test kontrolü tetikleniyor...]{}",
                    YELLOW, RESET
                );
            }
            let nudge = ChatMessage::user(
                "[SPAVN VERIFICATION GATE]: You modified file(s) via edit/write without running a verification check (`bash` build/test). Run a verification command now, then report the final result.",
            );
            append_message(session_file, &nudge);
            messages.push(nudge);
            continue;
        }

        if !quiet && (content_started.get() || result.usage.total_tokens > 0) {
            render_assistant_footer(
                &step_endpoint.key,
                ctx.access.short_badge(),
                result.ttft_ms,
                secs,
                tok_s,
            );
        }

        break;
    }

    Ok(())
}

pub(crate) fn render_session_resume_preview(session_path: &Path, messages: &[ChatMessage]) {
    let fname = session_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let short_name = fname
        .trim_start_matches("session-")
        .trim_end_matches(".jsonl");
    let text_msgs: Vec<&ChatMessage> = messages
        .iter()
        .filter(|m| {
            (m.role == "user" || m.role == "assistant")
                && m.content
                    .as_deref()
                    .map(|s| !hud::strip_internal_tags_for_display(s).is_empty())
                    .unwrap_or(false)
        })
        .collect();
    let last_user_preview = text_msgs
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_deref())
        .map(hud::strip_internal_tags_for_display)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains('╭') && !l.contains('╰') && !l.contains('│'))
        .collect::<Vec<_>>()
        .join(" ");
    let snippet: String = last_user_preview.chars().take(56).collect();
    let suffix = if snippet.is_empty() {
        String::new()
    } else if last_user_preview.chars().count() > 56 {
        format!(" · \"{}…\"", snippet)
    } else {
        format!(" · \"{}\"", snippet)
    };

    let (_, cols) = term_size();
    let full_w = cols.saturating_sub(2).max(6);
    let raw = format!(
        "{}╭─ 🗂️  {}{}{} {}({} {}{}){}",
        CYAN,
        BOLD,
        short_name,
        RESET,
        DIM,
        text_msgs.len(),
        tr_en("mesaj", "msgs"),
        suffix,
        RESET
    );
    let (trunc, _) = truncate_ansi_to_width(&raw, full_w);
    println!("\n{}", trunc);
}

fn main() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        reset_scroll_region();
        default_hook(info);
    }));
    install_sigint_handler();
    let cfg = load_user_config();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut model_key = std::env::var("SPAVN_MODEL").unwrap_or_else(|_| cfg.default_model.clone());
    let mut access_mode = AccessMode::Auto;
    let mut default_host = "local".to_string();
    let mut resume_last = false;
    let mut resume_session_arg: Option<String> = None;
    let mut interactive_resume = false;
    let mut force_interactive = false;
    let mut show_thinking = true;
    let mut quiet = false;
    let mut json_output = false;
    let mut phase_opt: Option<AgentPhase> = None;
    let mut prompt_parts: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-m" | "--model" => {
                if i + 1 < args.len() {
                    model_key = args[i + 1].clone();
                    i += 2;
                    continue;
                }
            }
            "-a" | "--access" => {
                if i + 1 < args.len() {
                    if let Some(m) = AccessMode::from_str(&args[i + 1]) {
                        access_mode = m;
                    }
                    i += 2;
                    continue;
                }
            }
            "-P" | "--phase" => {
                if i + 1 < args.len() {
                    if let Some(ph) = AgentPhase::from_str(&args[i + 1]) {
                        phase_opt = Some(ph);
                    }
                    i += 2;
                    continue;
                }
            }
            "-H" | "--host" => {
                if i + 1 < args.len() {
                    default_host = args[i + 1].clone();
                    i += 2;
                    continue;
                }
            }
            "-s" | "--session" => {
                if i + 1 < args.len() {
                    resume_session_arg = Some(args[i + 1].clone());
                    resume_last = true;
                    i += 2;
                    continue;
                }
            }
            "-i" | "--interactive" => {
                force_interactive = true;
                i += 1;
                continue;
            }
            "-c" | "--continue" => {
                resume_last = true;
                i += 1;
                continue;
            }
            "-r" | "--resume" => {
                interactive_resume = true;
                i += 1;
                continue;
            }
            "-t" | "--think" => {
                show_thinking = !show_thinking;
                i += 1;
                continue;
            }
            "-q" | "--quiet" => {
                quiet = true;
                i += 1;
                continue;
            }
            "-j" | "--json" => {
                quiet = true;
                json_output = true;
                i += 1;
                continue;
            }
            "-v" | "-V" | "--version" => {
                println!(
                    "spavn v{} ({}/{})",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                );
                return;
            }
            "-h" | "--help" => {
                println!(
                    "SPAVN v{} ({}/{}) — Native Rust Coding Agent\n\
                     Usage: spavn [options] [prompt...]\n\n\
                     Options:\n\
                     \x20 -m, --model <key>           Select model (default: {})\n\
                     \x20 -a, --access <mode>         Access mode: safe | auto | sovereign\n\
                     \x20 -P, --phase <phase>         Agent phase: plan | code\n\
                     \x20 -H, --host <host>           Default target host (default: local)\n\
                     \x20 -s, --session <id|fork:id>  Resume or fork a specific session\n\
                     \x20 -c, --continue              Continue latest session\n\
                     \x20 -r, --resume                Interactive session picker\n\
                     \x20 -i, --interactive           Start interactive TUI after initial prompt\n\
                     \x20 -t, --think                 Toggle thinking stream display\n\
                     \x20 -q, --quiet                 Quiet/headless output mode\n\
                     \x20 -j, --json                  Structured JSON output mode\n\
                     \x20 -V, --version               Print version and exit\n\
                     \x20 -h, --help                  Print this help message",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    cfg.default_model
                );
                return;
            }
            _ => {
                prompt_parts.push(args[i].clone());
            }
        }
        i += 1;
    }

    let cwd = current_workdir();
    let mut endpoint = resolve_endpoint(&model_key);
    let mut active_phase = phase_opt.unwrap_or(AgentPhase::Code);
    let mut ctx = ToolContext::new(access_mode, default_host, PathBuf::from(&cwd), active_phase);

    let resolve_session_target = |raw_spec: &str| -> Option<(PathBuf, Vec<ChatMessage>)> {
        let (do_fork, spec) = match raw_spec.strip_prefix("fork:") {
            Some(rest) => (true, rest.trim()),
            None => (false, raw_spec.trim()),
        };
        let target_path = if spec.is_empty() || spec.eq_ignore_ascii_case("latest") {
            latest_session_path()?
        } else {
            let direct = PathBuf::from(spec);
            if direct.exists() {
                direct
            } else {
                let in_dir = sessions_dir().join(spec);
                if in_dir.exists() {
                    in_dir
                } else {
                    let with_ext = sessions_dir().join(format!("{}.jsonl", spec));
                    if with_ext.exists() {
                        with_ext
                    } else {
                        return None;
                    }
                }
            }
        };
        let loaded = load_session(&target_path);
        if loaded.is_empty() {
            return None;
        }
        if do_fork {
            let forked = fork_session(&loaded);
            Some((forked, loaded))
        } else {
            Some((target_path, loaded))
        }
    };

    let (mut session_file, mut messages) = if let Some(ref spec) = resume_session_arg {
        if let Some(pair) = resolve_session_target(spec) {
            pair
        } else {
            let p = new_session_path();
            let sys = ChatMessage::system(build_system_prompt(&cwd, endpoint.is_local()));
            append_message(&p, &sys);
            (p, vec![sys])
        }
    } else if resume_last {
        if let Some(path) = latest_session_path() {
            let loaded = load_session(&path);
            if !loaded.is_empty() {
                (path, loaded)
            } else {
                let p = new_session_path();
                let sys = ChatMessage::system(build_system_prompt(&cwd, endpoint.is_local()));
                append_message(&p, &sys);
                (p, vec![sys])
            }
        } else {
            let p = new_session_path();
            let sys = ChatMessage::system(build_system_prompt(&cwd, endpoint.is_local()));
            append_message(&p, &sys);
            (p, vec![sys])
        }
    } else {
        let p = new_session_path();
        let sys = ChatMessage::system(build_system_prompt(&cwd, endpoint.is_local()));
        append_message(&p, &sys);
        (p, vec![sys])
    };

    if let Some(first) = messages.first_mut().filter(|m| m.role == "system") {
        *first = ChatMessage::system(build_system_prompt(&cwd, endpoint.is_local()));
    }

    if !prompt_parts.is_empty() && force_interactive {
        push_queued_message(prompt_parts.join(" "));
        prompt_parts.clear();
    }

    if !prompt_parts.is_empty() {
        if quiet {
            show_thinking = false;
        }
        let raw_text = prompt_parts.join(" ");
        let (mut expanded_text, activated_skills) = expand_inline_skills(&raw_text);
        if !activated_skills.is_empty() && !quiet {
            println!(
                "{}🧩 Aktif Yetenek(ler) Yüklendi: @{}{}",
                GREEN,
                activated_skills.join(", @"),
                RESET
            );
        }
        if let Some(phase_preamble) = ctx.phase.preamble() {
            expanded_text = format!("{}\n\n{}", phase_preamble, expanded_text);
        }
        let user_msg = ChatMessage::user(expanded_text);
        append_message(&session_file, &user_msg);
        messages.push(user_msg);

        if let Err(err) = run_agent_turn(
            &endpoint,
            &mut ctx,
            &mut messages,
            &session_file,
            show_thinking,
            None,
            quiet,
        ) {
            if json_output {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "error",
                        "model": endpoint.key,
                        "session": session_file.to_string_lossy(),
                        "error": err,
                    })
                );
            } else if quiet {
                eprintln!("Hata: {}", err);
            } else {
                eprintln!("{}✗ Hata: {}{}", RED, err, RESET);
            }
            std::process::exit(1);
        }
        if json_output {
            let reply = messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant")
                .and_then(|m| m.content.as_deref())
                .unwrap_or("")
                .trim_end();
            println!(
                "{}",
                serde_json::json!({
                    "status": "ok",
                    "model": endpoint.key,
                    "phase": ctx.phase.as_str(),
                    "session": session_file.to_string_lossy(),
                    "reply": reply,
                    "messages_count": messages.len(),
                })
            );
        } else if quiet {
            if let Some(last) = messages.iter().rev().find(|m| m.role == "assistant") {
                if let Some(text) = last.content.as_deref() {
                    if !text.trim().is_empty() {
                        println!("{}", text.trim_end());
                    }
                }
            }
        }
        return;
    }

    let mut session_name = session_file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    setup_scroll_region();
    render_welcome_header(
        &endpoint,
        ctx.access,
        &ctx.default_host,
        &cwd,
        &session_name,
    );

    if !endpoint.is_local() && (endpoint.api_key.is_empty() || endpoint.api_key == "local") {
        println!(
            "\n{}👋 {}{}",
            YELLOW,
            tr_en(
                "İlk kurulum: Henüz aktif bir API anahtarı veya yerel model yapılandırılmamış. Sağlayıcı kurulum sihirbazı açılıyor (/login)...",
                "First-run setup: No active API key or local model configured yet. Opening provider setup wizard (/login)..."
            ),
            RESET
        );
        if let Some(new_model_key) = hud::interactive_provider_login(None) {
            endpoint = resolve_endpoint(&new_model_key);
            if let Some(first) = messages.first_mut().filter(|m| m.role == "system") {
                *first = ChatMessage::system(build_system_prompt(&cwd, endpoint.is_local()));
                rewrite_session(&session_file, &messages);
            }
        }
    }

    let mut quota_badge = fetch_quick_quota_line();
    let mut cmd_history: Vec<String> = Vec::new();
    let mut active_role = AgentRoleKey::Auto;
    let mut prefill_input: Option<String> = None;

    if interactive_resume {
        if let Some(chosen_path) = interactive_select_session(&session_file) {
            let loaded = load_session(&chosen_path);
            if !loaded.is_empty() {
                session_file = chosen_path;
                session_name = session_file
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                messages = loaded;
                if let Some(first) = messages.first_mut().filter(|m| m.role == "system") {
                    *first = ChatMessage::system(build_system_prompt(&cwd, endpoint.is_local()));
                }
                render_session_resume_preview(&session_file, &messages);
            }
        }
    } else if resume_last {
        render_session_resume_preview(&session_file, &messages);
    }

    loop {
        let max_win = endpoint.context_window_tokens();
        let cur_view = compact_messages_for_model(&messages, endpoint.is_local(), max_win);
        hud::set_context_window_usage(hud::estimate_messages_tokens(&cur_view), max_win);
        // Automatically pop next queued message if available and no edit prefill is pending!
        let mut input = if prefill_input.is_none() {
            if let Some(queued_msg) = pop_next_queued_message() {
                println!(
                    "\n{}📥 [Kuyruktan Otomatik Çalıştırılıyor ({} kalan)]: {}{}{}",
                    GREEN,
                    queued_message_count(),
                    BOLD,
                    queued_msg,
                    RESET
                );
                queued_msg
            } else {
                let ev = read_bottom_dock_input(
                    &endpoint,
                    &mut ctx.access,
                    &active_phase,
                    &ctx.default_host,
                    &quota_badge,
                    &cmd_history,
                    None,
                    &cwd,
                    &session_name,
                    &messages,
                );
                match ev {
                    PromptEvent::Exit => {
                        reset_scroll_region();
                        break;
                    }
                    PromptEvent::CycleAccess => continue,
                    PromptEvent::CyclePhase => {
                        active_phase = active_phase.toggled();
                        ctx.phase = active_phase;
                        println!(
                            "\n{}{} — phase değişti{}",
                            CYAN,
                            active_phase.badge(),
                            RESET
                        );
                        continue;
                    }
                    PromptEvent::ShowSkills => "/skills".to_string(),
                    PromptEvent::Reload => "/reload".to_string(),
                    PromptEvent::Submit(s) => s,
                }
            }
        } else {
            let pre = prefill_input.take();
            let ev = read_bottom_dock_input(
                &endpoint,
                &mut ctx.access,
                &active_phase,
                &ctx.default_host,
                &quota_badge,
                &cmd_history,
                pre.as_deref(),
                &cwd,
                &session_name,
                &messages,
            );
            match ev {
                PromptEvent::Exit => {
                    reset_scroll_region();
                    break;
                }
                PromptEvent::CycleAccess => continue,
                PromptEvent::CyclePhase => {
                    active_phase = active_phase.toggled();
                    ctx.phase = active_phase;
                    println!(
                        "\n{}{} — phase değişti{}",
                        CYAN,
                        active_phase.badge(),
                        RESET
                    );
                    continue;
                }
                PromptEvent::ShowSkills => "/skills".to_string(),
                PromptEvent::Reload => "/reload".to_string(),
                PromptEvent::Submit(s) => s,
            }
        };

        if input.is_empty() {
            continue;
        }
        if input == "/" || input == "/menu" {
            if let Some(chosen_cmd) = interactive_select_slash_command() {
                input = chosen_cmd;
            } else {
                continue;
            }
        }
        cmd_history.push(input.clone());

        match dispatch_slash_command(
            &input,
            &cwd,
            &mut endpoint,
            &mut ctx,
            &mut active_phase,
            &mut active_role,
            &mut show_thinking,
            &mut quota_badge,
            &mut prefill_input,
            &mut session_file,
            &mut session_name,
            &mut messages,
        ) {
            SlashOutcome::ExitLoop => break,
            SlashOutcome::Handled => continue,
            SlashOutcome::NotSlash => {}
        }

        let (mut expanded_input, activated_skills) = expand_inline_skills(&input);
        let dispatched_role = if active_role == AgentRoleKey::Auto {
            dispatch_role_from_prompt(&input)
        } else {
            active_role
        };
        if !endpoint.is_local() {
            let relevant_artifacts = retrieve_relevant_artifacts(&cwd, &input, 2);
            if !relevant_artifacts.is_empty() {
                let mut art_block = String::from("<spavn_knowledge_artifacts>\n");
                for a in &relevant_artifacts {
                    let mut unique_files = Vec::new();
                    for f in &a.files {
                        if !unique_files.contains(f) {
                            unique_files.push(f.clone());
                        }
                    }
                    art_block.push_str(&format!(
                        "- [{}] ({}): {}\n",
                        a.ts,
                        unique_files.join(", "),
                        a.summary
                    ));
                }
                art_block.push_str("</spavn_knowledge_artifacts>\n\n");
                expanded_input = format!("{}{}", art_block, expanded_input);
            }
        }
        if let Some(phase_preamble) = active_phase.preamble() {
            if matches!(
                dispatched_role,
                AgentRoleKey::Reviewer | AgentRoleKey::Security | AgentRoleKey::Docs
            ) {
                expanded_input = format!("{}\n\n{}", dispatched_role.preamble(), expanded_input);
            }
            expanded_input = format!("{}\n\n{}", phase_preamble, expanded_input);
        } else {
            expanded_input = format!("{}\n\n{}", dispatched_role.preamble(), expanded_input);
        }

        let user_header_badge = if active_phase == AgentPhase::Plan {
            active_phase.badge()
        } else {
            dispatched_role.badge()
        };
        let dragged_imgs = crate::router::extract_dragged_images_info(&input);
        let mut display_input = input.clone();
        for img in &dragged_imgs {
            let escaped = img.path.replace(' ', "\\ ");
            display_input = display_input
                .replace(&escaped, &format!("[🖼️ {}]", img.filename))
                .replace(&img.path, &format!("[🖼️ {}]", img.filename));
        }
        println!(
            "\n{}╭─ 👤 Siz · {}{}{}",
            CYAN, DIM, user_header_badge, RESET
        );
        for line in display_input.lines() {
            println!("{}│{} {}{}{}", CYAN, RESET, BOLD, line, RESET);
        }
        for img in &dragged_imgs {
            hud::render_inline_image_preview(img);
        }
        if !activated_skills.is_empty() {
            println!(
                "{}│{} {}🧩 Enjekte Edilen Yetenek: @{}{}",
                CYAN,
                RESET,
                GREEN,
                activated_skills.join(", @"),
                RESET
            );
        }
        println!("{}╰─{}", CYAN, RESET);
        ctx.current_model = endpoint.key.clone();
        ctx.current_prompt = input.clone();
        let undo_before = ctx.undo_stack.len();

        let user_msg = ChatMessage::user(expanded_input);
        append_message(&session_file, &user_msg);
        messages.push(user_msg);
        if let Err(err) = run_agent_turn(
            &endpoint,
            &mut ctx,
            &mut messages,
            &session_file,
            show_thinking,
            Some(&quota_badge),
            false,
        ) {
            eprintln!("\n{}✗ Hata: {}{}", RED, err, RESET);
        }

        if ctx.undo_stack.len() > undo_before {
            let touched: Vec<String> = ctx.undo_stack[undo_before..]
                .iter()
                .map(|u| u.target.clone())
                .collect();
            record_knowledge_artifact(&cwd, &touched, &input);
        }
        draw_bottom_dock(&endpoint, ctx.access, &ctx.default_host, &quota_badge, "");
    }
}

#[cfg(test)]
mod premature_stop_tests {
    use super::is_premature_stop;

    #[test]
    fn detects_announcements_not_final_answers() {
        for yes in [
            "Let me read the file first.",
            "Şimdi dosyayı okuyacağım.",
            "I'll now run the tests",
            "Next step:",
        ] {
            assert!(is_premature_stop(yes), "{yes}");
        }
        for no in [
            "Done. Let me know if you need anything else.",
            "Bitti. Başka bir şey ister misin?",
            "Tamamlandı, testler geçiyor.",
            "Which file should I edit?",
            "### Değişiklikler:\n- `src/main.rs` güncellendi.\n- `src/router.rs` düzeltildi.",
            "Örnek:\n```rust\nfn main() {}\n```",
        ] {
            assert!(!is_premature_stop(no), "{no}");
        }
    }

    #[test]
    fn queue_and_steer_pop_last_for_live_edit_and_cancel() {
        super::clear_queued_messages();
        super::clear_steer_messages();
        super::push_queued_message("ilk görev".to_string());
        super::push_queued_message("ikinci görev".to_string());
        assert_eq!(
            super::peek_last_pending_message(),
            Some((false, "ikinci görev".to_string()))
        );
        assert_eq!(
            super::pop_last_pending_message(),
            Some((false, "ikinci görev".to_string()))
        );
        super::push_steer_message("acil müdahale".to_string());
        assert_eq!(
            super::pop_last_pending_message(),
            Some((true, "acil müdahale".to_string()))
        );
        assert_eq!(
            super::pop_last_pending_message(),
            Some((false, "ilk görev".to_string()))
        );
        assert_eq!(super::pop_last_pending_message(), None);
    }

    #[test]
    fn local_compact_messages_preserves_kv_prefix_across_turn_steps() {
        use crate::router::ChatMessage;
        let mut msgs = vec![
            ChatMessage::system("system prompt".to_string()),
            ChatMessage::user("first task".to_string()),
        ];
        let make_assistant = |txt: String, reasoning: String| ChatMessage {
            role: "assistant".to_string(),
            content: Some(txt),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            thought_signature: None,
            reasoning_content: Some(reasoning),
        };
        for i in 0..8 {
            msgs.push(make_assistant(format!("step {i}"), "r".repeat(4000)));
            msgs.push(ChatMessage::tool_result(
                format!("call_{i}"),
                "read",
                "x".repeat(6000),
            ));
        }
        msgs.push(ChatMessage::user(
            "second task: audit commit and fix test".to_string(),
        ));
        for step in 0..5 {
            msgs.push(make_assistant(
                format!("active step {step}"),
                format!("thinking {step}"),
            ));
            msgs.push(ChatMessage::tool_result(
                format!("active_call_{step}"),
                "read",
                "y".repeat(1200),
            ));
        }
        let view_a = super::compact_messages_for_model(&msgs, true, 131_072);
        // Append one more assistant + tool step in the same turn
        msgs.push(make_assistant(
            "active step 5".to_string(),
            "thinking 5".to_string(),
        ));
        msgs.push(ChatMessage::tool_result(
            "active_call_5".to_string(),
            "read",
            "y".repeat(1200),
        ));
        let view_b = super::compact_messages_for_model(&msgs, true, 131_072);
        assert_eq!(view_a.len() + 2, view_b.len());
        for i in 0..view_a.len() {
            assert_eq!(
                view_a[i].content, view_b[i].content,
                "KV prefix content mutated at message index {i}"
            );
            assert_eq!(
                view_a[i].reasoning_content, view_b[i].reasoning_content,
                "KV prefix reasoning_content mutated at message index {i}"
            );
        }
    }
}
