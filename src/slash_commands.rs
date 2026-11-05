use crate::config::{self, is_en, load_user_config, tr_en};
use crate::hud::{
    self, build_session_recap_line, fetch_quick_quota_line, interactive_provider_login,
    interactive_queue_manager, interactive_select_access, interactive_select_host,
    interactive_select_language, interactive_select_model, interactive_select_role,
    interactive_select_session, interactive_select_skill, render_fleet_radar,
    render_full_quota_card, render_session_resume_preview, render_update_notice,
    render_welcome_header, reset_scroll_region, set_idle_recap, setup_scroll_region, term_size,
    toggle_idle_recap, QueueAction,
};
use crate::memory::cached_system_prompt;
use crate::router::{resolve_endpoint, ChatMessage, ModelEndpoint};
use crate::session::{
    append_message, fork_session, load_session, new_session_path, perform_manual_compact,
    rewrite_session,
};
use crate::skills::{
    discover_skills, load_skill_content, remove_custom_skill, render_skills_panel,
    save_custom_skill,
};
use crate::spavn_engine::{
    add_goal, add_pin, complete_goal, remove_pin, render_audit_panel, render_goals_panel,
    render_janitor_panel, render_pins_panel, render_repo_wiki, AgentPhase, AgentRoleKey,
};
use crate::state::{push_queued_message, queued_message_count};
use crate::tools::{AccessMode, ToolContext};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

pub enum SlashOutcome {
    Handled,
    ExitLoop,
    NotSlash,
}

/// Model switch can flip local/cloud, so rebuild the system prompt to match.
fn refresh_system_prompt(
    cwd: &str,
    endpoint: &ModelEndpoint,
    session_file: &PathBuf,
    messages: &mut [ChatMessage],
) {
    if let Some(first) = messages.first_mut().filter(|m| m.role == "system") {
        *first = ChatMessage::system(cached_system_prompt(cwd, endpoint.wants_compact_prompt()));
        rewrite_session(session_file, messages);
    }
}

/// Copies `src` onto a fresh inode beside `dest`, ad-hoc signs it on macOS, then renames it into
/// place. Writing over the inode `dest` already points at leaves a stale code signature in the
/// kernel cache, and the next `execve()` of that path is killed before `main()` runs.
fn stage_binary(src: &Path, dest: &Path) -> bool {
    let Some(parent) = dest.parent() else {
        return false;
    };
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("spavn");
    let tmp = parent.join(format!(".{}.tmp.{}", name, std::process::id()));
    if std::fs::copy(src, &tmp).is_err() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        for (cmd, args) in [("codesign", &["-s", "-", "-f"][..]), ("xattr", &["-c"][..])] {
            let _ = std::process::Command::new(cmd)
                .args(args)
                .arg(&tmp)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    if std::fs::rename(&tmp, dest).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

/// Points `spvn` at `spavn` as a symlink, matching `install.sh`.
///
/// A copy would be a second inode with its own signature to go stale, which is exactly how a
/// working `spavn` and a killed `spvn` end up side by side.
fn link_alias(binary: &Path) {
    let Some(parent) = binary.parent() else {
        return;
    };
    let alias = parent.join("spvn");
    let _ = std::fs::remove_file(&alias);
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(binary, &alias);
    #[cfg(not(unix))]
    let _ = std::fs::copy(binary, &alias);
}

/// True when `path` really execs, checked with `--version`.
///
/// Staging plus a probe is what keeps a broken install from costing the session: without it the
/// loop execs the bad path and the terminal reports nothing but `[1] killed`.
fn probe_binary(path: &Path) -> bool {
    let mut child = match std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                return false;
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_slash_command(
    input: &str,
    cwd: &str,
    endpoint: &mut ModelEndpoint,
    ctx: &mut ToolContext,
    active_phase: &mut AgentPhase,
    active_role: &mut AgentRoleKey,
    show_thinking: &mut bool,
    quota_badge: &mut String,
    prefill_input: &mut Option<String>,
    session_file: &mut PathBuf,
    session_name: &mut String,
    messages: &mut Vec<ChatMessage>,
) -> SlashOutcome {
    if matches!(
        input,
        "/exit" | "/quit" | "/x" | ":q" | ":wq" | "q!" | "exit" | "quit"
    ) {
        reset_scroll_region();
        return SlashOutcome::ExitLoop;
    }

    if input == "/reload" || input == "/r" || input == "/reload bin" || input == "/rb" {
        if !messages.is_empty() {
            messages.retain(|m| {
                !(m.role == "assistant"
                    && m.tool_calls.is_none()
                    && m.content
                        .as_deref()
                        .map(|c| c.contains("DSML") && c.contains("invoke"))
                        .unwrap_or(false))
            });
            if !messages.is_empty() {
                messages[0] =
                    ChatMessage::system(cached_system_prompt(cwd, endpoint.wants_compact_prompt()));
                rewrite_session(session_file, messages);
            }
        }
        reset_scroll_region();
        let skill_count = discover_skills().len();
        println!(
            "\n{}🔄 [SPAVN Canlı Reload]: Binary, sistem çekirdeği ve {} yetenek (skill) oturum korunarak yenileniyor...{}",
            GREEN, skill_count, RESET
        );
        let exe_path = {
            let home = std::env::var("HOME").unwrap_or_default();
            let home_bin = PathBuf::from(&home).join(".local/bin/spavn");
            let cwd_release_bin = PathBuf::from(cwd).join("target/release/spavn");
            let dev_release_bin = PathBuf::from(&home).join("apps/spavn-cli/target/release/spavn");
            let release_bin = if cwd_release_bin.exists() {
                cwd_release_bin
            } else {
                dev_release_bin
            };
            // If a newer release binary exists, stage it on a fresh inode, ad-hoc sign it, then
            // rename it over the installed path. Writing over the existing inode leaves a stale
            // code signature in the kernel cache and `execve()` is killed outright — and `spvn`
            // must stay a symlink to `spavn`, because a second copy is a second inode that can go
            // stale on its own.
            let candidate = if release_bin.exists() {
                let rel_mtime = std::fs::metadata(&release_bin)
                    .and_then(|m| m.modified())
                    .ok();
                let home_mtime = std::fs::metadata(&home_bin).and_then(|m| m.modified()).ok();
                if rel_mtime >= home_mtime {
                    &release_bin
                } else if home_bin.exists() {
                    &home_bin
                } else {
                    &release_bin
                }
            } else {
                &home_bin
            };
            if candidate.exists() && home_bin.parent().map(|p| p.exists()).unwrap_or(false) {
                if stage_binary(candidate, &home_bin) {
                    link_alias(&home_bin);
                    if probe_binary(&home_bin) {
                        home_bin
                    } else {
                        // The new binary does not exec (stale or broken signature). Keep this
                        // session alive on the running image instead of exec'ing into a kill.
                        eprintln!(
                            "{}⚠ {} {} {}",
                            YELLOW,
                            home_bin.display(),
                            tr_en(
                                "çalıştırılamıyor, mevcut binary ile devam ediliyor.",
                                "cannot exec, continuing on the running binary."
                            ),
                            RESET
                        );
                        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("spavn"))
                    }
                } else {
                    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("spavn"))
                }
            } else {
                std::env::current_exe().unwrap_or_else(|_| PathBuf::from("spavn"))
            }
        };
        let access_str = match ctx.access {
            AccessMode::Safe => "safe",
            AccessMode::Auto => "auto",
            AccessMode::Sovereign => "sovereign",
        };
        let session_path_str = session_file.to_string_lossy().to_string();
        let reload_args = [
            "-s",
            &session_path_str,
            "-m",
            &endpoint.key,
            "-a",
            access_str,
            "-P",
            active_phase.as_str(),
            "--host",
            &ctx.default_host,
        ];
        #[cfg(unix)]
        {
            let _ = std::process::Command::new(&exe_path)
                .args(reload_args)
                .exec();
        }
        #[cfg(not(unix))]
        {
            if let Ok(mut child) = std::process::Command::new(&exe_path)
                .args(reload_args)
                .spawn()
            {
                let _ = child.wait();
                std::process::exit(0);
            }
        }
        return SlashOutcome::ExitLoop;
    }

    if matches!(
        input,
        "/byok" | "/b" | "/keys" | "/login" | "/provider" | "/providers" | "/connect" | "/auth"
    ) || input.starts_with("/login ")
        || input.starts_with("/provider ")
        || input.starts_with("/connect ")
    {
        let pre = input
            .strip_prefix("/login ")
            .or_else(|| input.strip_prefix("/provider "))
            .or_else(|| input.strip_prefix("/connect "))
            .map(str::trim);
        if let Some(chosen_model) = interactive_provider_login(pre) {
            *endpoint = resolve_endpoint(&chosen_model);
            let mut cfg = load_user_config();
            cfg.default_model = endpoint.key.clone();
            let _ = config::save_user_config(&cfg);
            refresh_system_prompt(cwd, endpoint, session_file, messages);
            println!(
                "\n{}🧠 Aktif Model Seçildi: {}{}{}",
                GREEN, BOLD, endpoint.label, RESET
            );
        }
        return SlashOutcome::Handled;
    }

    if matches!(input, "/lang" | "/language" | "/dil") {
        if let Some(chosen_lang) = interactive_select_language() {
            if !messages.is_empty() {
                messages[0] =
                    ChatMessage::system(cached_system_prompt(cwd, endpoint.wants_compact_prompt()));
            }
            let label = if chosen_lang == "en" {
                "🇬🇧 English (EN)"
            } else {
                "🇹🇷 Türkçe (TR)"
            };
            println!(
                "\n{}🌐 {}: {}{}{}",
                GREEN,
                tr_en(
                    "Arayüz ve Ajan Dili Güncellendi",
                    "Interface & Agent Language Updated"
                ),
                BOLD,
                label,
                RESET
            );
        }
        return SlashOutcome::Handled;
    }

    if input == "/en" || input == "/tr" {
        let mut cfg = load_user_config();
        cfg.language = input.trim_start_matches('/').to_string();
        let _ = config::save_user_config(&cfg);
        if !messages.is_empty() {
            messages[0] =
                ChatMessage::system(cached_system_prompt(cwd, endpoint.wants_compact_prompt()));
        }
        println!(
            "\n{}🌐 {}: {}{}{}",
            GREEN,
            tr_en("Dil Güncellendi", "Language Updated"),
            BOLD,
            if is_en() {
                "🇬🇧 English (EN)"
            } else {
                "🇹🇷 Türkçe (TR)"
            },
            RESET
        );
        return SlashOutcome::Handled;
    }

    if input == "/queue" || input == "/qlist" || input == "/qedit" {
        if let QueueAction::EditMessage(msg_to_edit) = interactive_queue_manager() {
            *prefill_input = Some(msg_to_edit);
        }
        return SlashOutcome::Handled;
    }
    if matches!(
        input,
        "/queue clear" | "/qclear" | "/queue cancel" | "/qcancel"
    ) {
        crate::clear_queued_messages();
        crate::clear_steer_messages();
        println!(
            "\n{}🗑️  {}{}",
            GREEN,
            tr_en(
                "Bekleyen tüm kuyruk ve canlı müdahale mesajları iptal edildi.",
                "All queued and live-steer messages have been cancelled."
            ),
            RESET
        );
        return SlashOutcome::Handled;
    }
    if matches!(input, "/queue pop" | "/qpop" | "/queue edit") {
        if let Some((_, popped)) = crate::pop_last_pending_message() {
            *prefill_input = Some(popped);
        } else {
            println!(
                "\n{}{}{}",
                YELLOW,
                tr_en(
                    "Kuyrukta düzenlenecek mesaj yok.",
                    "No queued message to edit."
                ),
                RESET
            );
        }
        return SlashOutcome::Handled;
    }
    if let Some(rest) = input.strip_prefix("/queue ") {
        push_queued_message(rest.trim().to_string());
        println!(
            "\n{}📥 Mesaj kuyruğa eklendi (#{}): {}{}{}",
            GREEN,
            queued_message_count(),
            BOLD,
            rest.trim(),
            RESET
        );
        return SlashOutcome::Handled;
    }

    if input == "/skills" || input == "/skill" || input == "/s" {
        if let Some(chosen_skill) = interactive_select_skill() {
            if let Some((entry, content)) = load_skill_content(&chosen_skill) {
                let skill_msg = ChatMessage::user(format!(
                    "<active_skill name=\"{}\" path=\"{}\">\n{}\n</active_skill>\n(Yukarıdaki @{} yeteneği oturuma yüklendi.)",
                    entry.name,
                    entry.path.display(),
                    content.trim(),
                    entry.name
                ));
                append_message(session_file, &skill_msg);
                messages.push(skill_msg);
                println!(
                    "\n{}🧩 Yetenek aktif oturuma yüklendi: @{}{} ({})",
                    GREEN, BOLD, entry.name, entry.summary
                );
            }
        } else {
            render_skills_panel();
        }
        return SlashOutcome::Handled;
    }
    if let Some(rest) = input.strip_prefix("/skill add ") {
        let mut parts = rest.splitn(2, ' ');
        let name = parts.next().unwrap_or("").trim();
        let rules = parts.next().unwrap_or("").trim();
        if name.is_empty() || rules.is_empty() {
            println!(
                "{}Kullanım: /skill add <ad> <talimat/kural metni>{}",
                YELLOW, RESET
            );
        } else {
            match save_custom_skill(name, rules) {
                Ok(p) => println!(
                    "\n{}🧩 Yetenek kaydedildi: @{} ({}){}",
                    GREEN,
                    name.trim_start_matches('@'),
                    p.display(),
                    RESET
                ),
                Err(e) => println!("\n{}✗ Hata: {}{}", RED, e, RESET),
            }
        }
        return SlashOutcome::Handled;
    }
    if let Some(rest) = input.strip_prefix("/skill show ") {
        if let Some((entry, content)) = load_skill_content(rest) {
            println!(
                "\n{}╭─ 🧩 @{} ({}) ──────────────────────────────────────────{}",
                CYAN,
                entry.name,
                entry.path.display(),
                RESET
            );
            for l in content.lines().take(40) {
                println!("{}│{} {}", CYAN, RESET, l);
            }
            println!(
                "{}╰────────────────────────────────────────────────────────────{}",
                CYAN, RESET
            );
        } else {
            println!("\n{}Yetenek bulunamadı: {}{}", YELLOW, rest, RESET);
        }
        return SlashOutcome::Handled;
    }
    if let Some(rest) = input.strip_prefix("/skill rm ") {
        match remove_custom_skill(rest) {
            Ok(()) => println!("\n{}🗑️  Yetenek silindi: @{}{}", GREEN, rest.trim(), RESET),
            Err(e) => println!("\n{}✗ {}{}", YELLOW, e, RESET),
        }
        return SlashOutcome::Handled;
    }

    if input == "/access" || input == "/a" {
        if let Some(chosen_access) = interactive_select_access(ctx.access) {
            ctx.access = chosen_access;
            println!(
                "\n{}🛡️  Erişim Yetki Modu seçildi: {}{}{}",
                GREEN,
                BOLD,
                ctx.access.badge(),
                RESET
            );
        }
        return SlashOutcome::Handled;
    }
    if let Some(rest) = input.strip_prefix("/access ") {
        if let Some(m) = AccessMode::parse_alias(rest) {
            ctx.access = m;
            println!(
                "\n{}🛡️  Erişim Yetki Modu ayarlandı: {}{}{}",
                GREEN,
                BOLD,
                ctx.access.badge(),
                RESET
            );
        } else {
            println!(
                "{}Kullanım: /access <safe | auto | sovereign>{}",
                YELLOW, RESET
            );
        }
        return SlashOutcome::Handled;
    }

    if input.eq_ignore_ascii_case("/plan")
        || input.eq_ignore_ascii_case("/code")
        || input.eq_ignore_ascii_case("/act")
    {
        let next = if input.eq_ignore_ascii_case("/plan") {
            AgentPhase::Plan
        } else {
            AgentPhase::Code
        };
        if next != *active_phase {
            *active_phase = next;
            ctx.phase = next;
            if next == AgentPhase::Plan {
                println!(
                    "\n{}📐 PLAN — salt-okunur: write/edit/bash kilitli. Planı yaz, kullanıcı /code ile açar.{}",
                    MAGENTA, RESET
                );
            } else {
                println!(
                    "\n{}💻 CODE — değişiklikler açık; yukarıdaki plan referans.{}",
                    GREEN, RESET
                );
            }
        }
        return SlashOutcome::Handled;
    }

    if input == "/model" || input == "/m" || input == "/models" || input == "/local" {
        let local_first = input == "/local";
        if let Some(chosen_model) = interactive_select_model(&endpoint.key, local_first) {
            *endpoint = resolve_endpoint(&chosen_model);
            let mut cfg = load_user_config();
            cfg.default_model = endpoint.key.clone();
            let _ = config::save_user_config(&cfg);
            refresh_system_prompt(cwd, endpoint, session_file, messages);
            println!(
                "\n{}🧠 Aktif Model Seçildi: {}{}{}",
                GREEN, BOLD, endpoint.label, RESET
            );
        }
        return SlashOutcome::Handled;
    }
    if let Some(rest) = input.strip_prefix("/model ") {
        *endpoint = resolve_endpoint(rest);
        let mut cfg = load_user_config();
        cfg.default_model = endpoint.key.clone();
        let _ = config::save_user_config(&cfg);
        refresh_system_prompt(cwd, endpoint, session_file, messages);
        println!(
            "\n{}🧠 Aktif Model: {}{}{}",
            GREEN, BOLD, endpoint.label, RESET
        );
        return SlashOutcome::Handled;
    }

    if input == "/host" || input == "/h" {
        if let Some(chosen_host) = interactive_select_host(&ctx.default_host) {
            ctx.default_host = chosen_host;
            println!(
                "\n{}🖥️  Varsayılan Hedef Cihaz Seçildi: {}{}{}",
                MAGENTA, BOLD, ctx.default_host, RESET
            );
        }
        return SlashOutcome::Handled;
    }
    if let Some(rest) = input.strip_prefix("/host ") {
        ctx.default_host = rest.trim().to_string();
        println!(
            "\n{}🖥️  Varsayılan Hedef Cihaz: {}{}{}",
            MAGENTA, BOLD, ctx.default_host, RESET
        );
        return SlashOutcome::Handled;
    }

    if input == "/quota" || input == "/q" {
        render_full_quota_card();
        *quota_badge = fetch_quick_quota_line();
        return SlashOutcome::Handled;
    }

    if input == "/fleet" || input == "/f" {
        render_fleet_radar();
        return SlashOutcome::Handled;
    }

    if input == "/bench" || input.starts_with("/bench ") {
        let target_key = input.strip_prefix("/bench").unwrap_or("").trim();
        let bench_ep = if target_key.is_empty() {
            endpoint.clone()
        } else {
            resolve_endpoint(target_key)
        };
        let mut spin = hud::LiveSpinner::start(&format!(
            "🏎️  {} ({})",
            tr_en(
                "Model hız ve araç çağrısı testi",
                "Benchmarking model speed & tool-calling"
            ),
            bench_ep.key
        ));
        let bench_msgs = vec![
            ChatMessage::system(
                "You are a fast coding benchmark probe. Call the `read` tool on `Cargo.toml` with start_line=1 and end_line=5.",
            ),
            ChatMessage::user("Read lines 1..5 of Cargo.toml using the read tool now."),
        ];
        let tools_schema = crate::tools::tool_definitions();
        let res = crate::router::stream_chat_completion(
            &bench_ep,
            &bench_msgs,
            &tools_schema,
            |_| {},
            |_| {},
        );
        spin.stop();
        match res {
            Ok(cr) => {
                let total_sec = (cr.elapsed_ms as f64 / 1000.0).max(0.001);
                let decode_ms = cr.elapsed_ms.saturating_sub(cr.ttft_ms).max(1);
                let decode_sec = decode_ms as f64 / 1000.0;
                let out_toks = if cr.usage.completion_tokens > 0 {
                    cr.usage.completion_tokens as f64
                } else {
                    let est_bytes = cr.message.content.as_ref().map(|s| s.len()).unwrap_or(0)
                        + cr.message
                            .reasoning_content
                            .as_ref()
                            .map(|s| s.len())
                            .unwrap_or(0)
                        + cr.message
                            .tool_calls
                            .as_ref()
                            .map(|tcs| {
                                tcs.iter()
                                    .map(|t| t.function.name.len() + t.function.arguments.len())
                                    .sum()
                            })
                            .unwrap_or(0);
                    (est_bytes.div_ceil(4)).max(1) as f64
                };
                let tok_per_sec = out_toks / decode_sec;
                let tool_ok = cr
                    .message
                    .tool_calls
                    .as_ref()
                    .map(|tcs| {
                        tcs.iter().any(|tc| {
                            tc.function.name == "read"
                                && serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                                    .is_ok()
                        })
                    })
                    .unwrap_or(false);
                let tool_badge = if tool_ok {
                    format!("{}✅ PASS (read JSON){}", GREEN, RESET)
                } else {
                    format!("{}⚠️  NO TOOL CALL{}", YELLOW, RESET)
                };
                println!(
                    "\n{}╭─ 🏎️  SPAVN MODEL BENCHMARK [{}] ──────────────────────────────{}",
                    CYAN, bench_ep.key, RESET
                );
                println!(
                    "{}│{}  • Model       : {}{}{}",
                    CYAN, RESET, BOLD, bench_ep.label, RESET
                );
                println!(
                    "{}│{}  • TTFT        : {}{} ms{} (VRAM yükleme + prefill)",
                    CYAN, RESET, CYAN, cr.ttft_ms, RESET
                );
                println!(
                    "{}│{}  • Hız (Decode): {}{:.1} tok/s{} ({:.2}s üretim / {:.2}s toplam)",
                    CYAN, RESET, GREEN, tok_per_sec, RESET, decode_sec, total_sec
                );
                println!("{}│{}  • Tool-Call   : {}", CYAN, RESET, tool_badge);
                println!(
                    "{}╰──────────────────────────────────────────────────────────────────{}",
                    CYAN, RESET
                );
            }
            Err(e) => {
                println!(
                    "\n{}✗ Benchmark hatası ({}): {}{}",
                    RED, bench_ep.key, e, RESET
                );
            }
        }
        return SlashOutcome::Handled;
    }

    if input == "/diff" || input == "/d" || input.starts_with("/diff ") {
        let path_filter = input.strip_prefix("/diff").unwrap_or("").trim();
        let mut cmd = std::process::Command::new("git");
        cmd.args(["diff", "--color=always"]);
        if !path_filter.is_empty() {
            cmd.arg("--").arg(path_filter);
        }
        match cmd.output() {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                if text.trim().is_empty() {
                    println!(
                        "\n{}{}{}",
                        GREEN,
                        tr_en(
                            "Çalışma ağacında kaydedilmemiş değişiklik yok.",
                            "Working tree clean. No uncommitted diffs."
                        ),
                        RESET
                    );
                } else {
                    println!(
                        "\n{}╭─ 🔍 GIT DIFF ──────────────────────────────────────────{}",
                        CYAN, RESET
                    );
                    let total_lines = text.lines().count();
                    for line in text.lines().take(120) {
                        println!("{}│{} {}", CYAN, RESET, line);
                    }
                    if total_lines > 120 {
                        println!(
                            "{}╰─ ... {} {}{}",
                            CYAN,
                            total_lines - 120,
                            tr_en(
                                "satır daha var (/diff <dosya>: filtre)",
                                "more lines (/diff <path>: filter)"
                            ),
                            RESET
                        );
                    } else {
                        println!(
                            "{}╰────────────────────────────────────────────────────────{}",
                            CYAN, RESET
                        );
                    }
                }
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                println!("\n{}✗ Git diff hatası: {}{}", RED, err.trim(), RESET);
            }
            Err(e) => println!("\n{}✗ Git komutu çalıştırılamadı: {}{}", RED, e, RESET),
        }
        return SlashOutcome::Handled;
    }

    if input == "/commit" || input == "/cm" || input.starts_with("/commit ") {
        let custom_msg = input
            .strip_prefix("/commit")
            .or_else(|| input.strip_prefix("/cm"))
            .unwrap_or("")
            .trim()
            .to_string();

        let stat_out = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();

        if stat_out.trim().is_empty() {
            println!(
                "\n{}{}{}",
                GREEN,
                tr_en(
                    "Commit edilecek bir değişiklik yok.",
                    "Nothing to commit, working tree clean."
                ),
                RESET
            );
            return SlashOutcome::Handled;
        }

        let changed_files: Vec<String> = stat_out
            .lines()
            .filter_map(|l| l.get(3..))
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
            .collect();

        let msg = if !custom_msg.is_empty() {
            custom_msg
        } else {
            suggest_commit_message(&changed_files)
        };

        println!(
            "\n{}╭─ 📦 {} ────────────────────────────────────{}",
            CYAN,
            tr_en("GIT COMMIT", "GIT COMMIT"),
            RESET
        );
        println!(
            "{}│{}  {}{}{} ({} {})",
            CYAN,
            RESET,
            BOLD,
            tr_en("Değişen Dosyalar", "Changed Files"),
            RESET,
            changed_files.len(),
            tr_en("dosya", "files")
        );
        for f in changed_files.iter().take(8) {
            println!("{}│{}    • {}", CYAN, RESET, f);
        }
        println!(
            "{}│{}  {}{}{} \"{}{}{}\"",
            CYAN,
            RESET,
            BOLD,
            tr_en("Mesaj", "Message"),
            RESET,
            GREEN,
            msg,
            RESET
        );
        print!(
            "{}╰─ {} {}",
            CYAN,
            tr_en(
                "Onaylıyor musunuz? [e = Evet / H = Hayır]",
                "Approve? [y = Yes / N = No]"
            ),
            RESET
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let mut ans = String::new();
        // Ok(0) = EOF: never treat closed stdin as approval.
        if !matches!(std::io::stdin().read_line(&mut ans), Ok(n) if n > 0) {
            println!("\n{}✗ Commit iptal edildi.{}\n", YELLOW, RESET);
            return SlashOutcome::Handled;
        }
        let clean = ans.trim().to_lowercase();
        if !matches!(clean.as_str(), "e" | "evet" | "y" | "yes") {
            println!("{}✗ Commit iptal edildi.{}\n", YELLOW, RESET);
            return SlashOutcome::Handled;
        }

        let add_ok = std::process::Command::new("git")
            .args(["add", "-A"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !add_ok {
            println!("{}✗ git add başarısız.{}\n", RED, RESET);
            return SlashOutcome::Handled;
        }

        match std::process::Command::new("git")
            .args(["commit", "-m", &msg])
            .output()
        {
            Ok(out) if out.status.success() => println!(
                "{}✓ {}{}\n",
                GREEN,
                tr_en("Commit başarıyla oluşturuldu.", "Committed successfully."),
                RESET
            ),
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                println!("{}✗ Commit başarısız: {}{}\n", RED, err.trim(), RESET);
            }
            Err(e) => println!("{}✗ Git hatası: {}{}\n", RED, e, RESET),
        }
        return SlashOutcome::Handled;
    }

    if input == "/undo" || input == "/u" {
        match ctx.pop_undo() {
            Some(msg) => println!("\n{}{}{}", GREEN, msg, RESET),
            None => println!("\n{}Geri alınacak dosya değişikliği yok.{}", YELLOW, RESET),
        }
        return SlashOutcome::Handled;
    }

    if input == "/todo"
        || input == "/tasks"
        || input.starts_with("/todo ")
        || input.starts_with("/tasks ")
    {
        let arg = input
            .strip_prefix("/todo")
            .or_else(|| input.strip_prefix("/tasks"))
            .unwrap_or("")
            .trim();
        if matches!(arg, "clear" | "c" | "temizle" | "reset") {
            crate::clear_live_tasks();
            println!(
                "\n{}✓ {}{}",
                GREEN,
                tr_en("Canlı görev listesi temizlendi.", "Live task list cleared."),
                RESET
            );
            return SlashOutcome::Handled;
        }
        if let Some(rest) = arg
            .strip_prefix("done ")
            .or_else(|| arg.strip_prefix("check "))
        {
            if let Ok(idx) = rest.trim().parse::<usize>() {
                if crate::toggle_live_task(idx) {
                    println!(
                        "\n{}✓ #{}: {}{}",
                        GREEN,
                        idx,
                        tr_en("durumu güncellendi.", "status toggled."),
                        RESET
                    );
                } else {
                    println!(
                        "\n{}#{}: {}{}",
                        YELLOW,
                        idx,
                        tr_en("geçerli bir görev numarası değil.", "invalid task number."),
                        RESET
                    );
                }
                return SlashOutcome::Handled;
            }
        }
        if let Some(rest) = arg.strip_prefix("add ") {
            crate::add_live_task(rest.trim().to_string());
        } else if !arg.is_empty() && !matches!(arg, "list" | "ls") {
            crate::add_live_task(arg.to_string());
        }
        let items = crate::list_live_tasks();
        if items.is_empty() {
            println!(
                "\n{}📋 {}{}",
                DIM,
                tr_en(
                    "Aktif görev yok. Eklemek için `/todo <görev>` yazın veya 2+ adımlı bir iş başlatın.",
                    "No active tasks. Run `/todo <task>` or start a multi-step task."
                ),
                RESET
            );
        } else {
            let done_cnt = items
                .iter()
                .filter(|t| t.status == crate::LiveTaskStatus::Done)
                .count();
            println!(
                "\n{}╭─ 📋 {} [{}/{}] (/todo done <n> · /todo clear){}",
                CYAN,
                tr_en("Canlı Görev Takibi", "Live Task Tracker"),
                done_cnt,
                items.len(),
                RESET
            );
            for (i, item) in items.iter().enumerate() {
                let (icon, color) = match item.status {
                    crate::LiveTaskStatus::Done => ("✓", GREEN),
                    crate::LiveTaskStatus::InProgress => ("▶", YELLOW),
                    crate::LiveTaskStatus::Pending => ("○", DIM),
                };
                println!(
                    "{}│{}  {}#{:<2} {} {}{}",
                    CYAN,
                    RESET,
                    color,
                    i + 1,
                    icon,
                    item.title,
                    RESET
                );
            }
            println!(
                "{}╰──────────────────────────────────────────────────────────────{}",
                CYAN, RESET
            );
        }
        return SlashOutcome::Handled;
    }

    if input == "/think" {
        *show_thinking = !*show_thinking;
        println!(
            "\n{}💭 Düşünce Akışı: {}{}",
            DIM,
            if *show_thinking { "AÇIK" } else { "KAPALI" },
            RESET
        );
        return SlashOutcome::Handled;
    }

    if input == "/recap" || input.starts_with("/recap ") {
        let arg = input.strip_prefix("/recap").unwrap_or("").trim();
        if matches!(arg, "toggle" | "off" | "on") {
            let enabled = match arg {
                "on" => set_idle_recap(true),
                "off" => set_idle_recap(false),
                _ => toggle_idle_recap(),
            };
            println!(
                "\n{}※ {}: {}{}{}",
                MAGENTA,
                tr_en(
                    "Otomatik Boşta Kalma Özeti (Idle Recap)",
                    "Automatic Idle Recap"
                ),
                BOLD,
                if enabled {
                    tr_en(
                        "AÇIK (6s boşta kalınca gösterilir)",
                        "ENABLED (shown after 6s idle)",
                    )
                } else {
                    tr_en("KAPALI", "DISABLED")
                },
                RESET
            );
        } else {
            let cols = term_size().1;
            if let Some(line) = build_session_recap_line(messages, cols) {
                println!("\n{}", line);
            } else {
                let enabled = hud::is_idle_recap_enabled();
                println!(
                    "\n{}※ {}: {}{}{}",
                    MAGENTA,
                    tr_en(
                        "Henüz özetlenecek tur yok · Idle Recap",
                        "No turns yet · Idle Recap"
                    ),
                    BOLD,
                    if enabled {
                        tr_en("AÇIK", "ENABLED")
                    } else {
                        tr_en("KAPALI", "DISABLED")
                    },
                    RESET
                );
            }
        }
        return SlashOutcome::Handled;
    }

    if input == "/compact" || input == "/c" {
        perform_manual_compact(endpoint, messages, None);
        rewrite_session(session_file, messages);
        crate::memory::invalidate_system_prompt_cache();
        return SlashOutcome::Handled;
    }
    if let Some(focus) = input
        .strip_prefix("/compact ")
        .or_else(|| input.strip_prefix("/c "))
    {
        perform_manual_compact(endpoint, messages, Some(focus.trim()));
        rewrite_session(session_file, messages);
        return SlashOutcome::Handled;
    }

    if input == "/resume" || input == "/sessions" {
        if let Some(chosen_path) = interactive_select_session(session_file) {
            let loaded = load_session(&chosen_path);
            if !loaded.is_empty() {
                *session_file = chosen_path;
                *session_name = session_file
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                *messages = loaded;
                refresh_system_prompt(cwd, endpoint, session_file, messages);
                render_session_resume_preview(session_file, messages);
            }
        }
        return SlashOutcome::Handled;
    }

    if input == "/new" || input == "/n" {
        crate::clear_live_tasks();
        crate::clear_queued_messages();
        *session_file = new_session_path();
        *session_name = session_file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let sys = ChatMessage::system(cached_system_prompt(cwd, endpoint.wants_compact_prompt()));
        append_message(session_file, &sys);
        *messages = vec![sys];
        setup_scroll_region();
        render_welcome_header(endpoint, ctx.access, &ctx.default_host, cwd, session_name);
        println!(
            "\n{}✨ Yeni temiz oturum başlatıldı: {}{}{}",
            GREEN, BOLD, session_name, RESET
        );
        return SlashOutcome::Handled;
    }

    if input == "/fork" {
        *session_file = fork_session(messages);
        *session_name = session_file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        println!(
            "\n{}🌿 Mevcut oturum yeni dosyaya klonlanıp dallandırıldı: {}{}{} ({} mesaj korundu)",
            GREEN,
            BOLD,
            session_name,
            RESET,
            messages.len()
        );
        return SlashOutcome::Handled;
    }

    if let Some(rest) = input
        .strip_prefix("/refactor ")
        .or_else(|| input.strip_prefix("/review "))
        .or_else(|| input.strip_prefix("/security "))
        .or_else(|| input.strip_prefix("/test "))
        .or_else(|| input.strip_prefix("/docs "))
    {
        let cmd_head = input.split_whitespace().next().unwrap_or("");
        if let Some(r) = AgentRoleKey::parse_alias(cmd_head.trim_start_matches('/')) {
            *active_role = r;
            push_queued_message(rest.trim().to_string());
            return SlashOutcome::Handled;
        }
    }

    if input == "/role" {
        if let Some(chosen_role) = interactive_select_role(*active_role) {
            *active_role = chosen_role;
            println!(
                "\n{}🎭 {}: {}{}{} {}{}",
                GREEN,
                tr_en("Aktif Spavn Uzmanlık Rolü", "Active Spavn Specialist Role"),
                BOLD,
                active_role.badge(),
                RESET,
                DIM,
                tr_en(
                    "(Hedef görevi aşağıya yazın veya 'devam et' diyerek başlatın)",
                    "(Type your target task below or type 'continue' to start)"
                ),
            );
            print!("{}", RESET);
        }
        return SlashOutcome::Handled;
    }
    if let Some(rest) = input.strip_prefix("/role ") {
        let mut parts = rest.trim().splitn(2, ' ');
        let role_tok = parts.next().unwrap_or("");
        let inline_prompt = parts.next().map(str::trim).filter(|s| !s.is_empty());
        if let Some(r) = AgentRoleKey::parse_alias(role_tok) {
            *active_role = r;
            println!(
                "\n{}🎭 {}: {}{}{}",
                GREEN,
                tr_en("Aktif Spavn Uzmanlık Rolü", "Active Spavn Specialist Role"),
                BOLD,
                active_role.badge(),
                RESET
            );
            if let Some(p) = inline_prompt {
                push_queued_message(p.to_string());
            }
        } else {
            println!(
                "{}Kullanım: /role <auto|coder|reviewer|security|test|docs|refactor> [isteğe bağlı görev]{}",
                YELLOW, RESET
            );
        }
        return SlashOutcome::Handled;
    }

    if input == "/janitor" || input == "/j" {
        render_janitor_panel(cwd);
        return SlashOutcome::Handled;
    }

    if input == "/audit" {
        render_audit_panel(None);
        return SlashOutcome::Handled;
    }
    if let Some(file_flt) = input
        .strip_prefix("/blame ")
        .or_else(|| input.strip_prefix("/audit "))
    {
        render_audit_panel(Some(file_flt.trim()));
        return SlashOutcome::Handled;
    }
    if input == "/blame" {
        render_audit_panel(None);
        return SlashOutcome::Handled;
    }

    if input == "/wiki" || input == "/w" {
        render_repo_wiki(cwd);
        return SlashOutcome::Handled;
    }

    if input == "/pin" || input == "/pins" || input == "/p" {
        render_pins_panel(cwd);
        return SlashOutcome::Handled;
    }
    if let Some(note) = input.strip_prefix("/pin ") {
        let pin = add_pin(cwd, note);
        if !messages.is_empty() {
            messages[0] =
                ChatMessage::system(cached_system_prompt(cwd, endpoint.wants_compact_prompt()));
        }
        println!(
            "\n{}📌 Proje bağlamına sabitlendi (#{}): {}{}{}",
            GREEN, pin.id, BOLD, pin.content, RESET
        );
        return SlashOutcome::Handled;
    }
    if let Some(id_str) = input.strip_prefix("/unpin ") {
        if let Ok(id) = id_str.trim().parse::<usize>() {
            if remove_pin(cwd, id) {
                if !messages.is_empty() {
                    messages[0] = ChatMessage::system(cached_system_prompt(
                        cwd,
                        endpoint.wants_compact_prompt(),
                    ));
                }
                println!("\n{}🗑️  Sabitlenmiş not silindi: #{}{}", GREEN, id, RESET);
            } else {
                println!("\n{}Pin bulunamadı: #{}{}", YELLOW, id, RESET);
            }
        }
        return SlashOutcome::Handled;
    }

    if input == "/goal" || input == "/goals" || input == "/g" {
        render_goals_panel(cwd);
        return SlashOutcome::Handled;
    }
    if let Some(id_str) = input.strip_prefix("/goal done ") {
        if let Ok(id) = id_str.trim().parse::<usize>() {
            if complete_goal(id) {
                if !messages.is_empty() {
                    messages[0] = ChatMessage::system(cached_system_prompt(
                        cwd,
                        endpoint.wants_compact_prompt(),
                    ));
                }
                println!(
                    "\n{}✅ Hedef tamamlandı olarak işaretlendi: #{}{}",
                    GREEN, id, RESET
                );
            } else {
                println!("\n{}Hedef bulunamadı: #{}{}", YELLOW, id, RESET);
            }
        }
        return SlashOutcome::Handled;
    }
    if let Some(obj) = input.strip_prefix("/goal ") {
        let g = add_goal(cwd, obj);
        if !messages.is_empty() {
            messages[0] =
                ChatMessage::system(cached_system_prompt(cwd, endpoint.wants_compact_prompt()));
        }
        println!(
            "\n{}🎯 Yeni proje hedefi eklendi (#{}): {}{}{}",
            GREEN, g.id, BOLD, g.objective, RESET
        );
        return SlashOutcome::Handled;
    }

    if input == "/clear" {
        crate::memory::invalidate_system_prompt_cache();
        let sys = ChatMessage::system(cached_system_prompt(cwd, endpoint.wants_compact_prompt()));
        *messages = vec![sys];
        rewrite_session(session_file, messages);
        setup_scroll_region();
        render_welcome_header(endpoint, ctx.access, &ctx.default_host, cwd, session_name);
        return SlashOutcome::Handled;
    }

    if input == "/update" || input == "/upgrade" || input == "/upd" {
        if crate::update::is_check_disabled() {
            println!(
                "\n{}⚠️  {}{}",
                YELLOW,
                tr_en(
                    "Sürüm denetimi kapalı (`updateCheck: false` veya SPAVN_UPDATE_CHECK).",
                    "Update check is off (`updateCheck: false` or SPAVN_UPDATE_CHECK)."
                ),
                RESET
            );
            return SlashOutcome::Handled;
        }
        match crate::update::check_now() {
            Ok(Some(info)) => render_update_notice(&info),
            Ok(None) => println!(
                "\n{}✅ SPAVN v{} güncel.{} ({}/{})",
                GREEN,
                env!("CARGO_PKG_VERSION"),
                RESET,
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            Err(e) => println!("\n{}⚠️  Sürüm denetimi yapılamadı: {}{}", YELLOW, e, RESET),
        }
        return SlashOutcome::Handled;
    }

    if input == "/help" {
        println!(
            "\n{}╭─ ⚡ SPAVN v0.1 KOMUTLAR, BYOK & MOTOR ÖZELLİKLERİ ──────────────────────────{}",
            CYAN, RESET
        );
        println!(
            "{}│{}  {}/ (veya /menu){}                Etkileşimli yön tuşlu ana komut menüsü",
            CYAN, RESET, BOLD, RESET
        );
        println!("{}│{}  {}/model (/m) · /local{}          İki sütunlu Sağlayıcı (←/→) & Model (↑/↓) seçici", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/bench [model]{}                Aktif/belirtilen modelde TTFT, tok/s ve Tool-Call testi", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/byok (/b){}                    BYOK API Anahtarı & Özel Sağlayıcı (Base URL) yöneticisi", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/login · /provider (/connect){} Sağlayıcıya doğrudan giriş (CLI Auth, Tarayıcı veya Yerel Sunucu)", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/queue (/qlist){}               Mesaj Kuyruğu düzenleme & Canlı Müdahale (Steer/Queue) modu", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/resume (-r) · /new · /fork{}   Geçmiş oturum seçici, yeni oturum ve dallandırma", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/recap (/recap toggle){}        Oturumun son adım özeti (※ recap:) ve 6s boşta kalma özeti", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/role{}                         Spavn uzmanlık rolü (Auto/Coder/Reviewer/Security/Test/Docs/Refactor)", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/plan · /code · \\{}             Faz: 📐 PLAN (salt-okunur) ↔ 💻 CODE (uygulama)", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/janitor (/j){}                 Spavn Janitor kalite kapılarını (check/types/lint/test) koş", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}/audit · /blame <dosya>{}       HMAC zincirli AI aksiyon ve dosya değişim izi", CYAN, RESET, BOLD, RESET);
        println!(
            "{}│{}  {}/diff [/yol] · /commit [mesaj]{} Git değişiklikleri ve onaylı akıllı commit",
            CYAN, RESET, BOLD, RESET
        );
        println!("{}│{}  {}/wiki (/w){}                    Anlık proje modül ve mimari haritası (Repo Wiki)", CYAN, RESET, BOLD, RESET);
        println!(
            "{}│{}  {}/pin <not> · /unpin <id>{}      Proje bağlamına kalıcı mimari kural sabitle",
            CYAN, RESET, BOLD, RESET
        );
        println!(
            "{}│{}  {}/goal <hedef> · /goal done N{}  Proje hedeflerini kaydet ve takip et",
            CYAN, RESET, BOLD, RESET
        );
        println!("{}│{}  {}/update (/upgrade){}           Yeni sürüm denetimi; güncelleme komutunu yazdır", CYAN, RESET, BOLD, RESET);
        println!("{}│{}  {}Shift+Tab · Esc · Ctrl+R{}      Yetki değiştir · İşlemi/Menüyü iptal et · Canlı Reload", CYAN, RESET, BOLD, RESET);
        println!(
            "{}╰────────────────────────────────────────────────────────────────────────────{}",
            CYAN, RESET
        );
        return SlashOutcome::Handled;
    }

    SlashOutcome::NotSlash
}

/// Heuristic Conventional-Commit message derived from the changed file list.
fn suggest_commit_message(changed_files: &[String]) -> String {
    let first = changed_files
        .first()
        .map(|s| s.as_str())
        .unwrap_or("project");
    if changed_files.iter().any(|f| f.contains("test")) {
        format!("test: update test suites ({})", first)
    } else if changed_files.iter().all(|f| f.ends_with(".md")) {
        "docs: update documentation and project rules".to_string()
    } else if changed_files
        .iter()
        .any(|f| f.contains("hud") || f.contains("ui"))
    {
        format!(
            "feat(hud): refine dock layout and visual elements ({})",
            first
        )
    } else if changed_files
        .iter()
        .any(|f| f.contains("tool") || f.contains("guard"))
    {
        format!("fix(tools): harden file and system execution ({})", first)
    } else {
        format!("feat: update {}", first)
    }
}

#[cfg(test)]
mod slash_tests {
    use super::*;
    use std::path::PathBuf;

    fn dummy_endpoint() -> ModelEndpoint {
        ModelEndpoint {
            key: "test".to_string(),
            model: "test-model".to_string(),
            base_url: "http://127.0.0.1:8000/v1".to_string(),
            api_key: String::new(),
            label: "test".to_string(),
            is_local_flag: true,
            has_quota_pool: false,
            configured_context_window: None,
        }
    }

    fn run_cmd(
        input: &str,
        ctx: &mut ToolContext,
        phase: &mut AgentPhase,
        role: &mut AgentRoleKey,
        thinking: &mut bool,
    ) -> SlashOutcome {
        let mut ep = dummy_endpoint();
        let mut quota = String::new();
        let mut prefill = None;
        let mut sess_file = PathBuf::from("/tmp/spavn-slash-test.jsonl");
        let mut sess_name = "test".to_string();
        let mut msgs = Vec::new();
        dispatch_slash_command(
            input,
            "/tmp",
            &mut ep,
            ctx,
            phase,
            role,
            thinking,
            &mut quota,
            &mut prefill,
            &mut sess_file,
            &mut sess_name,
            &mut msgs,
        )
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spavn-stage-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn probe_binary_rejects_a_path_that_cannot_exec() {
        let dir = scratch("probe");
        // A script, not `/bin/sh`: the probe appends `--version`, and `dash --version` (Ubuntu's
        // `/bin/sh`) exits 2, so `/bin/sh` is not a portable "this execs" control.
        let ok = dir.join("ok.sh");
        std::fs::write(&ok, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&ok, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert!(probe_binary(&ok), "a script that exits 0 must probe ok");

        let failing = dir.join("failing.sh");
        std::fs::write(&failing, "#!/bin/sh\nexit 3\n").unwrap();
        std::fs::set_permissions(
            &failing,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        assert!(
            !probe_binary(&failing),
            "a non-zero exit must fail the probe"
        );

        let junk = dir.join("spavn");
        std::fs::write(&junk, "not a binary").unwrap();
        std::fs::set_permissions(&junk, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .unwrap();
        assert!(!probe_binary(&junk), "a non-executable file must fail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The failure this whole path exists for: on Apple Silicon an arm64 binary with no valid
    /// signature is SIGKILLed by `amfid` before `main()`, so the shell only reports `[1] killed`
    /// (137). A probe that returns true for that binary would exec the session into a kill.
    #[cfg(target_os = "macos")]
    #[test]
    fn probe_binary_rejects_a_binary_whose_signature_is_gone() {
        // The probe must see a binary that answers `--version`, so use the real release artifact
        // rather than the test harness (which treats `--version` as a filter and exits non-zero).
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/spavn");
        let codesign = Path::new("/usr/bin/codesign");
        if !real.exists() || !codesign.exists() {
            return;
        }
        let dir = scratch("unsigned");
        let victim = dir.join("spavn");
        if std::fs::copy(&real, &victim).is_err() {
            return;
        }
        assert!(probe_binary(&victim), "the copy must run while signed");
        let removed = std::process::Command::new(codesign)
            .arg("--remove-signature")
            .arg(&victim)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !removed.map(|s| s.success()).unwrap_or(false) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            !probe_binary(&victim),
            "an unsigned binary is killed on exec and must never be exec'd into"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_binary_leaves_a_runnable_binary_on_a_fresh_inode() {
        let dir = scratch("stage");
        // A shell script, not a Mach-O: `codesign -s - -f` rewrites the signature of a Mach-O and
        // changes its size, which would make the byte comparison below test layout, not staging.
        let src = dir.join("src.sh");
        std::fs::write(&src, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&src, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let dest = dir.join("spavn");
        std::fs::write(&dest, "stale").unwrap();
        let before = std::fs::metadata(&dest).unwrap().ino();

        assert!(stage_binary(&src, &dest), "staging should succeed");
        assert_ne!(
            std::fs::metadata(&dest).unwrap().ino(),
            before,
            "rename must replace the inode, not overwrite it"
        );
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            std::fs::read(&src).unwrap(),
            "the staged file must be a full copy of the source"
        );
        assert!(probe_binary(&dest), "the staged file must still exec");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn link_alias_replaces_a_stale_copy_with_a_symlink() {
        let dir = scratch("alias");
        let bin = dir.join("spavn");
        std::fs::write(&bin, "x").unwrap();
        let alias = dir.join("spvn");
        std::fs::write(&alias, "stale copy").unwrap();
        link_alias(&bin);
        let meta = std::fs::symlink_metadata(&alias).unwrap();
        assert!(meta.file_type().is_symlink(), "spvn must become a symlink");
        assert_eq!(std::fs::read_link(&alias).unwrap(), bin);
        let _ = std::fs::remove_dir_all(&dir);
    }

    use std::os::unix::fs::MetadataExt;

    #[test]
    fn non_slash_input_returns_not_slash() {
        let mut ctx = ToolContext::new(
            AccessMode::Safe,
            "local".to_string(),
            PathBuf::from("/tmp"),
            AgentPhase::Code,
        );
        let mut phase = AgentPhase::Code;
        let mut role = AgentRoleKey::Auto;
        let mut thinking = true;
        assert!(matches!(
            run_cmd(
                "hello world",
                &mut ctx,
                &mut phase,
                &mut role,
                &mut thinking
            ),
            SlashOutcome::NotSlash
        ));
        assert!(matches!(
            run_cmd(
                "/unknown_command_xyz",
                &mut ctx,
                &mut phase,
                &mut role,
                &mut thinking
            ),
            SlashOutcome::NotSlash
        ));
    }

    #[test]
    fn plan_and_code_commands_toggle_phase_and_sync_ctx() {
        let mut ctx = ToolContext::new(
            AccessMode::Safe,
            "local".to_string(),
            PathBuf::from("/tmp"),
            AgentPhase::Code,
        );
        let mut phase = AgentPhase::Code;
        let mut role = AgentRoleKey::Auto;
        let mut thinking = true;

        assert!(matches!(
            run_cmd("/plan", &mut ctx, &mut phase, &mut role, &mut thinking),
            SlashOutcome::Handled
        ));
        assert_eq!(phase, AgentPhase::Plan);
        assert_eq!(ctx.phase, AgentPhase::Plan);

        assert!(matches!(
            run_cmd("/code", &mut ctx, &mut phase, &mut role, &mut thinking),
            SlashOutcome::Handled
        ));
        assert_eq!(phase, AgentPhase::Code);
        assert_eq!(ctx.phase, AgentPhase::Code);
    }

    #[test]
    fn access_command_updates_context_mode() {
        let mut ctx = ToolContext::new(
            AccessMode::Safe,
            "local".to_string(),
            PathBuf::from("/tmp"),
            AgentPhase::Code,
        );
        let mut phase = AgentPhase::Code;
        let mut role = AgentRoleKey::Auto;
        let mut thinking = true;

        assert!(matches!(
            run_cmd(
                "/access auto",
                &mut ctx,
                &mut phase,
                &mut role,
                &mut thinking
            ),
            SlashOutcome::Handled
        ));
        assert_eq!(ctx.access, AccessMode::Auto);

        assert!(matches!(
            run_cmd(
                "/access sovereign",
                &mut ctx,
                &mut phase,
                &mut role,
                &mut thinking
            ),
            SlashOutcome::Handled
        ));
        assert_eq!(ctx.access, AccessMode::Sovereign);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(local: bool) -> ModelEndpoint {
        ModelEndpoint {
            key: "k".into(),
            model: "m".into(),
            base_url: "http://x/v1".into(),
            api_key: "k".into(),
            label: "l".into(),
            is_local_flag: local,
            has_quota_pool: false,
            configured_context_window: None,
        }
    }

    #[test]
    fn model_switch_rebuilds_system_prompt_for_locality() {
        let path = std::env::temp_dir().join(format!("spavn-refresh-{}.jsonl", std::process::id()));
        let mut msgs = vec![ChatMessage::system("old"), ChatMessage::user("hi")];
        refresh_system_prompt(".", &ep(true), &path, &mut msgs);
        let compact = msgs[0].content.clone().unwrap();
        assert!(compact.contains("# Rules") && !compact.contains("Engineering Constitution"));
        refresh_system_prompt(".", &ep(false), &path, &mut msgs);
        assert!(msgs[0]
            .content
            .as_ref()
            .unwrap()
            .contains("Engineering Constitution"));
        assert_eq!(msgs.len(), 2);
        let _ = std::fs::remove_file(path);
    }
}
