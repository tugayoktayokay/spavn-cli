use crate::config::{expand_user_path, home_dir, load_user_config};
use crate::guard::{assess_shell_command, PathPolicy, PathRisk, ShellRisk};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    Safe,
    Auto,
    Sovereign,
}

impl AccessMode {
    pub fn badge(&self) -> &'static str {
        match self {
            AccessMode::Safe => {
                crate::config::tr_en("🔒 SAFE (Her Adımda Onay)", "🔒 SAFE (Ask Every Step)")
            }
            AccessMode::Auto => {
                crate::config::tr_en("⚡ AUTO (Akıllı Onay)", "⚡ AUTO (Smart Approval)")
            }
            AccessMode::Sovereign => crate::config::tr_en(
                "🔥 SOVEREIGN (Tam Otonom)",
                "🔥 SOVEREIGN (Full Autonomous)",
            ),
        }
    }

    pub fn short_badge(&self) -> &'static str {
        match self {
            AccessMode::Safe => "🔒 SAFE",
            AccessMode::Auto => "⚡ AUTO",
            AccessMode::Sovereign => "🔥 SOVEREIGN",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            AccessMode::Safe => AccessMode::Auto,
            AccessMode::Auto => AccessMode::Sovereign,
            AccessMode::Sovereign => AccessMode::Safe,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "safe" | "1" | "ask" => Some(AccessMode::Safe),
            "auto" | "2" | "default" | "normal" => Some(AccessMode::Auto),
            "sovereign" | "3" | "full" | "god" | "yolo" => Some(AccessMode::Sovereign),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct UndoSnapshot {
    pub target: String,
    pub previous_content: Option<String>,
    pub description: String,
}

pub struct ToolContext {
    pub access: AccessMode,
    pub default_host: String,
    pub undo_stack: Vec<UndoSnapshot>,
    pub read_mtimes: std::collections::HashMap<String, std::time::SystemTime>,
    pub read_ranges: std::collections::HashMap<
        String,
        (std::time::SystemTime, Option<usize>, Option<usize>, usize),
    >,
    pub tool_step_counter: usize,
    pub current_model: String,
    pub current_prompt: String,
    pub policy: PathPolicy,
}

impl ToolContext {
    pub fn new(access: AccessMode, default_host: String, workspace_root: PathBuf) -> Self {
        let policy = PathPolicy::new(&workspace_root);
        Self {
            access,
            default_host,
            undo_stack: Vec::new(),
            read_mtimes: std::collections::HashMap::new(),
            read_ranges: std::collections::HashMap::new(),
            tool_step_counter: 0,
            current_model: "default".to_string(),
            current_prompt: String::new(),
            policy,
        }
    }

    fn enforce_path(&mut self, raw_path: &str, write: bool, verb: &str) -> Result<(), String> {
        match self.policy.check(raw_path, write) {
            PathRisk::Allow => Ok(()),
            PathRisk::Deny { reason } => Err(format!(
                "BLOCKED by SPAVN Path Sandbox: {} ({} — {})",
                reason, verb, raw_path
            )),
            PathRisk::Ask {
                reason,
                even_sovereign,
            } => {
                if !even_sovereign && self.access == AccessMode::Sovereign {
                    return Ok(());
                }
                if write && self.access == AccessMode::Safe {
                    return Ok(());
                }
                if prompt_user_approval(self, &format!("{} — {}", reason, verb), raw_path) {
                    Ok(())
                } else {
                    Err(format!(
                        "BLOCKED by SPAVN Path Sandbox: kullanıcı izin vermedi ({} — {})",
                        verb, raw_path
                    ))
                }
            }
        }
    }

    pub fn record_file_mtime(&mut self, raw_path: &str) {
        if split_remote_prefix(raw_path).is_some() {
            return;
        }
        let path = expand_tilde(raw_path);
        if let Ok(mtime) = fs::metadata(&path).and_then(|m| m.modified()) {
            let key = path.to_string_lossy().to_string();
            self.read_mtimes.insert(key.clone(), mtime);
            self.read_ranges.remove(&key);
        }
    }

    pub fn check_external_mtime_shift(&self, raw_path: &str) -> String {
        if split_remote_prefix(raw_path).is_some() {
            return String::new();
        }
        let path = expand_tilde(raw_path);
        let key = path.to_string_lossy().to_string();
        if let Some(&recorded) = self.read_mtimes.get(&key) {
            if let Ok(current) = fs::metadata(&path).and_then(|m| m.modified()) {
                if current > recorded {
                    println!(
                        "{}⚠️  [Code-Shifting Guard]: {} dosyası son okumadan (read) sonra dışarıdan değiştirildi!{}",
                        YELLOW, raw_path, RESET
                    );
                    return " [CODE-SHIFTING GUARD WARNING: File was modified externally since last read — verify surrounding lines]".to_string();
                }
            }
        }
        String::new()
    }

    pub fn pop_undo(&mut self) -> Option<String> {
        let snap = self.undo_stack.pop()?;
        match snap.previous_content {
            Some(prev) => {
                if let Err(e) = write_target_file(&snap.target, &prev) {
                    return Some(format!("Undo hatası ({}): {}", snap.target, e));
                }
                Some(format!(
                    "↺ Geri alındı: {} ({})",
                    snap.target, snap.description
                ))
            }
            None => {
                remove_target_file(&snap.target);
                Some(format!("↺ Oluşturulan dosya silindi: {}", snap.target))
            }
        }
    }
}

pub fn tool_definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file or directory locally or on a configured remote host ('<host>:/path'). Supports optional start_line and end_line (1-based).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File or directory path (optionally prefixed with '<host>:')" },
                        "start_line": { "type": "integer", "description": "Optional 1-based start line" },
                        "end_line": { "type": "integer", "description": "Optional 1-based inclusive end line" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Surgical hashless file edit. Replaces old_text with new_text (with whitespace-normalized fallback).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to file to edit" },
                        "old_text": { "type": "string", "description": "Exact existing snippet to replace" },
                        "new_text": { "type": "string", "description": "Replacement snippet" }
                    },
                    "required": ["path", "old_text", "new_text"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write",
                "description": "Create or overwrite a file locally or on a configured remote host ('<host>:/path').",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Target file path" },
                        "content": { "type": "string", "description": "Full file content to write" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Execute a shell command on local machine or a configured remote host.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to execute" },
                        "host": { "type": "string", "description": "Target host alias (default: 'local')" },
                        "cwd": { "type": "string", "description": "Optional working directory" },
                        "timeout_sec": { "type": "integer", "description": "Command timeout in seconds (default: 45)" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search",
                "description": "Fast ripgrep (rg) regex search across files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex pattern to search for" },
                        "path": { "type": "string", "description": "Directory or file path to search (default '.')" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "memory",
                "description": "Search or record persistent project/user memory and architectural decisions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["recall", "remember"], "description": "'recall' to search memory, 'remember' to save a note/decision" },
                        "query": { "type": "string", "description": "Search query (for recall) or note text (for remember)" },
                        "kind": { "type": "string", "description": "For remember: 'durum', 'karar', 'bilgi', or 'not'" }
                    },
                    "required": ["op", "query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "skill",
                "description": "Load specialized domain instructions on-demand from ~/.spavn/skills or ~/.claude/skills.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Skill name to load" }
                    },
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web",
                "description": "Search the live internet (`op='search'`, `query`) or fetch clean Markdown from a URL (`op='fetch'`, `url`).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["search", "fetch"], "description": "'search' for web/docs search, 'fetch' to read a URL as clean Markdown" },
                        "query": { "type": "string", "description": "Search query (when op='search') or URL (when op='fetch')" },
                        "url": { "type": "string", "description": "Target http/https URL when op='fetch'" }
                    },
                    "required": ["op"]
                }
            }
        }
    ])
}

fn expand_tilde(path_str: &str) -> PathBuf {
    if path_str == "~" {
        return home_dir();
    }
    expand_user_path(path_str)
}

fn resolve_ssh_target(alias: &str) -> String {
    let cfg = load_user_config();
    for h in &cfg.hosts {
        if h.key.eq_ignore_ascii_case(alias) {
            return h.ssh_target.clone();
        }
    }
    alias.to_string()
}

fn split_remote_prefix(raw: &str) -> Option<(String, &str)> {
    let (prefix, rest) = raw.split_once(':')?;
    if prefix.is_empty() || prefix.contains('/') || prefix.contains('\\') || prefix.len() == 1 {
        return None;
    }
    let cfg = load_user_config();
    for h in &cfg.hosts {
        if h.key != "local"
            && (h.key.eq_ignore_ascii_case(prefix) || h.ssh_target.eq_ignore_ascii_case(prefix))
        {
            return Some((h.ssh_target.clone(), rest));
        }
    }
    None
}

fn shell_quote_remote_path(raw: &str) -> String {
    let escape_single = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    if let Some(rest) = raw.strip_prefix("~/") {
        format!("~/{}", escape_single(rest))
    } else if raw == "~" {
        "~".to_string()
    } else {
        escape_single(raw)
    }
}

fn read_target_file(raw_path: &str) -> Result<String, String> {
    if let Some((ssh_host, remote_path)) = split_remote_prefix(raw_path) {
        let quoted = shell_quote_remote_path(remote_path);
        let out = Command::new("ssh")
            .args([
                "-o",
                "ConnectTimeout=5",
                &ssh_host,
                &format!("cat -- {}", quoted),
            ])
            .output()
            .map_err(|e| format!("SSH error: {}", e))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(format!(
                "Remote read error ({}): {}",
                ssh_host,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    } else {
        let path = expand_tilde(raw_path);
        fs::read_to_string(&path).map_err(|e| {
            let hint = suggest_similar_path(raw_path);
            format!("Read error ({}): {}{}", path.display(), e, hint)
        })
    }
}

fn suggest_similar_path(raw_path: &str) -> String {
    let target_name = Path::new(raw_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if target_name.is_empty() {
        return String::new();
    }
    let cwd = crate::memory::current_workdir();
    fn walk_find(dir: &Path, target: &str, depth: usize) -> Option<PathBuf> {
        if depth > 3 {
            return None;
        }
        let rd = fs::read_dir(dir).ok()?;
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') || name_str == "target" || name_str == "node_modules" {
                continue;
            }
            if p.is_file() && name_str.eq_ignore_ascii_case(target) {
                return Some(p);
            } else if p.is_dir() {
                if let Some(found) = walk_find(&p, target, depth + 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    if let Some(found) = walk_find(Path::new(&cwd), target_name, 0) {
        let rel = found.strip_prefix(Path::new(&cwd)).unwrap_or(&found);
        return format!(" -> Did you mean '{}'?", rel.display());
    }
    String::new()
}
fn write_target_file(raw_path: &str, content: &str) -> Result<(), String> {
    if let Some((ssh_host, remote_path)) = split_remote_prefix(raw_path) {
        let quoted = shell_quote_remote_path(remote_path);
        let mut child = Command::new("ssh")
            .args([
                "-o",
                "ConnectTimeout=5",
                &ssh_host,
                &format!("mkdir -p \"$(dirname -- {})\" && cat > {}", quoted, quoted),
            ])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("SSH write error: {}", e))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(content.as_bytes());
        }
        match child.wait() {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("Remote write failed with status {}", s)),
            Err(e) => Err(format!("Remote write error: {}", e)),
        }
    } else {
        let path = expand_tilde(raw_path);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&path, content).map_err(|e| format!("Write error ({}): {}", path.display(), e))
    }
}

fn remove_target_file(raw_path: &str) {
    if let Some((ssh_host, remote_path)) = split_remote_prefix(raw_path) {
        let quoted = shell_quote_remote_path(remote_path);
        let _ = Command::new("ssh")
            .args([
                "-o",
                "ConnectTimeout=5",
                &ssh_host,
                &format!("rm -f -- {}", quoted),
            ])
            .output();
    } else {
        let path = expand_tilde(raw_path);
        let _ = fs::remove_file(path);
    }
}

fn run_command_with_timeout(
    mut cmd: Command,
    timeout_sec: u64,
) -> Result<(i32, Vec<u8>, Vec<u8>, bool), String> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = stdout_pipe {
            let _ = std::io::Read::read_to_end(&mut p, &mut buf);
        }
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = stderr_pipe {
            let _ = std::io::Read::read_to_end(&mut p, &mut buf);
        }
        buf
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_sec.max(1));
    let mut timed_out = false;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(0),
            Ok(None) => {
                if crate::is_interrupted() {
                    let _ = child.kill();
                    let _ = child.wait();
                    let out_bytes = out_handle.join().unwrap_or_default();
                    let mut err_bytes = err_handle.join().unwrap_or_default();
                    err_bytes
                        .extend_from_slice(b"\n[SPAVN Interrupt: Komut Esc/Ctrl+C ile durduruldu]");
                    return Ok((-2, out_bytes, err_bytes, false));
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break -1;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(e) => return Err(e.to_string()),
        }
    };

    let out_bytes = out_handle.join().unwrap_or_default();
    let err_bytes = err_handle.join().unwrap_or_default();
    Ok((exit_code, out_bytes, err_bytes, timed_out))
}

fn prompt_user_approval(ctx: &mut ToolContext, action_title: &str, detail: &str) -> bool {
    crate::hud::pause_active_spinner();
    println!(
        "\n{}╭─ 🛡️  {} ({}){}",
        YELLOW,
        crate::config::tr_en("SPAVN ERİŞİM KAPISI", "SPAVN ACCESS GATE"),
        ctx.access.short_badge(),
        RESET
    );
    println!("{}│{} {}{}{}", YELLOW, RESET, BOLD, action_title, RESET);
    for line in detail.lines().take(6) {
        println!("{}│{} {}{}{}", YELLOW, RESET, DIM, line, RESET);
    }
    print!(
        "{}╰─ {}: {}",
        YELLOW,
        crate::config::tr_en(
            "Onaylıyor musunuz? [E = Evet / h = Hayır / s = Sovereign Moda Geç]",
            "Approve? [Y = Yes / n = No / s = Switch to Sovereign Mode]"
        ),
        RESET
    );
    let _ = io::stdout().flush();

    let mut ans = String::new();
    if io::stdin().read_line(&mut ans).is_err() {
        crate::hud::resume_active_spinner();
        return false;
    }
    let clean = ans.trim().to_lowercase();
    if clean == "s" || clean == "sovereign" {
        ctx.access = AccessMode::Sovereign;
        println!(
            "{}🔥 {}{}",
            GREEN,
            crate::config::tr_en(
                "Erişim modu SOVEREIGN (Tam Otonom) olarak güncellendi.",
                "Access mode switched to SOVEREIGN (Full Autonomous)."
            ),
            RESET
        );
        crate::hud::resume_active_spinner();
        return true;
    }
    let approved =
        clean.is_empty() || clean == "e" || clean == "evet" || clean == "y" || clean == "yes";
    crate::hud::resume_active_spinner();
    approved
}

pub fn render_diff_preview(path_display: &str, old_text: &str, new_text: &str) {
    crate::hud::pause_active_spinner();
    println!("{}╭─ 📝 Diff: {}{}", DIM, path_display, RESET);
    for line in old_text.lines().take(12) {
        println!("{}│ {}- {}{}", DIM, RED, line, RESET);
    }
    if old_text.lines().count() > 12 {
        println!(
            "{}│ {}  ... ({} {}){}",
            DIM,
            RED,
            old_text.lines().count(),
            crate::config::tr_en("satır silindi", "lines removed"),
            RESET
        );
    }
    for line in new_text.lines().take(12) {
        println!("{}│ {}+ {}{}", DIM, GREEN, line, RESET);
    }
    if new_text.lines().count() > 12 {
        println!(
            "{}│ {}  ... ({} {}){}",
            DIM,
            GREEN,
            new_text.lines().count(),
            crate::config::tr_en("satır eklendi", "lines added"),
            RESET
        );
    }
    println!(
        "{}╰──────────────────────────────────────────────────────────────{}",
        DIM, RESET
    );
    crate::hud::resume_active_spinner();
}

fn format_lines(content: &str, start_line: Option<usize>, end_line: Option<usize>) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let s = start_line.unwrap_or(1).max(1);
    let e = end_line.unwrap_or(total).min(total);
    if s > total || s > e {
        return format!(
            "(File has {} lines; requested range {}..{} is empty)",
            total, s, e
        );
    }

    let slice = &lines[(s - 1)..e];
    if slice.len() > 240 && start_line.is_none() && end_line.is_none() {
        let mut out = String::new();
        for (i, line) in slice.iter().take(120).enumerate() {
            out.push_str(&format!("{:>4}: {}\n", s + i, line));
        }
        out.push_str(&format!(
            "\n... [SPAVN Precision Optimizer: omitted {} middle lines of {} total; pass start_line/end_line to inspect] ...\n\n",
            slice.len() - 180,
            total
        ));
        for (i, line) in slice.iter().enumerate().skip(slice.len() - 60) {
            out.push_str(&format!("{:>4}: {}\n", s + i, line));
        }
        return out;
    }

    let mut out = String::new();
    for (i, line) in slice.iter().enumerate() {
        out.push_str(&format!("{:>4}: {}\n", s + i, line));
    }
    out
}

fn get_str_alias<'a>(args: &'a Value, keys: &[&str]) -> &'a str {
    for k in keys {
        if let Some(s) = args.get(*k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s;
            }
        }
    }
    ""
}

fn exec_read(ctx: &mut ToolContext, args: &Value) -> String {
    let raw_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
    if let Err(blocked) = ctx.enforce_path(raw_path, false, "read") {
        return blocked;
    }
    let start_line = args
        .get("start_line")
        .or_else(|| args.get("offset"))
        .or_else(|| args.get("start"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let end_line = args
        .get("end_line")
        .or_else(|| args.get("end"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    if let Some((ssh_host, remote_path)) = split_remote_prefix(raw_path) {
        let quoted = shell_quote_remote_path(remote_path);
        let remote_cmd = format!(
            "if [ -d {q} ]; then ls -1p {q} | head -n 200; else cat -- {q}; fi",
            q = quoted
        );
        let out = Command::new("ssh")
            .args(["-o", "ConnectTimeout=5", &ssh_host, &remote_cmd])
            .output();
        return match out {
            Ok(o) if o.status.success() => {
                format_lines(&String::from_utf8_lossy(&o.stdout), start_line, end_line)
            }
            Ok(o) => format!(
                "Remote read error ({}): {}",
                ssh_host,
                String::from_utf8_lossy(&o.stderr)
            ),
            Err(e) => format!("SSH error: {}", e),
        };
    }

    let path = expand_tilde(raw_path);
    if path.is_dir() {
        let mut entries = Vec::new();
        if let Ok(rd) = fs::read_dir(&path) {
            for entry in rd.flatten().take(200) {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                entries.push(if is_dir { format!("{}/", name) } else { name });
            }
        }
        entries.sort();
        return entries.join("\n");
    }

    let key = path.to_string_lossy().to_string();
    if let Ok(mtime) = fs::metadata(&path).and_then(|m| m.modified()) {
        if let Some(&(prev_mtime, prev_s, prev_e, prev_step)) = ctx.read_ranges.get(&key) {
            if prev_mtime == mtime
                && prev_s == start_line
                && prev_e == end_line
                && ctx.tool_step_counter.saturating_sub(prev_step) <= 5
            {
                return format!(
                    "[SPAVN Read Dedup: {} is unchanged on disk since your read {} step(s) ago. Its exact content is already in your context — refer to that earlier read result instead of re-reading.]",
                    raw_path,
                    ctx.tool_step_counter.saturating_sub(prev_step).max(1)
                );
            }
        }
    }

    match fs::read_to_string(&path) {
        Ok(content) => {
            ctx.record_file_mtime(raw_path);
            if let Ok(mtime) = fs::metadata(&path).and_then(|m| m.modified()) {
                ctx.read_ranges
                    .insert(key, (mtime, start_line, end_line, ctx.tool_step_counter));
            }
            format_lines(&content, start_line, end_line)
        }
        Err(e) => {
            let hint = suggest_similar_path(raw_path);
            format!("Read error ({}): {}{}", path.display(), e, hint)
        }
    }
}

fn strip_copied_line_numbers(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let mut stripped = Vec::with_capacity(lines.len());
    let mut stripped_any = false;
    for line in lines {
        let trimmed = line.trim_start();
        let digits_len = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits_len > 0 && digits_len <= 6 {
            let rest = &trimmed[digits_len..];
            if let Some(after_colon) = rest.strip_prefix(": ") {
                stripped.push(after_colon);
                stripped_any = true;
                continue;
            } else if let Some(after_colon) = rest.strip_prefix(':') {
                stripped.push(after_colon);
                stripped_any = true;
                continue;
            }
        }
        stripped.push(line);
    }
    if stripped_any {
        Some(stripped.join("\n"))
    } else {
        None
    }
}

fn normalize_curly_quotes(s: &str) -> String {
    s.replace(['“', '”'], "\"")
        .replace(['‘', '’'], "'")
        .replace("\r\n", "\n")
}

fn run_instant_syntax_check(raw_path: &str, content: &str) -> String {
    if split_remote_prefix(raw_path).is_some() {
        return String::new();
    }
    let path = expand_tilde(raw_path);
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return String::new();
    };
    match ext {
        "json" => match serde_json::from_str::<Value>(content) {
            Ok(_) => " [Syntax Check: Valid JSON]".to_string(),
            Err(e) => format!(
                "\n[AUTO-DIAGNOSTIC WARNING: Invalid JSON syntax in {}: {}]",
                raw_path, e
            ),
        },
        "py" => {
            if let Ok(out) = Command::new("python3")
                .args(["-m", "py_compile", &path.to_string_lossy()])
                .output()
            {
                if out.status.success() {
                    " [Syntax Check: Python OK]".to_string()
                } else {
                    format!(
                        "\n[AUTO-DIAGNOSTIC ERROR in {}]:\n{}",
                        raw_path,
                        String::from_utf8_lossy(&out.stderr).trim()
                    )
                }
            } else {
                String::new()
            }
        }
        "js" | "mjs" | "cjs" => {
            if let Ok(out) = Command::new("node")
                .args(["--check", &path.to_string_lossy()])
                .output()
            {
                if out.status.success() {
                    " [Syntax Check: JS OK]".to_string()
                } else {
                    format!(
                        "\n[AUTO-DIAGNOSTIC ERROR in {}]:\n{}",
                        raw_path,
                        String::from_utf8_lossy(&out.stderr).trim()
                    )
                }
            } else {
                String::new()
            }
        }
        "sh" | "zsh" => {
            if let Ok(out) = Command::new(crate::config::shell_bin())
                .args(["-n", &path.to_string_lossy()])
                .output()
            {
                if out.status.success() {
                    " [Syntax Check: Shell OK]".to_string()
                } else {
                    format!(
                        "\n[AUTO-DIAGNOSTIC ERROR in {}]:\n{}",
                        raw_path,
                        String::from_utf8_lossy(&out.stderr).trim()
                    )
                }
            } else {
                String::new()
            }
        }
        "rs" | "ts" | "tsx" | "go" | "swift" => {
            let open_braces = content.matches('{').count();
            let close_braces = content.matches('}').count();
            if open_braces != close_braces {
                format!(
                    "\n[AUTO-DIAGNOSTIC WARNING in {}: Brace count mismatch ('{{': {}, '}}': {}) — verify block closure]",
                    raw_path, open_braces, close_braces
                )
            } else {
                format!(" [Syntax Check: {} Braces Balanced]", ext.to_uppercase())
            }
        }
        _ => String::new(),
    }
}

fn exec_edit(ctx: &mut ToolContext, args: &Value) -> String {
    let raw_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
    if let Err(blocked) = ctx.enforce_path(raw_path, true, "edit") {
        return blocked;
    }
    let old_text = get_str_alias(
        args,
        &["old_text", "old_string", "old_str", "search", "target"],
    );
    let new_text = args
        .get("new_text")
        .or_else(|| args.get("new_string"))
        .or_else(|| args.get("new_str"))
        .or_else(|| args.get("replace"))
        .or_else(|| args.get("replacement"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if old_text.is_empty() {
        return "Edit error: old_text cannot be empty".to_string();
    }

    let leaked_secrets = crate::spavn_engine::detect_secrets_in_text(new_text);
    if !leaked_secrets.is_empty() && ctx.access != AccessMode::Sovereign {
        let secret_msg = format!(
            "🛡️  [SPAVN Secret Guard]: {} içinde hassas anahtar ({}) tespit edildi — onaylıyor musunuz?",
            raw_path,
            leaked_secrets.join(", ")
        );
        if !prompt_user_approval(ctx, &secret_msg, new_text) {
            return format!(
                "BLOCKED by SPAVN Secret Guard: {} contains potential secret(s): {}. Use environment variables instead.",
                raw_path,
                leaked_secrets.join(", ")
            );
        }
    }

    if ctx.access == AccessMode::Safe {
        if !prompt_user_approval(
            ctx,
            &format!("Dosya Düzenleme (edit): {}", raw_path),
            new_text,
        ) {
            return "User rejected edit in SAFE mode.".to_string();
        }
    }

    let mtime_warn = ctx.check_external_mtime_shift(raw_path);

    let content = match read_target_file(raw_path) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let mut effective_old = old_text.to_string();
    let mut effective_new = new_text.to_string();
    if !content.contains(&effective_old) {
        if let Some(stripped_old) = strip_copied_line_numbers(&effective_old) {
            if content.contains(&stripped_old) {
                effective_old = stripped_old;
                if let Some(stripped_new) = strip_copied_line_numbers(&effective_new) {
                    effective_new = stripped_new;
                }
            }
        }
    }
    if !content.contains(&effective_old) {
        let norm_old = normalize_curly_quotes(&effective_old);
        if norm_old != effective_old && content.contains(&norm_old) {
            effective_old = norm_old;
            effective_new = normalize_curly_quotes(&effective_new);
        }
    }
    let old_text = effective_old.as_str();
    let new_text = effective_new.as_str();

    let exact_matches: Vec<usize> = content
        .match_indices(old_text)
        .map(|(idx, _)| idx)
        .collect();
    if exact_matches.len() > 1 {
        let match_lines: Vec<String> = exact_matches
            .iter()
            .map(|&pos| (content[..pos].lines().count().max(1)).to_string())
            .collect();
        return format!(
            "Edit error: old_text matches {} locations in {} (lines: {}). Include 2-3 surrounding lines in old_text to disambiguate.",
            exact_matches.len(),
            raw_path,
            match_lines.join(", ")
        );
    }

    if let Some(&pos) = exact_matches.first() {
        ctx.undo_stack.push(UndoSnapshot {
            target: raw_path.to_string(),
            previous_content: Some(content.clone()),
            description: "edit".to_string(),
        });
        let mut updated = String::with_capacity(content.len() + new_text.len());
        updated.push_str(&content[..pos]);
        updated.push_str(new_text);
        updated.push_str(&content[pos + old_text.len()..]);
        if let Err(e) = write_target_file(raw_path, &updated) {
            return e;
        }
        ctx.record_file_mtime(raw_path);
        crate::spavn_engine::record_audit_action(
            "file_edit",
            &ctx.current_model,
            &ctx.default_host,
            raw_path,
            new_text.lines().count(),
            old_text.lines().count(),
            &ctx.current_prompt,
        );
        render_diff_preview(raw_path, old_text, new_text);
        let diag = run_instant_syntax_check(raw_path, &updated);
        return format!(
            "Successfully edited {} (exact match — file state is current in context, no need to re-read){}{}",
            raw_path, mtime_warn, diag
        );
    }

    let src_lines: Vec<&str> = content.lines().collect();
    let needle_lines: Vec<&str> = old_text.lines().map(|l| l.trim()).collect();
    if !needle_lines.is_empty() && needle_lines.len() <= src_lines.len() {
        let mut ws_hits: Vec<usize> = Vec::new();
        for start_idx in 0..=(src_lines.len() - needle_lines.len()) {
            let matches = src_lines[start_idx..start_idx + needle_lines.len()]
                .iter()
                .zip(needle_lines.iter())
                .all(|(a, b)| a.trim() == *b);
            if matches {
                ws_hits.push(start_idx);
            }
        }
        if ws_hits.len() > 1 {
            let hit_lines: Vec<String> = ws_hits.iter().map(|&idx| (idx + 1).to_string()).collect();
            return format!(
                "Edit error: old_text matches {} locations in {} (lines: {}). Include more surrounding context.",
                ws_hits.len(),
                raw_path,
                hit_lines.join(", ")
            );
        }
        if let Some(&start_idx) = ws_hits.first() {
            ctx.undo_stack.push(UndoSnapshot {
                target: raw_path.to_string(),
                previous_content: Some(content.clone()),
                description: format!("edit line {}", start_idx + 1),
            });
            let mut new_lines: Vec<String> = Vec::new();
            for l in &src_lines[..start_idx] {
                new_lines.push((*l).to_string());
            }
            for l in new_text.lines() {
                new_lines.push(l.to_string());
            }
            for l in &src_lines[start_idx + needle_lines.len()..] {
                new_lines.push((*l).to_string());
            }
            let mut joined = new_lines.join("\n");
            if content.ends_with('\n') {
                joined.push('\n');
            }
            if let Err(e) = write_target_file(raw_path, &joined) {
                return e;
            }
            ctx.record_file_mtime(raw_path);
            crate::spavn_engine::record_audit_action(
                "file_edit",
                &ctx.current_model,
                &ctx.default_host,
                raw_path,
                new_text.lines().count(),
                old_text.lines().count(),
                &ctx.current_prompt,
            );
            render_diff_preview(raw_path, old_text, new_text);
            let diag = run_instant_syntax_check(raw_path, &joined);
            return format!(
                "Successfully edited {} (whitespace-tolerant match at line {} — file state is current in context, no need to re-read){}{}",
                raw_path,
                start_idx + 1,
                mtime_warn,
                diag
            );
        }
    }

    let non_empty_needle: Vec<&str> = old_text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if !non_empty_needle.is_empty() {
        let non_empty_src: Vec<(usize, &str)> = src_lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i, l.trim()))
            .filter(|(_, l)| !l.is_empty())
            .collect();
        if non_empty_needle.len() <= non_empty_src.len() {
            let mut fuzzy_hits: Vec<(usize, usize)> = Vec::new();
            for win_idx in 0..=(non_empty_src.len() - non_empty_needle.len()) {
                let matches = non_empty_src[win_idx..win_idx + non_empty_needle.len()]
                    .iter()
                    .zip(non_empty_needle.iter())
                    .all(|((_, a), b)| *a == *b);
                if matches {
                    let first_src_line = non_empty_src[win_idx].0;
                    let last_src_line = non_empty_src[win_idx + non_empty_needle.len() - 1].0;
                    fuzzy_hits.push((first_src_line, last_src_line));
                }
            }
            if fuzzy_hits.len() > 1 {
                let hit_lines: Vec<String> = fuzzy_hits
                    .iter()
                    .map(|(s, _)| (s + 1).to_string())
                    .collect();
                return format!(
                    "Edit error: old_text matches {} locations in {} (lines: {}). Include more surrounding context.",
                    fuzzy_hits.len(),
                    raw_path,
                    hit_lines.join(", ")
                );
            }
            if let Some(&(first_src_line, last_src_line)) = fuzzy_hits.first() {
                ctx.undo_stack.push(UndoSnapshot {
                    target: raw_path.to_string(),
                    previous_content: Some(content.clone()),
                    description: format!(
                        "edit lines {}..{}",
                        first_src_line + 1,
                        last_src_line + 1
                    ),
                });
                let mut new_lines: Vec<String> = Vec::new();
                for l in &src_lines[..first_src_line] {
                    new_lines.push((*l).to_string());
                }
                for l in new_text.lines() {
                    new_lines.push(l.to_string());
                }
                for l in &src_lines[last_src_line + 1..] {
                    new_lines.push((*l).to_string());
                }
                let mut joined = new_lines.join("\n");
                if content.ends_with('\n') {
                    joined.push('\n');
                }
                if let Err(e) = write_target_file(raw_path, &joined) {
                    return e;
                }
                ctx.record_file_mtime(raw_path);
                crate::spavn_engine::record_audit_action(
                    "file_edit",
                    &ctx.current_model,
                    &ctx.default_host,
                    raw_path,
                    new_text.lines().count(),
                    old_text.lines().count(),
                    &ctx.current_prompt,
                );
                render_diff_preview(raw_path, old_text, new_text);
                let diag = run_instant_syntax_check(raw_path, &joined);
                return format!(
                    "Successfully edited {} (fuzzy blank-line-tolerant match at lines {}-{} — file state is current in context, no need to re-read){}{}",
                    raw_path,
                    first_src_line + 1,
                    last_src_line + 1,
                    mtime_warn,
                    diag
                );
            }
        }
    }

    if let Some(first_anchor) = non_empty_needle.first() {
        let anchor_probe = if first_anchor.len() > 16 {
            let mut end = 16;
            while end > 0 && !first_anchor.is_char_boundary(end) {
                end -= 1;
            }
            &first_anchor[..end]
        } else {
            *first_anchor
        };
        if !anchor_probe.is_empty() {
            if let Some((hit_idx, _)) = src_lines
                .iter()
                .enumerate()
                .find(|(_, l)| l.contains(anchor_probe))
            {
                let ctx_start = hit_idx.saturating_sub(5);
                let ctx_end = (hit_idx + 18).min(src_lines.len());
                let mut snippet = String::new();
                for (idx, line) in src_lines[ctx_start..ctx_end].iter().enumerate() {
                    snippet.push_str(&format!("{:>4}: {}\n", ctx_start + idx + 1, line));
                }
                return format!(
                    "Edit error: exact old_text not found in {}. However, a similar anchor was found around line {}. Here is the exact current file content at lines {}-{} (copy old_text verbatim from here and retry immediately):\n\n{}",
                    raw_path,
                    hit_idx + 1,
                    ctx_start + 1,
                    ctx_end,
                    snippet
                );
            }
        }
    }

    format!(
        "Edit error: old_text not found in {} (file has {} lines). Read the target lines first to get the exact current text.",
        raw_path,
        src_lines.len()
    )
}

fn exec_write(ctx: &mut ToolContext, args: &Value) -> String {
    let raw_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
    if let Err(blocked) = ctx.enforce_path(raw_path, true, "write") {
        return blocked;
    }
    let content = get_str_alias(args, &["content", "text", "body", "code"]);

    let leaked_secrets = crate::spavn_engine::detect_secrets_in_text(content);
    if !leaked_secrets.is_empty() && ctx.access != AccessMode::Sovereign {
        let secret_msg = format!(
            "🛡️  [SPAVN Secret Guard]: {} içinde hassas anahtar ({}) tespit edildi — onaylıyor musunuz?",
            raw_path,
            leaked_secrets.join(", ")
        );
        if !prompt_user_approval(ctx, &secret_msg, content) {
            return format!(
                "BLOCKED by SPAVN Secret Guard: {} contains potential secret(s): {}. Use environment variables instead.",
                raw_path,
                leaked_secrets.join(", ")
            );
        }
    }

    if ctx.access == AccessMode::Safe {
        if !prompt_user_approval(ctx, &format!("Dosya Yazma (write): {}", raw_path), content) {
            return "User rejected write in SAFE mode.".to_string();
        }
    }

    let prev = read_target_file(raw_path).ok();
    let prev_lines = prev.as_ref().map(|p| p.lines().count()).unwrap_or(0);
    ctx.undo_stack.push(UndoSnapshot {
        target: raw_path.to_string(),
        previous_content: prev,
        description: "write".to_string(),
    });

    match write_target_file(raw_path, content) {
        Ok(()) => {
            ctx.record_file_mtime(raw_path);
            crate::spavn_engine::record_audit_action(
                "file_write",
                &ctx.current_model,
                &ctx.default_host,
                raw_path,
                content.lines().count(),
                prev_lines,
                &ctx.current_prompt,
            );
            let diag = run_instant_syntax_check(raw_path, content);
            format!(
                "Successfully wrote {} bytes to {} (file state is current in context, no need to re-read){}",
                content.len(),
                raw_path,
                diag
            )
        }
        Err(e) => e,
    }
}

fn check_bash_gate(ctx: &mut ToolContext, cmd: &str, host: &str) -> Result<(), String> {
    let risk = assess_shell_command(cmd, &ctx.policy);

    if let ShellRisk::Catastrophic(reason) = &risk {
        return Err(format!(
            "BLOCKED by SPAVN Protected Path Policy: {}",
            reason
        ));
    }

    let auto_sensitive = matches!(risk, ShellRisk::Destructive(_)) || host == "server";
    match ctx.access {
        AccessMode::Safe => {
            if !prompt_user_approval(ctx, &format!("Komut Çalıştırma [{}]", host), cmd) {
                return Err("User rejected command in SAFE mode.".to_string());
            }
        }
        AccessMode::Auto => {
            if auto_sensitive {
                let reason = match &risk {
                    ShellRisk::Destructive(reason) => reason.clone(),
                    _ => format!("uzak host üzerinde çalıştırma: {}", host),
                };
                if !prompt_user_approval(
                    ctx,
                    &format!("Kritik Komut Onayı [{}] — {}", host, reason),
                    cmd,
                ) {
                    return Err("User declined sensitive command in AUTO mode.".to_string());
                }
            }
        }
        AccessMode::Sovereign => {}
    }

    Ok(())
}

fn exec_bash(ctx: &mut ToolContext, args: &Value) -> String {
    let command = get_str_alias(args, &["command", "cmd", "script", "shell"]);
    let explicit_host = get_str_alias(args, &["host", "target"]);
    let host = if explicit_host.is_empty() {
        ctx.default_host.clone()
    } else {
        explicit_host.to_string()
    };
    let cwd = args.get("cwd").and_then(|v| v.as_str());
    let timeout_sec = args
        .get("timeout_sec")
        .and_then(|v| v.as_u64())
        .unwrap_or(45);

    if let Err(blocked) = check_bash_gate(ctx, command, &host) {
        return blocked;
    }

    let cmd_obj = if host != "local" && !host.is_empty() {
        let ssh_target = resolve_ssh_target(&host);
        let remote_cmd = if let Some(dir) = cwd {
            format!("cd {} && {}", shell_quote_remote_path(dir), command)
        } else {
            command.to_string()
        };
        let mut c = Command::new("ssh");
        c.args(["-o", "ConnectTimeout=8", &ssh_target, &remote_cmd]);
        c
    } else {
        let mut c = Command::new(crate::config::shell_bin());
        c.args(["-c", command]);
        if let Some(dir) = cwd {
            let expanded = expand_tilde(dir);
            if expanded.exists() {
                c.current_dir(expanded);
            }
        }
        c
    };

    match run_command_with_timeout(cmd_obj, timeout_sec) {
        Ok((code, stdout, stderr, timed_out)) => {
            let mut combined = String::from_utf8_lossy(&stdout).to_string();
            let err = String::from_utf8_lossy(&stderr);
            if !err.is_empty() {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&err);
            }
            if timed_out {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&format!(
                    "[SPAVN Timeout: Komut {}s sınırını aştığı için otomatik sonlandırıldı]",
                    timeout_sec
                ));
            }
            if combined.len() > 12_000 {
                let mut head_end = 6_000.min(combined.len());
                while head_end > 0 && !combined.is_char_boundary(head_end) {
                    head_end -= 1;
                }
                let mut tail_start = combined.len().saturating_sub(4_000);
                while tail_start < combined.len() && !combined.is_char_boundary(tail_start) {
                    tail_start += 1;
                }
                let head = &combined[..head_end];
                let tail = &combined[tail_start..];
                combined = format!(
                    "{}\n\n... [SPAVN truncated {} bytes of middle output] ...\n\n{}",
                    head,
                    tail_start.saturating_sub(head_end),
                    tail
                );
            }
            if combined.trim().is_empty() {
                format!("(exit code {})", code)
            } else {
                combined
            }
        }
        Err(e) => format!("Execution error: {}", e),
    }
}

fn exec_search(ctx: &mut ToolContext, args: &Value) -> String {
    let pattern = get_str_alias(args, &["pattern", "query", "regex", "search"]);
    let raw_path = {
        let p = get_str_alias(args, &["path", "dir", "directory", "file"]);
        if p.is_empty() {
            "."
        } else {
            p
        }
    };
    let explicit_host = get_str_alias(args, &["host", "target"]);

    if let Some((ssh_host, remote_path)) = split_remote_prefix(raw_path) {
        let q_pat = shell_quote_remote_path(pattern);
        let q_path = shell_quote_remote_path(remote_path);
        let remote_cmd = format!(
            "export PATH=\"/opt/homebrew/bin:/usr/local/bin:$PATH\"; if command -v rg >/dev/null 2>&1; then rg -n --max-count 80 -- {} {}; else grep -rn -- {} {} | head -n 80; fi",
            q_pat, q_path, q_pat, q_path
        );
        let out = Command::new("ssh")
            .args(["-o", "ConnectTimeout=6", &ssh_host, &remote_cmd])
            .output();
        return match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout).to_string();
                if s.trim().is_empty() {
                    "(no matches)".to_string()
                } else {
                    s
                }
            }
            Err(e) => format!("Remote search error: {}", e),
        };
    }

    let target_host = if !explicit_host.is_empty() {
        explicit_host
    } else {
        ctx.default_host.as_str()
    };
    if target_host != "local" && !target_host.is_empty() {
        let ssh_host = resolve_ssh_target(target_host);
        let q_pat = shell_quote_remote_path(pattern);
        let q_path = shell_quote_remote_path(raw_path);
        let remote_cmd = format!(
            "export PATH=\"/opt/homebrew/bin:/usr/local/bin:$PATH\"; if command -v rg >/dev/null 2>&1; then rg -n --max-count 80 -- {} {}; else grep -rn -- {} {} | head -n 80; fi",
            q_pat, q_path, q_pat, q_path
        );
        let out = Command::new("ssh")
            .args(["-o", "ConnectTimeout=6", &ssh_host, &remote_cmd])
            .output();
        return match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout).to_string();
                if s.trim().is_empty() {
                    "(no matches)".to_string()
                } else {
                    s
                }
            }
            Err(e) => format!("Remote search error: {}", e),
        };
    }

    if let Err(blocked) = ctx.enforce_path(raw_path, false, "search") {
        return blocked;
    }
    let path = expand_tilde(raw_path);
    let rg_bin = if Path::new("/opt/homebrew/bin/rg").exists() {
        "/opt/homebrew/bin/rg"
    } else {
        "rg"
    };
    let mut cmd = Command::new(rg_bin);
    cmd.args([
        "-n",
        "--max-count",
        "25",
        "--max-depth",
        "4",
        "--max-filesize",
        "512K",
        "--max-columns",
        "220",
        "-g",
        "!Library/**",
        "-g",
        "!Applications/**",
        "-g",
        "!Pictures/**",
        "-g",
        "!Movies/**",
        "-g",
        "!Music/**",
        "-g",
        "!*OneDrive*/**",
        "-g",
        "!.Trash/**",
        "-g",
        "!node_modules/**",
        "-g",
        "!target/**",
        "-g",
        "!.git/**",
        "-g",
        "!.omp/**",
        "-g",
        "!.cache/**",
        "-g",
        "!.cargo/**",
        "-g",
        "!.npm/**",
        pattern,
        &path.to_string_lossy(),
    ]);

    match run_command_with_timeout(cmd, 8) {
        Ok((_, stdout, _, timed_out)) => {
            let raw = String::from_utf8_lossy(&stdout);
            let mut lines: Vec<&str> = raw.lines().take(80).collect();
            if timed_out {
                lines.push("[SPAVN Search: 8s zaman sınırı doldu, ilk eşleşmeler gösteriliyor]");
            }
            if lines.is_empty() {
                "(no matches)".to_string()
            } else {
                lines.join("\n")
            }
        }
        Err(e) => format!("Search error: {}", e),
    }
}

fn exec_memory(ctx: &mut ToolContext, args: &Value) -> String {
    let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("recall");
    let query = get_str_alias(args, &["query", "text", "content", "note"]);
    let cfg = load_user_config();
    let cwd = crate::memory::current_workdir();

    if op == "remember" {
        if ctx.access == AccessMode::Safe {
            if !prompt_user_approval(ctx, "Hafızaya Kayıt (remember)", query) {
                return "User rejected memory write in SAFE mode.".to_string();
            }
        }
        let raw_kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("not");
        let kind = match raw_kind {
            "not" | "tercih" | "bilgi" | "durum" | "karar" => raw_kind,
            _ => "not",
        };
        if let Some(ref remember_bin) = cfg.memory_remember_bin {
            let script_path = expand_tilde(remember_bin);
            if script_path.exists() {
                if let Ok(out) = Command::new("python3")
                    .args([
                        &script_path.to_string_lossy().to_string(),
                        kind,
                        query,
                        "spavn",
                    ])
                    .output()
                {
                    if out.status.success() {
                        let msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        return format!(
                            "{}🧠 Hafızaya kaydedildi ({}): {}{}",
                            CYAN, kind, msg, RESET
                        );
                    }
                }
            }
        }
        let pin =
            crate::spavn_engine::add_pin(&cwd, &format!("[{}] {}", kind.to_uppercase(), query));
        return format!(
            "{}📌 Spavn Proje Hafızasına kaydedildi (Pin #{}): {}{}",
            CYAN, pin.id, pin.content, RESET
        );
    }

    if let Some(ref recall_bin) = cfg.memory_recall_bin {
        let script_path = expand_tilde(recall_bin);
        if script_path.exists() {
            if let Ok(out) = Command::new("python3")
                .args([&script_path.to_string_lossy().to_string(), query])
                .output()
            {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if !text.trim().is_empty() {
                    return text;
                }
            }
        }
    }

    let arts = crate::spavn_engine::retrieve_relevant_artifacts(&cwd, query, 5);
    if arts.is_empty() {
        "(Eşleşen hafıza kaydı bulunamadı)".to_string()
    } else {
        arts.into_iter()
            .map(|a| format!("- [{}] ({}): {}", a.ts, a.files.join(", "), a.summary))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn parse_args_resilient(raw: &str) -> Value {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return v;
    }
    let stripped = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Value>(stripped) {
        return v;
    }
    if let (Some(s), Some(e)) = (stripped.find('{'), stripped.rfind('}')) {
        if s < e {
            if let Ok(v) = serde_json::from_str::<Value>(&stripped[s..=e]) {
                return v;
            }
        }
    }
    json!({})
}

fn exec_skill(args: &Value) -> String {
    let name = get_str_alias(args, &["name", "skill", "id", "query"]);
    if name.is_empty() {
        let all = crate::skills::discover_skills();
        let names: Vec<String> = all
            .into_iter()
            .map(|s| format!("@{} ({})", s.name, s.summary))
            .collect();
        return format!("Available skills:\n{}", names.join("\n"));
    }
    if let Some((entry, content)) = crate::skills::load_skill_content(name) {
        format!(
            "# Loaded Skill: @{} ({})\n{}",
            entry.name,
            entry.path.display(),
            content.trim()
        )
    } else {
        format!(
            "Skill '{}' not found. Use `/skills` to list available skills.",
            name
        )
    }
}

fn strip_html_to_clean_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(12_000));
    let lower = html.to_lowercase();
    let bytes = html.as_bytes();
    let lower_bytes = lower.as_bytes();
    let mut i = 0usize;
    let mut in_tag = false;
    let mut last_was_space = false;

    while i < bytes.len() {
        if !in_tag && i + 7 < lower_bytes.len() {
            if lower_bytes[i..].starts_with(b"<script") {
                if let Some(end) = lower[i..].find("</script>") {
                    i += end + 9;
                    continue;
                }
            }
            if lower_bytes[i..].starts_with(b"<style") {
                if let Some(end) = lower[i..].find("</style>") {
                    i += end + 8;
                    continue;
                }
            }
            if lower_bytes[i..].starts_with(b"<svg") {
                if let Some(end) = lower[i..].find("</svg>") {
                    i += end + 6;
                    continue;
                }
            }
        }
        let b = bytes[i];
        if b == b'<' {
            if lower_bytes[i..].starts_with(b"<br")
                || lower_bytes[i..].starts_with(b"<p")
                || lower_bytes[i..].starts_with(b"</p")
                || lower_bytes[i..].starts_with(b"<div")
                || lower_bytes[i..].starts_with(b"<li")
                || lower_bytes[i..].starts_with(b"<h1")
                || lower_bytes[i..].starts_with(b"<h2")
                || lower_bytes[i..].starts_with(b"<h3")
                || lower_bytes[i..].starts_with(b"<tr")
            {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                last_was_space = true;
            }
            in_tag = true;
            i += 1;
            continue;
        }
        if b == b'>' {
            in_tag = false;
            i += 1;
            continue;
        }
        if !in_tag {
            let ch = html[i..].chars().next().unwrap_or(' ');
            if ch == '\n' || ch == '\r' {
                if !out.ends_with("\n\n") {
                    out.push('\n');
                }
                last_was_space = true;
            } else if ch.is_whitespace() {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            } else {
                out.push(ch);
                last_was_space = false;
            }
            i += ch.len_utf8();
            if out.len() >= 10_000 {
                out.push_str("\n...[SPAVN Web: truncated to 10,000 chars]...");
                break;
            }
        } else {
            i += 1;
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .trim()
        .to_string()
}

fn call_exa_mcp(tool_name: &str, arguments: Value) -> Option<String> {
    let endpoint = match std::env::var("EXA_API_KEY") {
        Ok(k) if !k.trim().is_empty() => format!("https://mcp.exa.ai/mcp?exaApiKey={}", k.trim()),
        _ => "https://mcp.exa.ai/mcp".to_string(),
    };
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments
        }
    });
    let (status, raw) = crate::http::post_json(
        &endpoint,
        &payload.to_string(),
        &[("Accept", "application/json, text/event-stream")],
        15,
    )
    .ok()?;
    if !(200..300).contains(&status) {
        return None;
    }
    for line in raw.lines() {
        let trimmed = line.trim();
        let json_str = if let Some(rest) = trimmed.strip_prefix("data:") {
            rest.trim()
        } else if trimmed.starts_with('{') {
            trimmed
        } else {
            continue;
        };
        if let Ok(val) = serde_json::from_str::<Value>(json_str) {
            if let Some(arr) = val
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.as_array())
            {
                for item in arr {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        if !text.trim().is_empty() {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

fn exec_web(args: &Value) -> String {
    let op_raw = get_str_alias(args, &["op", "action", "mode"]);
    let url = get_str_alias(args, &["url", "href", "link"]);
    let query = get_str_alias(args, &["query", "q", "search", "url"]);
    let is_fetch = op_raw.eq_ignore_ascii_case("fetch")
        || op_raw.eq_ignore_ascii_case("read")
        || (!url.is_empty() && (url.starts_with("http://") || url.starts_with("https://")))
        || (query.starts_with("http://") || query.starts_with("https://"));

    if is_fetch {
        let target_url = if !url.is_empty() { url } else { query };
        if target_url.is_empty() {
            return "Web fetch error: missing 'url' parameter.".to_string();
        }
        let is_local_url = target_url.contains("localhost") || target_url.contains("127.0.0.1");
        if !is_local_url {
            if let Some(exa_md) = call_exa_mcp(
                "web_fetch_exa",
                json!({
                    "urls": [target_url],
                    "maxCharacters": 9000
                }),
            ) {
                return exa_md;
            }
        }
        return match crate::http::get_text(
            target_url,
            &[
                (
                    "User-Agent",
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/537.36",
                ),
                ("Accept", "text/markdown, text/plain;q=0.9, text/html;q=0.8"),
            ],
            12,
        ) {
            Ok((status, body)) if (200..300).contains(&status) => {
                if body.contains("<html") || body.contains("<body") || body.contains("<!DOCTYPE") {
                    strip_html_to_clean_text(&body)
                } else {
                    body.chars().take(10_000).collect()
                }
            }
            Ok((status, body)) => format!(
                "Web fetch failed: HTTP {} — {}",
                status,
                body.trim().chars().take(300).collect::<String>()
            ),
            Err(e) => format!("Web fetch error: {}", e),
        };
    }

    if query.is_empty() {
        return "Web search error: missing 'query' parameter.".to_string();
    }
    if let Some(exa_res) = call_exa_mcp(
        "web_search_exa",
        json!({
            "query": query,
            "numResults": 6
        }),
    ) {
        return exa_res;
    }
    "Web search returned no results.".to_string()
}

pub fn execute_tool(ctx: &mut ToolContext, name: &str, args_json: &str) -> String {
    ctx.tool_step_counter += 1;
    let args = parse_args_resilient(args_json);
    let normalized = name.trim().to_lowercase();
    match normalized.as_str() {
        "read" | "read_file" | "cat" | "view" => exec_read(ctx, &args),
        "edit" | "edit_file" | "replace" | "patch" => exec_edit(ctx, &args),
        "write" | "write_file" | "create_file" | "save" => exec_write(ctx, &args),
        "bash" | "run_command" | "shell" | "exec" | "terminal" => exec_bash(ctx, &args),
        "search" | "grep" | "rg" | "find_in_files" => exec_search(ctx, &args),
        "memory" | "recall_memory" | "remember" => exec_memory(ctx, &args),
        "skill" | "load_skill" | "use_skill" => exec_skill(&args),
        "web" | "web_search" | "websearch" | "webfetch" | "web_fetch" => exec_web(&args),
        _ => format!(
            "Unknown tool '{}'. Valid tools are: read, edit, write, bash, search, memory, skill, web.",
            name
        ),
    }
}
