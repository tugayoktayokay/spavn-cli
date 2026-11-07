mod turn;
use turn::*;

use spavn::config::{load_user_config, tr_en};
use spavn::hud;
use spavn::hud::{
    draw_bottom_dock, fetch_quick_quota_line, interactive_select_session,
    interactive_select_slash_command, read_bottom_dock_input, render_welcome_header,
    reset_scroll_region, setup_scroll_region, PromptEvent,
};
use spavn::memory::{cached_system_prompt, current_workdir};
use spavn::router::{resolve_endpoint, ChatMessage};
use spavn::session::{
    append_message, fork_session, latest_session_path, load_session, new_session_path,
    rewrite_session, sessions_dir,
};
use spavn::skills::expand_inline_skills;
use spavn::slash_commands::{dispatch_slash_command, SlashOutcome};
use spavn::spavn_engine::{
    dispatch_role_from_prompt, record_knowledge_artifact, retrieve_relevant_artifacts, AgentPhase,
    AgentRoleKey,
};
use spavn::state::{install_sigint_handler, push_queued_message, queued_message_count};
use spavn::tools::{AccessMode, ToolContext};
use std::path::PathBuf;

const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

fn main() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        reset_scroll_region();
        hud::restore_window_title();
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
                    if let Some(m) = AccessMode::parse_alias(&args[i + 1]) {
                        access_mode = m;
                    }
                    i += 2;
                    continue;
                }
            }
            "-P" | "--phase" => {
                if i + 1 < args.len() {
                    if let Some(ph) = AgentPhase::parse_alias(&args[i + 1]) {
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
            "-p" | "--prompt" => {
                if i + 1 < args.len() {
                    prompt_parts.push(args[i + 1].clone());
                    i += 2;
                    continue;
                }
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
            "update" | "upgrade" if i == 0 => {
                // `load_user_config()` upgrade çekirdeğinde tetiklenip dili config.json'dan
                // ezer; headless dalında dil OS'tan gelir, bu yüzden çıktıdan önce kesiyoruz.
                let outcome = spavn::update::upgrade_installed_binary();
                spavn::config::set_lang_en(spavn::config::detect_os_lang_en());
                match outcome {
                    spavn::update::UpgradeOutcome::UpToDate => println!(
                        "spavn v{} {}",
                        env!("CARGO_PKG_VERSION"),
                        tr_en(
                            "güncel — daha yeni sürüm yok.",
                            "up to date — no newer release."
                        )
                    ),
                    spavn::update::UpgradeOutcome::NoAsset(latest) => {
                        println!(
                            "v{latest} {}. ({}/{})",
                            tr_en(
                                "yayında ama bu platform için hazır ikili yok",
                                "is out, but no prebuilt binary exists for this platform yet"
                            ),
                            std::env::consts::OS,
                            std::env::consts::ARCH,
                        );
                        println!(
                            "{}: curl -fsSL https://raw.githubusercontent.com/{}/main/install.sh | sh",
                            tr_en("Elle güncelle", "Update manually"),
                            spavn::update::repo_slug().unwrap_or("tugayoktayokay/spavn-cli"),
                        );
                    }
                    spavn::update::UpgradeOutcome::Staged { latest, bin } => println!(
                        "spavn v{} → v{latest} {}. {} — {}.",
                        env!("CARGO_PKG_VERSION"),
                        tr_en("kuruldu", "installed"),
                        bin.display(),
                        tr_en(
                            "yeni sürümü bir sonraki başlatışta kullan",
                            "the new version will be used on next start",
                        ),
                    ),
                    spavn::update::UpgradeOutcome::CheckError(e) => {
                        eprintln!("{}: {e}", tr_en("Güncelleme başarısız", "Update failed"));
                        std::process::exit(1);
                    }
                }
                return;
            }
            "-h" | "--help" => {
                println!(
                    "SPAVN v{} ({}/{}) — Native Rust Coding Agent & Harness\n\
                     Usage: spavn [options] [prompt...]\n\n\
                     Run without arguments to open the interactive TUI. A prompt is executed\n\
                     headless, then the TUI opens only when -i/--interactive is given.\n\n\
                     Options:\n\
                     \x20 -m, --model <key>           Select model (default: {})\n\
                     \x20 -a, --access <mode>         Access mode: safe | auto | sovereign\n\
                     \x20 -P, --phase <phase>         Agent phase: plan | code\n\
                     \x20 -H, --host <host>           Default target host (default: local)\n\
                     \x20 -s, --session <id|fork:id>  Resume or fork a specific session\n\
                     \x20 -c, --continue              Continue latest session\n\
                     \x20 -r, --resume                Interactive session picker\n\
                     \x20 -i, --interactive           Start interactive TUI after initial prompt\n\
                     \x20 -p, --prompt <text>         Explicit prompt (same as a positional argument)\n\
                     \x20 -t, --think                 Toggle thinking stream display\n\
                     \x20 -q, --quiet                 Quiet/headless output mode\n\
                     \x20 -j, --json                  Structured JSON output mode\n\
                     \x20 -V, --version               Print version and exit\n\
                     \x20 -h, --help                  Print this help message\n\n\
                     Examples:\n\
                     \x20 spavn                                  Open the interactive TUI\n\
                     \x20 spavn \"explain src/router.rs\"           One headless turn, then exit\n\
                     \x20 spavn -a safe -m local \"fix the tests\"  Explicit mode, model, prompt\n\
                     \x20 spavn -c                               Continue the latest session\n\
                     \x20 spavn -s fork:session-17938104.jsonl    Fork a session and keep history\n\n\
                     Paths:\n\
                     \x20 config    ~/.spavn/config.json      (override the root with SPAVN_HOME)\n\
                     \x20 sessions  ~/.spavn/sessions/         resumable JSONL, mode 0600\n\
                     \x20 audit     ~/.spavn/audit/            HMAC-chained action ledger\n\n\
                     Environment:\n\
                     \x20 SPAVN_MODEL, SPAVN_HOME, SPAVN_LANG, SPAVN_SHELL\n\
                     \x20 SPAVN_UPDATE_CHECK=0 disables the release check\n\
                     \x20 SPAVN_LOCAL_MODELS_URL, SPAVN_LOCAL_BASE_URL, SPAVN_FORCE_ANTHROPIC\n\
                     \x20 OPENAI_API_KEY, ANTHROPIC_API_KEY, GEMINI_API_KEY, OPENROUTER_API_KEY,\n\
                     \x20 DEEPSEEK_API_KEY, GROQ_API_KEY, XAI_API_KEY, SPAVN_API_KEY, SPAVN_BASE_URL\n\n\
                     Files:\n\
                     \x20 ~/.spavn/PROFILE.md               user profile injected into the prompt\n\
                     \x20 ~/.spavn/skills/                  on-demand domain instructions\n\
                     \x20 ~/.spavn/tool-output/             spilled tool output, newest 40 kept\n\
                     \x20 ~/.spavn/audit.key                HMAC key for the audit chain\n\n\
                     Inside the TUI: /help lists every slash command, / lists the menu.\n\
                     /update checks for a newer release and prints the upgrade command.\n\
                     Docs: https://github.com/tugayoktayokay/spavn-cli#readme",
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
            let sys =
                ChatMessage::system(cached_system_prompt(&cwd, endpoint.wants_compact_prompt()));
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
                let sys = ChatMessage::system(cached_system_prompt(
                    &cwd,
                    endpoint.wants_compact_prompt(),
                ));
                append_message(&p, &sys);
                (p, vec![sys])
            }
        } else {
            let p = new_session_path();
            let sys =
                ChatMessage::system(cached_system_prompt(&cwd, endpoint.wants_compact_prompt()));
            append_message(&p, &sys);
            (p, vec![sys])
        }
    } else {
        let p = new_session_path();
        let sys = ChatMessage::system(cached_system_prompt(&cwd, endpoint.wants_compact_prompt()));
        append_message(&p, &sys);
        (p, vec![sys])
    };

    if let Some(first) = messages.first_mut().filter(|m| m.role == "system") {
        *first = ChatMessage::system(cached_system_prompt(&cwd, endpoint.wants_compact_prompt()));
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
        hud::set_window_topic(&raw_text);
        let (mut expanded_text, activated_skills) = expand_inline_skills(&raw_text);
        if !activated_skills.is_empty() && !quiet {
            println!(
                "{}🧩 {} @{}{}",
                GREEN,
                tr_en("Aktif Yetenek(ler) Yüklendi", "Active Skill(s) Loaded"),
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
                eprintln!("{}: {}", tr_en("Hata", "Error"), err);
            } else {
                eprintln!("{}✗ {}: {}{}", RED, tr_en("Hata", "Error"), err, RESET);
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
    // Read the cache before the check can rewrite it: the banner shows what was already known, and
    // anything this run discovers arrives later through the input loop instead.
    let known_update = spavn::update::cached_notice();
    spavn::update::spawn_background_check();
    render_welcome_header(
        &endpoint,
        ctx.access,
        &ctx.default_host,
        &cwd,
        &session_name,
    );
    if let Some(info) = known_update {
        hud::render_update_notice(&info);
    }

    if !endpoint.is_local()
        && !endpoint.has_quota_pool
        && (endpoint.api_key.is_empty() || endpoint.api_key == "local")
    {
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
                *first = ChatMessage::system(cached_system_prompt(
                    &cwd,
                    endpoint.wants_compact_prompt(),
                ));
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
                    *first = ChatMessage::system(cached_system_prompt(
                        &cwd,
                        endpoint.wants_compact_prompt(),
                    ));
                }
                hud::render_session_resume_preview(&session_file, &messages);
            }
        }
    } else if resume_last {
        hud::render_session_resume_preview(&session_file, &messages);
    }

    // A resumed session already has a topic in its last user turn; a fresh one keeps the
    // brand-only title until the first prompt arrives.
    if let Some(topic) = messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_deref())
    {
        hud::set_window_topic(topic);
    }

    loop {
        let max_win = endpoint.context_window_tokens();
        let cur_view = compact_messages_for_model(&messages, endpoint.is_local(), max_win);
        hud::set_context_window_usage(hud::estimate_messages_tokens(&cur_view), max_win);
        // Automatically pop next queued message if available and no edit prefill is pending!
        let mut input = if prefill_input.is_none() {
            if let Some(queued_msg) = spavn::state::pop_next_queued_message() {
                println!(
                    "\n{}📥 [Auto-running from queue ({} remaining)]: {}{}{}",
                    GREEN,
                    queued_message_count(),
                    BOLD,
                    queued_msg,
                    RESET
                );
                queued_msg
            } else {
                let draft = hud::take_spinner_draft();
                let ev = read_bottom_dock_input(
                    &endpoint,
                    &mut ctx.access,
                    &active_phase,
                    &ctx.default_host,
                    &quota_badge,
                    &cmd_history,
                    draft.as_deref(),
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
                            "\n{}{} — phase changed{}",
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
                        "\n{}{} — phase changed{}",
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
        let dragged_imgs = spavn::router::extract_dragged_images_info(&input);
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
        // Before the auto title lands the newest user line stays the topic; after it only the
        // title is, so a follow-up prompt never rewrites it (OMP `session/title.ts` behaviour).
        if let Some(title) = spavn::session_title::session_title() {
            hud::set_window_topic(&title);
        } else {
            if messages.is_empty() {
                spavn::session_title::spawn_auto_title(&endpoint, &input);
            }
            hud::set_window_topic(&input);
        }
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
        hud::print_copy_hint();

        if ctx.undo_stack.len() > undo_before {
            let touched: Vec<String> = ctx.undo_stack[undo_before..]
                .iter()
                .map(|u| u.target.clone())
                .collect();
            record_knowledge_artifact(&cwd, &touched, &input);
        }
        draw_bottom_dock(&endpoint, ctx.access, &ctx.default_host, &quota_badge, "");
    }

    hud::restore_window_title();
}
