use super::*;

use std::io::Read;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct BgJobEntry {
    pub id: String,
    pub pid: u32,
    pub command: String,
    pub log_path: PathBuf,
    pub started_at: std::time::Instant,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct BgJobCompletionNotice {
    pub job_id: String,
    pub pid: u32,
    pub command: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub elapsed_sec: u64,
    pub log_path: PathBuf,
    pub tail_preview: String,
}

static BG_JOBS: LazyLock<Mutex<Vec<BgJobEntry>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static BG_NOTIFICATIONS: LazyLock<Mutex<Vec<BgJobCompletionNotice>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static BG_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

fn bg_jobs_registry() -> &'static Mutex<Vec<BgJobEntry>> {
    &BG_JOBS
}

pub fn list_bg_jobs() -> Vec<BgJobEntry> {
    BG_JOBS.lock().map(|g| g.clone()).unwrap_or_default()
}

pub fn pop_completed_bg_notifications() -> Vec<BgJobCompletionNotice> {
    BG_NOTIFICATIONS
        .lock()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default()
}

fn consume_bg_notification(job_id: &str) {
    if let Ok(mut q) = BG_NOTIFICATIONS.lock() {
        q.retain(|n| n.job_id != job_id);
    }
}

pub fn handle_bg_job_slash(arg: &str) -> String {
    let trimmed = arg.trim();
    if trimmed.is_empty() || trimmed == "list" {
        handle_bg_job_op("list", "", None)
    } else if let Some(rest) = trimmed.strip_prefix("kill ") {
        handle_bg_job_op("kill", rest.trim(), None)
    } else if let Some(rest) = trimmed.strip_prefix("wait ") {
        handle_bg_job_op("wait", rest.trim(), None)
    } else if let Some(rest) = trimmed.strip_prefix("status ") {
        handle_bg_job_op("status", rest.trim(), None)
    } else {
        handle_bg_job_op("status", trimmed, None)
    }
}

fn strip_pwd_sentinel_lines(raw: &str) -> String {
    let mut kept = Vec::new();
    for line in raw.lines() {
        if line.starts_with("__SPAVN_PWD_") && line.contains("__:") {
            continue;
        }
        kept.push(line);
    }
    kept.join("\n")
}

fn read_log_tail_lines(path: &Path, max_lines: usize) -> String {
    let Ok(raw) = fs::read_to_string(path) else {
        return crate::config::tr_en("(henüz çıktı yok)", "(No output yet)").to_owned();
    };
    let cleaned = strip_pwd_sentinel_lines(&raw);
    if cleaned.trim().is_empty() {
        return crate::config::tr_en("(henüz çıktı yok)", "(No output yet)").to_owned();
    };
    let lines: Vec<&str> = cleaned.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn handle_bg_job_op(op: &str, job_id_raw: &str, wait_timeout_sec: Option<u64>) -> String {
    let key = job_id_raw.trim();
    match op {
        "list" => {
            let jobs = list_bg_jobs();
            if jobs.is_empty() {
                return "No background jobs in this session.".to_string();
            }
            let mut out = String::from("Background jobs:\n");
            for j in jobs {
                let st = match j.exit_code {
                    None => "running".to_string(),
                    Some(c) => format!("exited ({})", c),
                };
                let cmd_preview: String = j
                    .command
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(72)
                    .collect();
                out.push_str(&format!(
                    "- `{}` (PID {}): {} | {}s | `{}` | log: `{}`\n",
                    j.id,
                    j.pid,
                    st,
                    j.started_at.elapsed().as_secs(),
                    cmd_preview,
                    j.log_path.display()
                ));
            }
            out.trim_end().to_string()
        }
        "status" => {
            let jobs = list_bg_jobs();
            let found = if key.is_empty() {
                jobs.last().cloned()
            } else {
                jobs.into_iter()
                    .find(|j| j.id == key || j.pid.to_string() == key)
            };
            let Some(j) = found else {
                return format!(
                    "Background job `{}` not found. Use `bash({{\"op\":\"list\"}})` to inspect active jobs.",
                    key
                );
            };
            if j.exit_code.is_some() {
                consume_bg_notification(&j.id);
            }
            let st = match j.exit_code {
                None => "running".to_string(),
                Some(c) => format!("exited ({})", c),
            };
            let tail = read_log_tail_lines(&j.log_path, 80);
            format!(
                "Job `{}` (PID {}) — status: {}, elapsed: {}s, log: `{}`\n--- tail (last 80 lines) ---\n{}",
                j.id,
                j.pid,
                st,
                j.started_at.elapsed().as_secs(),
                j.log_path.display(),
                tail
            )
        }
        "wait" => {
            let target_id = {
                let jobs = list_bg_jobs();
                let found = if key.is_empty() {
                    jobs.last().cloned()
                } else {
                    jobs.into_iter()
                        .find(|j| j.id == key || j.pid.to_string() == key)
                };
                let Some(j) = found else {
                    return format!(
                        "Background job `{}` not found. Use `bash({{\"op\":\"list\"}})` to inspect active jobs.",
                        key
                    );
                };
                j.id
            };
            let max_wait = Duration::from_secs(wait_timeout_sec.unwrap_or(300).clamp(1, 3600));
            let wait_start = std::time::Instant::now();
            loop {
                let snapshot = list_bg_jobs().into_iter().find(|j| j.id == target_id);
                let Some(j) = snapshot else {
                    return format!("Background job `{}` no longer exists.", target_id);
                };
                if let Some(code) = j.exit_code {
                    consume_bg_notification(&j.id);
                    let tail = read_log_tail_lines(&j.log_path, 80);
                    return format!(
                        "Job `{}` (PID {}) — status: exited ({}), elapsed: {}s, log: `{}`\n--- tail (last 80 lines) ---\n{}",
                        j.id,
                        j.pid,
                        code,
                        j.started_at.elapsed().as_secs(),
                        j.log_path.display(),
                        tail
                    );
                }
                if crate::is_interrupted() {
                    let tail = read_log_tail_lines(&j.log_path, 40);
                    return format!(
                        "Wait interrupted for `{}` (PID {}); job continues running in background.\n--- latest output ---\n{}",
                        j.id, j.pid, tail
                    );
                }
                if crate::steer_message_count() > 0 {
                    let tail = read_log_tail_lines(&j.log_path, 40);
                    return format!(
                        "Job `{}` (PID {}) — status: running ({}s elapsed, log: `{}`)\nBackgrounded early to handle an incoming message; the command keeps running.\n--- latest output ---\n{}",
                        j.id,
                        j.pid,
                        j.started_at.elapsed().as_secs(),
                        j.log_path.display(),
                        tail
                    );
                }
                if wait_start.elapsed() >= max_wait {
                    let tail = read_log_tail_lines(&j.log_path, 60);
                    return format!(
                        "Job `{}` (PID {}) — status: still running after {}s wait (total elapsed: {}s, log: `{}`).\n--- latest output ---\n{}",
                        j.id,
                        j.pid,
                        max_wait.as_secs(),
                        j.started_at.elapsed().as_secs(),
                        j.log_path.display(),
                        tail
                    );
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        "kill" => {
            let found_pid_id = {
                let Ok(mut guard) = bg_jobs_registry().lock() else {
                    return "Error locking background job registry".to_string();
                };
                let entry = if key.is_empty() {
                    guard.last_mut()
                } else {
                    guard
                        .iter_mut()
                        .find(|j| j.id == key || j.pid.to_string() == key)
                };
                entry.map(|j| {
                    let pid = j.pid;
                    let id = j.id.clone();
                    let log = j.log_path.clone();
                    if j.exit_code.is_none() {
                        j.exit_code = Some(-9);
                    }
                    (id, pid, log)
                })
            };
            let Some((id, pid, log_path)) = found_pid_id else {
                return format!("Background job `{}` not found.", key);
            };
            consume_bg_notification(&id);
            #[cfg(unix)]
            unsafe {
                let pgid = pid as i32;
                let _ = crate::tools::kill(-pgid, 15);
                let _ = crate::tools::kill(pgid, 15);
                std::thread::sleep(std::time::Duration::from_millis(40));
                let _ = crate::tools::kill(-pgid, 9);
                let _ = crate::tools::kill(pgid, 9);
            }
            let tail = read_log_tail_lines(&log_path, 40);
            format!(
                "Killed background job `{}` (PID {}).\n--- final output ---\n{}",
                id, pid, tail
            )
        }
        _ => format!(
            "Unknown bash op `{}` (expected `run`, `status`, `wait`, `list`, or `kill`).",
            op
        ),
    }
}
pub(crate) fn check_bash_gate(
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
            if !prompt_user_approval(
                ctx,
                &format!(
                    "{} [{}]",
                    crate::config::tr_en("Komut Çalıştırma", "Command Execution"),
                    host
                ),
                cmd,
            ) {
                return Err("User rejected command in SAFE mode.".to_string());
            }
        }
        AccessMode::Auto => {
            if auto_sensitive {
                let reason = match &risk {
                    ShellRisk::Destructive(reason) => reason.clone(),
                    _ => format!(
                        "{}: {}",
                        crate::config::tr_en(
                            "hassas host üzerinde çalıştırma",
                            "running on sensitive host"
                        ),
                        host
                    ),
                };
                if !prompt_user_approval(
                    ctx,
                    &format!(
                        "{} [{}] — {}",
                        crate::config::tr_en("Kritik Komut Onayı", "Critical Command Approval"),
                        host,
                        reason
                    ),
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
/// `allow_local` from the caller. `Public` asks only in SAFE: reading a public page never touches
/// the workspace, so PLAN (a read-only research phase) needs it as much as CODE does.
pub(crate) fn check_egress_gate(
    ctx: &mut ToolContext,
    url: &str,
    allow_local: bool,
) -> Option<String> {
    use crate::guard::{classify_egress, EgressTarget};
    match classify_egress(url)? {
        EgressTarget::Private => Some(format!(
            "{}: {}",
            crate::config::tr_en(
                "BLOCKED by SPAVN Egress Policy",
                "BLOCKED by SPAVN Egress Policy"
            ),
            crate::config::tr_en(
                "{0} özel/link-local bir adres hedefliyor.",
                "{0} targets a private/link-local address."
            )
            .replace("{0}", url)
        )),
        EgressTarget::Loopback => {
            if allow_local {
                return None;
            }
            Some(format!(
                "{}: {}",
                crate::config::tr_en(
                    "BLOCKED by SPAVN Egress Policy",
                    "BLOCKED by SPAVN Egress Policy"
                ),
                crate::config::tr_en(
                    "{0} loopback adresi; `allow_local: true` olmadan açılmaz.",
                    "{0} is a loopback address; not opened without `allow_local: true`."
                )
                .replace("{0}", url)
            ))
        }
        EgressTarget::Public => {
            // A sub-agent has no terminal: prompting would resolve to "no" and cost it the
            // only tool that can reach documentation, so policy alone decides there.
            let needs_prompt = ctx.can_prompt && ctx.access == AccessMode::Safe;
            if needs_prompt
                && !prompt_user_approval(
                    ctx,
                    crate::config::tr_en("Web erişimi (fetch)", "Web access (fetch)"),
                    url,
                )
            {
                return Some(
                    crate::config::tr_en(
                        "Web fetch reddedildi (onay yok). Sayfa OKUNMADI; içeriği hakkında tahmin yürütme, kullanıcıya okunamadığını söyle.",
                        "Web fetch rejected (no approval). The page was NOT read; do not guess its contents, tell the user it could not be read.",
                    )
                    .to_owned(),
                );
            }
            None
        }
    }
}

/// Bash Interceptor: blocks standalone shell commands (`grep -rn`, `find`, `cat`,
/// `sed -n`, `head`, `tail`) when the dedicated built-in tool (`search`, `glob`, `read`)
/// handles the request faster, respects ignore globs (`node_modules`, `target`, `.git`),
/// and never stalls the turn for 120s.
pub(crate) fn intercept_shell_anti_pattern(cmd: &str) -> Option<String> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Never intercept multi-stage pipelines or redirections/mutations unless the first stage
    // is an unguarded recursive grep/find across the filesystem.
    let clean_for_check = trimmed.replace("2>/dev/null", "").replace("2>&1", "");
    let segments: Vec<&str> = clean_for_check
        .split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let raw_first = segments
        .first()
        .copied()
        .unwrap_or(clean_for_check.trim());
    let first_seg = if raw_first.split_whitespace().next() == Some("cd") {
        let mut s = raw_first.trim().to_string();
        loop {
            let toks: Vec<&str> = s.split_whitespace().collect();
            if toks.first() == Some(&"cd") {
                if let Some(pos) = s.find("&&") {
                    s = s[pos + 2..].trim().to_string();
                    continue;
                }
                if let Some(pos) = s.find(';') {
                    s = s[pos + 1..].trim().to_string();
                    continue;
                }
            }
            break;
        }
        s
    } else {
        raw_first
            .split([';', '&'])
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let tokens: Vec<&str> = first_seg.split_whitespace().collect();
    let &bin = tokens.first()?;
    let only_pager_pipe = segments.len() == 1
        || (segments.len() == 2
            && segments[1]
                .split_whitespace()
                .next()
                .is_some_and(|b| matches!(b, "head" | "tail" | "wc")));
    match bin {
        "grep" | "egrep" | "fgrep" | "rg" => {
            let is_recursive = bin == "rg"
                || tokens
                    .iter()
                    .skip(1)
                    .any(|t| t.starts_with('-') && (t.contains('r') || t.contains('R')));
            if is_recursive && only_pager_pipe {
                return Some(
                    "[SPAVN Bash Interceptor]: Blocked standalone `grep`/`rg` in `bash` (walks `node_modules`/build artifacts and causes timeouts). Use the built-in `search` tool (`search({\"pattern\": \"...\", \"path\": \"...\"})`), which automatically skips ignored directories and caps per-file matches."
                        .to_string(),
                );
            }
        }
        "find" => {
            let has_action = tokens
                .iter()
                .any(|&t| matches!(t, "-exec" | "-delete" | "-ok" | "-print0"));
            if !has_action && only_pager_pipe {
                return Some(
                    "[SPAVN Bash Interceptor]: Blocked standalone `find` in `bash`. Use the built-in `glob` tool (`glob({\"pattern\": \"**/*...\", \"path\": \"...\"})`) which automatically skips `node_modules`, `target`, and `.git`."
                        .to_string(),
                );
            }
        }
        "cat" | "head" | "tail" | "sed" => {
            let has_complex_ops = first_seg.contains('>')
                || first_seg.contains('<')
                || first_seg.contains("&&")
                || first_seg.contains("||")
                || first_seg.contains(';')
                || first_seg.contains("$(");
            let is_sed_in_place =
                bin == "sed" && tokens.iter().skip(1).any(|t| t.starts_with("-i"));
            if !has_complex_ops && !is_sed_in_place && tokens.len() >= 2 {
                return Some(format!(
                    "[SPAVN Bash Interceptor]: Blocked standalone `{}` in `bash`. Use the built-in `read` tool with line ranges (`read({{\"path\": \"file:start-end\"}})`) instead of shelling out.",
                    bin
                ));
            }
        }
        _ => {}
    }
    None
}

pub(crate) fn exec_bash(ctx: &mut ToolContext, args: &Value) -> String {
    let command = get_str_alias(args, &["command", "cmd", "script", "shell"]);
    let op = get_str_alias(args, &["op", "action"]).to_ascii_lowercase();
    let job_id_arg = get_str_alias(args, &["job_id", "job", "id", "pid"]);
    let requested_timeout_opt = args
        .get("timeout_sec")
        .or_else(|| args.get("timeout"))
        .and_then(|v| v.as_u64());
    if matches!(op.as_str(), "status" | "wait" | "list" | "kill")
        || (command.trim().is_empty() && !job_id_arg.is_empty())
    {
        let effective_op = if op.is_empty() { "status" } else { op.as_str() };
        return handle_bg_job_op(effective_op, job_id_arg, requested_timeout_opt);
    }
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
    let user_cfg = crate::config::load_user_config();
    let (hard_timeout_sec, clamp_notice) = crate::tools::clamp_tool_timeout(
        "bash",
        requested_timeout_opt,
        user_cfg.tools_max_timeout_sec,
    );
    let background = args
        .get("background")
        .or_else(|| args.get("async"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let auto_bg_sec = args
        .get("auto_background_sec")
        .and_then(|v| v.as_u64())
        .unwrap_or(user_cfg.bash_auto_background_sec);
    let auto_bg_ms = if user_cfg.bash_auto_background
        && auto_bg_sec > 0
        && hard_timeout_sec.map_or(true, |t| t > auto_bg_sec)
    {
        Some(crate::tools::compute_auto_background_ms(
            auto_bg_sec.saturating_mul(1000),
            hard_timeout_sec.map(|t| t.saturating_mul(1000)),
        ))
    } else {
        None
    };
    let lang = get_str_alias(args, &["lang", "language", "runtime"]).to_ascii_lowercase();
    let script_interp: Option<&'static str> = match lang.as_str() {
        "py" | "python" | "python3" => Some("python3"),
        "js" | "javascript" | "node" => Some("node"),
        _ => None,
    };
    if let Err(blocked) = check_bash_gate(ctx, command, &host, cwd) {
        return blocked;
    }
    if let Some(intercept_msg) = intercept_shell_anti_pattern(command) {
        return intercept_msg;
    }

    if !ctx.last_cwd.is_dir() {
        ctx.last_cwd = ctx.policy.cwd.clone();
    }
    let effective_local_cwd: PathBuf =
        if let Some(dir) = cwd.map(str::trim).filter(|d| !d.is_empty()) {
            let expanded = expand_tilde(dir);
            let resolved = if expanded.is_absolute() {
                expanded
            } else {
                ctx.last_cwd.join(expanded)
            };
            if resolved.is_dir() {
                resolved
            } else {
                ctx.last_cwd.clone()
            }
        } else {
            ctx.last_cwd.clone()
        };

    let pwd_sentinel = format!("__SPAVN_PWD_{}__", std::process::id());
    let mut used_pwd_wrapper = false;

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
    } else if let Some(interp) = script_interp {
        let mut c = Command::new(interp);
        c.arg("-");
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
        if effective_local_cwd.is_dir() {
            c.current_dir(&effective_local_cwd);
        } else if ctx.policy.cwd.is_dir() {
            c.current_dir(&ctx.policy.cwd);
        }
        c
    } else {
        let shell_bin = crate::config::shell_bin();
        let low_shell = shell_bin.to_ascii_lowercase();
        let is_posix_shell = !low_shell.contains("pwsh")
            && !low_shell.contains("powershell")
            && !low_shell.ends_with("cmd.exe")
            && low_shell != "cmd";
        let wrapped_cmd = if is_posix_shell && !background {
            used_pwd_wrapper = true;
            format!(
                "{}\n__spavn_ec=$?; set +e +u 2>/dev/null || true; printf '\\n{}:%d:%s\\n' \"$__spavn_ec\" \"$(pwd -P 2>/dev/null || pwd)\"; exit $__spavn_ec",
                command, pwd_sentinel
            )
        } else {
            command.to_string()
        };
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
            sb.args(["-p", &profile, &shell_bin, "-c", &wrapped_cmd]);
            sb
        } else {
            let mut sh = Command::new(&shell_bin);
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
                sh.args(["-c", &wrapped_cmd]);
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
        if effective_local_cwd.is_dir() {
            c.current_dir(&effective_local_cwd);
        } else if ctx.policy.cwd.is_dir() {
            c.current_dir(&ctx.policy.cwd);
        }
        c
    };

    if background {
        let (log_path, log_file) = match allocate_bg_log_file(ctx.tool_step_counter) {
            Ok(pair) => pair,
            Err(err_msg) => return err_msg,
        };
        cmd_obj.stdin(std::process::Stdio::null());
        if let Ok(err_file) = log_file.try_clone() {
            cmd_obj
                .stdout(std::process::Stdio::from(log_file))
                .stderr(std::process::Stdio::from(err_file));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd_obj.pre_exec(|| {
                    crate::tools::setsid_and_cloexec();
                    Ok(())
                });
            }
        }
        return match cmd_obj.spawn() {
            Ok(child) => {
                let pid = child.id();
                let started_at = Instant::now();
                let job_id = register_and_monitor_bg_child(
                    child,
                    pid,
                    command.to_string(),
                    log_path.clone(),
                    started_at,
                    hard_timeout_sec,
                    None,
                );
                let mut msg = format!(
                    "Background job `{}` started (PID: {}). Output is streaming to `{}` — inspect, wait, or stop with `bash({{\"op\":\"status\",\"job_id\":\"{}\"}})`, `bash({{\"op\":\"wait\",\"job_id\":\"{}\"}})`, or `bash({{\"op\":\"kill\",\"job_id\":\"{}\"}})`.",
                    job_id,
                    pid,
                    log_path.display(),
                    job_id,
                    job_id,
                    job_id
                );
                if let Some(notice) = clamp_notice {
                    msg.push_str(&format!("\n[{}]", notice));
                }
                msg
            }
            Err(e) => format!("Background spawn error: {}", e),
        };
    }

    let stdin_payload = script_interp.map(|_| command.as_bytes());
    let exec_started = Instant::now();
    match run_bash_with_omp_lifecycle(
        cmd_obj,
        command,
        ctx.tool_step_counter,
        hard_timeout_sec,
        auto_bg_ms,
        stdin_payload,
    ) {
        Ok(BashLifecycleOutcome::Completed {
            mut code,
            stdout,
            stderr,
            timed_out,
            aborted,
        }) => {
            let mut out_str = String::from_utf8_lossy(&stdout).to_string();
            if used_pwd_wrapper {
                let marker_prefix = format!("{}:", pwd_sentinel);
                if let Some(pos) = out_str.rfind(&marker_prefix) {
                    let after = &out_str[pos + marker_prefix.len()..];
                    let line_end = after.find('\n').unwrap_or(after.len());
                    let payload = after[..line_end].trim_end_matches('\r');
                    if let Some((ec_str, pwd_str)) = payload.split_once(':') {
                        if let Ok(parsed_ec) = ec_str.trim().parse::<i32>() {
                            code = parsed_ec;
                        }
                        let new_pwd = PathBuf::from(pwd_str.trim());
                        if new_pwd.is_dir() {
                            ctx.last_cwd = new_pwd;
                        }
                    }
                    let cut_start = if pos > 0 && out_str.as_bytes()[pos - 1] == b'\n' {
                        pos - 1
                    } else {
                        pos
                    };
                    out_str.truncate(cut_start);
                }
            }
            let mut combined = collapse_binary_noise_lines(&out_str);
            let err = collapse_binary_noise_lines(&String::from_utf8_lossy(&stderr));
            if !err.is_empty() {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&err);
            }
            let wall_ms = exec_started.elapsed().as_millis() as u64;
            if !timed_out && !aborted && wall_ms >= 30_000 {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&format!(
                    "[Wall time: {:.1}s]",
                    exec_started.elapsed().as_secs_f64()
                ));
            }
            if let Some(notice) = clamp_notice {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&format!("[{}]", notice));
            }
            if aborted {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str("[Command aborted]");
            } else if timed_out {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                let sec_label = hard_timeout_sec.unwrap_or(300);
                combined.push_str(&format!(
                    "[SPAVN Timeout: Command exceeded {}s limit, terminated automatically]",
                    sec_label
                ));
            }
            let error_snippets = if !timed_out
                && !aborted
                && (host == "local" || host.is_empty())
                && (code != 0 || has_compiler_or_test_error_signature(&combined))
            {
                extract_compiler_error_snippets(
                    &ctx.last_cwd,
                    &combined,
                    Some(&mut ctx.read_mtimes),
                )
            } else {
                String::new()
            };
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
            if !error_snippets.is_empty() && !combined.contains("<auto_error_context>") {
                combined.push_str(&error_snippets);
            }
            if (host == "local" || host.is_empty()) && ctx.last_cwd != ctx.policy.cwd {
                let ws_canon = ctx
                    .policy
                    .cwd
                    .canonicalize()
                    .unwrap_or_else(|_| ctx.policy.cwd.clone());
                let cur_canon = ctx
                    .last_cwd
                    .canonicalize()
                    .unwrap_or_else(|_| ctx.last_cwd.clone());
                if cur_canon != ws_canon {
                    if !combined.is_empty() && !combined.ends_with('\n') {
                        combined.push('\n');
                    }
                    combined.push_str(&format!("[SPAVN Shell CWD: {}]", ctx.last_cwd.display()));
                }
            }
            if combined.trim().is_empty() {
                format!("(exit code {})", code)
            } else if code != 0 && !combined.contains("(exit code ") {
                format!("{}\n(exit code {})", combined, code)
            } else {
                combined
            }
        }
        Ok(BashLifecycleOutcome::PromotedToBackground {
            job_id,
            pid,
            log_path,
            partial_output,
            reason,
        }) => {
            let mut out = strip_pwd_sentinel_lines(&collapse_binary_noise_lines(&partial_output))
                .trim_end()
                .to_string();
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            if let Some(notice) = clamp_notice {
                out.push_str(&format!("[{}]\n", notice));
            }
            let timeout_desc = match hard_timeout_sec {
                Some(s) => format!("hard timeout: {}s", s),
                None => "hard timeout: disabled".to_string(),
            };
            match reason {
                BgPromotionReason::Steer => {
                    out.push_str("Backgrounded early to handle an incoming message; the command keeps running.\n");
                    out.push_str(&format!(
                        "[Job `{}` (PID {}, {}) continues in background — log: `{}`. Use `bash({{\"op\":\"status\",\"job_id\":\"{}\"}})`, `bash({{\"op\":\"wait\",\"job_id\":\"{}\"}})`, or `bash({{\"op\":\"kill\",\"job_id\":\"{}\"}})`.]",
                        job_id,
                        pid,
                        timeout_desc,
                        log_path.display(),
                        job_id,
                        job_id,
                        job_id
                    ));
                }
                BgPromotionReason::AutoThreshold { elapsed_sec } => {
                    out.push_str(&format!(
                        "[SPAVN Auto-Background: Command still running after {}s → promoted to background job `{}` (PID {}, {}). Output is streaming to `{}` — use `bash({{\"op\":\"wait\",\"job_id\":\"{}\"}})` to await completion, `bash({{\"op\":\"status\",\"job_id\":\"{}\"}})` to inspect progress, or `bash({{\"op\":\"kill\",\"job_id\":\"{}\"}})` to terminate.]",
                        elapsed_sec,
                        job_id,
                        pid,
                        timeout_desc,
                        log_path.display(),
                        job_id,
                        job_id,
                        job_id
                    ));
                }
            }
            out
        }
        Err(e) => format!("Execution error: {}", e),
    }
}

enum BgPromotionReason {
    Steer,
    AutoThreshold { elapsed_sec: u64 },
}

enum BashLifecycleOutcome {
    Completed {
        code: i32,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        timed_out: bool,
        aborted: bool,
    },
    PromotedToBackground {
        job_id: String,
        pid: u32,
        log_path: PathBuf,
        partial_output: String,
        reason: BgPromotionReason,
    },
}

struct LiveStreamTap {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    combined: Vec<u8>,
    log_file: Option<fs::File>,
}

fn allocate_bg_log_file(step_counter: usize) -> Result<(PathBuf, fs::File), String> {
    let log_dir = crate::config::spavn_dir().join("bg");
    let _ = fs::create_dir_all(&log_dir);
    crate::config::set_owner_only_mode(&log_dir, 0o700);
    for attempt in 0..8u32 {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos().wrapping_add(attempt))
            .unwrap_or(attempt);
        let log_path = log_dir.join(format!(
            "bg-{}-{}-{:09}.log",
            std::process::id(),
            step_counter,
            nonce
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log_path)
        {
            Ok(f) => {
                crate::config::set_owner_only_mode(&log_path, 0o600);
                return Ok((log_path, f));
            }
            Err(_) if attempt < 7 => continue,
            Err(e) => return Err(format!("Background spawn error: cannot create log: {}", e)),
        }
    }
    Err("Background spawn error: could not allocate unique log file".to_string())
}

fn spawn_tap_reader<R: Read + Send + 'static>(
    mut reader: R,
    tap: Arc<Mutex<LiveStreamTap>>,
    is_stderr: bool,
) -> std::thread::JoinHandle<()> {
    const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut guard) = tap.lock() {
                        let target = if is_stderr {
                            &mut guard.stderr
                        } else {
                            &mut guard.stdout
                        };
                        if target.len() < MAX_CAPTURE_BYTES {
                            let take = (MAX_CAPTURE_BYTES - target.len()).min(n);
                            target.extend_from_slice(&chunk[..take]);
                        }
                        if guard.combined.len() < MAX_CAPTURE_BYTES {
                            let take = (MAX_CAPTURE_BYTES - guard.combined.len()).min(n);
                            guard.combined.extend_from_slice(&chunk[..take]);
                        }
                        if let Some(ref mut f) = guard.log_file {
                            let _ = f.write_all(&chunk[..n]);
                            let _ = f.flush();
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

fn kill_child_process_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        let pgid = child.id() as i32;
        let _ = crate::tools::kill(-pgid, 15);
        let _ = crate::tools::kill(pgid, 15);
        std::thread::sleep(Duration::from_millis(30));
        let _ = crate::tools::kill(-pgid, 9);
        let _ = crate::tools::kill(pgid, 9);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn register_and_monitor_bg_child(
    mut child: std::process::Child,
    pid: u32,
    command: String,
    log_path: PathBuf,
    started_at: Instant,
    hard_timeout_sec: Option<u64>,
    tap_handles: Option<(
        Arc<Mutex<LiveStreamTap>>,
        Option<std::thread::JoinHandle<()>>,
        Option<std::thread::JoinHandle<()>>,
    )>,
) -> String {
    let seq = BG_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let job_id = format!("bg_{}", seq);
    let entry = BgJobEntry {
        id: job_id.clone(),
        pid,
        command: command.clone(),
        log_path: log_path.clone(),
        started_at,
        exit_code: None,
    };
    if let Ok(mut guard) = bg_jobs_registry().lock() {
        guard.push(entry);
    }
    let job_id_wait = job_id.clone();
    std::thread::spawn(move || {
        let hard_deadline = hard_timeout_sec.map(|s| started_at + Duration::from_secs(s));
        let mut timed_out = false;
        let code = loop {
            match child.try_wait() {
                Ok(Some(st)) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        break st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0));
                    }
                    #[cfg(not(unix))]
                    {
                        break st.code().unwrap_or(-1);
                    }
                }
                Ok(None) => {
                    if let Some(dl) = hard_deadline {
                        if Instant::now() >= dl {
                            timed_out = true;
                            kill_child_process_group(&mut child);
                            break -1;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
                Err(_) => break -1,
            }
        };
        if let Some((tap, out_h, err_h)) = tap_handles {
            if let Some(h) = out_h {
                let _ = h.join();
            }
            if let Some(h) = err_h {
                let _ = h.join();
            }
            if timed_out {
                if let Ok(mut guard) = tap.lock() {
                    if let Some(ref mut f) = guard.log_file {
                        let _ = writeln!(
                            f,
                            "\n[Command timed out after {} seconds]",
                            hard_timeout_sec.unwrap_or(300)
                        );
                        let _ = f.flush();
                    }
                }
            }
        } else if timed_out {
            if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&log_path) {
                let _ = writeln!(
                    f,
                    "\n[Command timed out after {} seconds]",
                    hard_timeout_sec.unwrap_or(300)
                );
            }
        }
        let was_already_killed = {
            let mut already_done = false;
            if let Ok(mut guard) = bg_jobs_registry().lock() {
                if let Some(e) = guard
                    .iter_mut()
                    .find(|j| j.id == job_id_wait && j.pid == pid)
                {
                    if e.exit_code.is_some() {
                        already_done = true;
                    } else {
                        e.exit_code = Some(code);
                    }
                }
            }
            already_done
        };
        if !was_already_killed {
            let tail_preview = read_log_tail_lines(&log_path, 40);
            if let Ok(mut q) = BG_NOTIFICATIONS.lock() {
                q.push(BgJobCompletionNotice {
                    job_id: job_id_wait,
                    pid,
                    command,
                    exit_code: code,
                    timed_out,
                    elapsed_sec: started_at.elapsed().as_secs(),
                    log_path,
                    tail_preview,
                });
            }
        }
    });
    job_id
}

fn run_bash_with_omp_lifecycle(
    mut cmd: Command,
    command_str: &str,
    step_counter: usize,
    hard_timeout_sec: Option<u64>,
    auto_bg_ms: Option<u64>,
    stdin_bytes: Option<&[u8]>,
) -> Result<BashLifecycleOutcome, String> {
    if stdin_bytes.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                crate::tools::setsid_and_cloexec();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn: {}", e))?;
    if let Some(payload) = stdin_bytes {
        if let Some(mut child_stdin) = child.stdin.take() {
            let payload_vec = payload.to_vec();
            std::thread::spawn(move || {
                let _ = child_stdin.write_all(&payload_vec);
                let _ = child_stdin.flush();
            });
        }
    }
    let tap = Arc::new(Mutex::new(LiveStreamTap {
        stdout: Vec::new(),
        stderr: Vec::new(),
        combined: Vec::new(),
        log_file: None,
    }));
    let out_handle = child
        .stdout
        .take()
        .map(|s| spawn_tap_reader(s, Arc::clone(&tap), false));
    let err_handle = child
        .stderr
        .take()
        .map(|s| spawn_tap_reader(s, Arc::clone(&tap), true));

    let started_at = Instant::now();
    let hard_deadline = hard_timeout_sec.map(|s| started_at + Duration::from_secs(s));
    let auto_bg_deadline = auto_bg_ms
        .filter(|&ms| ms > 0)
        .map(|ms| started_at + Duration::from_millis(ms));

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if let Some(h) = out_handle {
                    let _ = h.join();
                }
                if let Some(h) = err_handle {
                    let _ = h.join();
                }
                #[cfg(unix)]
                let code = {
                    use std::os::unix::process::ExitStatusExt;
                    status
                        .code()
                        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
                };
                #[cfg(not(unix))]
                let code = status.code().unwrap_or(-1);
                let (stdout, stderr) = if let Ok(mut g) = tap.lock() {
                    (
                        std::mem::take(&mut g.stdout),
                        std::mem::take(&mut g.stderr),
                    )
                } else {
                    (Vec::new(), Vec::new())
                };
                return Ok(BashLifecycleOutcome::Completed {
                    code,
                    stdout,
                    stderr,
                    timed_out: false,
                    aborted: false,
                });
            }
            Ok(None) => {
                if crate::is_interrupted() {
                    kill_child_process_group(&mut child);
                    if let Some(h) = out_handle {
                        let _ = h.join();
                    }
                    if let Some(h) = err_handle {
                        let _ = h.join();
                    }
                    let (stdout, stderr) = if let Ok(mut g) = tap.lock() {
                        (
                            std::mem::take(&mut g.stdout),
                            std::mem::take(&mut g.stderr),
                        )
                    } else {
                        (Vec::new(), Vec::new())
                    };
                    return Ok(BashLifecycleOutcome::Completed {
                        code: 130,
                        stdout,
                        stderr,
                        timed_out: false,
                        aborted: true,
                    });
                }
                let steer_pending = crate::steer_message_count() > 0;
                let auto_bg_hit = auto_bg_deadline.is_some_and(|dl| Instant::now() >= dl);
                if steer_pending || auto_bg_hit {
                    if let Ok((log_path, mut log_file)) = allocate_bg_log_file(step_counter) {
                        let partial_output = if let Ok(mut g) = tap.lock() {
                            if !g.combined.is_empty() {
                                let _ = log_file.write_all(&g.combined);
                                let _ = log_file.flush();
                            }
                            let preview = String::from_utf8_lossy(&g.combined).to_string();
                            g.log_file = Some(log_file);
                            preview
                        } else {
                            String::new()
                        };
                        let pid = child.id();
                        let elapsed_sec = started_at.elapsed().as_secs();
                        let job_id = register_and_monitor_bg_child(
                            child,
                            pid,
                            command_str.to_string(),
                            log_path.clone(),
                            started_at,
                            hard_timeout_sec,
                            Some((Arc::clone(&tap), out_handle, err_handle)),
                        );
                        let reason = if steer_pending {
                            BgPromotionReason::Steer
                        } else {
                            BgPromotionReason::AutoThreshold { elapsed_sec }
                        };
                        return Ok(BashLifecycleOutcome::PromotedToBackground {
                            job_id,
                            pid,
                            log_path,
                            partial_output,
                            reason,
                        });
                    }
                }
                if let Some(dl) = hard_deadline {
                    if Instant::now() >= dl {
                        kill_child_process_group(&mut child);
                        if let Some(h) = out_handle {
                            let _ = h.join();
                        }
                        if let Some(h) = err_handle {
                            let _ = h.join();
                        }
                        let (stdout, stderr) = if let Ok(mut g) = tap.lock() {
                            (
                                std::mem::take(&mut g.stdout),
                                std::mem::take(&mut g.stderr),
                            )
                        } else {
                            (Vec::new(), Vec::new())
                        };
                        return Ok(BashLifecycleOutcome::Completed {
                            code: -1,
                            stdout,
                            stderr,
                            timed_out: true,
                            aborted: false,
                        });
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("Wait error: {}", e)),
        }
    }
}

/// Replaces lines corrupted by raw binary decoding (`\u{FFFD}`, NUL or C1 control bursts)
/// with a compact `[... N binary line(s) omitted ...]` summary so binary dumps never
/// garble the HUD rail or pollute model context.
fn collapse_binary_noise_lines(text: &str) -> String {
    if !text.contains('\u{fffd}') && !text.contains('\0') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut omitted = 0usize;
    for line in text.lines() {
        if crate::hud::toolcard::is_binary_noise_line(line) {
            omitted += 1;
            continue;
        }
        if omitted > 0 {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("[... {} binary line(s) omitted ...]", omitted));
            omitted = 0;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    if omitted > 0 {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("[... {} binary line(s) omitted ...]", omitted));
    }
    if text.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}
