pub mod edit;
pub mod mcp_subagent;
pub mod read;
pub mod schema;
pub mod search;
pub mod shell;
pub mod web;
#[cfg(test)]
mod tests;

pub use edit::{
    build_live_project_signatures, check_unfinished_placeholders, run_project_verification_gate,
};
pub use schema::{tool_definitions, tool_definitions_for_phase, tool_definitions_for_turn};
pub(crate) use edit::*;
pub(crate) use mcp_subagent::*;
pub(crate) use read::*;
pub(crate) use search::*;
pub(crate) use shell::*;
pub(crate) use web::*;

use crate::config::{expand_user_path, home_dir, load_user_config};
use crate::guard::{assess_shell_command, PathPolicy, PathRisk, ShellRisk};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const CYAN: &str = "\x1b[36m";
pub(crate) const GREEN: &str = "\x1b[32m";
pub(crate) const YELLOW: &str = "\x1b[33m";
pub(crate) const DIM: &str = "\x1b[2m";
pub(crate) const BOLD: &str = "\x1b[1m";
pub(crate) const RESET: &str = "\x1b[0m";

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


pub(crate) fn expand_tilde(path_str: &str) -> PathBuf {
    if path_str == "~" {
        return home_dir();
    }
    expand_user_path(path_str)
}

pub(crate) fn resolve_ssh_target(alias: &str) -> String {
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
pub(crate) fn validate_host_arg(host: &str) -> Result<(), String> {
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

pub(crate) fn split_remote_prefix(raw: &str) -> Option<(String, &str)> {
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

pub(crate) fn shell_quote_remote_path(raw: &str) -> String {
    let escape_single = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    if let Some(rest) = raw.strip_prefix("~/") {
        format!("~/{}", escape_single(rest))
    } else if raw == "~" {
        "~".to_string()
    } else {
        escape_single(raw)
    }
}

pub(crate) fn read_target_file(raw_path: &str) -> Result<String, String> {
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

pub(crate) fn suggest_similar_path(raw_path: &str) -> String {
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
pub(crate) fn write_target_file(raw_path: &str, content: &str) -> Result<(), String> {
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

pub(crate) static ATOMIC_WRITE_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Temp file in the same dir + rename: a crash or full disk never leaves a truncated target.
/// Follows symlinks (including broken symlinks) and keeps the existing file's permissions.
pub(crate) fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
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

pub(crate) fn remove_target_file(raw_path: &str) {
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

pub(crate) const MAX_CAPTURE_BYTES: u64 = 4 * 1024 * 1024;

/// Keeps the first MAX_CAPTURE_BYTES, then drains the pipe so the child never blocks.
pub(crate) fn capture_capped<R: std::io::Read + Send + 'static>(
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
pub(crate) fn kill_group(child: &mut std::process::Child) {
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

/// Turns a spawn failure into something the model can act on.
///
/// `search` and `glob` shell out to `rg`; when it is missing, `io::Error` says only "No such file
/// or directory", which reads like a path bug and sends the model hunting for the wrong thing.
pub(crate) fn spawn_error_message(cmd: &Command, err: &std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::NotFound {
        let program = cmd.get_program().to_string_lossy().to_string();
        return format!(
            "`{}` is not installed or not on PATH; install it, or use `read`/`bash` instead",
            program
        );
    }
    err.to_string()
}

pub(crate) fn run_command_with_timeout(
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
    let mut child = cmd.spawn().map_err(|e| spawn_error_message(&cmd, &e))?;
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
pub(crate) fn gate_safe(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect()
}

pub(crate) fn prompt_user_approval(ctx: &mut ToolContext, action_title: &str, detail: &str) -> bool {
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

