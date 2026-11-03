mod config;
mod guard;
mod http;
mod hud;
mod memory;
mod router;
mod session;
mod skills;
mod spavn_engine;
mod tools;

use config::{is_en, load_user_config, tr_en};
use hud::{
    draw_bottom_dock, fetch_quick_quota_line, interactive_byok_manager, interactive_queue_manager,
    interactive_select_access, interactive_select_host, interactive_select_language,
    interactive_select_model, interactive_select_role, interactive_select_session,
    interactive_select_skill, interactive_select_slash_command, read_bottom_dock_input,
    render_assistant_footer, render_assistant_header, render_fleet_radar, render_full_quota_card,
    render_welcome_header, reset_scroll_region, setup_scroll_region, term_size,
    truncate_ansi_to_width, LiveSpinner, MarkdownStreamRenderer, PromptEvent, QueueAction,
};
use memory::{build_system_prompt, current_workdir};
use router::{resolve_endpoint, stream_chat_completion, ChatMessage, ModelEndpoint};
use session::{
    append_message, fork_session, latest_session_path, load_session, new_session_path,
    rewrite_session,
};
use skills::{
    discover_skills, expand_inline_skills, load_skill_content, remove_custom_skill,
    render_skills_panel, save_custom_skill,
};
use spavn_engine::{
    add_goal, add_pin, complete_goal, dispatch_role_from_prompt, record_knowledge_artifact,
    remove_pin, render_audit_panel, render_goals_panel, render_janitor_panel, render_pins_panel,
    render_repo_wiki, retrieve_relevant_artifacts, AgentRoleKey,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tools::{execute_tool, tool_definitions, AccessMode, ToolContext};

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

pub fn push_queued_message(msg: String) {
    if let Ok(mut q) = PROMPT_QUEUE.lock() {
        q.push_back(msg);
    }
}

pub fn pop_next_queued_message() -> Option<String> {
    PROMPT_QUEUE.lock().ok()?.pop_front()
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

extern "C" fn handle_sigint(_sig: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

extern "C" fn handle_sigwinch(_sig: i32) {
    RESIZED.store(true, Ordering::SeqCst);
}

#[link(name = "c")]
unsafe extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
}

pub fn install_sigint_handler() {
    unsafe {
        let _ = signal(2, handle_sigint);
        let _ = signal(28, handle_sigwinch);
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

fn compact_messages_for_model(messages: &[ChatMessage], is_local: bool) -> Vec<ChatMessage> {
    let keep_recent = if is_local { 6 } else { 14 };
    let len = messages.len();
    let cutoff = len.saturating_sub(keep_recent);

    let last_user_idx = messages
        .iter()
        .rposition(|m| m.role == "user")
        .unwrap_or(len);

    messages
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let mut cloned = m.clone();

            if idx < last_user_idx {
                cloned.reasoning_content = None;
            }

            if m.role == "user" && idx != last_user_idx {
                if let Some(ref content) = cloned.content {
                    if content.contains("<active_skill") {
                        cloned.content = Some(evict_old_skill_blocks(content));
                    }
                }
            }
            if idx > 0 && idx < cutoff && m.role == "tool" {
                if let Some(ref content) = m.content {
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

fn perform_manual_compact(
    _endpoint: &ModelEndpoint,
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
    for m in messages.iter_mut().skip(1) {
        if let Some(ref c) = m.content {
            if c.contains("<active_skill") {
                m.content = Some(evict_old_skill_blocks(c));
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
        let tail_start = messages.len().saturating_sub(4);
        let mut digest_lines = Vec::new();
        for m in &messages[1..tail_start] {
            if m.role == "user" || m.role == "assistant" {
                if let Some(ref c) = m.content {
                    let first_line = c.lines().next().unwrap_or("").trim();
                    if !first_line.is_empty() {
                        let snippet: String = first_line.chars().take(120).collect();
                        digest_lines.push(format!("- [{}]: {}", m.role, snippet));
                    }
                }
            }
        }
        let focus_note = focus.map(|f| format!(" (Odak: {})", f)).unwrap_or_default();
        let mut new_msgs = vec![sys];
        if !digest_lines.is_empty() {
            new_msgs.push(ChatMessage::user(format!(
                "[SPAVN Auto-Compact Özet{}]:\n{}",
                focus_note,
                digest_lines.join("\n")
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

fn run_agent_turn(
    endpoint: &ModelEndpoint,
    ctx: &mut ToolContext,
    messages: &mut Vec<ChatMessage>,
    session_file: &PathBuf,
    show_thinking: bool,
    quota_badge: Option<&str>,
) -> Result<(), String> {
    clear_interrupted();
    hud::set_sticky_dock_status(
        endpoint,
        ctx.access,
        &ctx.default_host,
        quota_badge.unwrap_or("aktif"),
    );
    let tools_spec = tool_definitions();
    let mut last_tool_sig = String::new();
    let mut repeat_count = 0usize;
    let mut unverified_edits = false;
    let mut verification_nudged = false;
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
            println!("\n{}⚡ [İşlem Esc/Ctrl+C ile durduruldu]{}", YELLOW, RESET);
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
            println!(
                "\n{}⚡ [SPAVN Hibrit Escalation: Yerel model döngüye girdi -> Bu adım {} ile çözülüyor...]{}",
                MAGENTA, cfg.fallback_cloud_model, RESET
            );
            resolve_endpoint(&cfg.fallback_cloud_model)
        } else {
            endpoint.clone()
        };

        let view_messages = compact_messages_for_model(messages, step_endpoint.is_local());
        hud::set_context_window_usage(
            hud::estimate_messages_tokens(&view_messages),
            step_endpoint.context_window_tokens(),
        );
        let mut spinner = LiveSpinner::start(&format!(
            "{} {}",
            step_endpoint.label,
            tr_en("düşünülüyor", "thinking")
        ));
        let content_started = std::cell::Cell::new(false);
        let content_chars = std::cell::Cell::new(0usize);
        let md_renderer = RefCell::new(MarkdownStreamRenderer::new());

        let result = match stream_chat_completion(
            &step_endpoint,
            &view_messages,
            &tools_spec,
            |think_delta| {
                hud::stream_thinking_delta(think_delta, show_thinking);
            },
            |content_delta| {
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
                hud::close_thinking_box_if_open();
                spinner.stop();
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
                    println!(
                        "\n{}⚠️  {} ({}) -> {} {}{}",
                        YELLOW,
                        tr_en("Yerel model hatası", "Local model error"),
                        err,
                        cfg.fallback_cloud_model,
                        tr_en(
                            "modeline otomatik geçiliyor...",
                            "failing over automatically..."
                        ),
                        RESET
                    );
                    let fallback_ep = resolve_endpoint(&cfg.fallback_cloud_model);
                    let mut fb_spinner = LiveSpinner::start(&format!(
                        "{} {}",
                        fallback_ep.label,
                        tr_en("düşünülüyor", "thinking")
                    ));
                    let fb_res = stream_chat_completion(
                        &fallback_ep,
                        &view_messages,
                        &tools_spec,
                        |think_delta| {
                            hud::stream_thinking_delta(think_delta, show_thinking);
                        },
                        |c| {
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
                    hud::close_thinking_box_if_open();
                    fb_spinner.stop();
                    fb_res?
                } else {
                    return Err(err);
                }
            }
        };

        if content_started.get() {
            md_renderer.borrow_mut().finish();
        }
        hud::close_thinking_box_if_open();
        spinner.stop();

        let assistant_msg = result.message;
        append_message(session_file, &assistant_msg);
        messages.push(assistant_msg.clone());
        let est_after = hud::estimate_messages_tokens(messages);
        let actual_used = (result.usage.total_tokens as usize).max(est_after);
        hud::set_context_window_usage(actual_used, step_endpoint.context_window_tokens());

        let secs = (result.elapsed_ms as f64) / 1000.0;
        let tok_s = if secs > 0.0 && result.usage.completion_tokens > 0 {
            (result.usage.completion_tokens as f64) / secs
        } else {
            0.0
        };

        if let Some(tool_calls) = assistant_msg.tool_calls {
            for tc in tool_calls {
                let tool_name = tc.function.name.trim().to_string();
                let sig = format!("{}:{}", tool_name, tc.function.arguments.trim());
                if sig == last_tool_sig {
                    repeat_count += 1;
                } else {
                    last_tool_sig = sig;
                    repeat_count = 0;
                }

                if matches!(
                    tool_name.as_str(),
                    "edit" | "edit_file" | "replace" | "write" | "write_file"
                ) {
                    unverified_edits = true;
                } else if matches!(
                    tool_name.as_str(),
                    "bash" | "run_command" | "shell" | "exec"
                ) {
                    unverified_edits = false;
                }

                let summary =
                    summarize_tool_args(&tool_name, &tc.function.arguments, &ctx.default_host);
                println!(
                    "{}╭─ ⚡ {}{} {}{}{}",
                    YELLOW, BOLD, tool_name, RESET, DIM, summary
                );
                let mut tool_spinner =
                    LiveSpinner::start(&format!("⚡ {} çalıştırılıyor", tool_name));
                let mut tool_output = execute_tool(ctx, &tool_name, &tc.function.arguments);
                tool_spinner.stop();
                if repeat_count >= 2 {
                    tool_output.push_str(&format!(
                        "\n\n[SPAVN LOOP GUARD (count={})]: You called `{}` with identical arguments {} times consecutively. Do NOT repeat the same call; change your approach or proceed directly to the next step.",
                        repeat_count + 1,
                        tool_name,
                        repeat_count + 1
                    ));
                }

                let preview_line = tool_output
                    .lines()
                    .next()
                    .unwrap_or("(tamamlandı)")
                    .chars()
                    .take(80)
                    .collect::<String>();
                println!(
                    "{}╰─ ↳ {}{} ({} satır){}",
                    YELLOW,
                    DIM,
                    preview_line,
                    tool_output.lines().count().max(1),
                    RESET
                );

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
            println!(
                "\n{}⚡ [SPAVN Turn Recovery: Çıktı token sınırına ulaştı ({}/3) -> Kaldığı yerden devam ediliyor...]{}",
                YELLOW, length_stop_retries, RESET
            );
            let cont_msg = ChatMessage::user(
                "[SPAVN LENGTH-STOP RECOVERY]: Your previous response hit the output token limit (`finish_reason=length`). Continue immediately from the exact point where you stopped without repeating prior output.",
            );
            append_message(session_file, &cont_msg);
            messages.push(cont_msg);
            continue;
        }

        // 2. Empty Assistant Stop Recovery (OMP `empty-stop-retry`)
        let text_trimmed = assistant_msg.content.as_deref().unwrap_or("").trim();
        if text_trimmed.is_empty() && empty_stop_retries < 2 {
            empty_stop_retries += 1;
            let empty_nudge = ChatMessage::user(
                "[SPAVN EMPTY-STOP RECOVERY]: You returned an empty response with no tool call and no text. Call the next required tool or provide the final answer now.",
            );
            append_message(session_file, &empty_nudge);
            messages.push(empty_nudge);
            continue;
        }

        // 3. Premature Action-Announcement Stop Recovery (OMP `unexpected-stop-retry`)
        let lower_tail: String = text_trimmed
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let looks_like_premature_stop = !text_trimmed.contains('?')
            && (lower_tail.ends_with(':')
                || lower_tail.contains("şimdi ")
                    && (lower_tail.contains("yapacağım")
                        || lower_tail.contains("düzenleyeceğim")
                        || lower_tail.contains("okuyacağım")
                        || lower_tail.contains("inceleyeceğim"))
                || lower_tail.contains("let me ")
                || lower_tail.contains("i will now ")
                || lower_tail.contains("i'll now "));
        if looks_like_premature_stop && premature_stop_retries < 2 {
            premature_stop_retries += 1;
            println!(
                "\n{}⚡ [SPAVN Turn Recovery: Yarım kalan aksiyon tespit edildi ({}/2) -> Araç çağrısı tetikleniyor...]{}",
                YELLOW, premature_stop_retries, RESET
            );
            let u_nudge = ChatMessage::user(
                "[SPAVN UNEXPECTED-STOP RECOVERY]: You announced an action you were about to take but stopped without calling a tool. Call the concrete tool now.",
            );
            append_message(session_file, &u_nudge);
            messages.push(u_nudge);
            continue;
        }

        // 4. Autonomous Verification Gate after file edits
        if unverified_edits && !verification_nudged {
            verification_nudged = true;
            unverified_edits = false;
            println!(
                "{}⚡ [SPAVN Otonom Doğrulama Kapısı: Dosya değişikliği sonrası derleme/test kontrolü tetikleniyor...]{}",
                YELLOW, RESET
            );
            let nudge = ChatMessage::user(
                "[SPAVN VERIFICATION GATE]: You modified file(s) via edit/write without running a verification check (`bash` build/test). Run a verification command now, then report the final result.",
            );
            append_message(session_file, &nudge);
            messages.push(nudge);
            continue;
        }

        if content_started.get() || result.usage.total_tokens > 0 {
            render_assistant_footer(
                &step_endpoint.key,
                ctx.access.short_badge(),
                result.ttft_ms,
                secs,
                tok_s,
            );
        }

        // 5. OMP Pre-Yield Queue Drain (`getFollowUpMessages` before `break`):
        // If the user queued a follow-up message while the turn was running, continue immediately!
        if let Some(next_follow_up) = pop_next_queued_message() {
            if next_follow_up.trim().starts_with('/') {
                if let Ok(mut q) = PROMPT_QUEUE.lock() {
                    q.push_front(next_follow_up);
                }
                break;
            }
            println!(
                "\n{}📥 {}: {}{}{}",
                CYAN,
                tr_en(
                    "[Sıradaki Kuyruk Mesajı Otomatik Başlatılıyor]",
                    "[Starting Next Queued Message Automatically]"
                ),
                BOLD,
                next_follow_up,
                RESET
            );
            let q_msg = ChatMessage::user(next_follow_up);
            append_message(session_file, &q_msg);
            messages.push(q_msg);
            continue;
        }

        break;
    }

    Ok(())
}

fn render_session_resume_preview(session_path: &PathBuf, messages: &[ChatMessage]) {
    let fname = session_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
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
    let recent: Vec<&ChatMessage> = text_msgs
        .iter()
        .rev()
        .take(2)
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let (_, cols) = term_size();
    let full_w = cols.saturating_sub(2).max(6);
    println!(
        "\n{}╭─ 🗂️  {}: {}{} ({} {}){}",
        CYAN,
        tr_en("Oturum Yüklendi", "Session Loaded"),
        BOLD,
        fname,
        text_msgs.len(),
        tr_en("mesaj", "messages"),
        RESET
    );
    if recent.is_empty() {
        println!(
            "{}╰─{} {}Henüz mesaj geçmişi yok.{}",
            CYAN, RESET, DIM, RESET
        );
    } else {
        let r_len = recent.len();
        for (i, m) in recent.into_iter().enumerate() {
            let spine = if i + 1 == r_len { "╰─" } else { "│ " };
            let role_tag = if m.role == "user" {
                format!("{}👤 Siz{}", BOLD, RESET)
            } else {
                format!("{}⚡ SPAVN{}", GREEN, RESET)
            };
            let raw = m.content.as_deref().unwrap_or("");
            let clean = hud::strip_internal_tags_for_display(raw);
            let one_line: String = clean
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let row_raw = format!(
                "{}{}{} {}: {}{}{}",
                CYAN, spine, RESET, role_tag, DIM, one_line, RESET
            );
            let (trunc_row, _) = truncate_ansi_to_width(&row_raw, full_w);
            println!("{}", trunc_row);
        }
    }
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
    let mut interactive_resume = false;
    let mut show_thinking = true;
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
            "-H" | "--host" => {
                if i + 1 < args.len() {
                    default_host = args[i + 1].clone();
                    i += 2;
                    continue;
                }
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
            "-h" | "--help" => {
                println!("Usage: spavn [-m <model>] [-a safe|auto|sovereign] [--host <host>] [-c|--continue] [-r|--resume] [prompt...]");
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
    let mut ctx = ToolContext::new(access_mode, default_host, PathBuf::from(&cwd));

    let (mut session_file, mut messages) = if resume_last {
        if let Some(path) = latest_session_path() {
            let loaded = load_session(&path);
            if !loaded.is_empty() {
                (path, loaded)
            } else {
                let p = new_session_path();
                let sys = ChatMessage::system(build_system_prompt(&cwd));
                append_message(&p, &sys);
                (p, vec![sys])
            }
        } else {
            let p = new_session_path();
            let sys = ChatMessage::system(build_system_prompt(&cwd));
            append_message(&p, &sys);
            (p, vec![sys])
        }
    } else {
        let p = new_session_path();
        let sys = ChatMessage::system(build_system_prompt(&cwd));
        append_message(&p, &sys);
        (p, vec![sys])
    };

    if !prompt_parts.is_empty() {
        let raw_text = prompt_parts.join(" ");
        let (expanded_text, activated_skills) = expand_inline_skills(&raw_text);
        if !activated_skills.is_empty() {
            println!(
                "{}🧩 Aktif Yetenek(ler) Yüklendi: @{}{}",
                GREEN,
                activated_skills.join(", @"),
                RESET
            );
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
        ) {
            eprintln!("{}✗ Hata: {}{}", RED, err, RESET);
            std::process::exit(1);
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
                render_session_resume_preview(&session_file, &messages);
            }
        }
    } else if resume_last {
        render_session_resume_preview(&session_file, &messages);
    }

    loop {
        let cur_view = compact_messages_for_model(&messages, endpoint.is_local());
        hud::set_context_window_usage(
            hud::estimate_messages_tokens(&cur_view),
            endpoint.context_window_tokens(),
        );
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

        if matches!(
            input.as_str(),
            "/exit" | "/quit" | "/x" | ":q" | ":wq" | "q!" | "exit" | "quit"
        ) {
            reset_scroll_region();
            break;
        }

        if input == "/reload" || input == "/r" || input == "/reload bin" || input == "/rb" {
            if !messages.is_empty() {
                messages[0] = ChatMessage::system(build_system_prompt(&cwd));
                rewrite_session(&session_file, &messages);
            }
            reset_scroll_region();
            let skill_count = discover_skills().len();
            println!(
                "\n{}🔄 [SPAVN Canlı Reload]: Binary, sistem çekirdeği ve {} yetenek (skill) oturum korunarak yenileniyor...{}",
                GREEN, skill_count, RESET
            );
            let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("spavn"));
            let access_str = match ctx.access {
                AccessMode::Safe => "safe",
                AccessMode::Auto => "auto",
                AccessMode::Sovereign => "sovereign",
            };
            let _ = std::process::Command::new(&exe_path)
                .args([
                    "-c",
                    "-m",
                    &endpoint.key,
                    "-a",
                    access_str,
                    "--host",
                    &ctx.default_host,
                ])
                .exec();
            break;
        }

        if input == "/byok" || input == "/b" || input == "/keys" {
            if let Some(chosen_model) = interactive_byok_manager() {
                endpoint = resolve_endpoint(&chosen_model);
                println!(
                    "\n{}🧠 Aktif Model Seçildi: {}{}{}",
                    GREEN, BOLD, endpoint.label, RESET
                );
            }
            continue;
        }

        if matches!(input.as_str(), "/lang" | "/language" | "/dil") {
            if let Some(chosen_lang) = interactive_select_language() {
                if !messages.is_empty() {
                    messages[0] = ChatMessage::system(build_system_prompt(&cwd));
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
            continue;
        }
        if input == "/en" || input == "/tr" {
            let mut cfg = load_user_config();
            cfg.language = input.trim_start_matches('/').to_string();
            let _ = config::save_user_config(&cfg);
            if !messages.is_empty() {
                messages[0] = ChatMessage::system(build_system_prompt(&cwd));
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
            continue;
        }
        if input == "/queue" || input == "/qlist" || input == "/qedit" {
            if let QueueAction::EditMessage(msg_to_edit) = interactive_queue_manager() {
                prefill_input = Some(msg_to_edit);
            }
            continue;
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
            continue;
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
                    append_message(&session_file, &skill_msg);
                    messages.push(skill_msg);
                    println!(
                        "\n{}🧩 Yetenek aktif oturuma yüklendi: @{}{} ({})",
                        GREEN, BOLD, entry.name, entry.summary
                    );
                }
            } else {
                render_skills_panel();
            }
            continue;
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
            continue;
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
            continue;
        }
        if let Some(rest) = input.strip_prefix("/skill rm ") {
            match remove_custom_skill(rest) {
                Ok(()) => println!("\n{}🗑️  Yetenek silindi: @{}{}", GREEN, rest.trim(), RESET),
                Err(e) => println!("\n{}✗ {}{}", YELLOW, e, RESET),
            }
            continue;
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
            continue;
        }
        if let Some(rest) = input.strip_prefix("/access ") {
            if let Some(m) = AccessMode::from_str(rest) {
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
            continue;
        }

        if input == "/model" || input == "/m" || input == "/models" || input == "/local" {
            let local_first = input == "/local";
            if let Some(chosen_model) = interactive_select_model(&endpoint.key, local_first) {
                endpoint = resolve_endpoint(&chosen_model);
                println!(
                    "\n{}🧠 Aktif Model Seçildi: {}{}{}",
                    GREEN, BOLD, endpoint.label, RESET
                );
            }
            continue;
        }
        if let Some(rest) = input.strip_prefix("/model ") {
            endpoint = resolve_endpoint(rest);
            println!(
                "\n{}🧠 Aktif Model: {}{}{}",
                GREEN, BOLD, endpoint.label, RESET
            );
            continue;
        }

        if input == "/host" || input == "/h" {
            if let Some(chosen_host) = interactive_select_host(&ctx.default_host) {
                ctx.default_host = chosen_host;
                println!(
                    "\n{}🖥️  Varsayılan Hedef Cihaz Seçildi: {}{}{}",
                    MAGENTA, BOLD, ctx.default_host, RESET
                );
            }
            continue;
        }
        if let Some(rest) = input.strip_prefix("/host ") {
            ctx.default_host = rest.trim().to_string();
            println!(
                "\n{}🖥️  Varsayılan Hedef Cihaz: {}{}{}",
                MAGENTA, BOLD, ctx.default_host, RESET
            );
            continue;
        }

        if input == "/quota" || input == "/q" {
            render_full_quota_card();
            quota_badge = fetch_quick_quota_line();
            continue;
        }

        if input == "/fleet" || input == "/f" {
            render_fleet_radar();
            continue;
        }

        if input == "/undo" || input == "/u" {
            match ctx.pop_undo() {
                Some(msg) => println!("\n{}{}{}", GREEN, msg, RESET),
                None => println!("\n{}Geri alınacak dosya değişikliği yok.{}", YELLOW, RESET),
            }
            continue;
        }

        if input == "/think" {
            show_thinking = !show_thinking;
            println!(
                "\n{}💭 Düşünce Akışı: {}{}",
                DIM,
                if show_thinking { "AÇIK" } else { "KAPALI" },
                RESET
            );
            continue;
        }

        if input == "/compact" || input == "/c" {
            perform_manual_compact(&endpoint, &mut messages, None);
            rewrite_session(&session_file, &messages);
            continue;
        }
        if let Some(focus) = input
            .strip_prefix("/compact ")
            .or_else(|| input.strip_prefix("/c "))
        {
            perform_manual_compact(&endpoint, &mut messages, Some(focus.trim()));
            rewrite_session(&session_file, &messages);
            continue;
        }
        if input == "/resume" || input == "/sessions" {
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
                    render_session_resume_preview(&session_file, &messages);
                }
            }
            continue;
        }

        if input == "/new" || input == "/n" {
            session_file = new_session_path();
            session_name = session_file
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let sys = ChatMessage::system(build_system_prompt(&cwd));
            append_message(&session_file, &sys);
            messages = vec![sys];
            setup_scroll_region();
            render_welcome_header(
                &endpoint,
                ctx.access,
                &ctx.default_host,
                &cwd,
                &session_name,
            );
            println!(
                "\n{}✨ Yeni temiz oturum başlatıldı: {}{}{}",
                GREEN, BOLD, session_name, RESET
            );
            continue;
        }

        if input == "/fork" {
            session_file = fork_session(&messages);
            session_name = session_file
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
            continue;
        }

        if input == "/role" {
            if let Some(chosen_role) = interactive_select_role(active_role) {
                active_role = chosen_role;
                println!(
                    "\n{}🎭 Aktif Spavn Uzmanlık Rolü: {}{}{}",
                    GREEN,
                    BOLD,
                    active_role.badge(),
                    RESET
                );
            }
            continue;
        }
        if let Some(rest) = input.strip_prefix("/role ") {
            if let Some(r) = AgentRoleKey::from_str(rest) {
                active_role = r;
                println!(
                    "\n{}🎭 Aktif Spavn Uzmanlık Rolü: {}{}{}",
                    GREEN,
                    BOLD,
                    active_role.badge(),
                    RESET
                );
            } else {
                println!(
                    "{}Kullanım: /role <auto|coder|reviewer|security|test|docs|refactor>{}",
                    YELLOW, RESET
                );
            }
            continue;
        }

        if input == "/janitor" || input == "/j" {
            render_janitor_panel(&cwd);
            continue;
        }

        if input == "/audit" {
            render_audit_panel(None);
            continue;
        }
        if let Some(file_flt) = input
            .strip_prefix("/blame ")
            .or_else(|| input.strip_prefix("/audit "))
        {
            render_audit_panel(Some(file_flt.trim()));
            continue;
        }
        if input == "/blame" {
            render_audit_panel(None);
            continue;
        }

        if input == "/wiki" || input == "/w" {
            render_repo_wiki(&cwd);
            continue;
        }

        if input == "/pin" || input == "/pins" || input == "/p" {
            render_pins_panel(&cwd);
            continue;
        }
        if let Some(note) = input.strip_prefix("/pin ") {
            let pin = add_pin(&cwd, note);
            if !messages.is_empty() {
                messages[0] = ChatMessage::system(build_system_prompt(&cwd));
            }
            println!(
                "\n{}📌 Proje bağlamına sabitlendi (#{}): {}{}{}",
                GREEN, pin.id, BOLD, pin.content, RESET
            );
            continue;
        }
        if let Some(id_str) = input.strip_prefix("/unpin ") {
            if let Ok(id) = id_str.trim().parse::<usize>() {
                if remove_pin(&cwd, id) {
                    if !messages.is_empty() {
                        messages[0] = ChatMessage::system(build_system_prompt(&cwd));
                    }
                    println!("\n{}🗑️  Sabitlenmiş not silindi: #{}{}", GREEN, id, RESET);
                } else {
                    println!("\n{}Pin bulunamadı: #{}{}", YELLOW, id, RESET);
                }
            }
            continue;
        }

        if input == "/goal" || input == "/goals" || input == "/g" {
            render_goals_panel(&cwd);
            continue;
        }
        if let Some(id_str) = input.strip_prefix("/goal done ") {
            if let Ok(id) = id_str.trim().parse::<usize>() {
                if complete_goal(id) {
                    if !messages.is_empty() {
                        messages[0] = ChatMessage::system(build_system_prompt(&cwd));
                    }
                    println!(
                        "\n{}✅ Hedef tamamlandı olarak işaretlendi: #{}{}",
                        GREEN, id, RESET
                    );
                } else {
                    println!("\n{}Hedef bulunamadı: #{}{}", YELLOW, id, RESET);
                }
            }
            continue;
        }
        if let Some(obj) = input.strip_prefix("/goal ") {
            let g = add_goal(&cwd, obj);
            if !messages.is_empty() {
                messages[0] = ChatMessage::system(build_system_prompt(&cwd));
            }
            println!(
                "\n{}🎯 Yeni proje hedefi eklendi (#{}): {}{}{}",
                GREEN, g.id, BOLD, g.objective, RESET
            );
            continue;
        }

        if input == "/clear" {
            let sys = ChatMessage::system(build_system_prompt(&cwd));
            messages = vec![sys];
            rewrite_session(&session_file, &messages);
            setup_scroll_region();
            render_welcome_header(
                &endpoint,
                ctx.access,
                &ctx.default_host,
                &cwd,
                &session_name,
            );
            continue;
        }

        if input == "/help" {
            println!("\n{}╭─ ⚡ SPAVN v0.1 KOMUTLAR, BYOK & MOTOR ÖZELLİKLERİ ──────────────────────────{}", CYAN, RESET);
            println!(
                "{}│{}  {}/ (veya /menu){}                Etkileşimli yön tuşlu ana komut menüsü",
                CYAN, RESET, BOLD, RESET
            );
            println!("{}│{}  {}/model (/m) · /local{}          İki sütunlu Sağlayıcı (←/→) & Model (↑/↓) seçici", CYAN, RESET, BOLD, RESET);
            println!("{}│{}  {}/byok (/b){}                    BYOK API Anahtarı & Özel Sağlayıcı (Base URL) yöneticisi", CYAN, RESET, BOLD, RESET);
            println!("{}│{}  {}/queue (/qlist){}               Mesaj Kuyruğu düzenleme & Canlı Müdahale (Steer/Queue) modu", CYAN, RESET, BOLD, RESET);
            println!("{}│{}  {}/resume (-r) · /new · /fork{}   Geçmiş oturum seçici, yeni oturum ve dallandırma", CYAN, RESET, BOLD, RESET);
            println!("{}│{}  {}/role{}                         Spavn uzmanlık rolü (Auto/Coder/Reviewer/Security/Test/Docs/Refactor)", CYAN, RESET, BOLD, RESET);
            println!("{}│{}  {}/janitor (/j){}                 Spavn Janitor kalite kapılarını (check/types/lint/test) koş", CYAN, RESET, BOLD, RESET);
            println!("{}│{}  {}/audit · /blame <dosya>{}       HMAC zincirli AI aksiyon ve dosya değişim izi", CYAN, RESET, BOLD, RESET);
            println!("{}│{}  {}/wiki (/w){}                    Anlık proje modül ve mimari haritası (Repo Wiki)", CYAN, RESET, BOLD, RESET);
            println!("{}│{}  {}/pin <not> · /unpin <id>{}      Proje bağlamına kalıcı mimari kural sabitle", CYAN, RESET, BOLD, RESET);
            println!(
                "{}│{}  {}/goal <hedef> · /goal done N{}  Proje hedeflerini kaydet ve takip et",
                CYAN, RESET, BOLD, RESET
            );
            println!("{}│{}  {}Shift+Tab · Esc · Ctrl+R{}      Yetki değiştir · İşlemi/Menüyü iptal et · Canlı Reload", CYAN, RESET, BOLD, RESET);
            println!(
                "{}╰────────────────────────────────────────────────────────────────────────────{}",
                CYAN, RESET
            );
            continue;
        }

        let (mut expanded_input, activated_skills) = expand_inline_skills(&input);
        let dispatched_role = if active_role == AgentRoleKey::Auto {
            dispatch_role_from_prompt(&input)
        } else {
            active_role
        };
        let relevant_artifacts = retrieve_relevant_artifacts(&cwd, &input, 2);
        if !relevant_artifacts.is_empty() {
            let mut art_block = String::from("<spavn_knowledge_artifacts>\n");
            for a in &relevant_artifacts {
                art_block.push_str(&format!(
                    "- [{}] ({}): {}\n",
                    a.ts,
                    a.files.join(", "),
                    a.summary
                ));
            }
            art_block.push_str("</spavn_knowledge_artifacts>\n\n");
            expanded_input = format!("{}{}", art_block, expanded_input);
        }
        expanded_input = format!("{}\n\n{}", dispatched_role.preamble(), expanded_input);

        println!(
            "\n{}╭─ 👤 Siz · {}{}{}",
            CYAN,
            DIM,
            dispatched_role.badge(),
            RESET
        );
        for line in input.lines() {
            println!("{}│{} {}{}{}", CYAN, RESET, BOLD, line, RESET);
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
