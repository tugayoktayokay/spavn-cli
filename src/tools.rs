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

    pub fn parse_alias(s: &str) -> Option<Self> {
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

#[derive(Clone)]
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
    pub phase: crate::spavn_engine::AgentPhase,
    pub policy: PathPolicy,
    /// False for workers with no terminal behind them (sub-agents). An approval prompt needs a
    /// human, so a context that cannot ask must decide on policy alone instead of hanging or
    /// silently answering "no" on every call.
    pub can_prompt: bool,
}

impl ToolContext {
    pub fn new(
        access: AccessMode,
        default_host: String,
        workspace_root: PathBuf,
        phase: crate::spavn_engine::AgentPhase,
    ) -> Self {
        let policy = PathPolicy::new(&workspace_root);
        Self {
            access,
            default_host,
            phase,
            undo_stack: Vec::new(),
            read_mtimes: std::collections::HashMap::new(),
            read_ranges: std::collections::HashMap::new(),
            tool_step_counter: 0,
            current_model: "default".to_string(),
            current_prompt: String::new(),
            policy,
            can_prompt: true,
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
                // `even_sovereign` marks secret material (`.ssh`, `.aws`, `secrets.env`, keychains).
                // Dropping it here meant SOVEREIGN — the default mode — returned private keys to
                // the model with no prompt at all. SOVEREIGN waives ordinary questions, never these.
                if !even_sovereign && self.access == AccessMode::Sovereign {
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
    let cfg = crate::config::load_user_config();
    let mut mcp_names: Vec<String> = cfg.mcp_servers.keys().cloned().collect();
    mcp_names.sort();
    let mcp_hint = if mcp_names.is_empty() {
        "No MCP servers configured (`mcpServers` in `~/.spavn/config.json`).".to_string()
    } else {
        format!("MCP servers: `{}`.", mcp_names.join("`, `"))
    };
    let lsp_desc = format!(
        "Symbol navigation (`op=definition|references|symbols`) and generic MCP client (`op='mcp'`). {}",
        mcp_hint
    );
    json!([
        {
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file or directory. Supports several disjoint ranges in one call via `path:N-M,K-L` — prefer that over repeated reads or `bash sed`.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path (optional `<host>:` prefix); may embed ranges, e.g. `src/x.rs:1-12,640-665`" },
                        "start_line": { "type": "integer", "description": "Start line (1-based)" },
                        "end_line": { "type": "integer", "description": "End line (1-based, inclusive)" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Replace `old_text` with `new_text` in a file, or apply several non-overlapping edits atomically via `edits`.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File to edit" },
                        "old_text": { "type": "string", "description": "Exact existing snippet (single edit)" },
                        "new_text": { "type": "string", "description": "Replacement snippet (single edit)" },
                        "edits": {
                            "type": "array",
                            "description": "Batch of `{old_text, new_text}` applied atomically to the same file",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old_text": { "type": "string" },
                                    "new_text": { "type": "string" }
                                },
                                "required": ["old_text", "new_text"]
                            }
                        }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write",
                "description": "Create or overwrite a file (`<host>:/path` for a remote host).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Target path" },
                        "content": { "type": "string", "description": "Full file content" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run a shell command locally or on a configured remote host.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Command" },
                        "host": { "type": "string", "description": "Host alias (default: local)" },
                        "cwd": { "type": "string", "description": "Working directory" },
                        "timeout_sec": { "type": "integer", "description": "Timeout seconds (default 120)" },
                        "background": { "type": "boolean", "description": "Run detached; returns PID + log path" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search",
                "description": "Regex content search (ripgrep).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex pattern" },
                        "path": { "type": "string", "description": "Path to search (default `.`)" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "glob",
                "description": "List files matching a glob, no content search. Skips node_modules/target/.git and dotfiles unless `hidden`.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob, e.g. `**/*.rs`" },
                        "path": { "type": "string", "description": "Root dir (default `.`; supports `<host>:/path`)" },
                        "limit": { "type": "integer", "description": "Max files (default 100, cap 1000)" },
                        "hidden": { "type": "boolean", "description": "Include dotfiles" },
                        "host": { "type": "string", "description": "Host alias" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "memory",
                "description": "Search or record persistent project/user memory and decisions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["recall", "remember"], "description": "recall | remember" },
                        "query": { "type": "string", "description": "Query, or note text" },
                        "kind": { "type": "string", "description": "durum | karar | bilgi | not" }
                    },
                    "required": ["op", "query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "skill",
                "description": "Load domain instructions from ~/.spavn/skills or ~/.claude/skills.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Skill name" }
                    },
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web",
                "description": "Web search (`op='search'`) or fetch a URL as Markdown (`op='fetch'`).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["search", "fetch"], "description": "search | fetch" },
                        "query": { "type": "string", "description": "Query, or URL" },
                        "url": { "type": "string", "description": "URL for op='fetch'" }
                    },
                    "required": ["op"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "subagent",
                "description": "Delegate isolated read-only investigations to sub-agents with their own context: one `task`, or several via `tasks` in parallel.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "The sub-question" },
                        "context": { "type": "string", "description": "File paths or background" },
                        "expected_output": { "type": "string", "description": "Findings to return" },
                        "model": { "type": "string", "description": "Model override, e.g. local | flash" },
                        "tasks": {
                            "type": "array",
                            "description": "Up to 4 sub-tasks run concurrently",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "task": { "type": "string" },
                                    "context": { "type": "string" },
                                    "expected_output": { "type": "string" }
                                },
                                "required": ["task"]
                            }
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "lsp",
                "description": lsp_desc,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["definition", "references", "symbols", "mcp"], "description": "Operation" },
                        "symbol": { "type": "string", "description": "Symbol for definition/references" },
                        "path": { "type": "string", "description": "Path for symbols" },
                        "server": { "type": "string", "description": "MCP server name" },
                        "tool": { "type": "string", "description": "MCP tool (omit to list)" },
                        "arguments": { "type": "object", "description": "JSON arguments" }
                    },
                    "required": ["op"]
                }
            }
        }
    ])
}

pub fn tool_definitions_for_phase(phase: crate::spavn_engine::AgentPhase) -> Value {
    // The schema shape only changes with the phase, so build it once per phase instead of
    // re-serializing 6.7 KB of JSON on every model request.
    static CACHE: std::sync::Mutex<[Option<Value>; 2]> = std::sync::Mutex::new([None, None]);
    let idx = usize::from(phase == crate::spavn_engine::AgentPhase::Plan);
    if let Ok(mut slot) = CACHE.lock() {
        if let Some(hit) = slot[idx].as_ref() {
            return hit.clone();
        }
        let built = build_tool_definitions_for_phase(phase);
        slot[idx] = Some(built.clone());
        return built;
    }
    build_tool_definitions_for_phase(phase)
}

fn build_tool_definitions_for_phase(phase: crate::spavn_engine::AgentPhase) -> Value {
    let all = tool_definitions();
    if phase == crate::spavn_engine::AgentPhase::Code {
        return all;
    }
    if let Value::Array(items) = all {
        let filtered: Vec<Value> = items
            .into_iter()
            .filter(|item| {
                let name = item
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                !matches!(name, "edit" | "write" | "bash")
            })
            .collect();
        Value::Array(filtered)
    } else {
        all
    }
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

/// Rejects a host alias that `ssh` would read as an option instead of a destination.
///
/// `ssh -o ConnectTimeout=8 <dest> <cmd>` parses `<dest>`, so `-oProxyCommand=<shell>` runs an
/// arbitrary *local* command before any gate can look at it — and in SOVEREIGN the gate never
/// looks. Configured aliases were already safe (`resolve_ssh_target` maps them); the raw
/// fallback returned the caller's string verbatim, which is the hole this closes.
fn validate_host_arg(host: &str) -> Result<(), String> {
    if host.is_empty() || host == "local" {
        return Ok(());
    }
    let looks_like_option = host.starts_with('-');
    let charset_ok = host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ':' | '[' | ']'));
    if looks_like_option || !charset_ok {
        return Err(format!(
            "BLOCKED by SPAVN Host Policy: `{}` is not a valid host alias.\n\
             A host must be a name from `hosts` in ~/.spavn/config.json (or `user@host`, `host:port`);\n\
             anything starting with `-` or containing spaces/quotes is rejected because ssh would\n\
             parse it as an option.",
            host
        ));
    }
    Ok(())
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
            if let Err(e) = stdin.write_all(content.as_bytes()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Remote write error: {}", e));
            }
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
        write_atomic(&path, content).map_err(|e| format!("Write error ({}): {}", path.display(), e))
    }
}

static ATOMIC_WRITE_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Temp file in the same dir + rename: a crash or full disk never leaves a truncated target.
/// Follows symlinks (including broken symlinks) and keeps the existing file's permissions.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| {
        if let Ok(link_dest) = fs::read_link(path) {
            if link_dest.is_absolute() {
                link_dest
            } else {
                path.parent().unwrap_or(Path::new(".")).join(link_dest)
            }
        } else {
            path.to_path_buf()
        }
    });
    if let Some(parent) = target.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let seq = ATOMIC_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = target.with_file_name(format!(
        ".{}.spavn-tmp-{}-{}",
        name,
        std::process::id(),
        seq
    ));
    let result = (|| {
        fs::write(&tmp, content)?;
        if let Ok(meta) = fs::metadata(&target) {
            fs::set_permissions(&tmp, meta.permissions())?;
        }
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

const MAX_CAPTURE_BYTES: u64 = 4 * 1024 * 1024;

/// Keeps the first MAX_CAPTURE_BYTES, then drains the pipe so the child never blocks.
fn capture_capped<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = std::io::Read::read_to_end(
                &mut std::io::Read::take(&mut p, MAX_CAPTURE_BYTES),
                &mut buf,
            );
            let _ = std::io::copy(&mut p, &mut std::io::sink());
        }
        buf
    })
}

/// SIGKILL the whole process group so grandchildren release the pipes.
fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        kill(-(child.id() as i32), 9);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .output();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn run_command_with_timeout(
    mut cmd: Command,
    timeout_sec: u64,
) -> Result<(i32, Vec<u8>, Vec<u8>, bool), String> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let out_handle = capture_capped(child.stdout.take());
    let err_handle = capture_capped(child.stderr.take());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_sec.max(1));
    let mut timed_out = false;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    break status
                        .code()
                        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
                }
                #[cfg(not(unix))]
                {
                    break status.code().unwrap_or(-1);
                }
            }
            Ok(None) => {
                if crate::is_interrupted() {
                    kill_group(&mut child);
                    let out_bytes = out_handle.join().unwrap_or_default();
                    let mut err_bytes = err_handle.join().unwrap_or_default();
                    err_bytes
                        .extend_from_slice(b"\n[SPAVN Interrupt: Komut Esc/Ctrl+C ile durduruldu]");
                    return Ok((-2, out_bytes, err_bytes, false));
                }
                if std::time::Instant::now() >= deadline {
                    kill_group(&mut child);
                    timed_out = true;
                    break -1;
                }
                std::thread::sleep(std::time::Duration::from_millis(4));
            }
            Err(e) => return Err(e.to_string()),
        }
    };

    let out_bytes = out_handle.join().unwrap_or_default();
    let err_bytes = err_handle.join().unwrap_or_default();
    Ok((exit_code, out_bytes, err_bytes, timed_out))
}

/// Strips everything a terminal would interpret, keeping tabs.
///
/// `detail` and `action_title` carry model-controlled bytes (file content, shell command, MCP
/// arguments). A leading `\x1b[1A\x1b[2K` reached the prompt verbatim and let a caller erase the
/// path line it was asking the user to approve, then print a benign one in its place.
fn gate_safe(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect()
}

fn prompt_user_approval(ctx: &mut ToolContext, action_title: &str, detail: &str) -> bool {
    // Fail closed without a TTY: the read below would block forever on a piped/inherited stdin
    // and the turn could never make progress. A prompt that cannot be answered is a "no".
    if !crate::hud::stdin_is_tty() {
        return false;
    }
    crate::hud::pause_active_spinner();
    println!(
        "\n{}╭─ 🛡️  {} ({}){}",
        YELLOW,
        crate::config::tr_en("SPAVN ERİŞİM KAPISI", "SPAVN ACCESS GATE"),
        ctx.access.short_badge(),
        RESET
    );
    println!(
        "{}│{} {}{}{}",
        YELLOW,
        RESET,
        BOLD,
        gate_safe(action_title),
        RESET
    );
    for line in detail.lines().take(6) {
        println!("{}│{} {}{}{}", YELLOW, RESET, DIM, gate_safe(line), RESET);
    }
    print!(
        "{}╰─ {}: {}",
        YELLOW,
        crate::config::tr_en(
            "Onaylıyor musunuz? [e = Evet / H = Hayır / s = Sovereign Moda Geç]",
            "Approve? [y = Yes / N = No / s = Switch to Sovereign Mode]"
        ),
        RESET
    );
    let _ = io::stdout().flush();

    let mut ans = String::new();
    // Ok(0) = EOF (closed/piped stdin): never treat it as the empty "default yes" answer.
    if !matches!(io::stdin().read_line(&mut ans), Ok(n) if n > 0) {
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
    // Enter alone declines: a stray keypress must not approve a risky action.
    let approved = matches!(clean.as_str(), "e" | "evet" | "y" | "yes");
    crate::hud::resume_active_spinner();
    approved
}

fn is_structural_outline_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let indent = line.len().saturating_sub(trimmed.len());
    if indent > 8 || trimmed.starts_with("//") || trimmed.starts_with('#') {
        return false;
    }
    const PREFIXES: &[&str] = &[
        "pub fn ",
        "pub async fn ",
        "pub const fn ",
        "pub unsafe fn ",
        "pub(crate) fn ",
        "pub(crate) async fn ",
        "pub(crate) const fn ",
        "pub(super) fn ",
        "fn ",
        "async fn ",
        "const fn ",
        "unsafe fn ",
        "pub struct ",
        "pub(crate) struct ",
        "struct ",
        "pub enum ",
        "pub(crate) enum ",
        "enum ",
        "pub trait ",
        "pub(crate) trait ",
        "trait ",
        "impl ",
        "impl<",
        "pub type ",
        "pub(crate) type ",
        "pub mod ",
        "mod ",
        "def ",
        "async def ",
        "class ",
        "export function ",
        "export async function ",
        "export default function ",
        "export class ",
        "export interface ",
        "export type ",
        "function ",
        "interface ",
    ];
    PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

/// One `N-M` (or bare `N`) span from a `path:N-M,K-L` selector.
type LineSpan = (usize, usize);

/// Parses `N-M`, `N`, `L12-L40`, or a comma-separated list of those.
///
/// Returns `None` when `suffix` is not purely line ranges, so callers can fall back to
/// treating the whole argument as a path (e.g. `~/x:y.rs` on a remote host).
fn parse_line_spans(suffix: &str) -> Option<Vec<LineSpan>> {
    let mut spans = Vec::new();
    for part in suffix.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let part = part.strip_prefix('L').unwrap_or(part);
        if let Some((s_str, e_str)) = part.split_once('-') {
            let e_str = e_str.strip_prefix('L').unwrap_or(e_str);
            let s = s_str.trim().parse::<usize>().ok()?;
            let e = e_str.trim().parse::<usize>().ok()?;
            spans.push((s, e));
        } else {
            let n = part.parse::<usize>().ok()?;
            spans.push((n, n));
        }
    }
    if spans.is_empty() {
        None
    } else {
        Some(spans)
    }
}

/// Parses the `:ranges` tail of a read selector into merged, ordered spans.
///
/// A bare `N` expands to `N..N+120` (rg-style peek) *only* when it is the sole span; inside a
/// multi-range list it means exactly line `N`, so `path:5,90` does not silently swallow 120
/// lines twice.
fn expand_line_spans(spans: Vec<LineSpan>) -> Vec<LineSpan> {
    let single = spans.len() == 1;
    let mut out: Vec<LineSpan> = spans
        .into_iter()
        .map(|(s, e)| {
            if single && s == e {
                (s, s + 120)
            } else {
                (s.min(e), s.max(e))
            }
        })
        .collect();
    out.sort_by_key(|(s, _)| *s);
    let mut merged: Vec<LineSpan> = Vec::with_capacity(out.len());
    for (s, e) in out {
        match merged.last_mut() {
            // Overlapping or touching spans read as one continuous block.
            Some((_, prev_e)) if s <= prev_e.saturating_add(1) => {
                *prev_e = (*prev_e).max(e);
            }
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// Splits `path:ranges` into the path and the merged, ordered spans.
///
/// `read` accepts the shape people already type at `sed -n`: several disjoint regions of one
/// file in a single call (`src/x.rs:1-12,640-665`). Without it every multi-region look costs
/// an extra `bash sed` round trip — measured at 53% of all file inspection.
fn split_inline_line_spans(raw: &str) -> (&str, Option<Vec<LineSpan>>) {
    if Path::new(raw).exists() || expand_tilde(raw).exists() {
        return (raw, None);
    }
    if let Some((base, suffix)) = raw.rsplit_once(':') {
        let is_bare_ssh_host = !base.contains(':') && split_remote_prefix(raw).is_some();
        if !base.is_empty() && !base.ends_with(':') && !is_bare_ssh_host {
            if let Some(spans) = parse_line_spans(suffix) {
                return (base, Some(expand_line_spans(spans)));
            }
            // rg-style `path:line:col`: the base itself ends with `:line`.
            if let Some((inner_base, line_str)) = base.rsplit_once(':') {
                if !inner_base.is_empty() && !inner_base.ends_with(':') {
                    if let Ok(line_no) = line_str.parse::<usize>() {
                        return (inner_base, Some(vec![(line_no, line_no + 120)]));
                    }
                }
            }
        }
    }
    (raw, None)
}

/// Renders several disjoint regions of `content` in one block, the way `sed -n 'a,b;c,d'`
/// does — but with absolute line numbers and an explicit gap marker per jump.
fn format_spans(content: &str, spans: &[LineSpan]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    if total == 0 {
        return "[EOF STOP: File is empty (0 lines).]\n".to_string();
    }
    let budget: usize = 250;
    let mut out = String::new();
    let mut printed = 0usize;
    let mut shown_any = false;
    for &(s, e) in spans {
        let s = s.max(1);
        if s > total {
            out.push_str(&format!(
                "... [requested {}-{} is past EOF ({} lines)] ...\n",
                s, e, total
            ));
            continue;
        }
        let e = e.min(total);
        let take = (e - s + 1).min(budget.saturating_sub(printed).max(1));
        if shown_any {
            out.push_str(&format!("... [jump to line {}] ...\n", s));
        }
        for (i, line) in lines[(s - 1)..(s - 1 + take)].iter().enumerate() {
            out.push_str(&format!("{:>4}: {}\n", s + i, line));
        }
        printed += take;
        shown_any = true;
        if take < e - s + 1 {
            out.push_str(&format!(
                "... [{} lines elided; {} line budget reached] ...\n",
                (e - s + 1) - take,
                budget
            ));
        }
    }
    if !shown_any {
        return format!(
            "(File has {} lines; none of the requested ranges exist)\n",
            total
        );
    }
    out.push_str(&format!("... [EOF: line {}/{}]\n", total, total));
    out
}

fn format_lines(content: &str, start_line: Option<usize>, end_line: Option<usize>) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let s = start_line.unwrap_or(1).max(1);
    if s > total {
        return format!(
            "[EOF STOP: File only has {} lines total (1..{}). Requested start_line={} is past EOF — STOP calling read on this file!]",
            total, total, s
        );
    }
    let default_end = if start_line.is_some() {
        (s + 199).min(total)
    } else {
        total
    };
    let e = end_line.unwrap_or(default_end).min(s + 249).min(total);
    if s > e {
        return format!(
            "(File has {} lines; requested range {}..{} is empty)",
            total, s, e
        );
    }

    let slice = &lines[(s - 1)..e];
    if total > 240 && start_line.is_none() && end_line.is_none() {
        let mut out = String::new();
        for (i, line) in lines.iter().take(100).enumerate() {
            out.push_str(&format!("{:>4}: {}\n", i + 1, line));
        }
        let mid_end = total.saturating_sub(60);
        out.push_str(&format!(
            "\n... [SPAVN Structural Outline of lines 101..{} ({} total lines) — pass start_line/end_line or path:N-M to read exact function body] ...\n",
            mid_end, total
        ));
        let mut collected_syms: Vec<(usize, &str)> = Vec::new();
        for (idx, line) in lines.iter().enumerate().take(mid_end).skip(100) {
            if is_structural_outline_line(line) {
                collected_syms.push((idx + 1, line));
            }
        }
        let max_syms = 120usize;
        if collected_syms.len() <= max_syms {
            for (line_no, line) in collected_syms {
                out.push_str(&format!("{:>4}: {}\n", line_no, line));
            }
        } else {
            let total_syms = collected_syms.len();
            for (i, (line_no, line)) in collected_syms.iter().enumerate() {
                if i < 90 || i + 25 >= total_syms {
                    out.push_str(&format!("{:>4}: {}\n", line_no, line));
                } else if i == 90 {
                    out.push_str(&format!(
                        "  .. [{} middle symbols elided] ..\n",
                        total_syms.saturating_sub(115)
                    ));
                }
            }
        }
        out.push_str("... [End of Structural Outline; showing final 60 lines] ...\n\n");
        for (i, line) in lines.iter().enumerate().skip(mid_end) {
            out.push_str(&format!("{:>4}: {}\n", i + 1, line));
        }
        out.push_str(&format!("... [EOF: line {}/{}]\n", total, total));
        return out;
    }

    let mut out = String::new();
    for (i, line) in slice.iter().enumerate() {
        out.push_str(&format!("{:>4}: {}\n", s + i, line));
    }
    if e == total {
        out.push_str(&format!("... [EOF: line {}/{}]\n", total, total));
    } else if s == 1 && total > e + 30 {
        let tail_start = total.saturating_sub(15).max(e);
        out.push_str(&format!(
            "... [Showing lines 1..{} of {} total; Structural Outline of {}..{} + final 15 lines below] ...\n",
            e,
            total,
            e + 1,
            tail_start
        ));
        for (idx, line) in lines.iter().enumerate().take(tail_start).skip(e) {
            if is_structural_outline_line(line) {
                out.push_str(&format!("{:>4}: {}\n", idx + 1, line));
            }
        }
        out.push_str(&format!(
            "... [Final lines {}..{}] ...\n",
            tail_start + 1,
            total
        ));
        for (idx, line) in lines.iter().enumerate().skip(tail_start) {
            out.push_str(&format!("{:>4}: {}\n", idx + 1, line));
        }
        out.push_str(&format!("... [EOF: line {}/{}]\n", total, total));
    } else {
        out.push_str(&format!(
            "... [Showing lines {}..{} of {} total]\n",
            s, e, total
        ));
    }
    out
}

pub(crate) fn get_str_alias<'a>(args: &'a Value, keys: &[&str]) -> &'a str {
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
    let raw_input_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
    let (raw_path, inline_spans) = split_inline_line_spans(raw_input_path);
    let (inline_start, inline_end) = match inline_spans.as_deref() {
        // A single span stays on the plain `start_line`/`end_line` path so the existing
        // head+outline heuristics (and the re-read dedup key) keep working unchanged.
        Some([(s, e)]) => (Some(*s), Some(*e)),
        _ => (None, None),
    };
    if let Err(blocked) = ctx.enforce_path(raw_path, false, "read") {
        return blocked;
    }
    let start_line = args
        .get("start_line")
        .or_else(|| args.get("offset"))
        .or_else(|| args.get("start"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .or(inline_start);
    let limit_lines = args
        .get("limit")
        .or_else(|| args.get("num_lines"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let end_line = args
        .get("end_line")
        .or_else(|| args.get("end"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .or_else(|| limit_lines.map(|lim| start_line.unwrap_or(1).max(1) + lim.saturating_sub(1)))
        .or(inline_end);
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
    if let Some(ext) = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
    {
        let mime_opt = match ext.as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "webp" => Some("image/webp"),
            "gif" => Some("image/gif"),
            _ => None,
        };
        if let Some(mime) = mime_opt {
            return match fs::read(&path) {
                Ok(bytes) => {
                    if bytes.len() > 15 * 1024 * 1024 {
                        format!(
                            "Read error ({}): image exceeds 15MB limit ({} bytes)",
                            path.display(),
                            bytes.len()
                        )
                    } else {
                        let kb = (bytes.len() as f64 / 1024.0).max(0.1);
                        let fname = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| raw_path.to_string());
                        let b64 = crate::router::base64_encode_bytes(&bytes);
                        let drag_img = crate::router::DraggedImage {
                            raw_span: raw_path.to_string(),
                            path: key.clone(),
                            filename: fname.clone(),
                            mime,
                            size_bytes: bytes.len(),
                            base64: b64,
                        };
                        crate::hud::render_inline_image_preview(&drag_img);
                        crate::router::register_read_image(&key, drag_img);
                        format!(
                            "[SPAVN Vision Image Loaded: {} ({}, {:.1} KB) — multimodal image attached to your visual context]",
                            key, mime, kb
                        )
                    }
                }
                Err(e) => {
                    let hint = suggest_similar_path(raw_path);
                    format!("Read error ({}): {}{}", path.display(), e, hint)
                }
            };
        }
    }
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
            match inline_spans.as_deref() {
                Some(spans) if spans.len() > 1 => format_spans(&content, spans),
                _ => format_lines(&content, start_line, end_line),
            }
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

fn leading_ws_len(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ' || b == b'\t').count()
}

fn reindent_replacement_lines(
    src_matched_lines: &[&str],
    old_text: &str,
    new_text: &str,
) -> Vec<String> {
    let src_indent = src_matched_lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| &l[..leading_ws_len(l)])
        .unwrap_or("");
    let old_indent_len = old_text
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(leading_ws_len)
        .unwrap_or(0);
    let new_min_indent_len = new_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(leading_ws_len)
        .min()
        .unwrap_or(0);

    // If the model dropped or shifted base indentation relative to the file's matched block,
    // re-anchor every non-empty line in `new_text` to `src_indent` while preserving relative nesting!
    let base_strip = if old_indent_len == 0 && new_min_indent_len == 0 && !src_indent.is_empty() {
        Some(0usize)
    } else if old_indent_len != src_indent.len() && old_indent_len == new_min_indent_len {
        Some(old_indent_len)
    } else {
        None
    };

    new_text
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else if let Some(strip_n) = base_strip {
                let take_from = strip_n.min(leading_ws_len(l));
                format!("{}{}", src_indent, &l[take_from..])
            } else {
                l.to_string()
            }
        })
        .collect()
}

fn find_project_manifest_dir(file_path: &Path, manifest_name: &str) -> Option<PathBuf> {
    let start_dir = if file_path.is_dir() {
        file_path.to_path_buf()
    } else {
        file_path
            .parent()
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok())?
    };
    let mut cur = start_dir.as_path();
    for _ in 0..5 {
        if cur.join(manifest_name).is_file() {
            return Some(cur.to_path_buf());
        }
        match cur.parent() {
            Some(parent) if parent != cur => cur = parent,
            _ => break,
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(manifest_name).is_file() {
            return Some(cwd);
        }
    }
    None
}

fn first_n_diagnostic_lines(out: &[u8], err: &[u8], max_lines: usize) -> String {
    let err_str = String::from_utf8_lossy(err);
    let out_str = String::from_utf8_lossy(out);
    let combined = if err_str.trim().is_empty() {
        out_str.trim().to_string()
    } else if out_str.trim().is_empty() {
        err_str.trim().to_string()
    } else {
        format!("{}\n{}", err_str.trim(), out_str.trim())
    };
    combined
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn build_live_project_signatures(cwd: &str) -> String {
    let root = expand_tilde(cwd);
    if !root.is_dir() {
        return String::new();
    }
    let mut files: Vec<PathBuf> = Vec::new();
    fn collect_src_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 3 || out.len() >= 24 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= 24 {
                break;
            }
            let p = entry.path();
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            if name.starts_with('.')
                || matches!(
                    name.as_ref(),
                    "target" | "node_modules" | "dist" | "build" | "vendor" | "__pycache__"
                )
            {
                continue;
            }
            if p.is_dir() {
                collect_src_files(&p, depth + 1, out);
            } else if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                if matches!(ext, "rs" | "ts" | "tsx" | "py" | "go" | "js") {
                    out.push(p);
                }
            }
        }
    }
    collect_src_files(&root, 0, &mut files);
    files.sort();
    let mut summary = String::new();
    let mut total_sigs = 0usize;
    for f in files {
        if total_sigs >= 65 || summary.len() >= 2400 {
            break;
        }
        let Ok(text) = fs::read_to_string(&f) else {
            continue;
        };
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(&f)
            .to_string_lossy()
            .to_string();
        let mut file_sigs = Vec::new();
        for (idx, line) in text.lines().enumerate() {
            if is_structural_outline_line(line) {
                let clean = line.trim().trim_end_matches('{').trim();
                file_sigs.push(format!("  L{}: {}", idx + 1, clean));
                total_sigs += 1;
                if file_sigs.len() >= 12 || total_sigs >= 65 {
                    break;
                }
            }
        }
        if !file_sigs.is_empty() {
            summary.push_str(&format!("{}:\n{}\n", rel, file_sigs.join("\n")));
        }
    }
    summary
}

pub fn check_unfinished_placeholders(modified_files: &[String]) -> Option<String> {
    for raw in modified_files {
        if split_remote_prefix(raw).is_some() {
            continue;
        }
        let path = expand_tilde(raw);
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if !matches!(ext, "rs" | "ts" | "tsx" | "py" | "go" | "js") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") || trimmed.starts_with('#') {
                    continue;
                }
                if (trimmed.contains("todo!()") || trimmed.contains("unimplemented!()"))
                    && !trimmed.contains('"')
                    && !trimmed.contains('\'')
                {
                    return Some(format!(
                        "[SPAVN ZERO-STOP GATE]: File `{}` (line {}) still contains an unfinished placeholder (`{}`). Implement the complete production logic before finishing.",
                        raw,
                        idx + 1,
                        trimmed
                    ));
                }
            }
        }
    }
    None
}

pub fn run_project_verification_gate(cwd: &str, modified_files: &[String]) -> Option<String> {
    if let Some(placeholder_err) = check_unfinished_placeholders(modified_files) {
        return Some(placeholder_err);
    }

    let root = expand_tilde(cwd);
    if let Some(cargo_dir) = find_project_manifest_dir(&root, "Cargo.toml") {
        let mut cmd = Command::new("cargo");
        cmd.args(["check", "--message-format=short", "-q"])
            .current_dir(&cargo_dir);
        if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 15) {
            if !timed_out && code != 0 {
                let diag = first_n_diagnostic_lines(&out, &err, 20);
                if !diag.is_empty() {
                    return Some(format!(
                        "[SPAVN ZERO-STOP COMPILER GATE]: `cargo check` failed with errors. Fix these compilation errors immediately before finishing:\n{}",
                        diag
                    ));
                }
            }
        }
    } else if let Some(ts_dir) = find_project_manifest_dir(&root, "tsconfig.json") {
        let mut cmd = Command::new("tsc");
        cmd.args(["--noEmit", "--pretty", "false"])
            .current_dir(&ts_dir);
        if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 12) {
            if !timed_out && code != 0 {
                let diag = first_n_diagnostic_lines(&out, &err, 20);
                if !diag.is_empty() {
                    return Some(format!(
                        "[SPAVN ZERO-STOP COMPILER GATE]: `tsc --noEmit` failed with errors. Fix these type/syntax errors immediately before finishing:\n{}",
                        diag
                    ));
                }
            }
        }
    } else {
        let py_files: Vec<String> = modified_files
            .iter()
            .filter(|raw| {
                split_remote_prefix(raw).is_none()
                    && expand_tilde(raw)
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.eq_ignore_ascii_case("py"))
                        .unwrap_or(false)
            })
            .cloned()
            .collect();
        for py in &py_files {
            let p = expand_tilde(py);
            if p.is_file() {
                let mut cmd = Command::new("python3");
                cmd.args(["-m", "py_compile", &p.to_string_lossy()]);
                if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 8) {
                    if !timed_out && code != 0 {
                        let diag = first_n_diagnostic_lines(&out, &err, 15);
                        if !diag.is_empty() {
                            return Some(format!(
                                "[SPAVN ZERO-STOP PYTHON GATE]: `py_compile` failed on `{}`:\n{}",
                                py, diag
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

fn count_code_braces(src: &str) -> (usize, usize) {
    let mut open = 0usize;
    let mut close = 0usize;
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        // Line comment `// ...`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment `/* ... */`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        // Rust raw string literal `r#"..."#` or `r"..."`
        if b == b'r' && i + 1 < len && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#') {
            let mut hashes = 0usize;
            let mut j = i + 1;
            while j < len && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < len && bytes[j] == b'"' {
                j += 1;
                while j < len {
                    if bytes[j] == b'"'
                        && j + hashes < len
                        && bytes[j + 1..j + 1 + hashes].iter().all(|&x| x == b'#')
                    {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        // Double-quoted string `"..."`
        if b == b'"' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Character literal `'x'` or `'\x'`
        if b == b'\'' {
            if i + 2 < len && bytes[i + 1] != b'\\' && bytes[i + 2] == b'\'' {
                i += 3;
                continue;
            }
            if i + 3 < len && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'' {
                i += 4;
                continue;
            }
        }
        if b == b'{' {
            open += 1;
        } else if b == b'}' {
            close += 1;
        }
        i += 1;
    }
    (open, close)
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
            let mut cmd = Command::new("python3");
            cmd.args(["-m", "py_compile", &path.to_string_lossy()]);
            if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 8) {
                if timed_out {
                    String::new()
                } else if code == 0 {
                    " [Syntax Check: Python OK]".to_string()
                } else {
                    format!(
                        "\n[AUTO-DIAGNOSTIC ERROR in {}]:\n{}",
                        raw_path,
                        first_n_diagnostic_lines(&out, &err, 15)
                    )
                }
            } else {
                String::new()
            }
        }
        "js" | "mjs" | "cjs" => {
            let mut cmd = Command::new("node");
            cmd.args(["--check", &path.to_string_lossy()]);
            if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 8) {
                if timed_out {
                    String::new()
                } else if code == 0 {
                    " [Syntax Check: JS OK]".to_string()
                } else {
                    format!(
                        "\n[AUTO-DIAGNOSTIC ERROR in {}]:\n{}",
                        raw_path,
                        first_n_diagnostic_lines(&out, &err, 15)
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
                        first_n_diagnostic_lines(&out.stdout, &out.stderr, 15)
                    )
                }
            } else {
                String::new()
            }
        }
        "rs" | "ts" | "tsx" | "go" | "swift" => {
            let (open_braces, close_braces) = count_code_braces(content);
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
    if let Some(edits_arr) = args.get("edits").and_then(|v| v.as_array()) {
        if !edits_arr.is_empty() {
            let original_snapshot = read_target_file(raw_path).ok();
            let mut results = Vec::new();
            for (idx, item) in edits_arr.iter().enumerate() {
                let sub_old = get_str_alias(
                    item,
                    &["old_text", "old_string", "old_str", "search", "target"],
                );
                let sub_new = item
                    .get("new_text")
                    .or_else(|| item.get("new_string"))
                    .or_else(|| item.get("new_str"))
                    .or_else(|| item.get("replace"))
                    .or_else(|| item.get("replacement"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let single_arg = json!({
                    "path": raw_path,
                    "old_text": sub_old,
                    "new_text": sub_new,
                });
                let out = exec_edit_single(ctx, &single_arg);
                if !out.starts_with("Successfully ") {
                    if idx > 0 {
                        if let Some(ref orig) = original_snapshot {
                            let _ = write_target_file(raw_path, orig);
                            for _ in 0..idx {
                                let _ = ctx.undo_stack.pop();
                            }
                        }
                    }
                    return format!(
                        "Multi-edit failed at hunk #{} (atomic rollback restored {} to pre-edit state): {}",
                        idx + 1,
                        raw_path,
                        out
                    );
                }
                results.push(out);
            }
            return results.last().cloned().unwrap_or_else(|| {
                format!(
                    "Successfully edited {} ({} hunks applied)",
                    raw_path,
                    edits_arr.len()
                )
            });
        }
    }
    exec_edit_single(ctx, args)
}

fn exec_edit_single(ctx: &mut ToolContext, args: &Value) -> String {
    let raw_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
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

    if ctx.access == AccessMode::Safe
        && !prompt_user_approval(
            ctx,
            &format!("Dosya Düzenleme (edit): {}", raw_path),
            new_text,
        )
    {
        return "User rejected edit in SAFE mode.".to_string();
    }

    let mtime_warn = ctx.check_external_mtime_shift(raw_path);

    let content = match read_target_file(raw_path) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let use_crlf = content.contains("\r\n") && !old_text.contains("\r\n");
    let mut effective_old = if use_crlf {
        old_text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        old_text.to_string()
    };
    let mut effective_new = if use_crlf {
        new_text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        new_text.to_string()
    };
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
            let matched_slice = &src_lines[start_idx..start_idx + needle_lines.len()];
            for l in reindent_replacement_lines(matched_slice, old_text, new_text) {
                new_lines.push(l);
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
                let matched_slice = &src_lines[first_src_line..=last_src_line];
                for l in reindent_replacement_lines(matched_slice, old_text, new_text) {
                    new_lines.push(l);
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

    if ctx.access == AccessMode::Safe
        && !prompt_user_approval(ctx, &format!("Dosya Yazma (write): {}", raw_path), content)
    {
        return "User rejected write in SAFE mode.".to_string();
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

fn check_bash_gate(
    ctx: &mut ToolContext,
    cmd: &str,
    host: &str,
    cwd: Option<&str>,
) -> Result<(), String> {
    let mut policy = ctx.policy.clone();
    if let Some(dir) = cwd.map(str::trim).filter(|d| !d.is_empty()) {
        policy.cwd = policy.resolve(dir);
    }
    let risk = assess_shell_command(cmd, &policy);

    if let ShellRisk::Catastrophic(reason) = &risk {
        return Err(format!(
            "BLOCKED by SPAVN Protected Path Policy: {}",
            reason
        ));
    }

    let auto_sensitive =
        matches!(risk, ShellRisk::Destructive(_)) || crate::config::host_is_sensitive(host);
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
                    _ => format!("hassas host üzerinde çalıştırma: {}", host),
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

/// Gate for every outbound fetch.
///
/// Three states, in order: `Private` (RFC1918, link-local, IMDS, CGNAT) is refused outright — no
/// prompt can make it safe, and it is where cloud credentials live. `Loopback` needs an explicit
/// `allow_local` from the caller. `Public` asks in SAFE, or in PLAN where the phase must stay
/// read-only for the workspace.
fn check_egress_gate(ctx: &mut ToolContext, url: &str, allow_local: bool) -> Option<String> {
    use crate::guard::{classify_egress, EgressTarget};
    match classify_egress(url)? {
        EgressTarget::Private => Some(format!(
            "BLOCKED by SPAVN Egress Policy: {} özel/link-local bir adres hedefliyor.",
            url
        )),
        EgressTarget::Loopback => {
            if allow_local {
                return None;
            }
            Some(format!(
                "BLOCKED by SPAVN Egress Policy: {} loopback adresi; `allow_local: true` olmadan açılmaz.",
                url
            ))
        }
        EgressTarget::Public => {
            // A sub-agent has no terminal: prompting would resolve to "no" and cost it the
            // only tool that can reach documentation, so policy alone decides there.
            let needs_prompt = ctx.can_prompt
                && (ctx.access == AccessMode::Safe
                    || ctx.phase == crate::spavn_engine::AgentPhase::Plan);
            if needs_prompt && !prompt_user_approval(ctx, "Web erişimi (fetch)", url) {
                return Some("Web fetch reddedildi (onay yok).".to_string());
            }
            None
        }
    }
}

fn exec_bash(ctx: &mut ToolContext, args: &Value) -> String {
    let command = get_str_alias(args, &["command", "cmd", "script", "shell"]);
    let explicit_host = get_str_alias(args, &["host", "target"]);
    let host = if explicit_host.is_empty() {
        ctx.default_host.clone()
    } else {
        explicit_host.to_string()
    };
    // Must run before the gate: the gate *displays* the host, and a poisoned alias would be
    // approved as text and then executed as an `ssh` option.
    if let Err(blocked) = validate_host_arg(&host) {
        return blocked;
    }
    let cwd = args.get("cwd").and_then(|v| v.as_str());
    let timeout_sec = args
        .get("timeout_sec")
        .and_then(|v| v.as_u64())
        .unwrap_or(120);
    let background = args
        .get("background")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let Err(blocked) = check_bash_gate(ctx, command, &host, cwd) {
        return blocked;
    }

    let mut cmd_obj = if host != "local" && !host.is_empty() {
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
        let shell_bin = crate::config::shell_bin();
        let mut c = if cfg!(target_os = "macos")
            && ctx.access == AccessMode::Safe
            && Path::new("/usr/bin/sandbox-exec").exists()
        {
            let ws = ctx.policy.cwd.to_string_lossy().replace('"', "\\\"");
            let profile = format!(
                "(version 1)(allow default)(deny file-write* (subpath \"/\"))(allow file-write* (subpath \"{}\") (subpath \"/tmp\") (subpath \"/private/tmp\") (subpath \"/var/folders\") (subpath \"/private/var/folders\") (literal \"/dev/null\"))",
                ws
            );
            let mut sb = Command::new("/usr/bin/sandbox-exec");
            sb.args(["-p", &profile, &shell_bin, "-c", command]);
            sb
        } else {
            let mut sh = Command::new(&shell_bin);
            let low_shell = shell_bin.to_ascii_lowercase();
            if low_shell.contains("pwsh") || low_shell.contains("powershell") {
                sh.args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    command,
                ]);
            } else if low_shell.ends_with("cmd.exe") || low_shell == "cmd" {
                sh.args(["/C", command]);
            } else {
                sh.args(["-c", command]);
            }
            sh
        };
        let home = home_dir();
        let extra_dirs = [
            home.join(".cargo/bin"),
            home.join(".local/bin"),
            home.join("bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        let cur_path = std::env::var("PATH").unwrap_or_default();
        let mut path_parts: Vec<String> = extra_dirs
            .iter()
            .filter(|d| d.is_dir())
            .map(|d| d.to_string_lossy().to_string())
            .filter(|s| !cur_path.split(':').any(|p| p == s))
            .collect();
        if !cur_path.is_empty() {
            path_parts.push(cur_path);
        }
        if !path_parts.is_empty() {
            c.env("PATH", path_parts.join(":"));
        }
        if let Some(dir) = cwd {
            let expanded = expand_tilde(dir);
            if expanded.exists() {
                c.current_dir(expanded);
            }
        }
        c
    };

    if background {
        // Owner-only directory instead of `temp_dir()`: `/tmp` is shared, so the old predictable
        // `spavn-bg-<pid>-<step>.log` name let another local user pre-create it (as a symlink) and
        // capture or clobber the job's output. `create_new` also refuses an existing path.
        let log_dir = crate::config::spavn_dir().join("bg");
        let _ = fs::create_dir_all(&log_dir);
        crate::config::set_owner_only_mode(&log_dir, 0o700);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let log_path = log_dir.join(format!(
            "bg-{}-{}-{:09}.log",
            std::process::id(),
            ctx.tool_step_counter,
            nonce
        ));
        let log_file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log_path)
        {
            Ok(f) => f,
            Err(e) => return format!("Background spawn error: cannot create log: {}", e),
        };
        crate::config::set_owner_only_mode(&log_path, 0o600);
        if let Ok(err_file) = log_file.try_clone() {
            cmd_obj
                .stdout(std::process::Stdio::from(log_file))
                .stderr(std::process::Stdio::from(err_file));
        }
        return match cmd_obj.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                format!(
                    "Background job started (PID: {}). Output is streaming to `{}` — use `read` or `bash` to inspect progress.",
                    pid,
                    log_path.display()
                )
            }
            Err(e) => format!("Background spawn error: {}", e),
        };
    }

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
                let dropped = tail_start.saturating_sub(head_end);
                let spilled = crate::spill::spill_output(&format!("bash-{command}"), &combined);
                combined = format!(
                    "{}\n\n{}\n\n{}",
                    &combined[..head_end],
                    crate::spill::truncation_marker(dropped, combined.len(), spilled.as_deref()),
                    &combined[tail_start..]
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

/// Directory patterns that ripgrep must never descend into. Shared by `search` and `glob` so a
/// file-hunting call cannot accidentally walk a home directory into a multi-minute stall.
///
/// The leading `**/` is load-bearing: a bare `!target/**` is matched relative to the search root,
/// so it silently stops working the moment the caller passes an absolute path.
const RG_IGNORE_GLOBS: &[&str] = &[
    "!**/Library/**",
    "!**/Applications/**",
    "!**/Pictures/**",
    "!**/Movies/**",
    "!**/Music/**",
    "!**/*OneDrive*/**",
    "!**/.Trash/**",
    "!**/node_modules/**",
    "!**/target/**",
    "!**/.git/**",
    "!**/.omp/**",
    "!**/.cache/**",
    "!**/.cargo/**",
    "!**/.npm/**",
];

fn push_ignore_globs(cmd: &mut Command) {
    for pat in RG_IGNORE_GLOBS {
        cmd.args(["-g", pat]);
    }
}

fn rg_binary() -> &'static str {
    if Path::new("/opt/homebrew/bin/rg").exists() {
        "/opt/homebrew/bin/rg"
    } else {
        "rg"
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
        if let Err(blocked) = validate_host_arg(target_host) {
            return blocked;
        }
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
    let mut cmd = Command::new(rg_binary());
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
    ]);
    push_ignore_globs(&mut cmd);
    cmd.arg(pattern).arg(path.as_os_str());

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

/// Default and hard cap for `glob` results: enough for a real project, small enough that a
/// runaway pattern cannot flood the model context.
const GLOB_DEFAULT_LIMIT: usize = 100;
const GLOB_MAX_LIMIT: usize = 1000;

fn exec_glob(ctx: &mut ToolContext, args: &Value) -> String {
    let pattern = {
        let p = get_str_alias(args, &["pattern", "glob", "query", "name", "match"]);
        if p.is_empty() {
            return "glob: missing `pattern` (e.g. `**/*.rs`)".to_string();
        }
        p
    };
    let raw_path = {
        let p = get_str_alias(args, &["path", "dir", "directory", "root", "cwd"]);
        if p.is_empty() {
            "."
        } else {
            p
        }
    };
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).clamp(1, GLOB_MAX_LIMIT))
        .unwrap_or(GLOB_DEFAULT_LIMIT);
    let hidden = args
        .get("hidden")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let explicit_host = get_str_alias(args, &["host", "target"]);

    if let Some((ssh_host, remote_path)) = split_remote_prefix(raw_path) {
        let q_pat = shell_quote_remote_path(pattern);
        let q_path = shell_quote_remote_path(remote_path);
        // The positive glob goes first so the negations below still win (rg is
        // last-match-wins). `--hidden` is the only thing that re-admits dotfiles.
        let remote_cmd = format!(
            "export PATH=\"/opt/homebrew/bin:/usr/local/bin:$PATH\"; \
             if command -v rg >/dev/null 2>&1; then \
                 rg --files --color never {} -g {} -- {} | head -n {}; \
             else \
                 find {} -type f -name {} | head -n {}; \
             fi",
            if hidden { "--hidden" } else { "" },
            q_pat,
            q_path,
            limit,
            q_path,
            q_pat,
            limit
        );
        let out = Command::new("ssh")
            .args(["-o", "ConnectTimeout=6", &ssh_host, &remote_cmd])
            .output();
        return match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout).to_string();
                if s.trim().is_empty() {
                    format!(
                        "(no files match `{}` in {}:{})",
                        pattern, ssh_host, remote_path
                    )
                } else {
                    s
                }
            }
            Err(e) => format!("Remote glob error: {}", e),
        };
    }

    let target_host = if !explicit_host.is_empty() {
        explicit_host
    } else {
        ctx.default_host.as_str()
    };
    if target_host != "local" && !target_host.is_empty() {
        return format!(
            "glob: remote host '{}' configured but path has no '<host>:' prefix",
            target_host
        );
    }

    if let Err(blocked) = ctx.enforce_path(raw_path, false, "glob") {
        return blocked;
    }
    let path = expand_tilde(raw_path);

    // Order matters: ripgrep applies `-g` overrides last-match-wins, and a *positive* glob also
    // bypasses the implicit hidden filter. So the pattern goes first, and the negations that
    // should win over it (dotfiles, build dirs) go last.
    let mut cmd = Command::new(rg_binary());
    cmd.args(["--files", "--color", "never"]);
    if hidden {
        cmd.arg("--hidden");
    }
    cmd.args(["-g", pattern]);
    if !hidden {
        cmd.args(["-g", "!**/.*"]);
    }
    push_ignore_globs(&mut cmd);
    cmd.arg(path.as_os_str());

    match run_command_with_timeout(cmd, 8) {
        Ok((_, stdout, _, timed_out)) => {
            let raw = String::from_utf8_lossy(&stdout);
            let lines: Vec<&str> = raw
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(limit)
                .collect();
            if lines.is_empty() {
                return format!("(no files match `{}` in {})", pattern, path.display());
            }
            let truncated = !timed_out && lines.len() == limit;
            let mut out = lines.join("\n");
            if timed_out {
                out.push_str("\n[glob: 8s time limit reached, showing the first matches]");
            } else if truncated {
                out.push_str(&format!(
                    "\n[glob: limit {} reached — narrow the pattern or pass a higher `limit`]",
                    limit
                ));
            }
            out
        }
        Err(e) => format!("Glob error: {}", e),
    }
}

fn exec_memory(ctx: &mut ToolContext, args: &Value) -> String {
    let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("recall");
    let query = get_str_alias(args, &["query", "text", "content", "note"]);
    let cfg = load_user_config();
    let cwd = crate::memory::current_workdir();

    if op == "remember" {
        if ctx.access == AccessMode::Safe
            && !prompt_user_approval(ctx, "Hafızaya Kayıt (remember)", query)
        {
            return "User rejected memory write in SAFE mode.".to_string();
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
                    .args([script_path.to_string_lossy().as_ref(), kind, query, "spavn"])
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
                .args([script_path.to_string_lossy().as_ref(), query])
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
    // Key goes in a header (documented by Exa), never in the URL where logs and proxies keep it.
    let api_key = std::env::var("EXA_API_KEY").unwrap_or_default();
    let mut headers = vec![("Accept", "application/json, text/event-stream")];
    if !api_key.trim().is_empty() {
        headers.push(("x-api-key", api_key.trim()));
    }
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments
        }
    });
    let (status, raw) =
        crate::http::post_json("https://mcp.exa.ai/mcp", &payload.to_string(), &headers, 15)
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

fn exec_web(ctx: &mut ToolContext, args: &Value) -> String {
    let op_raw = get_str_alias(args, &["op", "action", "mode"]);
    let url = get_str_alias(args, &["url", "href", "link"]);
    // `url` is deliberately absent: with it here, `web search {"query":"https://…"}` silently
    // became a fetch — an unrequested outbound GET the model never asked for.
    let query = get_str_alias(args, &["query", "q", "search"]);
    let is_fetch = op_raw.eq_ignore_ascii_case("fetch")
        || op_raw.eq_ignore_ascii_case("read")
        || (!url.is_empty() && (url.starts_with("http://") || url.starts_with("https://")));

    if is_fetch {
        let target_url = if !url.is_empty() { url } else { query };
        if target_url.is_empty() {
            return "Web fetch error: missing 'url' parameter.".to_string();
        }
        let allow_local = args
            .get("allow_local")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(blocked) = check_egress_gate(ctx, target_url, allow_local) {
            return blocked;
        }
        if !crate::guard::egress_is_blocked(target_url) {
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
        let fetched = crate::http::get_text_public(
            target_url,
            &[
                (
                    "User-Agent",
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/537.36",
                ),
                ("Accept", "text/markdown, text/plain;q=0.9, text/html;q=0.8"),
            ],
            12,
        );
        return match fetched {
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

fn run_single_subagent_worker(
    model_key: &str,
    default_host: &str,
    policy: &PathPolicy,
    task: &str,
    extra_ctx: &str,
    expected: &str,
) -> String {
    let ep = crate::router::resolve_endpoint(model_key);
    let mut sub_ctx = ToolContext::new(
        AccessMode::Safe,
        default_host.to_string(),
        policy.cwd.clone(),
        crate::spavn_engine::AgentPhase::Plan,
    );
    sub_ctx.policy = policy.clone();
    sub_ctx.can_prompt = false;

    let mut sub_tools = tool_definitions_for_phase(crate::spavn_engine::AgentPhase::Plan);
    if let Value::Array(ref mut items) = sub_tools {
        items.retain(|it| {
            it.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                != Some("subagent")
        });
    }

    let mut user_prompt = format!("## Sub-Agent Task\n{}", task.trim());
    if !extra_ctx.trim().is_empty() {
        user_prompt.push_str(&format!("\n\n## Context\n{}", extra_ctx.trim()));
    }
    if !expected.trim().is_empty() {
        user_prompt.push_str(&format!("\n\n## Expected Output\n{}", expected.trim()));
    }

    let mut msgs = vec![
        crate::router::ChatMessage::system(
            "You are a focused, read-only SPAVN Sub-Agent. Use `read` and `search` to investigate the codebase and return a concise, high-signal summary with exact file:line references. Do not speculate.",
        ),
        crate::router::ChatMessage::user(user_prompt),
    ];

    for step in 0..6 {
        if crate::is_interrupted() {
            return "[Subagent cancelled via Esc/Ctrl+C]".to_string();
        }
        crate::hud::set_spinner_label(&format!("🐝 Alt-ajan araştırıyor (adım {}/6)", step + 1));
        let Ok(res) = crate::router::stream_chat_completion(&ep, &msgs, &sub_tools, |_| {}, |_| {})
        else {
            break;
        };
        let msg = res.message;
        msgs.push(msg.clone());
        if let Some(tcs) = msg.tool_calls {
            for tc in tcs.into_iter().take(4) {
                let tname = tc.function.name.trim().to_string();
                let out = if matches!(
                    tname.as_str(),
                    "read" | "search" | "memory" | "web" | "lsp" | "skill"
                ) {
                    execute_tool(&mut sub_ctx, &tname, &tc.function.arguments)
                } else {
                    "Subagent error: only read-only tools (read, search, memory, web, lsp, skill) are allowed."
                        .to_string()
                };
                msgs.push(crate::router::ChatMessage::tool_result(tc.id, tname, out));
            }
            continue;
        }
        let ans = msg
            .content
            .or(msg.reasoning_content)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !ans.is_empty() {
            return ans;
        }
        break;
    }

    msgs.into_iter()
        .rev()
        .find(|m| m.role == "assistant")
        .and_then(|m| m.content.or(m.reasoning_content))
        .unwrap_or_else(|| "(Sub-agent completed without textual summary)".to_string())
}

/// Collapses a sub-agent task into one short, single-line header label.
fn one_line_task(task: &str) -> String {
    let flat = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 72 {
        return flat;
    }
    let mut cut = flat.chars().take(72).collect::<String>();
    cut.push('…');
    cut
}

fn exec_subagent(ctx: &mut ToolContext, args: &Value) -> String {
    let model_override = get_str_alias(args, &["model", "endpoint"]);
    let target_model = if model_override.is_empty() {
        ctx.current_model.clone()
    } else if model_override.eq_ignore_ascii_case("local") {
        let cfg = crate::config::load_user_config();
        cfg.local_discovery_url
            .as_deref()
            .and_then(crate::router::detect_active_local_model)
            .unwrap_or(cfg.default_model)
    } else {
        model_override.to_string()
    };

    if let Some(tasks_arr) = args.get("tasks").and_then(|v| v.as_array()) {
        if !tasks_arr.is_empty() {
            let batch: Vec<(String, String, String)> = tasks_arr
                .iter()
                .take(4)
                .filter_map(|item| {
                    let t = get_str_alias(item, &["task", "prompt", "query", "objective"]);
                    if t.trim().is_empty() {
                        return None;
                    }
                    let c = get_str_alias(item, &["context", "files", "background"]);
                    let e = get_str_alias(item, &["expected_output", "format", "output"]);
                    Some((t.to_string(), c.to_string(), e.to_string()))
                })
                .collect();
            if batch.is_empty() {
                return "Subagent error: missing 'task' parameter in tasks array.".to_string();
            }
            let host = ctx.default_host.clone();
            let policy = ctx.policy.clone();
            let results: Vec<String> = std::thread::scope(|s| {
                let handles: Vec<_> = batch
                    .iter()
                    .enumerate()
                    .map(|(idx, (t, c, e))| {
                        let m = target_model.as_str();
                        let h = host.as_str();
                        let p = &policy;
                        s.spawn(move || {
                            let res = run_single_subagent_worker(m, h, p, t, c, e);
                            // The task text can be paragraphs long; the header only needs
                            // enough of it to tell the sub-agents apart.
                            format!("### Sub-Agent #{} ({})\n{}", idx + 1, one_line_task(t), res)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .enumerate()
                    .map(|(idx, h)| match h.join() {
                        Ok(out) => out,
                        // A panicking worker used to vanish silently, leaving the parent
                        // model one result short with no hint that anything failed.
                        Err(_) => format!(
                            "### Sub-Agent #{} (failed)\n[Subagent panicked; its result is missing. Re-run it alone with `subagent`.]",
                            idx + 1
                        ),
                    })
                    .collect()
            });
            return results.join("\n\n");
        }
    }

    let task = get_str_alias(args, &["task", "prompt", "query", "objective"]);
    if task.trim().is_empty() {
        return "Subagent error: missing 'task' parameter.".to_string();
    }
    let extra_ctx = get_str_alias(args, &["context", "files", "background"]);
    let expected = get_str_alias(args, &["expected_output", "format", "output"]);

    run_single_subagent_worker(
        &target_model,
        &ctx.default_host,
        &ctx.policy,
        task,
        extra_ctx,
        expected,
    )
}

fn format_mcp_jsonrpc_response(server_name: &str, tool_name: &str, raw_json: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(raw_json) else {
        return raw_json.chars().take(12_000).collect();
    };
    if let Some(err) = v.get("error") {
        return format!("MCP error ({}/{}): {}", server_name, tool_name, err);
    }
    let Some(res) = v.get("result") else {
        return raw_json.chars().take(12_000).collect();
    };
    if let Some(tools_arr) = res.get("tools").and_then(|t| t.as_array()) {
        let mut lines = vec![format!(
            "MCP server `{}` tools ({}):",
            server_name,
            tools_arr.len()
        )];
        for t in tools_arr {
            let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let desc: String = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .chars()
                .take(180)
                .collect();
            lines.push(format!("- `{}`: {}", name, desc));
        }
        return lines.join("\n");
    }
    if let Some(content_arr) = res.get("content").and_then(|c| c.as_array()) {
        let mut out_parts = Vec::new();
        for (idx, item) in content_arr.iter().enumerate() {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "text" => {
                    if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                        out_parts.push(txt.to_string());
                    }
                }
                "image" => {
                    if let Some(b64) = item.get("data").and_then(|d| d.as_str()) {
                        let raw_mime = item
                            .get("mimeType")
                            .and_then(|m| m.as_str())
                            .unwrap_or("image/png");
                        let mime: &'static str = match raw_mime {
                            "image/jpeg" | "image/jpg" => "image/jpeg",
                            "image/webp" => "image/webp",
                            "image/gif" => "image/gif",
                            _ => "image/png",
                        };
                        let approx_bytes = (b64.len() * 3) / 4;
                        let kb = approx_bytes as f64 / 1024.0;
                        let fname = format!("mcp-{}-{}-{}.png", server_name, tool_name, idx);
                        let vpath = format!("/tmp/{}", fname);
                        let drag_img = crate::router::DraggedImage {
                            raw_span: vpath.clone(),
                            path: vpath.clone(),
                            filename: fname,
                            mime,
                            size_bytes: approx_bytes,
                            base64: b64.to_string(),
                        };
                        crate::hud::render_inline_image_preview(&drag_img);
                        crate::router::register_read_image(&vpath, drag_img);
                        out_parts.push(format!(
                            "[SPAVN Vision Image Loaded: {} ({}, {:.1} KB) — multimodal image attached to your visual context]",
                            vpath, mime, kb
                        ));
                    }
                }
                _ => {
                    out_parts.push(item.to_string());
                }
            }
        }
        if !out_parts.is_empty() {
            return cap_mcp_text(&out_parts.join("\n"), 16_000);
        }
    }
    cap_mcp_text(&res.to_string(), 12_000)
}

/// Bounds an MCP payload and says so, instead of silently losing the tail.
///
/// The stdio path has no transport-level body limit (the HTTP path inherits ureq's 10 MB), so a
/// chatty server could pour megabytes into the context window in one call.
fn cap_mcp_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!(
        "{}\n… (MCP yanıtı kırpıldı: {} karakterden ilk {})",
        kept,
        text.chars().count(),
        max_chars
    )
}

/// Verbs an MCP tool may be assumed not to mutate on.
///
/// The gate used to be a deny-list (`send`, `write`, `delete`, …), which missed `commit`,
/// `move_file`, `push`, `checkout`, `evaluate` and `merge_pull_request`. Treating everything that
/// is not a known read verb as mutating fails closed instead. A server the user knows is
/// side-effect free can opt out with `"readOnly": true` in `mcpServers`.
const MCP_READ_VERBS: &[&str] = &[
    "capture_display",
    "describe",
    "fetch",
    "find",
    "get",
    "inspect",
    "list",
    "read",
    "resolve",
    "search",
    "terms",
    "think",
];

/// True when `server:tool` may have side effects and so needs the phase/approval gate.
fn is_mutating_mcp_tool(server_name: &str, tool_name: &str, server_is_read_only: bool) -> bool {
    if tool_name.trim().is_empty() || server_is_read_only {
        return false;
    }
    // The `sovereign` computer-use server drives a real desktop; nothing it exposes is safe to
    // run unattended regardless of verb spelling.
    if server_name.eq_ignore_ascii_case("sovereign") {
        return true;
    }
    let name = tool_name.to_ascii_lowercase();
    let name = name.strip_prefix("sovereign_").unwrap_or(&name);
    !MCP_READ_VERBS.iter().any(|verb| {
        name == *verb
            || name.starts_with(&format!("{}_", verb))
            || name.starts_with(&format!("{}-", verb))
    })
}

fn exec_lsp_and_mcp(ctx: &mut ToolContext, args: &Value) -> String {
    let op = get_str_alias(args, &["op", "action", "mode"]).to_ascii_lowercase();
    if op == "mcp" || !get_str_alias(args, &["server", "mcp_server"]).is_empty() {
        let server_name = get_str_alias(args, &["server", "mcp_server"]);
        let raw_tool_name = get_str_alias(args, &["tool", "name", "method"]);
        let tool_name = if server_name == "sovereign"
            && !raw_tool_name.is_empty()
            && !raw_tool_name.starts_with("sovereign_")
        {
            format!("sovereign_{}", raw_tool_name)
        } else {
            raw_tool_name.to_string()
        };
        let mcp_args = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let server_is_read_only = load_user_config()
            .mcp_servers
            .get(server_name)
            .map(|s| s.read_only)
            .unwrap_or(false);
        if is_mutating_mcp_tool(server_name, &tool_name, server_is_read_only) {
            if ctx.phase == crate::spavn_engine::AgentPhase::Plan {
                return format!(
                    "BLOCKED in PLAN phase: mutating MCP tool `{}:{}` is not allowed in read-only planning mode.",
                    server_name, tool_name
                );
            }
            if ctx.access == AccessMode::Safe {
                let preview = format!("{} {}", tool_name, mcp_args);
                if !prompt_user_approval(ctx, &format!("mcp:{}", server_name), &preview) {
                    return format!(
                        "Kullanıcı MCP çağrısını reddetti: {}:{}",
                        server_name, tool_name
                    );
                }
            }
        }
        let cfg = crate::config::load_user_config();
        let Some(srv) = cfg.mcp_servers.get(server_name) else {
            let mut available: Vec<String> = cfg.mcp_servers.keys().cloned().collect();
            available.sort();
            return format!(
                "MCP server '{}' not found in ~/.spavn/config.json `mcpServers`. Configured servers: {:?}",
                server_name, available
            );
        };
        if let Some(url) = &srv.url {
            let req_body = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": if tool_name.is_empty() { "tools/list" } else { "tools/call" },
                "params": if tool_name.is_empty() {
                    json!({})
                } else {
                    json!({ "name": tool_name, "arguments": mcp_args })
                }
            });
            return match crate::http::post_json(
                url,
                &req_body.to_string(),
                &[("Accept", "application/json, text/event-stream")],
                30,
            ) {
                Ok((_, body)) => format_mcp_jsonrpc_response(server_name, &tool_name, &body),
                Err(e) => format!("MCP HTTP error ({}): {}", server_name, e),
            };
        }
        if let Some(cmd_bin) = &srv.command {
            let expanded_bin = crate::config::expand_user_path(cmd_bin);
            let expanded_args: Vec<String> = srv
                .args
                .iter()
                .map(|a| {
                    if a.starts_with("~/") {
                        crate::config::expand_user_path(a).display().to_string()
                    } else {
                        a.clone()
                    }
                })
                .collect();
            let mut cmd = Command::new(&expanded_bin);
            // The child used to inherit the whole environment, so a compromised or mispackaged
            // MCP server could read every API key the user has exported. Only PATH/HOME plus the
            // server's own configured values cross the boundary.
            let inherited_path = std::env::var("PATH").unwrap_or_default();
            let expanded_env: Vec<(String, String)> = srv
                .env
                .iter()
                .map(|(k, v)| {
                    let value = if v.starts_with("~/") {
                        crate::config::expand_user_path(v).display().to_string()
                    } else {
                        v.clone()
                    };
                    (k.clone(), value)
                })
                .collect();
            cmd.args(&expanded_args)
                .env_clear()
                .env("PATH", inherited_path)
                .env("HOME", crate::config::home_dir())
                .envs(expanded_env)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let Ok(mut child) = cmd.spawn() else {
                return format!("MCP error: failed to spawn `{}`", expanded_bin.display());
            };
            let init_msg = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "spavn", "version": env!("CARGO_PKG_VERSION") }
                }
            });
            let call_msg = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": if tool_name.is_empty() { "tools/list" } else { "tools/call" },
                "params": if tool_name.is_empty() {
                    json!({})
                } else {
                    json!({ "name": tool_name, "arguments": mcp_args })
                }
            });
            if let Some(mut stdin) = child.stdin.take() {
                let _ = writeln!(stdin, "{}", init_msg);
                let _ = writeln!(
                    stdin,
                    "{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}"
                );
                let _ = writeln!(stdin, "{}", call_msg);
            }
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            if let Some(stdout) = child.stdout.take() {
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    let reader = std::io::BufReader::new(stdout);
                    let mut last_json = String::new();
                    // A single JSON-RPC line has no size bound on the wire; capping it here keeps a
                    // chatty or hostile server from forcing a multi-megabyte parse per call.
                    const MCP_LINE_CAP: usize = 2 * 1024 * 1024;
                    for line in reader.lines().map_while(Result::ok) {
                        let trimmed = line.trim();
                        if trimmed.len() > MCP_LINE_CAP {
                            let _ = tx.send(format!(
                                "{{\"error\":\"MCP response line exceeded {} bytes\"}}",
                                MCP_LINE_CAP
                            ));
                            return;
                        }
                        if trimmed.starts_with('{') {
                            last_json = trimmed.to_string();
                            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                                if v.get("id").and_then(|id| id.as_i64()) == Some(2) {
                                    let _ = tx.send(trimmed.to_string());
                                    return;
                                }
                            }
                        }
                    }
                    if !last_json.is_empty() {
                        let _ = tx.send(last_json);
                    }
                });
            }
            let received = rx.recv_timeout(std::time::Duration::from_secs(45)).ok();
            let _ = child.kill();
            let _ = child.wait();
            if let Some(json_line) = received {
                return format_mcp_jsonrpc_response(server_name, &tool_name, &json_line);
            }
            return "(MCP server returned no response within 45s)".to_string();
        }
        return format!(
            "MCP server '{}' has neither `command` nor `url`.",
            server_name
        );
    }

    let symbol = get_str_alias(args, &["symbol", "name", "query", "identifier"]);
    let raw_path = {
        let p = get_str_alias(args, &["path", "file", "dir"]);
        if p.is_empty() {
            "."
        } else {
            p
        }
    };
    if op == "symbols" || (symbol.is_empty() && !raw_path.is_empty()) {
        let target = expand_tilde(raw_path);
        if target.is_file() {
            if let Ok(content) = fs::read_to_string(&target) {
                let mut syms = Vec::new();
                for (idx, line) in content.lines().enumerate() {
                    if is_structural_outline_line(line) {
                        syms.push(format!("{:>4}: {}", idx + 1, line.trim()));
                    }
                }
                if syms.is_empty() {
                    return "(No structural symbols found in file)".to_string();
                }
                return syms.join("\n");
            }
        }
        return build_live_project_signatures(raw_path);
    }

    if symbol.is_empty() {
        return "LSP error: missing 'symbol' parameter.".to_string();
    }
    let pattern = if op == "definition" || op == "def" {
        format!(
            r"(fn|struct|enum|trait|type|const|static|def|class|function|interface|func)\s+{}\b",
            symbol
        )
    } else {
        format!(r"\b{}\b", symbol)
    };
    exec_search(
        ctx,
        &json!({
            "pattern": pattern,
            "path": raw_path
        }),
    )
}

pub fn execute_tool(ctx: &mut ToolContext, name: &str, args_json: &str) -> String {
    ctx.tool_step_counter += 1;
    let args = parse_args_resilient(args_json);
    let normalized = name.trim().to_lowercase();
    if ctx.phase == crate::spavn_engine::AgentPhase::Plan && phase_locked_tool(&normalized) {
        return format!(
            "[SPAVN] READ-ONLY PHASE (PLAN): tool '{}' is locked. Present your plan and stop; the user unlocks changes with /code or \\\\ . Do not retry write, edit or bash.",
            normalized
        );
    }
    match normalized.as_str() {
        "read" | "read_file" | "cat" | "view" => exec_read(ctx, &args),
        "edit" | "edit_file" | "replace" | "patch" => exec_edit(ctx, &args),
        "write" | "write_file" | "create_file" | "save" => exec_write(ctx, &args),
        "bash" | "run_command" | "shell" | "exec" | "terminal" => exec_bash(ctx, &args),
        "search" | "grep" | "rg" | "find_in_files" => exec_search(ctx, &args),
        "glob" | "find_files" | "list_files" | "file_glob" => exec_glob(ctx, &args),
        "memory" | "recall_memory" | "remember" => exec_memory(ctx, &args),
        "skill" | "load_skill" | "use_skill" => exec_skill(&args),
        "web" | "web_search" | "websearch" | "webfetch" | "web_fetch" => exec_web(ctx, &args),
        "subagent" | "spawn_submodels" | "task" | "delegate" => exec_subagent(ctx, &args),
        "lsp" | "mcp" | "goto_definition" | "find_references" | "document_symbols" => {
            exec_lsp_and_mcp(ctx, &args)
        }
        _ => format!(
            "Unknown tool '{}'. Valid tools are: read, edit, write, bash, search, glob, memory, skill, web, subagent, lsp.",
            name
        ),
    }
}

/// Tools that neither mutate the workspace nor reach for shared process state.
///
/// Deliberately excluded: `edit`/`write` (they push onto `undo_stack`, which must stay in
/// declared order for `/undo`), `bash` (may hold `target/` locks or prompt), `subagent`
/// (spawns its own threads) and `mcp` (an unknown server may have side effects).
pub fn parallel_safe_tool(name: &str) -> bool {
    matches!(
        name.trim().to_lowercase().as_str(),
        "read"
            | "read_file"
            | "cat"
            | "view"
            | "search"
            | "grep"
            | "rg"
            | "find_in_files"
            | "glob"
            | "find_files"
            | "list_files"
            | "file_glob"
            | "skill"
            | "load_skill"
            | "use_skill"
            | "web"
            | "web_search"
            | "websearch"
            | "webfetch"
            | "web_fetch"
    )
}

/// Runs a run of independent read-only calls concurrently and returns their results in the
/// order they were declared.
///
/// Each worker gets its own `ToolContext`, so `read_mtimes`, `read_ranges` and
/// `tool_step_counter` are never shared across threads. Every worker is stamped with a distinct
/// step number in declared order, which is what keeps the code-shifting guard meaningful; the
/// per-file bookkeeping is then merged back into `ctx` sequentially so the serial path sees the
/// same state it would have after calling each tool in turn.
pub fn execute_read_only_batch(ctx: &mut ToolContext, calls: &[(String, String)]) -> Vec<String> {
    if calls.len() <= 1 {
        return Vec::new();
    }
    // Two workers could both reach `prompt_user_approval`, and two prompts read the same stdin.
    // Outside SOVEREIGN any path may prompt, so a batch is refused outright.
    if ctx.access != AccessMode::Sovereign
        || calls.iter().any(|(name, _)| !parallel_safe_tool(name))
    {
        return Vec::new();
    }
    let base = ctx.tool_step_counter;
    let mut locals: Vec<ToolContext> = calls
        .iter()
        .enumerate()
        .map(|(idx, _)| {
            let mut local = ctx.clone();
            // A batch is one step of the loop, not one step per call, so hand each worker a
            // stable, distinct stamp instead of letting them all share `base + 1`.
            local.tool_step_counter = base + idx + 1;
            local
        })
        .collect();

    let results: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = locals
            .iter_mut()
            .zip(calls.iter())
            .map(|(local, (name, args))| s.spawn(move || execute_tool(local, name, args)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| "Tool error: worker thread panicked.".to_string())
            })
            .collect()
    });

    for local in locals {
        ctx.read_mtimes.extend(local.read_mtimes);
        ctx.read_ranges.extend(local.read_ranges);
    }
    ctx.tool_step_counter = base + calls.len();
    results
}

pub fn phase_locked_tool(name: &str) -> bool {
    matches!(
        name,
        "write"
            | "write_file"
            | "create_file"
            | "save"
            | "edit"
            | "edit_file"
            | "replace"
            | "patch"
            | "bash"
            | "run_command"
            | "shell"
            | "exec"
            | "terminal"
    )
}

#[cfg(test)]
mod phase_tests {
    use super::*;
    use crate::spavn_engine::AgentPhase;

    fn ctx(phase: AgentPhase) -> ToolContext {
        ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            PathBuf::from("/tmp"),
            phase,
        )
    }

    #[test]
    fn plan_locks_write_edit_bash() {
        let mut c = ctx(AgentPhase::Plan);
        for tool in ["write", "edit", "bash"] {
            let out = execute_tool(&mut c, tool, "{}");
            assert!(
                out.contains("READ-ONLY PHASE"),
                "tool {tool} should be locked in PLAN, got: {out}"
            );
        }
    }

    #[test]
    fn code_allows_read() {
        let mut c = ctx(AgentPhase::Code);
        let out = execute_tool(&mut c, "read", r#"{"path":"/nonexistent-phase-test-xyz"}"#);
        assert!(!out.contains("READ-ONLY PHASE"), "read in CODE: {out}");
    }

    #[test]
    fn plan_allows_read_and_memory() {
        // Guards the fail-closed approval path: with no TTY the prompt answers "no", so any
        // read that needs approval would silently become a block in every test below.
        assert!(!crate::hud::stdin_is_tty(), "tests run without a tty");
        let mut c = ctx(AgentPhase::Plan);
        let out = execute_tool(&mut c, "memory", r#"{"op":"recall","query":"x"}"#);
        assert!(!out.contains("READ-ONLY PHASE"), "memory in PLAN: {out}");
        let out = execute_tool(&mut c, "read", r#"{"path":"/nonexistent-phase-test-xyz"}"#);
        assert!(!out.contains("READ-ONLY PHASE"), "read in PLAN: {out}");
    }

    #[test]
    fn batch_keeps_declared_order_and_distinct_steps() {
        let mut c = ctx(AgentPhase::Code);
        let calls = vec![
            (
                "glob".to_string(),
                r#"{"pattern":"Cargo.toml"}"#.to_string(),
            ),
            (
                "read".to_string(),
                r#"{"path":"/nonexistent-batch-a"}"#.to_string(),
            ),
            (
                "read".to_string(),
                r#"{"path":"/nonexistent-batch-b"}"#.to_string(),
            ),
        ];
        let out = super::execute_read_only_batch(&mut c, &calls);
        assert_eq!(out.len(), 3, "one result per call: {out:?}");
        assert!(out[1].starts_with("Read error"), "{}", out[1]);
        assert!(out[2].starts_with("Read error"), "{}", out[2]);
        assert!(!out[0].contains("Read error"), "{}", out[0]);
        // Three calls consumed three steps, even though they ran as one turn.
        assert_eq!(c.tool_step_counter, 3);
    }

    #[test]
    fn batch_refuses_a_single_call() {
        let mut c = ctx(AgentPhase::Code);
        let calls = vec![("read".to_string(), r#"{"path":"/x"}"#.to_string())];
        assert!(super::execute_read_only_batch(&mut c, &calls).is_empty());
    }

    #[test]
    fn a_non_sovereign_context_never_batches() {
        let mut c = ToolContext::new(
            AccessMode::Safe,
            "local".to_string(),
            PathBuf::from("/tmp"),
            AgentPhase::Code,
        );
        let calls = vec![
            (
                "read".to_string(),
                r#"{"path":"/nonexistent-a"}"#.to_string(),
            ),
            (
                "read".to_string(),
                r#"{"path":"/nonexistent-b"}"#.to_string(),
            ),
        ];
        assert!(super::execute_read_only_batch(&mut c, &calls).is_empty());
        assert_eq!(c.tool_step_counter, 0, "a refused batch consumes no steps");
    }

    /// A batched read must leave the same per-file state a serial read would, otherwise the
    /// very next `edit` is accused of shifting code the model did read.
    #[test]
    fn batched_read_satisfies_the_code_shifting_guard() {
        let dir = std::env::temp_dir().join(format!("spavn-batch-guard-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.txt");
        fs::write(&target, "one\ntwo\n").unwrap();
        let other = dir.join("b.txt");
        fs::write(&other, "alpha\nbeta\n").unwrap();

        let mut c = ctx(AgentPhase::Code);
        let calls = vec![
            (
                "read".to_string(),
                format!(
                    r#"{{"path":"{}","start_line":1,"end_line":2}}"#,
                    target.display()
                ),
            ),
            (
                "read".to_string(),
                format!(
                    r#"{{"path":"{}","start_line":1,"end_line":2}}"#,
                    other.display()
                ),
            ),
        ];
        let out = super::execute_read_only_batch(&mut c, &calls);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(c
            .read_mtimes
            .contains_key(&target.to_string_lossy().to_string()));

        let edited = execute_tool(
            &mut c,
            "edit",
            &format!(
                r#"{{"path":"{}","old_text":"one","new_text":"uno"}}"#,
                target.display()
            ),
        );
        assert!(
            !edited.contains("CODE-SHIFTING GUARD"),
            "batched read should satisfy the guard: {edited}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mutating_tools_never_join_a_batch() {
        for name in ["edit", "write", "bash", "subagent", "lsp", "memory"] {
            assert!(!super::parallel_safe_tool(name), "{name} must stay serial");
        }
        for name in ["read", "search", "glob", "skill", "web"] {
            assert!(super::parallel_safe_tool(name), "{name} should batch");
        }
    }

    #[test]
    fn phase_from_str_and_toggle() {
        assert_eq!(AgentPhase::parse_alias("plan"), Some(AgentPhase::Plan));
        assert_eq!(AgentPhase::parse_alias("CODE"), Some(AgentPhase::Code));
        assert_eq!(AgentPhase::parse_alias("bogus"), None);
        assert_eq!(AgentPhase::Plan.toggled(), AgentPhase::Code);
        assert_eq!(AgentPhase::Code.toggled(), AgentPhase::Plan);
    }

    #[test]
    fn plan_schema_omits_mutating_tools() {
        let plan_tools = super::tool_definitions_for_phase(AgentPhase::Plan);
        let arr = plan_tools.as_array().expect("array");
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|v| v.get("function")?.get("name")?.as_str())
            .collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"search"));
        assert!(names.contains(&"subagent"));
        assert!(names.contains(&"lsp"));
        assert!(!names.contains(&"edit"));
        assert!(!names.contains(&"write"));
        assert!(!names.contains(&"bash"));
    }

    #[test]
    fn subagent_rejects_empty_task_gracefully() {
        let mut c = ctx(AgentPhase::Plan);
        let out = execute_tool(&mut c, "subagent", "{}");
        assert!(out.contains("missing 'task' parameter"), "{out}");
    }

    #[test]
    fn read_tool_handles_png_image_as_multimodal_vision_instead_of_utf8_error() {
        let dir = std::env::temp_dir().join(format!("spavn-vision-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let img_path = dir.join("test_render.png");
        // Non-UTF8 PNG header bytes (`\x89PNG\r\n\x1a\n`)
        fs::write(&img_path, b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR").unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            dir.clone(),
            AgentPhase::Code,
        );
        let args = serde_json::json!({ "path": img_path.to_string_lossy() });
        let out = execute_tool(&mut c, "read", &args.to_string());
        assert!(
            out.contains("[SPAVN Vision Image Loaded:"),
            "expected vision image load, got: {out}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reindent_replacement_preserves_file_base_indent_and_nesting() {
        let matched = ["        if cond {", "            do_a();", "        }"];
        let old_text = "if cond {\n    do_a();\n}";
        let new_text = "if cond {\n    do_b();\n}";
        let out = super::reindent_replacement_lines(&matched, old_text, new_text);
        assert_eq!(
            out,
            vec![
                "        if cond {".to_string(),
                "            do_b();".to_string(),
                "        }".to_string()
            ]
        );
    }
}

#[cfg(all(test, unix))]
mod timeout_tests {
    use super::*;

    #[test]
    fn timeout_kills_grandchildren_and_does_not_hang() {
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 30 & sleep 30"]);
        let t = std::time::Instant::now();
        let (_, _, _, timed_out) = run_command_with_timeout(c, 1).unwrap();
        assert!(timed_out && t.elapsed().as_secs() < 5);
    }

    #[test]
    fn signal_exit_is_nonzero() {
        let mut c = Command::new("sh");
        c.args(["-c", "kill -9 $$"]);
        assert_eq!(run_command_with_timeout(c, 5).unwrap().0, 137);
    }
}

#[cfg(all(test, unix))]
#[cfg(all(test, unix))]
mod bash_spill_tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            std::env::temp_dir(),
            crate::spavn_engine::AgentPhase::Code,
        )
    }

    #[test]
    fn oversized_output_keeps_head_and_tail_and_points_at_the_spill_file() {
        let out = exec_bash(&mut ctx(), &json!({ "command": "seq 1 6000" }));
        assert!(out.contains("SPAVN truncated"), "no marker: {out}");
        assert!(
            out.starts_with("1\n"),
            "head lost: {}",
            &out[..40.min(out.len())]
        );
        assert!(out.trim_end().ends_with("6000"), "tail lost");
        let path = out
            .split('`')
            .find(|seg| seg.contains("spavn") && seg.ends_with(".txt"))
            .unwrap_or_else(|| panic!("no spill path in marker: {out}"));
        let full = fs::read_to_string(path).expect("spill file should exist and be readable");
        assert!(
            full.contains("\n3000\n"),
            "middle should survive in the spill file"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn small_output_is_returned_untouched_and_spills_nothing() {
        let out = exec_bash(&mut ctx(), &json!({ "command": "echo hello-spill-probe" }));
        assert_eq!(out.trim(), "hello-spill-probe");
        assert!(!out.contains("truncated"));
    }
}

#[cfg(test)]
mod subagent_header_tests {
    use super::*;

    #[test]
    fn short_tasks_are_passed_through_and_flattened() {
        assert_eq!(
            one_line_task("  find   the\nmain entry "),
            "find the main entry"
        );
    }

    #[test]
    fn long_tasks_are_cut_to_one_header_line() {
        let long = "investigate ".repeat(40);
        let label = one_line_task(&long);
        assert!(label.chars().count() <= 73, "too long: {}", label.len());
        assert!(label.ends_with('…'));
        assert!(!label.contains('\n'));
    }

    #[test]
    fn multibyte_tasks_never_split_a_char() {
        let label = one_line_task(&"ü".repeat(200));
        assert_eq!(label.chars().count(), 73);
    }
}

#[cfg(test)]
mod line_span_tests {
    use super::*;

    #[test]
    fn parses_single_bare_and_range_forms() {
        assert_eq!(parse_line_spans("42"), Some(vec![(42, 42)]));
        assert_eq!(parse_line_spans("10-20"), Some(vec![(10, 20)]));
        assert_eq!(parse_line_spans("L10-L20"), Some(vec![(10, 20)]));
        assert_eq!(parse_line_spans("10-L20"), Some(vec![(10, 20)]));
    }

    #[test]
    fn parses_comma_separated_lists() {
        assert_eq!(
            parse_line_spans("1-12,640-665"),
            Some(vec![(1, 12), (640, 665)])
        );
        assert_eq!(parse_line_spans("5,90"), Some(vec![(5, 5), (90, 90)]));
    }

    #[test]
    fn rejects_non_numeric_suffixes() {
        assert_eq!(parse_line_spans("rs"), None);
        assert_eq!(parse_line_spans(""), None);
        assert_eq!(parse_line_spans("1-"), None);
        assert_eq!(parse_line_spans("1,,2"), None);
    }

    #[test]
    fn bare_single_expands_but_multi_list_does_not() {
        assert_eq!(expand_line_spans(vec![(30, 30)]), vec![(30, 150)]);
        assert_eq!(
            expand_line_spans(vec![(5, 5), (90, 90)]),
            vec![(5, 5), (90, 90)]
        );
    }

    #[test]
    fn overlapping_and_touching_spans_merge() {
        assert_eq!(
            expand_line_spans(vec![(10, 20), (20, 30)]),
            vec![(10, 30)],
            "overlap must collapse"
        );
        assert_eq!(
            expand_line_spans(vec![(10, 20), (21, 30)]),
            vec![(10, 30)],
            "adjacent spans read as one block"
        );
        assert_eq!(
            expand_line_spans(vec![(40, 50), (1, 5)]),
            vec![(1, 5), (40, 50)],
            "output is ordered"
        );
    }

    #[test]
    fn selector_splits_a_real_multi_range_path() {
        let (path, spans) = split_inline_line_spans("src/hud/menus.rs:103-118,1060-1070");
        assert_eq!(path, "src/hud/menus.rs");
        assert_eq!(spans, Some(vec![(103, 118), (1060, 1070)]));
    }

    #[test]
    fn selector_leaves_plain_paths_untouched() {
        let (path, spans) = split_inline_line_spans("src/main.rs");
        assert_eq!(path, "src/main.rs");
        assert_eq!(spans, None);
    }

    #[test]
    fn rendered_spans_carry_absolute_lines_and_jump_markers() {
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let out = format_spans(&body, &[(2, 4), (20, 22)]);
        assert!(out.contains("   2: line 2"), "{}", out);
        assert!(out.contains("   4: line 4"), "{}", out);
        assert!(out.contains("[jump to line 20]"), "{}", out);
        assert!(out.contains("  22: line 22"), "{}", out);
        assert!(out.contains("EOF: line 30/30"), "{}", out);
        assert!(!out.contains("line 10"), "gap lines must not leak: {}", out);
    }
}

#[cfg(all(test, unix))]
mod glob_tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            PathBuf::from("/tmp"),
            crate::spavn_engine::AgentPhase::Code,
        )
    }

    /// Builds a throwaway tree: two `.rs` files, one ignored dir, one dotfile.
    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spavn-glob-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/deep")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("src/deep/lib.rs"), "").unwrap();
        fs::write(dir.join("target/junk.rs"), "").unwrap();
        fs::write(dir.join("notes.md"), "").unwrap();
        fs::write(dir.join(".hidden.rs"), "").unwrap();
        dir
    }

    #[test]
    fn matches_only_requested_extension_and_skips_ignored_dirs() {
        let dir = fixture("ext");
        let out = exec_glob(
            &mut ctx(),
            &json!({ "pattern": "**/*.rs", "path": dir.to_string_lossy() }),
        );
        assert!(out.contains("main.rs"), "{}", out);
        assert!(out.contains("lib.rs"), "{}", out);
        assert!(!out.contains("target/"), "ignored dir leaked: {}", out);
        assert!(
            !out.contains("notes.md"),
            "extension filter leaked: {}",
            out
        );
        assert!(
            !out.contains(".hidden.rs"),
            "dotfile leaked by default: {}",
            out
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_flag_opts_dotfiles_back_in() {
        let dir = fixture("hidden");
        let out = exec_glob(
            &mut ctx(),
            &json!({ "pattern": "**/*.rs", "path": dir.to_string_lossy(), "hidden": true }),
        );
        assert!(out.contains(".hidden.rs"), "{}", out);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_truncates_and_says_so() {
        let dir = fixture("limit");
        let out = exec_glob(
            &mut ctx(),
            &json!({ "pattern": "**/*.rs", "path": dir.to_string_lossy(), "limit": 1 }),
        );
        assert_eq!(
            out.lines().filter(|l| l.ends_with(".rs")).count(),
            1,
            "{}",
            out
        );
        assert!(out.contains("limit 1 reached"), "{}", out);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_pattern_and_empty_match_are_explained_not_empty() {
        let dir = fixture("empty");
        assert!(exec_glob(&mut ctx(), &json!({})).contains("missing `pattern`"));
        let out = exec_glob(
            &mut ctx(),
            &json!({ "pattern": "**/*.zig", "path": dir.to_string_lossy() }),
        );
        assert!(out.contains("no files match"), "{}", out);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Regression guard for the latent bug the `**/` prefix fixes: `search` shares this glob
    /// list, so an absolute path used to walk straight into `target/` before it was normalised.
    #[test]
    fn ignore_globs_are_root_independent() {
        for pat in RG_IGNORE_GLOBS {
            assert!(
                pat.starts_with("!**/"),
                "glob {} is matched relative to the search root and breaks on absolute paths",
                pat
            );
        }
    }

    #[test]
    fn limit_is_clamped_to_the_hard_cap() {
        // A huge `limit` must not translate into an unbounded read; the clamp is pure arithmetic.
        let requested = 10_000_000u64;
        let clamped = (requested as usize).clamp(1, GLOB_MAX_LIMIT);
        assert_eq!(clamped, GLOB_MAX_LIMIT);
    }
}

#[cfg(all(test, unix))]
mod atomic_write_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn keeps_mode_follows_symlink_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("spavn-atomic-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let real = dir.join("run.sh");
        fs::write(&real, "old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.join("link.sh");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomic(&link, "new").unwrap();

        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            2,
            "temp file left behind"
        );
        let fresh = dir.join("new.txt");
        write_atomic(&fresh, "x").unwrap();
        assert_eq!(fs::read_to_string(&fresh).unwrap(), "x");

        let broken_target = dir.join("broken_target.txt");
        let broken_link = dir.join("broken_link.txt");
        std::os::unix::fs::symlink("broken_target.txt", &broken_link).unwrap();
        write_atomic(&broken_link, "healed").unwrap();
        assert!(fs::symlink_metadata(&broken_link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&broken_target).unwrap(), "healed");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_bash_gate_respects_cwd_argument_for_protected_paths() {
        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            PathBuf::from("/tmp/ws"),
            crate::spavn_engine::AgentPhase::Code,
        );
        c.policy = PathPolicy::for_test("/tmp/ws", "/tmp/ws", vec!["/protected/apps"]);
        let res = check_bash_gate(&mut c, "rm -rf proj", "local", Some("/protected/apps"));
        assert!(
            res.is_err(),
            "rm -rf proj with cwd=/protected/apps must be blocked as Catastrophic"
        );
    }

    #[test]
    fn multi_hunk_edit_applies_multiple_replacements_in_one_call() {
        let dir = std::env::temp_dir().join(format!("spavn-multiedit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.rs");
        fs::write(&file, "fn a() -> i32 { 1 }\nfn b() -> i32 { 2 }\n").unwrap();

        let mut c = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            dir.clone(),
            crate::spavn_engine::AgentPhase::Code,
        );
        let args = serde_json::json!({
            "path": file.to_string_lossy(),
            "edits": [
                { "old_text": "fn a() -> i32 { 1 }", "new_text": "fn a() -> i32 { 10 }" },
                { "old_text": "fn b() -> i32 { 2 }", "new_text": "fn b() -> i32 { 20 }" }
            ]
        });
        let res = execute_tool(&mut c, "edit", &args.to_string());
        assert!(res.starts_with("Successfully "), "{res}");
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "fn a() -> i32 { 10 }\nfn b() -> i32 { 20 }\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;
    use crate::spavn_engine::AgentPhase;

    fn ctx_with(access: AccessMode, ws: &str) -> ToolContext {
        let mut c = ToolContext::new(
            access,
            "local".to_string(),
            PathBuf::from(ws),
            AgentPhase::Code,
        );
        c.policy = PathPolicy::new(Path::new(ws));
        c
    }

    #[test]
    fn ssh_keys_stay_blocked_even_in_sovereign() {
        // SOVEREIGN skips the prompt, so the path sandbox is the only thing standing between a
        // prompt-injected `read` and a private key. It must deny on its own.
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp/spavn-sec-ws");
        let home = crate::config::home_dir();
        for rel in [".ssh/id_rsa", ".spavn/secrets.env", ".aws/credentials"] {
            let path = home.join(rel);
            let args = serde_json::json!({ "path": path.to_string_lossy() }).to_string();
            let out = execute_tool(&mut c, "read", &args);
            assert!(
                out.contains("BLOCKED"),
                "{} must be denied, got: {}",
                rel,
                out.chars().take(120).collect::<String>()
            );
        }
    }

    #[test]
    fn a_host_that_ssh_reads_as_an_option_is_rejected() {
        // `ssh -o ConnectTimeout=8 <host> <cmd>` parses <host>, so a leading `-` is a local
        // command execution primitive (ProxyCommand), not a destination.
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp/spavn-sec-ws");
        let args = serde_json::json!({
            "command": "echo hi",
            "host": "-oProxyCommand=touch /tmp/spavn-sec-pwned"
        })
        .to_string();
        let out = execute_tool(&mut c, "bash", &args);
        assert!(out.contains("Host Policy"), "{out}");
        assert!(
            !Path::new("/tmp/spavn-sec-pwned").exists(),
            "the injected ProxyCommand must never run"
        );
    }

    #[test]
    fn remote_search_rejects_an_option_style_host() {
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp/spavn-sec-ws");
        let args = serde_json::json!({
            "pattern": "x",
            "path": "src",
            "host": "-oProxyCommand=id"
        })
        .to_string();
        let out = execute_tool(&mut c, "search", &args);
        assert!(out.contains("Host Policy"), "{out}");
    }

    #[test]
    fn background_logs_do_not_land_in_world_writable_tmp() {
        // A background `bash` used to write its log and stderr into $TMPDIR, which is shared and
        // predictable: another local user could pre-create the path (symlink) and read or
        // redirect the output. They now live under the 0700 spavn home.
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp/spavn-sec-ws");
        let args =
            serde_json::json!({ "command": "echo sec-probe", "background": true }).to_string();
        let out = execute_tool(&mut c, "bash", &args);
        assert!(out.contains(".spavn/bg/"), "{out}");
        let log = out
            .split('`')
            .find(|s| s.contains(".log"))
            .unwrap_or("")
            .to_string();
        assert!(!log.is_empty(), "no log path in: {out}");
        assert!(
            !log.starts_with("/tmp/") && !log.contains("/var/folders/"),
            "log leaked into a shared temp dir: {log}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&log).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "background log must be owner-only");
        }
        let _ = fs::remove_file(&log);
    }

    #[test]
    fn an_unanswerable_approval_denies_the_action() {
        // No TTY means no one can answer the prompt. Blocking forever would hang the turn, and
        // "no answer" must never be read as consent.
        assert!(
            !crate::hud::stdin_is_tty(),
            "this test needs a non-tty stdin"
        );
        let dir = std::env::temp_dir().join(format!("spavn-sec-approve-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("would-be-written.txt");
        let mut c = ctx_with(AccessMode::Safe, &dir.to_string_lossy());
        let args = serde_json::json!({
            "path": target.to_string_lossy(),
            "content": "denied"
        })
        .to_string();
        let out = execute_tool(&mut c, "write", &args);
        assert!(out.contains("rejected") || out.contains("BLOCKED"), "{out}");
        assert!(!target.exists(), "a denied write must not touch the disk");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_refuses_a_private_target_before_any_socket_opens() {
        let mut c = ctx_with(AccessMode::Sovereign, "/tmp");
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/admin",
            "http://192.168.1.1/",
            "http://127.0.0.1.nip.io/",
            "http://[::1]:11434/api/tags",
        ] {
            let args = serde_json::json!({ "op": "fetch", "url": url }).to_string();
            let out = execute_tool(&mut c, "web", &args);
            assert!(out.contains("Egress Policy"), "{url} → {out}");
        }
    }

    #[test]
    fn web_fetch_asks_in_safe_mode() {
        // Sovereign egress to a public host is unattended by design; SAFE must still ask, and
        // with no TTY that ask fails closed.
        let mut c = ctx_with(AccessMode::Safe, "/tmp");
        let args = serde_json::json!({ "op": "fetch", "url": "https://example.com/" }).to_string();
        let out = execute_tool(&mut c, "web", &args);
        assert!(out.contains("reddedildi") || out.contains("GLED"), "{out}");
    }

    #[test]
    fn mcp_mutating_tool_is_treated_as_mutating_by_default() {
        // The gate used to be a deny-list of verbs, so `commit`/`move_file` slipped through.
        for tool in [
            "commit",
            "move_file",
            "create_pull_request",
            "merge_pull_request",
            "delete_branch",
        ] {
            assert!(is_mutating_mcp_tool("some-server", tool, false), "{tool}");
        }
        for tool in ["get_page", "list_issues", "search_code", "read_file"] {
            assert!(!is_mutating_mcp_tool("some-server", tool, false), "{tool}");
        }
        // A server that declares itself read-only skips the gate entirely.
        assert!(!is_mutating_mcp_tool("docs", "commit", true));
    }

    #[test]
    fn mcp_response_is_capped() {
        let huge = format!(
            "{{\"result\":{{\"content\":[{{\"text\":\"{}\"}}]}}}}",
            "x".repeat(400_000)
        );
        let out = format_mcp_jsonrpc_response("srv", "tool", &huge);
        assert!(out.contains("kırpıldı"), "{} bytes", out.len());
        assert!(out.len() < 200_000, "{} bytes", out.len());
    }
}
