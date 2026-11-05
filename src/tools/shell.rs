use super::*;

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
pub(crate) fn check_egress_gate(ctx: &mut ToolContext, url: &str, allow_local: bool) -> Option<String> {
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

/// OMP-style Bash Interceptor: blocks standalone shell commands (`grep -rn`, `find`, `cat`,
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
    let first_seg = trimmed
        .split(['|', ';', '&'])
        .next()
        .unwrap_or(trimmed)
        .trim();
    let tokens: Vec<&str> = first_seg.split_whitespace().collect();
    let Some(&bin) = tokens.first() else {
        return None;
    };
    match bin {
        "grep" | "egrep" | "fgrep" => {
            let is_recursive = tokens
                .iter()
                .skip(1)
                .any(|t| t.starts_with('-') && (t.contains('r') || t.contains('R')));
            if is_recursive {
                return Some(
                    "[SPAVN Bash Interceptor]: Blocked recursive `grep` in `bash` (walks `node_modules`/build artifacts and causes 120s timeouts). Use the built-in `search` tool (`search({\"pattern\": \"...\", \"path\": \"...\"})`), which automatically skips ignored directories and caps per-file matches."
                        .to_string(),
                );
            }
        }
        "find" => {
            let has_action = tokens
                .iter()
                .any(|&t| matches!(t, "-exec" | "-delete" | "-ok" | "-print0"));
            if !has_action && !trimmed.contains('|') {
                return Some(
                    "[SPAVN Bash Interceptor]: Blocked standalone `find` in `bash`. Use the built-in `glob` tool (`glob({\"pattern\": \"**/*...\", \"path\": \"...\"})`) which automatically skips `node_modules`, `target`, and `.git`."
                        .to_string(),
                );
            }
        }
        "cat" | "head" | "tail" | "sed" => {
            let has_shell_ops = trimmed.contains('|')
                || trimmed.contains('>')
                || trimmed.contains('<')
                || trimmed.contains("&&")
                || trimmed.contains("||")
                || trimmed.contains(';')
                || trimmed.contains("$(");
            let is_sed_in_place =
                bin == "sed" && tokens.iter().skip(1).any(|t| t.starts_with("-i"));
            if !has_shell_ops && !is_sed_in_place && tokens.len() >= 2 {
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
        .unwrap_or(45);
    let background = args
        .get("background")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let Err(blocked) = check_bash_gate(ctx, command, &host, cwd) {
        return blocked;
    }
    if host == "local" || host.is_empty() {
        if let Some(intercept_msg) = intercept_shell_anti_pattern(command) {
            return intercept_msg;
        }
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
