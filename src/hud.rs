use crate::config::{
    builtin_byok_catalog, expand_user_path, is_en, load_user_config, resolve_api_key,
    save_user_config, tr_en, upsert_spavn_secret, ModelConfigEntry, ProviderGroupConfig,
};
use crate::router::ModelEndpoint;
use crate::tools::AccessMode;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

static LAST_TERM_ROWS: AtomicUsize = AtomicUsize::new(0);
static LAST_TERM_COLS: AtomicUsize = AtomicUsize::new(0);
static STICKY_DOCK_STATUS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static SPINNER_PAUSED: AtomicBool = AtomicBool::new(false);
static IDLE_RECAP_ENABLED: AtomicBool = AtomicBool::new(true);
static USED_CONTEXT_TOKENS: AtomicUsize = AtomicUsize::new(1_200);
static MAX_CONTEXT_TOKENS: AtomicUsize = AtomicUsize::new(200_000);
static STREAM_EXTRA_TOKENS: AtomicUsize = AtomicUsize::new(0);

pub fn set_context_window_usage(used_tokens: usize, max_tokens: usize) {
    USED_CONTEXT_TOKENS.store(used_tokens, Ordering::Relaxed);
    MAX_CONTEXT_TOKENS.store(max_tokens.max(1), Ordering::Relaxed);
    STREAM_EXTRA_TOKENS.store(0, Ordering::Relaxed);
}

pub fn toggle_idle_recap() -> bool {
    let prev = IDLE_RECAP_ENABLED.load(Ordering::Relaxed);
    let next = !prev;
    IDLE_RECAP_ENABLED.store(next, Ordering::Relaxed);
    next
}

pub fn set_idle_recap(enabled: bool) -> bool {
    IDLE_RECAP_ENABLED.store(enabled, Ordering::Relaxed);
    enabled
}

pub fn is_idle_recap_enabled() -> bool {
    IDLE_RECAP_ENABLED.load(Ordering::Relaxed)
}

pub fn build_session_recap_line(
    messages: &[crate::router::ChatMessage],
    max_cols: usize,
) -> Option<String> {
    let mut last_user = String::new();
    let mut last_assistant = String::new();
    let mut turn_count = 0usize;

    for m in messages.iter().skip(1) {
        if m.role == "user" {
            if let Some(ref raw) = m.content {
                if raw.starts_with("[SPAVN VERIFICATION GATE]")
                    || raw.starts_with("[SPAVN Auto-Compact")
                    || raw.starts_with("[SPAVN Semantic Context Handoff")
                    || raw.starts_with("[LIVE USER STEERING")
                {
                    continue;
                }
                let cleaned = strip_internal_tags_for_display(raw);
                let flat = cleaned
                    .lines()
                    .map(str::trim)
                    .filter(|l| {
                        !l.is_empty() && !l.contains('╭') && !l.contains('╰') && !l.contains('│')
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                if !flat.is_empty() {
                    last_user = flat;
                    turn_count += 1;
                }
            }
        } else if m.role == "assistant" {
            if let Some(ref raw) = m.content {
                let first_meaningful = raw
                    .lines()
                    .map(|l| {
                        l.trim()
                            .trim_start_matches(['#', '-', '*', '>', '•'])
                            .trim()
                    })
                    .find(|l| !l.is_empty() && !l.starts_with("```"))
                    .unwrap_or("");
                let clean_inline = first_meaningful.replace(['*', '`'], "");
                let flat = clean_inline
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !flat.is_empty() {
                    last_assistant = flat;
                }
            }
        }
    }

    if last_user.is_empty() && last_assistant.is_empty() {
        return None;
    }

    let summary = if !last_user.is_empty() && !last_assistant.is_empty() {
        let u_short: String = last_user.chars().take(38).collect();
        let u_disp = if last_user.chars().count() > 38 {
            format!("{}…", u_short)
        } else {
            u_short
        };
        format!(
            "\"{}\"{} → {}",
            u_disp,
            if turn_count > 1 {
                format!(" (#{})", turn_count)
            } else {
                String::new()
            },
            last_assistant
        )
    } else if !last_assistant.is_empty() {
        last_assistant
    } else {
        format!("\"{}\"", last_user)
    };

    let prefix = format!(
        "{}│{}  {}※ {}:{} {}",
        CYAN,
        RESET,
        MAGENTA,
        tr_en("recap", "recap"),
        RESET,
        DIM
    );
    let raw_line = format!("{}{}{}", prefix, summary, RESET);
    let (trunc, _) = truncate_ansi_to_width(&raw_line, max_cols.saturating_sub(2).max(16));
    Some(format!("{}{}", trunc, RESET))
}

pub fn update_max_context_window(max_tokens: usize) {
    MAX_CONTEXT_TOKENS.store(max_tokens.max(1), Ordering::Relaxed);
}

pub fn estimate_messages_tokens(messages: &[crate::router::ChatMessage]) -> usize {
    let mut chars = 0usize;
    for m in messages {
        if let Some(ref c) = m.content {
            chars += c.len();
        }
        if let Some(ref r) = m.reasoning_content {
            chars += r.len();
        }
        if let Some(ref tcs) = m.tool_calls {
            for tc in tcs {
                chars += tc.function.name.len() + tc.function.arguments.len() + 16;
            }
        }
        chars += 24;
    }
    (chars / 4) + 1_100
}

pub fn format_token_short(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        let m = tokens as f64 / 1_000_000.0;
        if (m - m.round()).abs() < 0.05 {
            format!("{:.0}M", m)
        } else {
            format!("{:.1}M", m)
        }
    } else if tokens == 32_768 {
        "32k".to_string()
    } else if tokens == 65_536 {
        "64k".to_string()
    } else if tokens == 131_072 {
        "131k".to_string()
    } else if tokens == 262_144 {
        "262k".to_string()
    } else if tokens >= 1_000 {
        let k = tokens as f64 / 1_000.0;
        if k >= 100.0 || (k - k.round()).abs() < 0.05 {
            format!("{:.0}k", k)
        } else {
            format!("{:.1}k", k)
        }
    } else {
        format!("{}", tokens)
    }
}

pub fn format_context_usage_badge() -> String {
    let used = USED_CONTEXT_TOKENS
        .load(Ordering::Relaxed)
        .saturating_add(STREAM_EXTRA_TOKENS.load(Ordering::Relaxed));
    let max = MAX_CONTEXT_TOKENS.load(Ordering::Relaxed).max(1);
    let pct = ((used as f64 / max as f64) * 100.0).round() as usize;
    let pct_display = if used > 0 && pct == 0 {
        1
    } else {
        pct.min(100)
    };
    let color = if pct_display < 50 {
        GREEN
    } else if pct_display < 80 {
        YELLOW
    } else {
        RED
    };
    let filled =
        ((pct_display as f64 / 20.0).round() as usize).clamp(if used > 0 { 1 } else { 0 }, 5);
    let bar = format!(
        "{}{}{}{}{}",
        color,
        "▰".repeat(filled),
        DIM,
        "▱".repeat(5 - filled),
        RESET
    );
    format!(
        "{} {}{}{} / {}{} ({}%){}",
        bar,
        color,
        format_token_short(used),
        RESET,
        DIM,
        format_token_short(max),
        pct_display,
        RESET
    )
}

pub fn effective_quota_badge(endpoint: &ModelEndpoint, quota_badge: &str) -> String {
    if endpoint.is_local() {
        format!("{}🟢 VRAM{}", GREEN, RESET)
    } else if endpoint.is_smart_pool_proxy() {
        let short_q = if let Some(idx) = quota_badge.find("Gem:") {
            quota_badge[idx..].trim()
        } else {
            quota_badge.trim()
        };
        format!("🔋 {}{}{}", DIM, short_q, RESET)
    } else {
        format!("{}🔑 BYOK{}", YELLOW, RESET)
    }
}

struct SpinnerScreenState {
    active: bool,
    paused: bool,
    label: String,
    started_at: Option<Instant>,
    dock_lines: Vec<String>,
    partial_line: String,
    partial_prefix: String,
    partial_shown: bool,
    thinking_box_open: bool,
    thinking_chars: usize,
    typing_buf: Vec<char>,
    typing_cursor: usize,
    typing_sel_anchor: Option<usize>,
    is_typing: bool,
    steer_mode: Option<bool>,
}

static SPINNER_SCREEN: std::sync::Mutex<SpinnerScreenState> =
    std::sync::Mutex::new(SpinnerScreenState {
        active: false,
        paused: false,
        label: String::new(),
        started_at: None,
        dock_lines: Vec::new(),
        partial_line: String::new(),
        partial_prefix: String::new(),
        partial_shown: false,
        thinking_box_open: false,
        thinking_chars: 0,
        typing_buf: Vec::new(),
        typing_cursor: 0,
        typing_sel_anchor: None,
        is_typing: false,
        steer_mode: None,
    });

fn rebuild_spinner_dock_lines(state: &mut SpinnerScreenState, status_hdr: &str, cols: usize) {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let max_w = cols.saturating_sub(6).max(20);
    let elapsed = state
        .started_at
        .map(|s| s.elapsed().as_secs_f32())
        .unwrap_or(0.0);
    let frame_idx = ((elapsed * 12.5) as usize) % FRAMES.len();
    let steer_mode = state.steer_mode.unwrap_or(false);
    let mode_tag = if steer_mode {
        format!(
            "{}⚡{}{}",
            YELLOW,
            tr_en("Anında Müdahale", "Live Steer"),
            RESET
        )
    } else {
        format!("{}📥{}{}", GREEN, tr_en("Sıraya Al", "Queue"), RESET)
    };
    let q_cnt = crate::queued_message_count();
    let s_cnt = crate::steer_message_count();
    let queue_status = if s_cnt > 0 {
        format!(
            " {}│{} {}⚡ {} {}{}",
            CYAN,
            RESET,
            YELLOW,
            s_cnt,
            tr_en("müdahale hazır", "steer ready"),
            RESET
        )
    } else if q_cnt > 0 {
        format!(
            " {}│{} {}📥 {} {}{}",
            CYAN,
            RESET,
            GREEN,
            q_cnt,
            tr_en("kuyrukta", "queued"),
            RESET
        )
    } else {
        String::new()
    };

    let top_raw = format!(
        "{} {}│{} {}{}{} {} ({:.1}s){}",
        status_hdr, CYAN, RESET, CYAN, FRAMES[frame_idx], RESET, state.label, elapsed, queue_status
    );
    let (top_line, _) = truncate_ansi_to_width(&top_raw, max_w);
    let has_typed = state.is_typing || !state.typing_buf.is_empty();
    let mut mid_lines: Vec<String> = Vec::new();
    if has_typed {
        let wrap_cols = max_w.saturating_sub(7).max(12);
        let c_pos = state.typing_cursor.min(state.typing_buf.len());
        let cur_sel = state.typing_sel_anchor.and_then(|a| {
            let a_clamped = a.min(state.typing_buf.len());
            let s = a_clamped.min(c_pos);
            let e = a_clamped.max(c_pos);
            (s < e).then_some((s, e))
        });
        let (wrapped, cursor_row, _) =
            wrap_input_lines_with_cursor(&state.typing_buf, c_pos, cur_sel, wrap_cols);
        let total_w = wrapped.len();
        let win_start = total_w.saturating_sub(4);
        let visible_slice = &wrapped[win_start..];
        for (i, line) in visible_slice.iter().enumerate() {
            let abs_row = win_start + i;
            let cursor_block = if abs_row == cursor_row {
                format!("{}█{}", CYAN, RESET)
            } else {
                String::new()
            };
            let styled = style_inline_image_tokens(line);
            let raw_row = if i == 0 {
                format!(
                    "{}│{} {}❯{} {}{}",
                    CYAN, RESET, BOLD, RESET, styled, cursor_block
                )
            } else {
                format!("{}│{}   {}{}", CYAN, RESET, styled, cursor_block)
            };
            let (trunc_row, _) = truncate_ansi_to_width(&raw_row, max_w);
            mid_lines.push(trunc_row);
        }
    } else if let Some((is_steer, pending_txt)) = crate::peek_last_pending_message() {
        let short_p: String = pending_txt
            .chars()
            .take(max_w.saturating_sub(36).max(16))
            .collect();
        let badge = if is_steer {
            format!("{}⚡ {}{}", YELLOW, tr_en("Müdahale", "Steer"), RESET)
        } else {
            format!("{}📥 {}{}", GREEN, tr_en("Kuyruk", "Queued"), RESET)
        };
        let raw_row = format!(
            "{}│{} {}❯{} {} {}·{} {}{}{} {}[↑: {} · Ctrl+X: {}]{}",
            CYAN,
            RESET,
            BOLD,
            RESET,
            badge,
            DIM,
            RESET,
            BOLD,
            short_p,
            RESET,
            DIM,
            tr_en("Düzenle", "Edit"),
            tr_en("İptal", "Cancel"),
            RESET
        );
        let (trunc_row, _) = truncate_ansi_to_width(&raw_row, max_w);
        mid_lines.push(trunc_row);
    } else {
        let raw_row = format!(
            "{}│{} {}❯{} {}{}{}",
            CYAN,
            RESET,
            BOLD,
            RESET,
            DIM,
            tr_en(
                "Mesaj yazın (işlem sürerken anında gönderin veya sıraya alın)...",
                "Type a message (send immediately to steer or queue for next turn)..."
            ),
            RESET
        );
        let (trunc_row, _) = truncate_ansi_to_width(&raw_row, max_w);
        mid_lines.push(trunc_row);
    }
    let esc_hint = if has_typed {
        tr_en(
            "Tab: Mod · Esc/Ctrl+U: Metni Sil",
            "Tab: Mode · Esc/Ctrl+U: Clear Text",
        )
    } else if s_cnt > 0 || q_cnt > 0 {
        tr_en(
            "↑: Kuyruktakini Düzenle · Ctrl+X: Kuyruğu İptal Et · Esc: Durdur",
            "↑: Edit Queued · Ctrl+X: Cancel Queued · Esc: Stop",
        )
    } else {
        tr_en(
            "Tab: Mod Değiştir · Esc: İptal",
            "Tab: Toggle Mode · Esc: Cancel",
        )
    };
    let bot_raw = format!(
        "{}╰─{} {} {}·{} {}{} ({}) · {}{}",
        CYAN,
        RESET,
        format_context_usage_badge(),
        DIM,
        RESET,
        DIM,
        tr_en("Enter: Gönder", "Enter: Send"),
        mode_tag,
        esc_hint,
        RESET
    );
    let (bot_line, _) = truncate_ansi_to_width(&bot_raw, max_w);
    let mut all_dock = Vec::with_capacity(mid_lines.len() + 2);
    all_dock.push(top_line);
    all_dock.extend(mid_lines);
    all_dock.push(bot_line);
    state.dock_lines = all_dock;
}

fn in_incomplete_ansi(s: &str) -> bool {
    if let Some(pos) = s.rfind('\x1b') {
        !s[pos + 1..].chars().any(|c| c.is_ascii_alphabetic())
    } else {
        false
    }
}

fn spinner_print_locked(state: &mut SpinnerScreenState, text: &str) {
    if text.is_empty() {
        return;
    }
    if !state.active || state.paused {
        print!("{}", text);
        let _ = io::stdout().flush();
        return;
    }
    let (_, cols) = term_size();
    let max_w = cols.saturating_sub(6).max(20);
    let status_hdr = get_sticky_dock_status();
    rebuild_spinner_dock_lines(state, &status_hdr, cols);
    let mut out = String::from("\x1b[?2026h\x1b[?25l\x1b[?7l\r\x1b[J");
    for ch in text.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            if state.thinking_box_open
                && !state.partial_prefix.is_empty()
                && state.partial_line == state.partial_prefix
            {
                state.partial_line.clear();
                continue;
            }
            out.push_str("\x1b[?7h");
            out.push_str(&state.partial_line);
            out.push_str("\x1b[0m\n");
            state.partial_line.clear();
        } else {
            state.partial_line.push(ch);
            if !in_incomplete_ansi(&state.partial_line) {
                let (_, vw) = truncate_ansi_to_width(&state.partial_line, usize::MAX);
                let is_table_box_line = !state.thinking_box_open
                    && (state.partial_line.contains('│')
                        || state.partial_line.contains('╭')
                        || state.partial_line.contains('├')
                        || state.partial_line.contains('╰'));
                let should_wrap = if is_table_box_line {
                    vw >= cols.saturating_sub(2).max(20)
                } else {
                    (ch == ' ' && vw >= max_w.saturating_sub(10)) || vw >= max_w
                };
                if should_wrap {
                    out.push_str("\x1b[?7h");
                    out.push_str(&state.partial_line);
                    out.push_str("\x1b[0m\n");
                    state.partial_line.clear();
                    if !state.partial_prefix.is_empty() {
                        state.partial_line.push_str(&state.partial_prefix);
                    } else {
                        state.partial_line.push_str("  ");
                    }
                }
            }
        }
    }

    out.push_str("\x1b[?7l");
    let dock_up = state.dock_lines.len().saturating_sub(1);
    let up_rows = if !state.partial_line.is_empty() {
        let (preview, _) = truncate_ansi_to_width(&state.partial_line, max_w);
        out.push_str(&preview);
        out.push_str("\x1b[0m\n");
        state.partial_shown = true;
        dock_up + 1
    } else {
        state.partial_shown = false;
        dock_up
    };
    out.push_str(&format!(
        "{}\x1b[{}A\r\x1b[?7h\x1b[?2026l",
        state.dock_lines.join("\n"),
        up_rows
    ));
    print!("{}", out);
    let _ = io::stdout().flush();
}

fn close_thinking_box_locked(state: &mut SpinnerScreenState) {
    state.thinking_chars = 0;
    if state.thinking_box_open {
        state.thinking_box_open = false;
        let pfx = std::mem::take(&mut state.partial_prefix);
        if !pfx.is_empty() && state.partial_line == pfx {
            state.partial_line.clear();
            spinner_print_locked(state, &format!("{}╰─{}\n", DIM, RESET));
        } else {
            spinner_print_locked(state, &format!("\n{}╰─{}\n", DIM, RESET));
        }
    }
}

pub fn spinner_print(text: &str) {
    if let Ok(mut state) = SPINNER_SCREEN.lock() {
        spinner_print_locked(&mut state, text);
    } else {
        print!("{}", text);
        let _ = io::stdout().flush();
    }
}

pub fn set_spinner_label(label: &str) {
    if let Ok(mut state) = SPINNER_SCREEN.lock() {
        state.label = label.to_string();
    }
}

pub fn stream_thinking_delta(think_delta: &str, show_thinking: bool) {
    STREAM_EXTRA_TOKENS.fetch_add(think_delta.len().div_ceil(4), Ordering::Relaxed);
    if let Ok(mut state) = SPINNER_SCREEN.lock() {
        state.thinking_chars += think_delta.chars().count();
        state.label = format!(
            "💭 {} ({} {})",
            tr_en("Düşünce akışı", "Thinking stream"),
            state.thinking_chars,
            tr_en("krk", "ch")
        );
        if show_thinking {
            if !state.thinking_box_open {
                state.thinking_box_open = true;
                state.partial_prefix = format!("{}{}│ ", DIM, CYAN);
                let hdr = format!(
                    "{}╭─ 💭 {}{}\n{}{}│ ",
                    DIM,
                    tr_en("Düşünce Akışı", "Thinking Stream"),
                    RESET,
                    DIM,
                    CYAN
                );
                spinner_print_locked(&mut state, &hdr);
            }
            let formatted = think_delta.replace('\n', &format!("\n{}{}│ ", DIM, CYAN));
            spinner_print_locked(&mut state, &formatted);
        }
    }
}

pub fn close_thinking_box_if_open() {
    if let Ok(mut state) = SPINNER_SCREEN.lock() {
        close_thinking_box_locked(&mut state);
    }
}

pub fn notify_tool_call_streaming(tool_name: &str, arg_bytes: usize) {
    if let Ok(mut state) = SPINNER_SCREEN.lock() {
        close_thinking_box_locked(&mut state);
        let tname = if tool_name.is_empty() {
            tr_en("araç", "tool")
        } else {
            tool_name
        };
        state.label = format!(
            "⚡ {} {} ({} B)",
            tname,
            tr_en("hazırlanıyor", "preparing"),
            arg_bytes
        );
    }
}

pub fn pause_active_spinner() {
    if !SPINNER_PAUSED.swap(true, Ordering::SeqCst) {
        if let Ok(mut state) = SPINNER_SCREEN.lock() {
            if state.active && !state.paused {
                state.paused = true;
                let mut out = String::from("\x1b[?2026h\x1b[?7l\r\x1b[J");
                if !state.partial_line.is_empty() {
                    out.push_str("\x1b[?7h");
                    out.push_str(&state.partial_line);
                    out.push_str("\x1b[0m\n");
                    state.partial_line.clear();
                }
                state.partial_shown = false;
                out.push_str("\x1b[?2004l\x1b[?7h\x1b[?25h\x1b[?2026l");
                print!("{}", out);
                let _ = io::stdout().flush();
            }
        }
    }
}

pub fn resume_active_spinner() {
    if SPINNER_PAUSED.swap(false, Ordering::SeqCst) {
        if let Ok(mut state) = SPINNER_SCREEN.lock() {
            if state.active && state.paused {
                state.paused = false;
                state.partial_shown = false;
                let dock_up = state.dock_lines.len().saturating_sub(1).max(1);
                print!(
                    "\x1b[?2004h\x1b[?2026h\x1b[?25l\x1b[?7l\r\x1b[J{}\x1b[{}A\r\x1b[?7h\x1b[?2026l",
                    state.dock_lines.join("\n"),
                    dock_up
                );
                let _ = io::stdout().flush();
            }
        }
    }
}

pub fn set_sticky_dock_status(
    endpoint: &ModelEndpoint,
    access: AccessMode,
    phase: &crate::spavn_engine::AgentPhase,
    default_host: &str,
    _quota_badge: &str,
) {
    update_max_context_window(endpoint.context_window_tokens());
    let access_color = match access {
        AccessMode::Safe => CYAN,
        AccessMode::Auto => YELLOW,
        AccessMode::Sovereign => RED,
    };
    let phase_color = match phase {
        crate::spavn_engine::AgentPhase::Plan => MAGENTA,
        crate::spavn_engine::AgentPhase::Code => GREEN,
    };
    let model_badge = if endpoint.is_local() {
        format!("🟢 {}{}{}", GREEN, endpoint.key, RESET)
    } else {
        format!("🧠 {}{}{}", GREEN, endpoint.key, RESET)
    };
    let host_part = if default_host == "local" || default_host.is_empty() {
        String::new()
    } else {
        format!(
            " {}·{} 🖥️  {}{}{}",
            DIM, RESET, MAGENTA, default_host, RESET
        )
    };
    let line = format!(
        "{}╭─{} {}{}{}{} {}·{} {}{}{} {}·{} {}{}",
        CYAN,
        RESET,
        BOLD,
        access_color,
        access.short_badge(),
        RESET,
        DIM,
        RESET,
        phase_color,
        phase.short_badge(),
        RESET,
        DIM,
        RESET,
        model_badge,
        host_part
    );
    if let Ok(mut guard) = STICKY_DOCK_STATUS.lock() {
        *guard = Some(line);
    }
}

pub fn get_sticky_dock_status() -> String {
    STICKY_DOCK_STATUS
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| format!("{}╭─ ⚡ SPAVN CANLI GİRİŞ{}", CYAN, RESET))
}

pub fn char_display_width(ch: char) -> usize {
    let u = ch as u32;
    if u == 0xFE0F || u == 0x200D || u < 32 {
        return 0;
    }
    if (0x1F300..=0x1FAFF).contains(&u)
        || (0x2600..=0x27BF).contains(&u)
        || (0x2300..=0x23FF).contains(&u)
        || (0x2B00..=0x2BFF).contains(&u)
        || (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0x9FFF).contains(&u)
    {
        return 2;
    }
    1
}

pub fn truncate_ansi_to_width(s: &str, max_cols: usize) -> (String, usize) {
    let mut out = String::with_capacity(s.len());
    let mut used = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            out.push(ch);
            if let Some(&next_c) = chars.peek() {
                if next_c == '[' {
                    if let Some(c) = chars.next() {
                        out.push(c);
                    }
                    for c in chars.by_ref() {
                        out.push(c);
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                } else if let Some(c) = chars.next() {
                    out.push(c);
                }
            }
            continue;
        }
        let w = char_display_width(ch);
        if used + w > max_cols {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push_str("\x1b[0m");
    (out, used)
}

const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const MAGENTA: &str = "\x1b[35m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    c_iflag: u64,
    c_oflag: u64,
    c_cflag: u64,
    c_lflag: u64,
    c_cc: [u8; 20],
    c_ispeed: u64,
    c_ospeed: u64,
}

#[cfg(not(target_os = "macos"))]
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 32],
    c_ispeed: u32,
    c_ospeed: u32,
}

#[cfg(target_os = "macos")]
const NCCS: usize = 20;
#[cfg(not(target_os = "macos"))]
const NCCS: usize = 32;

#[cfg(target_os = "macos")]
fn empty_termios() -> Termios {
    Termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: 0,
        c_lflag: 0,
        c_cc: [0; NCCS],
        c_ispeed: 0,
        c_ospeed: 0,
    }
}

#[cfg(not(target_os = "macos"))]
fn empty_termios() -> Termios {
    Termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: 0,
        c_lflag: 0,
        c_line: 0,
        c_cc: [0; NCCS],
        c_ispeed: 0,
        c_ospeed: 0,
    }
}

#[cfg(unix)]
extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn tcgetattr(fd: i32, termios_p: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, optional_actions: i32, termios_p: *const Termios) -> i32;
    fn read(fd: i32, buf: *mut std::ffi::c_void, count: usize) -> isize;
}

#[cfg(windows)]
#[repr(C)]
struct Coord {
    x: i16,
    y: i16,
}

#[cfg(windows)]
#[repr(C)]
struct SmallRect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

#[cfg(windows)]
#[repr(C)]
struct ConsoleScreenBufferInfo {
    dw_size: Coord,
    dw_cursor_position: Coord,
    w_attributes: u16,
    sr_window: SmallRect,
    dw_maximum_window_size: Coord,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(n_std_handle: u32) -> *mut std::ffi::c_void;
    fn GetConsoleMode(h_console_handle: *mut std::ffi::c_void, lp_mode: *mut u32) -> i32;
    fn SetConsoleMode(h_console_handle: *mut std::ffi::c_void, dw_mode: u32) -> i32;
    fn GetConsoleScreenBufferInfo(
        h_console_output: *mut std::ffi::c_void,
        lp_info: *mut ConsoleScreenBufferInfo,
    ) -> i32;
    fn ReadFile(
        h_file: *mut std::ffi::c_void,
        lp_buffer: *mut std::ffi::c_void,
        n_number_of_bytes_to_read: u32,
        lp_number_of_bytes_read: *mut u32,
        lp_overlapped: *mut std::ffi::c_void,
    ) -> i32;
    fn WaitForSingleObject(h_handle: *mut std::ffi::c_void, dw_milliseconds: u32) -> u32;
}

#[cfg(windows)]
unsafe fn tcgetattr(_fd: i32, termios_p: *mut Termios) -> i32 {
    let h_in = GetStdHandle((-10i32) as u32);
    let h_out = GetStdHandle((-11i32) as u32);
    let mut out_mode: u32 = 0;
    if GetConsoleMode(h_out, &mut out_mode) != 0 {
        let _ = SetConsoleMode(h_out, out_mode | 0x0004 | 0x0001);
    }
    let mut in_mode: u32 = 0;
    if GetConsoleMode(h_in, &mut in_mode) != 0 {
        (*termios_p).c_lflag = in_mode;
        0
    } else {
        -1
    }
}

#[cfg(windows)]
unsafe fn tcsetattr(_fd: i32, _optional_actions: i32, termios_p: *const Termios) -> i32 {
    let h_in = GetStdHandle((-10i32) as u32);
    let saved = (*termios_p).c_lflag;
    let mode = if (saved & (ICANON | ECHO)) == 0 {
        0x0200 | 0x0080
    } else {
        saved
    };
    if SetConsoleMode(h_in, mode) != 0 {
        0
    } else {
        -1
    }
}

#[cfg(windows)]
unsafe fn read(_fd: i32, buf: *mut std::ffi::c_void, count: usize) -> isize {
    let h_in = GetStdHandle((-10i32) as u32);
    if WaitForSingleObject(h_in, 80) != 0 {
        return 0;
    }
    let mut n_read: u32 = 0;
    if ReadFile(h_in, buf, count as u32, &mut n_read, std::ptr::null_mut()) != 0 {
        n_read as isize
    } else {
        0
    }
}

#[cfg(target_os = "macos")]
type TcFlag = u64;
#[cfg(not(target_os = "macos"))]
type TcFlag = u32;

#[cfg(target_os = "macos")]
const TIOCGWINSZ: u64 = 0x4008_7468;
#[cfg(not(target_os = "macos"))]
const TIOCGWINSZ: u64 = 0x5413;

#[cfg(target_os = "macos")]
const ICANON: TcFlag = 0x0000_0100;
#[cfg(not(target_os = "macos"))]
const ICANON: TcFlag = 0x0000_0002;

const ECHO: TcFlag = 0x0000_0008;
const TCSANOW: i32 = 0;

/// Restores the saved termios on drop, so a missed return path or a panic never leaves the tty raw.
struct TermiosGuard(Termios);

impl Drop for TermiosGuard {
    fn drop(&mut self) {
        unsafe {
            tcsetattr(0, TCSANOW, &self.0);
        }
    }
}

#[cfg(target_os = "macos")]
const VMIN_IDX: usize = 16;
#[cfg(target_os = "macos")]
const VTIME_IDX: usize = 17;
#[cfg(not(target_os = "macos"))]
const VMIN_IDX: usize = 6;
#[cfg(not(target_os = "macos"))]
const VTIME_IDX: usize = 5;
pub fn term_size() -> (usize, usize) {
    #[cfg(unix)]
    unsafe {
        let mut ws = WinSize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if ioctl(1, TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            return (ws.ws_row as usize, ws.ws_col as usize);
        }
    }
    #[cfg(windows)]
    unsafe {
        let h_out = GetStdHandle((-11i32) as u32);
        let mut info = ConsoleScreenBufferInfo {
            dw_size: Coord { x: 0, y: 0 },
            dw_cursor_position: Coord { x: 0, y: 0 },
            w_attributes: 0,
            sr_window: SmallRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dw_maximum_window_size: Coord { x: 0, y: 0 },
        };
        if GetConsoleScreenBufferInfo(h_out, &mut info) != 0 {
            let cols = (info.sr_window.right - info.sr_window.left + 1).max(20) as usize;
            let rows = (info.sr_window.bottom - info.sr_window.top + 1).max(10) as usize;
            return (rows, cols);
        }
    }
    (36, 100)
}

pub fn setup_scroll_region() {
    let (rows, cols) = term_size();
    LAST_TERM_ROWS.store(rows, Ordering::SeqCst);
    LAST_TERM_COLS.store(cols, Ordering::SeqCst);
    print!("\x1b7\x1b[r\x1b8\x1b[?7h\x1b[?25h");
    let _ = io::stdout().flush();
}

pub fn reset_scroll_region() {
    print!("\x1b7\x1b[r\x1b8\x1b[?7h\x1b[?25h");
    let _ = io::stdout().flush();
}

pub fn draw_bottom_dock(
    _endpoint: &ModelEndpoint,
    _access: AccessMode,
    _default_host: &str,
    _quota_badge: &str,
    _input_buf: &str,
) {
}

pub fn compute_input_suggestions(input_buf: &str) -> Vec<(String, String)> {
    if input_buf.starts_with('/') && !input_buf.contains(' ') {
        let cmds = [
            (
                "/model",
                tr_en(
                    "/model (Sağlayıcı & Model Seç)",
                    "/model (Select Provider & Model)",
                ),
            ),
            (
                "/byok",
                tr_en(
                    "/byok (API Anahtarı & Özel Sağlayıcı Ekle)",
                    "/byok (Add API Key & Custom Provider)",
                ),
            ),
            (
                "/login",
                tr_en(
                    "/login (Sağlayıcıya Doğrudan Bağlan / Giriş Yap)",
                    "/login (Direct Provider Login & Connection)",
                ),
            ),
            (
                "/provider",
                tr_en(
                    "/provider (Sağlayıcı & Sunucu Bağlantı Merkezi)",
                    "/provider (Provider & Server Connection Hub)",
                ),
            ),
            (
                "/local",
                tr_en("/local (Yerel Modeller)", "/local (Local Models)"),
            ),
            (
                "/bench",
                tr_en(
                    "/bench (Model Hız & Tool-Call Testi)",
                    "/bench (Model Speed & Tool-Call Benchmark)",
                ),
            ),
            (
                "/queue",
                tr_en(
                    "/queue (Mesaj Kuyruğu & Gönderim Modu)",
                    "/queue (Message Queue & Busy Mode)",
                ),
            ),
            (
                "/lang",
                tr_en(
                    "/lang (Dil Değiştir: TR / EN)",
                    "/lang (Switch Language: TR / EN)",
                ),
            ),
            (
                "/resume",
                tr_en("/resume (Geçmiş Oturumlar)", "/resume (Recent Sessions)"),
            ),
            (
                "/recap",
                tr_en(
                    "/recap (Oturum Özeti & Idle Recap Aç/Kapat)",
                    "/recap (Session Recap & Toggle Idle Recap)",
                ),
            ),
            ("/new", tr_en("/new (Yeni Oturum)", "/new (New Session)")),
            (
                "/fork",
                tr_en("/fork (Oturumu Dallandır)", "/fork (Fork Session)"),
            ),
            (
                "/role",
                tr_en("/role (Uzmanlık Rolü Seç)", "/role (Select Agent Role)"),
            ),
            (
                "/plan",
                tr_en(
                    "/plan (📐 Salt-Okunur Tasarım Fazı)",
                    "/plan (📐 Read-Only Plan Phase)",
                ),
            ),
            (
                "/code",
                tr_en(
                    "/code (💻 Kodlama & Uygulama Fazı)",
                    "/code (💻 Coding & Execution Phase)",
                ),
            ),
            (
                "/janitor",
                tr_en("/janitor (Kalite Kapıları)", "/janitor (Quality Gates)"),
            ),
            (
                "/audit",
                tr_en("/audit (HMAC Aksiyon İzi)", "/audit (HMAC Action Ledger)"),
            ),
            (
                "/blame",
                tr_en("/blame <dosya> (Dosya İzi)", "/blame <file> (File Blame)"),
            ),
            (
                "/wiki",
                tr_en(
                    "/wiki (Repo Mimari Özeti)",
                    "/wiki (Repo Architecture Brief)",
                ),
            ),
            (
                "/pin",
                tr_en(
                    "/pin <kural> (Bağlam Sabitle)",
                    "/pin <rule> (Pin Context Rule)",
                ),
            ),
            (
                "/goal",
                tr_en(
                    "/goal <hedef> (Proje Hedefi)",
                    "/goal <text> (Project Goal)",
                ),
            ),
            ("/host", tr_en("/host (Hedef Cihaz)", "/host (Target Host)")),
            ("/skills", tr_en("/skills (Yetenekler)", "/skills (Skills)")),
            (
                "/access",
                tr_en("/access (Yetki Modu)", "/access (Access Mode)"),
            ),
            ("/quota", tr_en("/quota (Kotalar)", "/quota (Quotas)")),
            (
                "/fleet",
                tr_en("/fleet (Filo Radarı)", "/fleet (Fleet Radar)"),
            ),
            (
                "/compact",
                tr_en("/compact (Sıkıştır)", "/compact (Compact Context)"),
            ),
            ("/undo", tr_en("/undo (Geri Al)", "/undo (Undo Last Edit)")),
            (
                "/reload",
                tr_en("/reload (Yenile)", "/reload (Live Reload)"),
            ),
            (
                "/clear",
                tr_en("/clear (Temizle)", "/clear (Clear Session)"),
            ),
            ("/exit", tr_en("/exit (Çıkış)", "/exit (Exit)")),
        ];
        let q = input_buf.trim().to_lowercase();
        return cmds
            .iter()
            .filter(|(k, _)| {
                k.starts_with(&q)
                    || (q == "/m" && *k == "/model")
                    || (q == "/b" && *k == "/byok")
                    || (q == "/l" && (*k == "/local" || *k == "/lang"))
                    || (q == "/h" && *k == "/host")
                    || (q == "/s" && *k == "/skills")
                    || (q == "/n" && *k == "/new")
                    || (q == "/j" && *k == "/janitor")
                    || (q == "/w" && *k == "/wiki")
                    || (q == "/g" && *k == "/goal")
                    || (q == "/p" && *k == "/pin")
                    || (q == "/q" && *k == "/queue")
                    || (q == "/f" && *k == "/fleet")
                    || (q == "/c" && *k == "/compact")
                    || (q == "/u" && *k == "/undo")
                    || (q == "/r" && *k == "/reload")
            })
            .map(|(k, l)| (k.to_string(), l.to_string()))
            .collect();
    }
    if let Some(at_idx) = input_buf.rfind('@') {
        let tail = &input_buf[at_idx..];
        if !tail.contains(' ') {
            let q = tail.trim_start_matches('@').to_lowercase();
            let skills = crate::skills::discover_skills();
            return skills
                .into_iter()
                .filter(|s| s.name.to_lowercase().starts_with(&q))
                .map(|s| {
                    let token = format!("@{}", s.name);
                    (token.clone(), token)
                })
                .collect();
        }
    }
    Vec::new()
}

pub fn flex_wrap_items(
    prefix_first: &str,
    prefix_cont: &str,
    sep: &str,
    items: &[String],
    max_w: usize,
) -> Vec<String> {
    let (_, pf_w) = truncate_ansi_to_width(prefix_first, usize::MAX);
    let (_, pc_w) = truncate_ansi_to_width(prefix_cont, usize::MAX);
    let (_, sep_w) = truncate_ansi_to_width(sep, usize::MAX);

    let mut lines: Vec<String> = Vec::new();
    let mut cur_line = String::from(prefix_first);
    let mut cur_w = pf_w;
    let mut first_in_line = true;

    for item in items {
        let (_, item_w) = truncate_ansi_to_width(item, usize::MAX);
        if !first_in_line && cur_w + sep_w + item_w > max_w {
            lines.push(format!("{}{}", cur_line, RESET));
            cur_line = String::from(prefix_cont);
            cur_w = pc_w;
            first_in_line = true;
        }
        if !first_in_line {
            cur_line.push_str(sep);
            cur_w += sep_w;
        }
        if cur_w + item_w > max_w {
            let avail = max_w.saturating_sub(cur_w).max(1);
            let (t_item, t_w) = truncate_ansi_to_width(item, avail);
            cur_line.push_str(&t_item);
            cur_w += t_w;
        } else {
            cur_line.push_str(item);
            cur_w += item_w;
        }
        first_in_line = false;
    }
    if !cur_line.is_empty() {
        lines.push(format!("{}{}", cur_line, RESET));
    }
    lines
}

fn image_token_span_before_cursor(chars: &[char], cursor_pos: usize) -> Option<(usize, usize)> {
    if cursor_pos == 0 || cursor_pos > chars.len() || chars[cursor_pos - 1] != ']' {
        return None;
    }
    let mut start = cursor_pos - 1;
    while start > 0 {
        start -= 1;
        if chars[start] == '[' && chars.get(start + 1) == Some(&'🖼') {
            return Some((start, cursor_pos));
        }
        if chars[start] == ']' {
            break;
        }
    }
    None
}

fn image_token_span_at_cursor(chars: &[char], cursor_pos: usize) -> Option<(usize, usize)> {
    if cursor_pos + 1 >= chars.len() || chars[cursor_pos] != '[' || chars[cursor_pos + 1] != '🖼'
    {
        return None;
    }
    for (end, &ch) in chars.iter().enumerate().skip(cursor_pos + 2) {
        if ch == ']' {
            return Some((cursor_pos, end + 1));
        }
    }
    None
}

fn style_inline_image_tokens(line: &str) -> String {
    if !line.contains("[🖼️ ") {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 16);
    let mut rest = line;
    while let Some(s) = rest.find("[🖼️ ") {
        out.push_str(&rest[..s]);
        if let Some(e) = rest[s..].find(']') {
            let token = &rest[s..=s + e];
            out.push_str(&format!("{}{}{}{}", BOLD, GREEN, token, RESET));
            rest = &rest[s + e + 1..];
        } else {
            out.push_str(&rest[s..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

fn wrap_input_lines_with_cursor(
    chars: &[char],
    cursor_pos: usize,
    sel_range: Option<(usize, usize)>,
    max_line_cols: usize,
) -> (Vec<String>, usize, usize) {
    let cap = max_line_cols.max(8);
    let mut lines: Vec<String> = Vec::new();
    let mut cur_line = String::new();
    let mut cur_w = 0usize;
    let mut cursor_row = 0usize;
    let mut cursor_col_offset = 0usize;
    let mut cursor_recorded = false;
    let mut in_sel = false;

    for (i, &ch) in chars.iter().enumerate() {
        if i == cursor_pos {
            cursor_row = lines.len();
            cursor_col_offset = cur_w;
            cursor_recorded = true;
        }
        let want_sel = sel_range
            .map(|(s, e)| s < e && i >= s && i < e)
            .unwrap_or(false);
        if ch == '\n' {
            if in_sel {
                cur_line.push_str("\x1b[27m");
                in_sel = false;
            }
            lines.push(std::mem::take(&mut cur_line));
            cur_w = 0;
            continue;
        }
        let w = char_display_width(ch).max(1);
        if cur_w + w > cap && !cur_line.is_empty() {
            if in_sel {
                cur_line.push_str("\x1b[27m");
                in_sel = false;
            }
            lines.push(std::mem::take(&mut cur_line));
            cur_w = 0;
            if i == cursor_pos {
                cursor_row = lines.len();
                cursor_col_offset = 0;
            }
        }
        if want_sel != in_sel {
            cur_line.push_str(if want_sel { "\x1b[7m" } else { "\x1b[27m" });
            in_sel = want_sel;
        }
        cur_line.push(ch);
        cur_w += w;
    }

    if in_sel {
        cur_line.push_str("\x1b[27m");
    }
    if !cursor_recorded {
        cursor_row = lines.len();
        cursor_col_offset = cur_w;
    }
    lines.push(cur_line);
    if lines.is_empty() {
        lines.push(String::new());
    }
    (lines, cursor_row, cursor_col_offset)
}

#[allow(clippy::too_many_arguments)]
fn render_inline_dock(
    endpoint: &ModelEndpoint,
    access: AccessMode,
    phase: &crate::spavn_engine::AgentPhase,
    default_host: &str,
    quota_badge: &str,
    input_chars: &[char],
    cursor_pos: usize,
    sel_range: Option<(usize, usize)>,
    suggestions: &[(String, String)],
    selected_idx: Option<usize>,
    queued_count: usize,
    busy_mode: &str,
    recap_line: Option<&str>,
    prev_top_w: &mut usize,
    prev_cursor_col: &mut usize,
    cols: usize,
) {
    let full_w = cols.saturating_sub(2).max(6);

    let access_color = match access {
        AccessMode::Safe => CYAN,
        AccessMode::Auto => YELLOW,
        AccessMode::Sovereign => RED,
    };
    let phase_color = match phase {
        crate::spavn_engine::AgentPhase::Plan => MAGENTA,
        crate::spavn_engine::AgentPhase::Code => GREEN,
    };

    let queue_tag = if queued_count > 0 {
        format!(" {}📥{}{}", GREEN, queued_count, RESET)
    } else {
        String::new()
    };
    let _ = busy_mode;
    let model_badge = if endpoint.is_local() {
        format!("🟢 {}{}{}", GREEN, endpoint.key, RESET)
    } else {
        format!("🧠 {}{}{}", GREEN, endpoint.key, RESET)
    };
    let mut status_items = vec![
        format!("{}{}{}{}", BOLD, access_color, access.short_badge(), RESET),
        format!("{}{}{}", phase_color, phase.short_badge(), RESET),
        model_badge,
    ];
    if default_host != "local" && !default_host.is_empty() {
        status_items.push(format!("🖥️  {}{}{}", MAGENTA, default_host, RESET));
    }
    status_items.push(effective_quota_badge(endpoint, quota_badge));
    if !queue_tag.is_empty() {
        status_items.push(queue_tag.trim().to_string());
    }
    set_sticky_dock_status(endpoint, access, phase, default_host, quota_badge);
    let top_prefix_first = format!("{}╭─{} ", CYAN, RESET);
    let top_prefix_cont = format!("{}│{}  ", CYAN, RESET);
    let top_sep = format!(" {}·{} ", DIM, RESET);
    let mut top_lines = flex_wrap_items(
        &top_prefix_first,
        &top_prefix_cont,
        &top_sep,
        &status_items,
        full_w,
    );
    if let Some(recap) = recap_line {
        if input_chars.is_empty() && suggestions.is_empty() {
            top_lines.push(recap.to_string());
        }
    }
    let top_count = top_lines.len().max(1);
    let top_joined = top_lines.join("\n");

    let max_input_cols = full_w.saturating_sub(5).max(8);
    let (wrapped_input_lines, cursor_row, cursor_col_offset) =
        wrap_input_lines_with_cursor(input_chars, cursor_pos, sel_range, max_input_cols);
    let formatted_input_lines: Vec<String> = wrapped_input_lines
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            let raw = if idx == 0 {
                if input_chars.is_empty() && suggestions.is_empty() {
                    format!(
                        "{}│{} {}❯{}   {}{}{}",
                        CYAN,
                        RESET,
                        BOLD,
                        RESET,
                        DIM,
                        tr_en(
                            "Mesaj yazın veya / ile komutları açın...",
                            "Type a prompt or press / for commands..."
                        ),
                        RESET
                    )
                } else {
                    format!(
                        "{}│{} {}❯{} {}",
                        CYAN,
                        RESET,
                        BOLD,
                        RESET,
                        style_inline_image_tokens(line)
                    )
                }
            } else {
                format!("{}│{}   {}", CYAN, RESET, style_inline_image_tokens(line))
            };
            let (trunc, _) = truncate_ansi_to_width(&raw, full_w);
            trunc
        })
        .collect();
    let input_joined = formatted_input_lines.join("\n");
    let input_line_count = formatted_input_lines.len().max(1);

    let bot_prefix_first = format!("{}╰─{} ", CYAN, RESET);
    let bot_prefix_cont = "   ".to_string();
    let bottom_lines = if !suggestions.is_empty() {
        let sel = selected_idx
            .unwrap_or(0)
            .min(suggestions.len().saturating_sub(1));
        let mut sug_items = Vec::new();
        let window_start = sel.saturating_sub(2);
        for (i, (_, label)) in suggestions.iter().enumerate().skip(window_start).take(4) {
            if i == sel {
                sug_items.push(format!("\x1b[1;7;36m ❯ {} \x1b[0m", label));
            } else {
                sug_items.push(format!("{}{}{}", DIM, label, RESET));
            }
        }
        sug_items.push(format!("{}(↑/↓/Tab · Enter · Esc){}", YELLOW, RESET));
        flex_wrap_items(
            &bot_prefix_first,
            &bot_prefix_cont,
            &format!(" {}·{} ", DIM, RESET),
            &sug_items,
            full_w,
        )
    } else {
        let mut h_items = vec![
            format_context_usage_badge(),
            format!("{}{}{}", DIM, tr_en("/: Menü", "/: Menu"), RESET),
            format!("{}/model{}", DIM, RESET),
            format!("{}/login{}", DIM, RESET),
            format!(
                "{}{}{}",
                DIM,
                tr_en("Shift+Tab: Yetki", "Shift+Tab: Access"),
                RESET
            ),
            format!("{}{}{}", DIM, tr_en("\\: Faz", "\\: Phase"), RESET),
        ];
        if !input_chars.is_empty() {
            if let Some((s, e)) = sel_range {
                if s < e {
                    h_items.push(format!(
                        "{}{}{}",
                        YELLOW,
                        tr_en(
                            &format!(
                                "Seçili: {} krk (Backspace: Sil · ←/→: Seçimi Kaldır)",
                                e - s
                            ),
                            &format!(
                                "Selected: {} chars (Backspace: Delete · ←/→: Unselect)",
                                e - s
                            )
                        ),
                        RESET
                    ));
                }
            } else {
                h_items.push(format!(
                    "{}{}{}",
                    YELLOW,
                    tr_en(
                        "⌃A: Tümünü Seç · Shift+←/→: Seç · Esc: Tümünü Sil",
                        "⌃A: Select All · Shift+←/→: Select · Esc: Clear All"
                    ),
                    RESET
                ));
            }
        }
        flex_wrap_items(
            &bot_prefix_first,
            &bot_prefix_cont,
            &format!(" {}·{} ", DIM, RESET),
            &h_items,
            full_w,
        )
    };

    let bottom_count = bottom_lines.len().max(1);
    let bottom_joined = bottom_lines.join("\n");

    let up_to_start = (*prev_top_w).max(1);
    let cursor_col = (5 + cursor_col_offset).min(full_w);
    let lines_below_cursor = bottom_count + input_line_count.saturating_sub(1 + cursor_row);
    print!(
        "\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[{}A\r\x1b[J{}\n{}\n{}\x1b[?7h\x1b[{}A\r\x1b[{}G\x1b[?25h\x1b[?2026l",
        up_to_start,
        top_joined,
        input_joined,
        bottom_joined,
        lines_below_cursor,
        cursor_col
    );
    let _ = io::stdout().flush();
    *prev_top_w = top_count + cursor_row;
    *prev_cursor_col = cursor_col;
}

fn erase_inline_dock(prev_top_w: usize, _prev_cursor_col: usize) {
    let up_to_start = prev_top_w.max(1);
    print!("\x1b[{}A\r\x1b[J\x1b[?7h\x1b[?25h", up_to_start);
    let _ = io::stdout().flush();
}

pub fn strip_internal_tags_for_display(raw: &str) -> String {
    let mut s = raw.trim();
    loop {
        if let Some(start) = s.find("<spavn_knowledge_artifacts>") {
            if let Some(end) = s[start..].find("</spavn_knowledge_artifacts>") {
                s = s[start + end + "</spavn_knowledge_artifacts>".len()..].trim();
                continue;
            }
        }
        if let Some(start) = s.find("<active_skill") {
            if let Some(end) = s[start..].find("</active_skill>") {
                s = s[start + end + "</active_skill>".len()..].trim();
                continue;
            }
        }
        if s.starts_with("[SPAVN ROLE:") || s.starts_with("[SPAVN PHASE:") {
            if let Some(nl) = s.find("\n\n") {
                s = s[nl + 2..].trim();
                continue;
            } else if let Some(nl) = s.find('\n') {
                s = s[nl + 1..].trim();
                continue;
            }
        }
        break;
    }
    s.to_string()
}
#[allow(dead_code)]
pub fn redraw_full_viewport(
    endpoint: &ModelEndpoint,
    access: AccessMode,
    default_host: &str,
    cwd: &str,
    session_name: &str,
    messages: &[crate::router::ChatMessage],
) {
    print!("\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[2J\x1b[3J\x1b[H\x1b[?7h");
    render_welcome_header(endpoint, access, default_host, cwd, session_name);
    let visible_msgs: Vec<&crate::router::ChatMessage> = messages
        .iter()
        .skip(1)
        .filter(|m| m.role == "user" || m.role == "assistant")
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    for m in visible_msgs {
        let Some(ref raw_content) = m.content else {
            continue;
        };
        if m.role == "user" {
            if raw_content.starts_with("[SPAVN VERIFICATION GATE]")
                || raw_content.starts_with("[SPAVN Auto-Compact")
                || raw_content.starts_with("[SPAVN Semantic Context Handoff")
                || raw_content.starts_with("[LIVE USER STEERING")
            {
                continue;
            }
            let clean = strip_internal_tags_for_display(raw_content);
            if clean.is_empty() {
                continue;
            }
            println!(
                "\n{}╭─{} {}👤 {}{}",
                CYAN,
                RESET,
                BOLD,
                tr_en("Siz", "You"),
                RESET
            );
            for line in clean.lines() {
                println!("{}│{} {}{}{}", CYAN, RESET, BOLD, line, RESET);
            }
            println!("{}╰─{}", CYAN, RESET);
        } else if m.role == "assistant" {
            let clean = raw_content.trim();
            if clean.is_empty() {
                continue;
            }
            render_assistant_header(&endpoint.key);
            let mut md = MarkdownStreamRenderer::new();
            md.push(clean);
            md.finish();
            println!("{}╰─{}", CYAN, RESET);
        }
    }
    print!("\x1b[?25h\x1b[?2026l");
    let _ = io::stdout().flush();
}

pub enum PromptEvent {
    Submit(String),
    CycleAccess,
    CyclePhase,
    Reload,
    ShowSkills,
    Exit,
}

#[allow(clippy::too_many_arguments)]
pub fn read_bottom_dock_input(
    endpoint: &ModelEndpoint,
    access: &mut AccessMode,
    phase: &crate::spavn_engine::AgentPhase,
    default_host: &str,
    quota_badge: &str,
    history: &[String],
    initial_prefill: Option<&str>,
    _cwd: &str,
    _session_name: &str,
    messages: &[crate::router::ChatMessage],
) -> PromptEvent {
    let mut orig_termios = empty_termios();

    let has_tty = unsafe { tcgetattr(0, &mut orig_termios) == 0 };
    if !has_tty {
        let mut line = String::new();
        if io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            return PromptEvent::Exit;
        }
        return PromptEvent::Submit(line.trim().to_string());
    }

    let _term_guard = TermiosGuard(orig_termios);
    let mut raw = orig_termios;
    raw.c_lflag &= !(ICANON | ECHO);
    raw.c_cc[VMIN_IDX] = 0;
    raw.c_cc[VTIME_IDX] = 1; // 100ms non-blocking timeout for resize detection
    unsafe {
        tcsetattr(0, TCSANOW, &raw);
    }
    print!("\x1b[?2004h\n\n\n\n\x1b[2A\r");
    let _ = io::stdout().flush();

    let mut chars: Vec<char> = initial_prefill.unwrap_or("").chars().collect();
    let mut cursor_pos: usize = chars.len();
    let mut sel_anchor: Option<usize> = None;
    let mut hist_idx = history.len();
    let mut suggestion_idx: Option<usize> = None;
    let mut kbuf = [0u8; 4096];
    let mut in_paste = false;
    let _ = crate::take_resized();
    let (mut last_rows, mut last_cols) = term_size();
    let mut needs_redraw = true;
    let mut full_resize_pending = false;
    let busy_mode = load_user_config().busy_input_mode;
    let mut prev_top_w: usize = 0;
    let mut prev_cursor_col: usize = 1;
    let mut last_input_at = Instant::now();
    let mut idle_recap_shown = false;
    loop {
        let (rows, cols) = term_size();
        if (rows, cols) != (last_rows, last_cols) || crate::take_resized() {
            // Wait until active mouse-drag resize pauses for 25ms before redrawing
            let mut stable = (rows, cols);
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(25));
                let _ = crate::take_resized();
                let next = term_size();
                if next == stable {
                    break;
                }
                stable = next;
            }
            last_rows = stable.0;
            last_cols = stable.1;
            full_resize_pending = true;
            needs_redraw = true;
        }

        let mut buf: String = chars.iter().collect();
        if buf.contains('/') || buf.contains('~') {
            let (collapsed, changed) = crate::router::collapse_pasted_image_paths(&buf);
            if changed {
                chars = collapsed.chars().collect();
                cursor_pos = chars.len();
                buf = collapsed;
            }
        }
        let suggestions = compute_input_suggestions(&buf);
        if suggestions.is_empty() {
            suggestion_idx = None;
        } else if let Some(idx) = suggestion_idx {
            if idx >= suggestions.len() {
                suggestion_idx = Some(0);
            }
        } else {
            suggestion_idx = Some(0);
        }

        let want_idle_recap = is_idle_recap_enabled()
            && messages.len() > 1
            && chars.is_empty()
            && suggestions.is_empty()
            && last_input_at.elapsed() >= Duration::from_secs(6);
        if want_idle_recap != idle_recap_shown {
            idle_recap_shown = want_idle_recap;
            needs_redraw = true;
        }

        if needs_redraw {
            if full_resize_pending {
                erase_inline_dock(prev_top_w, prev_cursor_col);
                print!("\n\r");
                prev_top_w = 0;
                prev_cursor_col = 1;
                full_resize_pending = false;
            }

            let recap_str = if idle_recap_shown {
                build_session_recap_line(messages, last_cols)
            } else {
                None
            };

            let cur_sel = sel_anchor.and_then(|a| {
                let s = a.min(cursor_pos);
                let e = a.max(cursor_pos);
                (s < e).then_some((s, e))
            });

            render_inline_dock(
                endpoint,
                *access,
                phase,
                default_host,
                quota_badge,
                &chars,
                cursor_pos,
                cur_sel,
                &suggestions,
                suggestion_idx,
                crate::queued_message_count(),
                &busy_mode,
                recap_str.as_deref(),
                &mut prev_top_w,
                &mut prev_cursor_col,
                last_cols,
            );
            needs_redraw = false;
        }
        let n_read = unsafe { read(0, kbuf.as_mut_ptr() as *mut std::ffi::c_void, kbuf.len()) };
        let n = if n_read == 0 {
            continue;
        } else if n_read < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted || err.kind() == io::ErrorKind::WouldBlock {
                needs_redraw = true;
                continue;
            }
            erase_inline_dock(prev_top_w, prev_cursor_col);
            unsafe {
                tcsetattr(0, TCSANOW, &orig_termios);
            }
            return PromptEvent::Exit;
        } else {
            last_input_at = Instant::now();
            needs_redraw = true;
            n_read as usize
        };

        if n == 1 && kbuf[0] == 27 {
            if sel_anchor.is_some() {
                sel_anchor = None;
            } else if !chars.is_empty() {
                chars.clear();
                cursor_pos = 0;
                suggestion_idx = None;
            }
            continue;
        }

        let mut idx = 0usize;
        while idx < n {
            let b = kbuf[idx];
            idx += 1;

            if b == 27 {
                let rem = &kbuf[idx - 1..n];
                if rem.starts_with(b"\x1b[200~") {
                    in_paste = true;
                    idx += 5;
                    continue;
                }
                if rem.starts_with(b"\x1b[201~") {
                    in_paste = false;
                    idx += 5;
                    continue;
                }
                // Option+Backspace (macOS sends \x1b\x7f or \x1b\x08): delete selection or word backward
                if idx < n && (kbuf[idx] == 127 || kbuf[idx] == 8) {
                    idx += 1;
                    if let Some(a) = sel_anchor.take() {
                        let s = a.min(cursor_pos);
                        let e = a.max(cursor_pos);
                        if s < e {
                            chars.drain(s..e);
                            cursor_pos = s;
                            continue;
                        }
                    }
                    if let Some((start, end)) = image_token_span_before_cursor(&chars, cursor_pos) {
                        chars.drain(start..end);
                        cursor_pos = start;
                    } else {
                        while cursor_pos > 0 && chars[cursor_pos - 1].is_whitespace() {
                            chars.remove(cursor_pos - 1);
                            cursor_pos -= 1;
                        }
                        while cursor_pos > 0 && !chars[cursor_pos - 1].is_whitespace() {
                            chars.remove(cursor_pos - 1);
                            cursor_pos -= 1;
                        }
                    }
                    continue;
                }
                // Option+Left (\x1bb) / Option+Right (\x1bf): word jump; Shift+Option+Left (\x1bB) / Right (\x1bF): word select
                if idx < n && (kbuf[idx] == b'b' || kbuf[idx] == b'B') {
                    let is_shift = kbuf[idx] == b'B';
                    idx += 1;
                    if is_shift {
                        if sel_anchor.is_none() {
                            sel_anchor = Some(cursor_pos);
                        }
                    } else {
                        sel_anchor = None;
                    }
                    if let Some((start, _)) = image_token_span_before_cursor(&chars, cursor_pos) {
                        cursor_pos = start;
                    } else {
                        while cursor_pos > 0 && chars[cursor_pos - 1].is_whitespace() {
                            cursor_pos -= 1;
                        }
                        while cursor_pos > 0 && !chars[cursor_pos - 1].is_whitespace() {
                            cursor_pos -= 1;
                        }
                    }
                    if sel_anchor == Some(cursor_pos) {
                        sel_anchor = None;
                    }
                    continue;
                }
                if idx < n && (kbuf[idx] == b'f' || kbuf[idx] == b'F') {
                    let is_shift = kbuf[idx] == b'F';
                    idx += 1;
                    if is_shift {
                        if sel_anchor.is_none() {
                            sel_anchor = Some(cursor_pos);
                        }
                    } else {
                        sel_anchor = None;
                    }
                    if let Some((_, end)) = image_token_span_at_cursor(&chars, cursor_pos) {
                        cursor_pos = end;
                    } else {
                        while cursor_pos < chars.len() && chars[cursor_pos].is_whitespace() {
                            cursor_pos += 1;
                        }
                        while cursor_pos < chars.len() && !chars[cursor_pos].is_whitespace() {
                            cursor_pos += 1;
                        }
                    }
                    if sel_anchor == Some(cursor_pos) {
                        sel_anchor = None;
                    }
                    continue;
                }
                if idx + 1 < n && (kbuf[idx] == b'[' || kbuf[idx] == b'O') {
                    // Check for modified CSI sequence like `\x1b[1;2D` (Shift+Left), `\x1b[2D` (rxvt/iTerm), `\x1b[1;3D` (Alt+Left)
                    let csi_slice = &kbuf[idx + 1..n];
                    let parsed_mod_csi: Option<(usize, u8, u8)> =
                        if csi_slice.starts_with(b"1;") && csi_slice.len() >= 4 {
                            Some((5, csi_slice[2], csi_slice[3]))
                        } else if csi_slice.len() >= 2
                            && matches!(csi_slice[0], b'2'..=b'9')
                            && matches!(csi_slice[1], b'A' | b'B' | b'C' | b'D' | b'H' | b'F')
                        {
                            Some((3, csi_slice[0], csi_slice[1]))
                        } else if kbuf[idx] == b'['
                            && !csi_slice.is_empty()
                            && matches!(csi_slice[0], b'a' | b'b' | b'c' | b'd')
                        {
                            // rxvt Shift+Arrows: \x1b[a, \x1b[b, \x1b[c, \x1b[d
                            Some((2, b'2', csi_slice[0].to_ascii_uppercase()))
                        } else {
                            None
                        };
                    if let Some((advance, mod_byte, final_b)) = parsed_mod_csi {
                        if matches!(mod_byte, b'2'..=b'9')
                            && matches!(final_b, b'A' | b'B' | b'C' | b'D' | b'H' | b'F')
                        {
                            idx += advance;
                            let mod_val = mod_byte - b'0';
                            let has_shift = ((mod_val - 1) & 1) != 0;
                            let has_alt_or_ctrl = ((mod_val - 1) & 6) != 0;
                            if has_shift {
                                if sel_anchor.is_none() {
                                    sel_anchor = Some(cursor_pos);
                                }
                            } else {
                                sel_anchor = None;
                            }
                            match final_b {
                                b'D' => {
                                    if has_alt_or_ctrl {
                                        if let Some((start, _)) =
                                            image_token_span_before_cursor(&chars, cursor_pos)
                                        {
                                            cursor_pos = start;
                                        } else {
                                            while cursor_pos > 0
                                                && chars[cursor_pos - 1].is_whitespace()
                                            {
                                                cursor_pos -= 1;
                                            }
                                            while cursor_pos > 0
                                                && !chars[cursor_pos - 1].is_whitespace()
                                            {
                                                cursor_pos -= 1;
                                            }
                                        }
                                    } else if let Some((start, _)) =
                                        image_token_span_before_cursor(&chars, cursor_pos)
                                    {
                                        cursor_pos = start;
                                    } else {
                                        cursor_pos = cursor_pos.saturating_sub(1);
                                    }
                                }
                                b'C' => {
                                    if has_alt_or_ctrl {
                                        if let Some((_, end)) =
                                            image_token_span_at_cursor(&chars, cursor_pos)
                                        {
                                            cursor_pos = end;
                                        } else {
                                            while cursor_pos < chars.len()
                                                && chars[cursor_pos].is_whitespace()
                                            {
                                                cursor_pos += 1;
                                            }
                                            while cursor_pos < chars.len()
                                                && !chars[cursor_pos].is_whitespace()
                                            {
                                                cursor_pos += 1;
                                            }
                                        }
                                    } else if let Some((_, end)) =
                                        image_token_span_at_cursor(&chars, cursor_pos)
                                    {
                                        cursor_pos = end;
                                    } else if cursor_pos < chars.len() {
                                        cursor_pos += 1;
                                    }
                                }
                                b'H' | b'A' => {
                                    cursor_pos = 0;
                                }
                                b'F' | b'B' => {
                                    cursor_pos = chars.len();
                                }
                                _ => {}
                            }
                            if sel_anchor == Some(cursor_pos) {
                                sel_anchor = None;
                            }
                            continue;
                        }
                    }

                    let csi_code = kbuf[idx + 1];
                    idx += 2;
                    let cur_buf_s: String = chars.iter().collect();
                    let cur_suggestions = compute_input_suggestions(&cur_buf_s);
                    match csi_code {
                        b'Z' => {
                            *access = access.next();
                            let _ = PromptEvent::CycleAccess;
                        }
                        b'A' => {
                            if sel_anchor.is_some() {
                                sel_anchor = None;
                            } else if !cur_suggestions.is_empty() {
                                let cur = suggestion_idx.unwrap_or(0);
                                let prev = if cur == 0 {
                                    cur_suggestions.len() - 1
                                } else {
                                    cur - 1
                                };
                                suggestion_idx = Some(prev);
                            } else if chars.is_empty()
                                && (crate::queued_message_count() > 0
                                    || crate::steer_message_count() > 0)
                            {
                                if let Some((_, popped)) = crate::pop_last_pending_message() {
                                    chars = popped.chars().collect();
                                    cursor_pos = chars.len();
                                }
                            } else if !history.is_empty() && hist_idx > 0 {
                                hist_idx -= 1;
                                chars = history[hist_idx].chars().collect();
                                cursor_pos = chars.len();
                            }
                        }
                        b'B' => {
                            if sel_anchor.is_some() {
                                sel_anchor = None;
                            } else if !cur_suggestions.is_empty() {
                                let cur = suggestion_idx.unwrap_or(0);
                                let next = (cur + 1) % cur_suggestions.len();
                                suggestion_idx = Some(next);
                            } else if hist_idx + 1 < history.len() {
                                hist_idx += 1;
                                chars = history[hist_idx].chars().collect();
                                cursor_pos = chars.len();
                            } else {
                                hist_idx = history.len();
                                chars.clear();
                                cursor_pos = 0;
                            }
                        }
                        b'C' => {
                            if let Some(a) = sel_anchor.take() {
                                cursor_pos = a.max(cursor_pos);
                            } else if let Some((_, end)) =
                                image_token_span_at_cursor(&chars, cursor_pos)
                            {
                                cursor_pos = end;
                            } else if cursor_pos < chars.len() {
                                cursor_pos += 1;
                            }
                        }
                        b'D' => {
                            if let Some(a) = sel_anchor.take() {
                                cursor_pos = a.min(cursor_pos);
                            } else if let Some((start, _)) =
                                image_token_span_before_cursor(&chars, cursor_pos)
                            {
                                cursor_pos = start;
                            } else {
                                cursor_pos = cursor_pos.saturating_sub(1);
                            }
                        }
                        b'H' => {
                            sel_anchor = None;
                            cursor_pos = 0;
                        }
                        b'F' => {
                            sel_anchor = None;
                            cursor_pos = chars.len();
                        }
                        b'3' => {
                            if idx < n && kbuf[idx] == b'~' {
                                idx += 1;
                            }
                            if let Some(a) = sel_anchor.take() {
                                let s = a.min(cursor_pos);
                                let e = a.max(cursor_pos);
                                if s < e {
                                    chars.drain(s..e);
                                    cursor_pos = s;
                                    continue;
                                }
                            }
                            if let Some((start, end)) =
                                image_token_span_at_cursor(&chars, cursor_pos)
                            {
                                chars.drain(start..end);
                            } else if cursor_pos < chars.len() {
                                chars.remove(cursor_pos);
                            }
                        }
                        b'1' | b'7' => {
                            if idx < n && kbuf[idx] == b'~' {
                                idx += 1;
                            }
                            sel_anchor = None;
                            cursor_pos = 0;
                        }
                        b'4' | b'8' => {
                            if idx < n && kbuf[idx] == b'~' {
                                idx += 1;
                            }
                            sel_anchor = None;
                            cursor_pos = chars.len();
                        }
                        _ => {
                            while idx < n && !(b'@'..=b'~').contains(&kbuf[idx - 1]) {
                                idx += 1;
                            }
                        }
                    }
                }
                continue;
            }

            // Also strip any orphaned `[200~` or `[201~` if ESC was read in a prior chunk
            if b == b'[' && idx + 3 < n && &kbuf[idx..idx + 4] == b"200~" {
                in_paste = true;
                idx += 4;
                continue;
            }
            if b == b'[' && idx + 3 < n && &kbuf[idx..idx + 4] == b"201~" {
                in_paste = false;
                idx += 4;
                continue;
            }

            match b {
                3 => {
                    if !chars.is_empty() {
                        chars.clear();
                        cursor_pos = 0;
                        suggestion_idx = None;
                        break;
                    }
                    erase_inline_dock(prev_top_w, prev_cursor_col);
                    unsafe {
                        tcsetattr(0, TCSANOW, &orig_termios);
                    }
                    return PromptEvent::Exit;
                }
                4 => {
                    if chars.is_empty() {
                        erase_inline_dock(prev_top_w, prev_cursor_col);
                        unsafe {
                            tcsetattr(0, TCSANOW, &orig_termios);
                        }
                        return PromptEvent::Exit;
                    } else if cursor_pos < chars.len() {
                        chars.remove(cursor_pos);
                    }
                }
                17 => {
                    erase_inline_dock(prev_top_w, prev_cursor_col);
                    unsafe {
                        tcsetattr(0, TCSANOW, &orig_termios);
                    }
                    return PromptEvent::Exit;
                }
                18 => {
                    erase_inline_dock(prev_top_w, prev_cursor_col);
                    unsafe {
                        tcsetattr(0, TCSANOW, &orig_termios);
                    }
                    return PromptEvent::Reload;
                }
                92 => {
                    if chars.is_empty() && n == 1 {
                        erase_inline_dock(prev_top_w, prev_cursor_col);
                        unsafe {
                            tcsetattr(0, TCSANOW, &orig_termios);
                        }
                        return PromptEvent::CyclePhase;
                    } else {
                        chars.insert(cursor_pos, '\\');
                        cursor_pos += 1;
                    }
                }
                19 => {
                    erase_inline_dock(prev_top_w, prev_cursor_col);
                    unsafe {
                        tcsetattr(0, TCSANOW, &orig_termios);
                    }
                    return PromptEvent::ShowSkills;
                }
                b'\r' | b'\n' => {
                    // Never auto-submit when pasting (either bracketed paste or multi-byte chunk)
                    if in_paste || n > 1 {
                        if idx < n
                            && kbuf[idx..n].iter().any(|&c| c >= 32 && c != b'~')
                            && chars.last().copied() != Some(' ')
                        {
                            chars.insert(cursor_pos, ' ');
                            cursor_pos += 1;
                        }
                        continue;
                    }
                    let cur_buf: String = chars.iter().collect();
                    let cur_sug = compute_input_suggestions(&cur_buf);
                    if !cur_sug.is_empty() && !cur_buf.starts_with('/') {
                        if let Some(at_idx) = cur_buf.rfind('@') {
                            let sel = suggestion_idx.unwrap_or(0).min(cur_sug.len() - 1);
                            let chosen = &cur_sug[sel].0;
                            let new_buf = format!("{}{} ", &cur_buf[..at_idx], chosen);
                            chars = new_buf.chars().collect();
                            cursor_pos = chars.len();
                            suggestion_idx = None;
                            break;
                        }
                    }
                    let submitted = if !cur_sug.is_empty()
                        && cur_buf.starts_with('/')
                        && !cur_buf.contains(' ')
                    {
                        let sel = suggestion_idx.unwrap_or(0).min(cur_sug.len() - 1);
                        cur_sug[sel].0.clone()
                    } else {
                        cur_buf.trim().to_string()
                    };
                    if submitted.is_empty() {
                        chars.clear();
                        cursor_pos = 0;
                        needs_redraw = false;
                        continue;
                    }
                    print!("\x1b[?2004l");
                    erase_inline_dock(prev_top_w, prev_cursor_col);
                    unsafe {
                        tcsetattr(0, TCSANOW, &orig_termios);
                    }
                    return PromptEvent::Submit(submitted);
                }
                1 => {
                    // Ctrl+A: if not already selecting all, select all! Press again to jump to start.
                    if !chars.is_empty() && sel_anchor != Some(0) {
                        sel_anchor = Some(0);
                        cursor_pos = chars.len();
                    } else {
                        sel_anchor = None;
                        cursor_pos = 0;
                    }
                }
                5 => {
                    sel_anchor = None;
                    cursor_pos = chars.len();
                }
                2 => {
                    sel_anchor = None;
                    cursor_pos = cursor_pos.saturating_sub(1);
                }
                6 => {
                    sel_anchor = None;
                    if cursor_pos < chars.len() {
                        cursor_pos += 1;
                    }
                }
                23 => {
                    if let Some(a) = sel_anchor.take() {
                        let s = a.min(cursor_pos);
                        let e = a.max(cursor_pos);
                        if s < e {
                            chars.drain(s..e);
                            cursor_pos = s;
                            continue;
                        }
                    }
                    if let Some((start, end)) = image_token_span_before_cursor(&chars, cursor_pos) {
                        chars.drain(start..end);
                        cursor_pos = start;
                    } else {
                        while cursor_pos > 0 && chars[cursor_pos - 1].is_whitespace() {
                            chars.remove(cursor_pos - 1);
                            cursor_pos -= 1;
                        }
                        while cursor_pos > 0 && !chars[cursor_pos - 1].is_whitespace() {
                            chars.remove(cursor_pos - 1);
                            cursor_pos -= 1;
                        }
                    }
                }
                11 => {
                    // Ctrl+K: delete from cursor to end of input
                    sel_anchor = None;
                    chars.truncate(cursor_pos);
                    suggestion_idx = None;
                }
                127 | 8 => {
                    if let Some(a) = sel_anchor.take() {
                        let s = a.min(cursor_pos);
                        let e = a.max(cursor_pos);
                        if s < e {
                            chars.drain(s..e);
                            cursor_pos = s;
                            continue;
                        }
                    }
                    if let Some((start, end)) = image_token_span_before_cursor(&chars, cursor_pos) {
                        chars.drain(start..end);
                        cursor_pos = start;
                    } else if cursor_pos > 0 {
                        chars.remove(cursor_pos - 1);
                        cursor_pos -= 1;
                    }
                }
                21 => {
                    sel_anchor = None;
                    chars.clear();
                    cursor_pos = 0;
                    suggestion_idx = None;
                }
                b'\t' => {
                    let cur_buf: String = chars.iter().collect();
                    let cur_sug = compute_input_suggestions(&cur_buf);
                    if !cur_sug.is_empty() {
                        let sel = suggestion_idx.unwrap_or(0).min(cur_sug.len() - 1);
                        let chosen = &cur_sug[sel].0;
                        if cur_buf.starts_with('/') {
                            if cur_buf == *chosen && cur_sug.len() > 1 {
                                let next = (sel + 1) % cur_sug.len();
                                suggestion_idx = Some(next);
                                chars = cur_sug[next].0.chars().collect();
                            } else {
                                chars = chosen.chars().collect();
                            }
                            cursor_pos = chars.len();
                        } else if let Some(at_idx) = cur_buf.rfind('@') {
                            let new_buf = format!("{}{} ", &cur_buf[..at_idx], chosen);
                            chars = new_buf.chars().collect();
                            cursor_pos = chars.len();
                            suggestion_idx = None;
                        }
                    } else {
                        chars.insert(cursor_pos, ' ');
                        cursor_pos += 1;
                    }
                }
                b if b >= 32 => {
                    let width = if b < 0x80 {
                        1
                    } else if (b & 0xE0) == 0xC0 {
                        2
                    } else if (b & 0xF0) == 0xE0 {
                        3
                    } else if (b & 0xF8) == 0xF0 {
                        4
                    } else {
                        1
                    };
                    let mut utf8_bytes = vec![b];
                    for _ in 1..width {
                        if idx < n {
                            utf8_bytes.push(kbuf[idx]);
                            idx += 1;
                        } else {
                            let mut next_b = [0u8; 1];
                            if unsafe { read(0, next_b.as_mut_ptr() as *mut std::ffi::c_void, 1) }
                                == 1
                            {
                                utf8_bytes.push(next_b[0]);
                            }
                        }
                    }
                    if let Ok(s) = String::from_utf8(utf8_bytes) {
                        if let Some(a) = sel_anchor.take() {
                            let st = a.min(cursor_pos);
                            let en = a.max(cursor_pos);
                            if st < en {
                                chars.drain(st..en);
                                cursor_pos = st;
                            }
                        }
                        for ch in s.chars() {
                            chars.insert(cursor_pos, ch);
                            cursor_pos += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// LiveSpinner with Live Turn Intervention:
/// - Pressing `Esc` or `Ctrl+C` immediately interrupts/cancels the running turn.
/// - Typing any character while the turn is running opens an inline intervention bar where
///   `Tab` toggles between `⚡ STEER (Anında Araya Gir)` and `📥 QUEUE (İşlem Bitince Çalıştır)`!
pub struct LiveSpinner {
    stop_flag: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LiveSpinner {
    /// No-op spinner for quiet/scripted runs: renders nothing, accepts no input.
    pub fn silent() -> Self {
        Self {
            stop_flag: Arc::new(AtomicBool::new(true)),
            handle: None,
        }
    }

    pub fn start(label: &str) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&stop_flag);
        let initial_label = label.to_string();

        SPINNER_PAUSED.store(false, Ordering::SeqCst);
        let status_hdr_init = get_sticky_dock_status();
        if let Ok(mut state) = SPINNER_SCREEN.lock() {
            state.active = true;
            state.paused = false;
            state.label = initial_label;
            state.started_at = Some(Instant::now());
            state.partial_line.clear();
            state.partial_prefix.clear();
            state.partial_shown = false;
            state.thinking_box_open = false;
            state.thinking_chars = 0;
            if state.steer_mode.is_none() {
                state.steer_mode = Some(load_user_config().busy_input_mode != "queue");
            }
            let (_, cols) = term_size();
            rebuild_spinner_dock_lines(&mut state, &status_hdr_init, cols);
            let dock_up = state.dock_lines.len().saturating_sub(1).max(1);
            print!(
                "\x1b[?2004h\x1b[?2026h\x1b[?25l\x1b[?7l{}\x1b[{}A\r\x1b[?7h\x1b[?2026l",
                state.dock_lines.join("\n"),
                dock_up
            );
            let _ = io::stdout().flush();
        }

        let handle = thread::spawn(move || {
            let mut orig_termios = empty_termios();
            let has_tty = unsafe { tcgetattr(0, &mut orig_termios) == 0 };
            let _term_guard = has_tty.then(|| TermiosGuard(orig_termios));
            if has_tty {
                let mut raw = orig_termios;
                raw.c_lflag &= !(ICANON | ECHO);
                raw.c_cc[VMIN_IDX] = 0;
                raw.c_cc[VTIME_IDX] = 0;
                unsafe {
                    tcsetattr(0, TCSANOW, &raw);
                }
            }
            let mut kbuf = [0u8; 4096];
            let status_hdr = get_sticky_dock_status();
            let (_, mut last_cols) = term_size();

            while !flag_clone.load(Ordering::Relaxed) {
                if SPINNER_PAUSED.load(Ordering::Relaxed) {
                    if has_tty {
                        unsafe {
                            tcsetattr(0, TCSANOW, &orig_termios);
                        }
                    }
                    while SPINNER_PAUSED.load(Ordering::Relaxed)
                        && !flag_clone.load(Ordering::Relaxed)
                    {
                        thread::sleep(Duration::from_millis(50));
                    }
                    if flag_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    if has_tty {
                        let mut raw = orig_termios;
                        raw.c_lflag &= !(ICANON | ECHO);
                        raw.c_cc[VMIN_IDX] = 0;
                        raw.c_cc[VTIME_IDX] = 0;
                        unsafe {
                            tcsetattr(0, TCSANOW, &raw);
                        }
                    }
                }
                let mut should_break = false;
                if has_tty {
                    let n_read =
                        unsafe { read(0, kbuf.as_mut_ptr() as *mut std::ffi::c_void, kbuf.len()) };
                    if n_read > 0 {
                        let n = n_read as usize;
                        if let Ok(mut state) = SPINNER_SCREEN.lock() {
                            state.typing_cursor = state.typing_cursor.min(state.typing_buf.len());
                            if n == 1 && kbuf[0] == 27 {
                                if state.typing_sel_anchor.is_some() {
                                    state.typing_sel_anchor = None;
                                } else if state.is_typing || !state.typing_buf.is_empty() {
                                    state.typing_buf.clear();
                                    state.typing_cursor = 0;
                                    state.typing_sel_anchor = None;
                                    state.is_typing = false;
                                } else {
                                    crate::trigger_interrupt();
                                    should_break = true;
                                }
                            } else if kbuf[..n].contains(&3) {
                                if !state.typing_buf.is_empty() {
                                    state.typing_buf.clear();
                                    state.typing_cursor = 0;
                                    state.typing_sel_anchor = None;
                                    state.is_typing = false;
                                } else {
                                    crate::trigger_interrupt();
                                    should_break = true;
                                }
                            } else {
                                let mut bi = 0usize;
                                while bi < n {
                                    let b = kbuf[bi];
                                    if b == 27 {
                                        let rem = &kbuf[bi..n];
                                        if rem.starts_with(b"\x1b[200~")
                                            || rem.starts_with(b"\x1b[201~")
                                        {
                                            bi += 6;
                                            continue;
                                        }
                                        if bi + 1 < n && (kbuf[bi + 1] == 127 || kbuf[bi + 1] == 8)
                                        {
                                            if let Some(a) = state.typing_sel_anchor.take() {
                                                let s = a
                                                    .min(state.typing_cursor)
                                                    .min(state.typing_buf.len());
                                                let e = a
                                                    .max(state.typing_cursor)
                                                    .min(state.typing_buf.len());
                                                if s < e {
                                                    state.typing_buf.drain(s..e);
                                                    state.typing_cursor = s;
                                                }
                                            } else {
                                                let mut c = state.typing_cursor;
                                                while c > 0
                                                    && state.typing_buf[c - 1].is_whitespace()
                                                {
                                                    state.typing_buf.remove(c - 1);
                                                    c -= 1;
                                                }
                                                while c > 0
                                                    && !state.typing_buf[c - 1].is_whitespace()
                                                {
                                                    state.typing_buf.remove(c - 1);
                                                    c -= 1;
                                                }
                                                state.typing_cursor = c;
                                            }
                                            if state.typing_buf.is_empty() {
                                                state.is_typing = false;
                                            }
                                            bi += 2;
                                            continue;
                                        }
                                        if bi + 1 < n
                                            && (kbuf[bi + 1] == b'b' || kbuf[bi + 1] == b'B')
                                        {
                                            let is_shift = kbuf[bi + 1] == b'B';
                                            if is_shift {
                                                if state.typing_sel_anchor.is_none() {
                                                    state.typing_sel_anchor =
                                                        Some(state.typing_cursor);
                                                }
                                            } else {
                                                state.typing_sel_anchor = None;
                                            }
                                            let mut c = state.typing_cursor;
                                            while c > 0 && state.typing_buf[c - 1].is_whitespace() {
                                                c -= 1;
                                            }
                                            while c > 0 && !state.typing_buf[c - 1].is_whitespace()
                                            {
                                                c -= 1;
                                            }
                                            state.typing_cursor = c;
                                            if state.typing_sel_anchor == Some(state.typing_cursor)
                                            {
                                                state.typing_sel_anchor = None;
                                            }
                                            bi += 2;
                                            continue;
                                        }
                                        if bi + 1 < n
                                            && (kbuf[bi + 1] == b'f' || kbuf[bi + 1] == b'F')
                                        {
                                            let is_shift = kbuf[bi + 1] == b'F';
                                            if is_shift {
                                                if state.typing_sel_anchor.is_none() {
                                                    state.typing_sel_anchor =
                                                        Some(state.typing_cursor);
                                                }
                                            } else {
                                                state.typing_sel_anchor = None;
                                            }
                                            let mut c = state.typing_cursor;
                                            while c < state.typing_buf.len()
                                                && state.typing_buf[c].is_whitespace()
                                            {
                                                c += 1;
                                            }
                                            while c < state.typing_buf.len()
                                                && !state.typing_buf[c].is_whitespace()
                                            {
                                                c += 1;
                                            }
                                            state.typing_cursor = c;
                                            if state.typing_sel_anchor == Some(state.typing_cursor)
                                            {
                                                state.typing_sel_anchor = None;
                                            }
                                            bi += 2;
                                            continue;
                                        }
                                        bi += 1;
                                        if bi < n && (kbuf[bi] == b'[' || kbuf[bi] == b'O') {
                                            bi += 1;
                                            let csi_rem = &kbuf[bi..n];
                                            if csi_rem.starts_with(b"1;") && csi_rem.len() >= 4 {
                                                let mod_byte = csi_rem[2];
                                                let final_b = csi_rem[3];
                                                if matches!(mod_byte, b'2'..=b'9')
                                                    && matches!(
                                                        final_b,
                                                        b'A' | b'B' | b'C' | b'D' | b'H' | b'F'
                                                    )
                                                {
                                                    bi += 4;
                                                    let mod_val = mod_byte - b'0';
                                                    let has_shift = ((mod_val - 1) & 1) != 0;
                                                    let has_alt_or_ctrl = ((mod_val - 1) & 6) != 0;
                                                    if has_shift {
                                                        if state.typing_sel_anchor.is_none() {
                                                            state.typing_sel_anchor =
                                                                Some(state.typing_cursor);
                                                        }
                                                    } else {
                                                        state.typing_sel_anchor = None;
                                                    }
                                                    match final_b {
                                                        b'D' => {
                                                            if has_alt_or_ctrl {
                                                                let mut c = state.typing_cursor;
                                                                while c > 0
                                                                    && state.typing_buf[c - 1]
                                                                        .is_whitespace()
                                                                {
                                                                    c -= 1;
                                                                }
                                                                while c > 0
                                                                    && !state.typing_buf[c - 1]
                                                                        .is_whitespace()
                                                                {
                                                                    c -= 1;
                                                                }
                                                                state.typing_cursor = c;
                                                            } else {
                                                                state.typing_cursor = state
                                                                    .typing_cursor
                                                                    .saturating_sub(1);
                                                            }
                                                        }
                                                        b'C' => {
                                                            if has_alt_or_ctrl {
                                                                let mut c = state.typing_cursor;
                                                                while c < state.typing_buf.len()
                                                                    && state.typing_buf[c]
                                                                        .is_whitespace()
                                                                {
                                                                    c += 1;
                                                                }
                                                                while c < state.typing_buf.len()
                                                                    && !state.typing_buf[c]
                                                                        .is_whitespace()
                                                                {
                                                                    c += 1;
                                                                }
                                                                state.typing_cursor = c;
                                                            } else if state.typing_cursor
                                                                < state.typing_buf.len()
                                                            {
                                                                state.typing_cursor += 1;
                                                            }
                                                        }
                                                        b'H' | b'A' => state.typing_cursor = 0,
                                                        b'F' | b'B' => {
                                                            state.typing_cursor =
                                                                state.typing_buf.len()
                                                        }
                                                        _ => {}
                                                    }
                                                    if state.typing_sel_anchor
                                                        == Some(state.typing_cursor)
                                                    {
                                                        state.typing_sel_anchor = None;
                                                    }
                                                    continue;
                                                }
                                            }
                                            if bi < n {
                                                match kbuf[bi] {
                                                    b'A' if state.typing_buf.is_empty() => {
                                                        if let Some((was_steer, popped)) =
                                                            crate::pop_last_pending_message()
                                                        {
                                                            state.steer_mode = Some(was_steer);
                                                            state.typing_buf =
                                                                popped.chars().collect();
                                                            state.typing_cursor =
                                                                state.typing_buf.len();
                                                            state.typing_sel_anchor = None;
                                                            state.is_typing =
                                                                !state.typing_buf.is_empty();
                                                        }
                                                        bi += 1;
                                                        continue;
                                                    }
                                                    b'C' => {
                                                        if let Some(a) =
                                                            state.typing_sel_anchor.take()
                                                        {
                                                            state.typing_cursor = a
                                                                .max(state.typing_cursor)
                                                                .min(state.typing_buf.len());
                                                        } else if state.typing_cursor
                                                            < state.typing_buf.len()
                                                        {
                                                            state.typing_cursor += 1;
                                                        }
                                                        bi += 1;
                                                        continue;
                                                    }
                                                    b'D' => {
                                                        if let Some(a) =
                                                            state.typing_sel_anchor.take()
                                                        {
                                                            state.typing_cursor =
                                                                a.min(state.typing_cursor);
                                                        } else {
                                                            state.typing_cursor = state
                                                                .typing_cursor
                                                                .saturating_sub(1);
                                                        }
                                                        bi += 1;
                                                        continue;
                                                    }
                                                    b'H' => {
                                                        state.typing_sel_anchor = None;
                                                        state.typing_cursor = 0;
                                                        bi += 1;
                                                        continue;
                                                    }
                                                    b'F' => {
                                                        state.typing_sel_anchor = None;
                                                        state.typing_cursor =
                                                            state.typing_buf.len();
                                                        bi += 1;
                                                        continue;
                                                    }
                                                    b'3' if bi + 1 < n && kbuf[bi + 1] == b'~' => {
                                                        if state.typing_buf.is_empty() {
                                                            let _ =
                                                                crate::pop_last_pending_message();
                                                        } else if let Some(a) =
                                                            state.typing_sel_anchor.take()
                                                        {
                                                            let s = a
                                                                .min(state.typing_cursor)
                                                                .min(state.typing_buf.len());
                                                            let e = a
                                                                .max(state.typing_cursor)
                                                                .min(state.typing_buf.len());
                                                            if s < e {
                                                                state.typing_buf.drain(s..e);
                                                                state.typing_cursor = s;
                                                            }
                                                        } else if state.typing_cursor
                                                            < state.typing_buf.len()
                                                        {
                                                            let c_idx = state.typing_cursor;
                                                            state.typing_buf.remove(c_idx);
                                                        }
                                                        if state.typing_buf.is_empty() {
                                                            state.is_typing = false;
                                                        }
                                                        bi += 2;
                                                        continue;
                                                    }
                                                    _ => {}
                                                }
                                            }
                                            while bi < n && !(b'@'..=b'~').contains(&kbuf[bi]) {
                                                bi += 1;
                                            }
                                            if bi < n {
                                                bi += 1;
                                            }
                                        } else if state.typing_sel_anchor.is_some() {
                                            state.typing_sel_anchor = None;
                                        } else if state.is_typing || !state.typing_buf.is_empty() {
                                            state.typing_buf.clear();
                                            state.typing_cursor = 0;
                                            state.typing_sel_anchor = None;
                                            state.is_typing = false;
                                        }
                                        continue;
                                    }
                                    match b {
                                        b'\t' => {
                                            let cur = state.steer_mode.unwrap_or(false);
                                            state.steer_mode = Some(!cur);
                                            bi += 1;
                                        }
                                        b'\r' | b'\n' => {
                                            if n > 1 {
                                                if bi + 1 < n
                                                    && kbuf[bi + 1..n].iter().any(|&c| c >= 32)
                                                {
                                                    state.is_typing = true;
                                                    if state.typing_buf.last().copied() != Some(' ')
                                                    {
                                                        let c_pos = state
                                                            .typing_cursor
                                                            .min(state.typing_buf.len());
                                                        state.typing_buf.insert(c_pos, ' ');
                                                        state.typing_cursor = c_pos + 1;
                                                    }
                                                }
                                                bi += 1;
                                                continue;
                                            }
                                            let msg: String = state.typing_buf.iter().collect();
                                            let trimmed = msg.trim().to_string();
                                            state.typing_buf.clear();
                                            state.typing_cursor = 0;
                                            state.typing_sel_anchor = None;
                                            state.is_typing = false;
                                            if !trimmed.is_empty() {
                                                if trimmed.starts_with('/') {
                                                    if matches!(
                                                        trimmed.as_str(),
                                                        "/reload"
                                                            | "/r"
                                                            | "/reload bin"
                                                            | "/rb"
                                                            | "/exit"
                                                            | "/quit"
                                                            | "/x"
                                                            | "/clear"
                                                            | "/stop"
                                                            | "/cancel"
                                                    ) {
                                                        crate::trigger_interrupt();
                                                        should_break = true;
                                                    } else if matches!(
                                                        trimmed.as_str(),
                                                        "/qclear"
                                                            | "/queue clear"
                                                            | "/qcancel"
                                                            | "/queue cancel"
                                                    ) {
                                                        crate::clear_queued_messages();
                                                        crate::clear_steer_messages();
                                                    } else {
                                                        crate::push_queued_message(trimmed.clone());
                                                    }
                                                } else if state.steer_mode.unwrap_or(false) {
                                                    crate::push_steer_message(trimmed);
                                                } else {
                                                    crate::push_queued_message(trimmed);
                                                }
                                            }
                                            bi += 1;
                                        }
                                        1 => {
                                            if !state.typing_buf.is_empty()
                                                && state.typing_sel_anchor != Some(0)
                                            {
                                                state.typing_sel_anchor = Some(0);
                                                state.typing_cursor = state.typing_buf.len();
                                            } else {
                                                state.typing_sel_anchor = None;
                                                state.typing_cursor = 0;
                                            }
                                            bi += 1;
                                        }
                                        5 => {
                                            state.typing_sel_anchor = None;
                                            state.typing_cursor = state.typing_buf.len();
                                            bi += 1;
                                        }
                                        11 => {
                                            state.typing_sel_anchor = None;
                                            let c_pos =
                                                state.typing_cursor.min(state.typing_buf.len());
                                            state.typing_buf.truncate(c_pos);
                                            if state.typing_buf.is_empty() {
                                                state.is_typing = false;
                                            }
                                            bi += 1;
                                        }
                                        127 | 8 => {
                                            if let Some(a) = state.typing_sel_anchor.take() {
                                                let s = a
                                                    .min(state.typing_cursor)
                                                    .min(state.typing_buf.len());
                                                let e = a
                                                    .max(state.typing_cursor)
                                                    .min(state.typing_buf.len());
                                                if s < e {
                                                    state.typing_buf.drain(s..e);
                                                    state.typing_cursor = s;
                                                }
                                            } else {
                                                let c_pos =
                                                    state.typing_cursor.min(state.typing_buf.len());
                                                if let Some((start, end)) =
                                                    image_token_span_before_cursor(
                                                        &state.typing_buf,
                                                        c_pos,
                                                    )
                                                {
                                                    state.typing_buf.drain(start..end);
                                                    state.typing_cursor = start;
                                                } else if c_pos > 0 {
                                                    state.typing_buf.remove(c_pos - 1);
                                                    state.typing_cursor = c_pos - 1;
                                                }
                                            }
                                            if state.typing_buf.is_empty() {
                                                state.is_typing = false;
                                            }
                                            bi += 1;
                                        }
                                        21 => {
                                            if state.typing_buf.is_empty() {
                                                let _ = crate::pop_last_pending_message();
                                            } else {
                                                state.typing_buf.clear();
                                                state.typing_cursor = 0;
                                                state.typing_sel_anchor = None;
                                                state.is_typing = false;
                                            }
                                            bi += 1;
                                        }
                                        24 => {
                                            // Ctrl+X: cancel last queued/steer message immediately
                                            let _ = crate::pop_last_pending_message();
                                            bi += 1;
                                        }
                                        23 => {
                                            if let Some(a) = state.typing_sel_anchor.take() {
                                                let s = a
                                                    .min(state.typing_cursor)
                                                    .min(state.typing_buf.len());
                                                let e = a
                                                    .max(state.typing_cursor)
                                                    .min(state.typing_buf.len());
                                                if s < e {
                                                    state.typing_buf.drain(s..e);
                                                    state.typing_cursor = s;
                                                }
                                            } else {
                                                let mut c =
                                                    state.typing_cursor.min(state.typing_buf.len());
                                                while c > 0
                                                    && state.typing_buf[c - 1].is_whitespace()
                                                {
                                                    state.typing_buf.remove(c - 1);
                                                    c -= 1;
                                                }
                                                while c > 0
                                                    && !state.typing_buf[c - 1].is_whitespace()
                                                {
                                                    state.typing_buf.remove(c - 1);
                                                    c -= 1;
                                                }
                                                state.typing_cursor = c;
                                            }
                                            if state.typing_buf.is_empty() {
                                                state.is_typing = false;
                                            }
                                            bi += 1;
                                        }
                                        b if b >= 32 => {
                                            let char_len = if b < 0x80 {
                                                1
                                            } else if (b & 0xE0) == 0xC0 {
                                                2
                                            } else if (b & 0xF0) == 0xE0 {
                                                3
                                            } else if (b & 0xF8) == 0xF0 {
                                                4
                                            } else {
                                                1
                                            };
                                            let mut utf8_bytes = Vec::with_capacity(char_len);
                                            utf8_bytes.push(b);
                                            bi += 1;
                                            for _ in 1..char_len {
                                                if bi < n {
                                                    utf8_bytes.push(kbuf[bi]);
                                                    bi += 1;
                                                }
                                            }
                                            if let Ok(s) = std::str::from_utf8(&utf8_bytes) {
                                                if let Some(a) = state.typing_sel_anchor.take() {
                                                    let st = a
                                                        .min(state.typing_cursor)
                                                        .min(state.typing_buf.len());
                                                    let en = a
                                                        .max(state.typing_cursor)
                                                        .min(state.typing_buf.len());
                                                    if st < en {
                                                        state.typing_buf.drain(st..en);
                                                        state.typing_cursor = st;
                                                    }
                                                }
                                                for ch in s.chars() {
                                                    state.is_typing = true;
                                                    let c_pos = state
                                                        .typing_cursor
                                                        .min(state.typing_buf.len());
                                                    state.typing_buf.insert(c_pos, ch);
                                                    state.typing_cursor = c_pos + 1;
                                                }
                                                let cur_s: String =
                                                    state.typing_buf.iter().collect();
                                                if cur_s.contains('/') || cur_s.contains('~') {
                                                    let (collapsed, changed) =
                                                        crate::router::collapse_pasted_image_paths(
                                                            &cur_s,
                                                        );
                                                    if changed {
                                                        state.typing_buf =
                                                            collapsed.chars().collect();
                                                        state.typing_cursor =
                                                            state.typing_buf.len();
                                                        state.typing_sel_anchor = None;
                                                    }
                                                }
                                            }
                                        }
                                        _ => {
                                            bi += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if should_break {
                    break;
                }
                let (_, mut cols) = term_size();
                if cols != last_cols || crate::take_resized() {
                    let mut stable_cols = cols;
                    for _ in 0..8 {
                        thread::sleep(Duration::from_millis(20));
                        let _ = crate::take_resized();
                        let (_, next_cols) = term_size();
                        if next_cols == stable_cols {
                            break;
                        }
                        stable_cols = next_cols;
                    }
                    cols = stable_cols;
                    last_cols = cols;
                }
                let max_w = cols.saturating_sub(6).max(20);

                if let Ok(mut state) = SPINNER_SCREEN.lock() {
                    if state.active && !state.paused {
                        rebuild_spinner_dock_lines(&mut state, &status_hdr, cols);
                        let dock_up = state.dock_lines.len().saturating_sub(1).max(1);
                        let mut out = String::from("\x1b[?2026h\x1b[?25l\x1b[?7l\r\x1b[J");
                        let up_rows = if !state.partial_line.is_empty() {
                            let (preview, _) = truncate_ansi_to_width(
                                &state.partial_line,
                                max_w.saturating_sub(2),
                            );
                            out.push_str(&preview);
                            out.push_str("\x1b[0m\n");
                            state.partial_shown = true;
                            dock_up + 1
                        } else {
                            state.partial_shown = false;
                            dock_up
                        };
                        out.push_str(&format!(
                            "{}\x1b[{}A\r\x1b[?7h\x1b[?2026l",
                            state.dock_lines.join("\n"),
                            up_rows
                        ));
                        print!("{}", out);
                        let _ = io::stdout().flush();
                    }
                }
                thread::sleep(Duration::from_millis(80));
            }
            if has_tty {
                unsafe {
                    tcsetattr(0, TCSANOW, &orig_termios);
                }
            }
            SPINNER_PAUSED.store(false, Ordering::SeqCst);
            if let Ok(mut state) = SPINNER_SCREEN.lock() {
                close_thinking_box_locked(&mut state);
                if state.active && !state.paused {
                    let mut out = String::from("\x1b[?2026h\x1b[?7l\r\x1b[J");
                    if !state.partial_line.is_empty() {
                        out.push_str("\x1b[?7h");
                        out.push_str(&state.partial_line);
                        out.push_str("\x1b[0m\n");
                        state.partial_line.clear();
                    }
                    state.partial_shown = false;
                    out.push_str("\x1b[?2004l\x1b[?7h\x1b[?25h\x1b[?2026l");
                    print!("{}", out);
                    let _ = io::stdout().flush();
                } else {
                    print!("\x1b[?2004l");
                    let _ = io::stdout().flush();
                }
                state.active = false;
                state.paused = false;
            }
        });

        Self {
            stop_flag,
            handle: Some(handle),
        }
    }

    pub fn stop(&mut self) {
        if !self.stop_flag.swap(true, Ordering::Relaxed) {
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
}

impl Drop for LiveSpinner {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn fetch_quick_quota_line() -> String {
    let cfg = load_user_config();
    if let Some(ref script_raw) = cfg.quota_quick_script {
        let script_path = expand_user_path(script_raw);
        if script_path.exists() {
            let node_bin = if Path::new("/opt/homebrew/bin/node").exists() {
                "/opt/homebrew/bin/node"
            } else {
                "node"
            };
            if let Ok(out) = Command::new(node_bin)
                .arg(script_path.to_string_lossy().to_string())
                .output()
            {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    tr_en("BYOK & Yerel Hazır (/byok)", "BYOK & Local Ready (/byok)").to_string()
}

pub fn render_welcome_header(
    _endpoint: &ModelEndpoint,
    _access: AccessMode,
    _default_host: &str,
    cwd: &str,
    session_name: &str,
) {
    let (_, cols) = term_size();
    let full_w = cols.saturating_sub(2).max(6);
    let home = std::env::var("HOME").unwrap_or_default();
    let short_cwd = if !home.is_empty() && cwd.starts_with(&home) {
        let rest = &cwd[home.len()..];
        if rest.is_empty() {
            "~".to_string()
        } else {
            format!("~{}", rest)
        }
    } else {
        cwd.to_string()
    };
    let short_session = session_name.trim_end_matches(".jsonl");
    let git_branch = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty());

    let p_top = format!("{}╭─{} ", CYAN, RESET);
    let p_bot = format!("{}╰─{} ", CYAN, RESET);
    let sep = format!(" {}·{} ", DIM, RESET);
    let mut row1_items = vec![
        format!("{}⚡ SPAVN v{}{}", BOLD, env!("CARGO_PKG_VERSION"), RESET),
        format!("📂 {}{}{}", DIM, short_cwd, RESET),
    ];
    if let Some(br) = git_branch {
        row1_items.push(format!("🌿 {}{}{}", GREEN, br, RESET));
    }
    row1_items.push(format!(
        "💬 {}{}{}",
        DIM,
        short_session.trim_start_matches("session-"),
        RESET
    ));

    println!();
    for line in flex_wrap_items(&p_top, &p_bot, &sep, &row1_items, full_w) {
        println!("{}", line);
    }
}

fn bar_for_pct(pct: f64) -> String {
    let filled = ((pct / 10.0).round() as usize).min(10);
    let empty = 10 - filled;
    let color = if pct >= 50.0 {
        GREEN
    } else if pct >= 20.0 {
        YELLOW
    } else {
        RED
    };
    format!(
        "{}{}{}{}",
        color,
        "█".repeat(filled),
        "░".repeat(empty),
        RESET
    )
}

pub fn render_full_quota_card() {
    let cfg = load_user_config();
    let Some(ref script_raw) = cfg.quota_full_script else {
        println!(
            "\n{}ℹ️  Harici kota betiği tanımlı değil (~/.spavn/config.json -> quota_full_script). BYOK sağlayıcı durumu için `/byok` komutunu kullanın.{}",
            YELLOW, RESET
        );
        return;
    };
    let script_path = expand_user_path(script_raw);
    if !script_path.exists() {
        println!(
            "\n{}Kota betiği bulunamadı: {}{}",
            YELLOW,
            script_path.display(),
            RESET
        );
        return;
    }

    let mut spin = LiveSpinner::start("Çoklu hesap kotaları sorgulanıyor");
    let node_bin = if Path::new("/opt/homebrew/bin/node").exists() {
        "/opt/homebrew/bin/node"
    } else {
        "node"
    };
    let out = Command::new(node_bin)
        .arg(script_path.to_string_lossy().to_string())
        .output();
    spin.stop();

    let Ok(o) = out else {
        println!("{}Kota bilgisi alınamadı.{}", RED, RESET);
        return;
    };
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&o.stdout) else {
        println!("{}Kota yanıtı ayrıştırılamadı.{}", RED, RESET);
        return;
    };

    println!(
        "\n{}╭─ 🔋 SPAVN KOTA PANELİ ─────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
    if let Some(arr) = parsed.as_array() {
        for acc in arr {
            let email = acc.get("email").and_then(|v| v.as_str()).unwrap_or("?");
            let active = acc.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
            let badge = if active { "🟢 [AKTİF]" } else { "⚪" };
            println!("{}│{} {} {}{}{}", CYAN, RESET, badge, BOLD, email, RESET);
            if let Some(groups) = acc.get("groups").and_then(|v| v.as_array()) {
                for g in groups {
                    let gname = g.get("displayName").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(buckets) = g.get("buckets").and_then(|v| v.as_array()) {
                        for b in buckets {
                            let bname = b.get("displayName").and_then(|v| v.as_str()).unwrap_or("");
                            let pct = b
                                .get("remainingPercent")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            println!(
                                "{}│{}    ├─ {:<18} {:<26} {} {:>5.1}%",
                                CYAN,
                                RESET,
                                gname,
                                bname,
                                bar_for_pct(pct),
                                pct
                            );
                        }
                    }
                }
            }
        }
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}

pub fn render_fleet_radar() {
    let cfg = load_user_config();
    let mut spin = LiveSpinner::start("Tanımlı çalışma hedefleri (hosts) taranıyor");

    let handles: Vec<_> = cfg
        .hosts
        .into_iter()
        .map(|h| {
            thread::spawn(move || {
                let start = Instant::now();
                let res = if h.ssh_target == "local" || h.key == "local" {
                    Command::new("uptime").output()
                } else {
                    Command::new("ssh")
                        .args(["-o", "ConnectTimeout=3", &h.ssh_target, "uptime"])
                        .output()
                };
                let ms = start.elapsed().as_millis();
                match res {
                    Ok(o) if o.status.success() => {
                        let up = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        (h.key, true, ms, up)
                    }
                    _ => (h.key, false, ms, "Ulaşılamadı".to_string()),
                }
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    spin.stop();

    println!(
        "\n{}╭─ 🛰️  SPAVN HEDEF SUNUCU RADARI (~/.spavn/config.json) ─────────────────────{}",
        CYAN, RESET
    );
    for (label, ok, ms, up) in results {
        let status = if ok {
            format!("{}🟢 ONLINE ({:>3}ms){}", GREEN, ms, RESET)
        } else {
            format!("{}🔴 OFFLINE{}", RED, RESET)
        };
        println!(
            "{}│{} {:<17} │ {} │ {}{}{}",
            CYAN, RESET, label, status, DIM, up, RESET
        );
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub key: String,
    pub badge: String,
    pub desc: String,
}

pub fn select_interactive_menu(
    title: &str,
    items: &[MenuItem],
    initial_idx: usize,
) -> Option<String> {
    if items.is_empty() {
        return None;
    }

    let mut orig_termios = empty_termios();

    let has_tty = unsafe { tcgetattr(0, &mut orig_termios) == 0 };
    if !has_tty {
        return Some(items[initial_idx.min(items.len() - 1)].key.clone());
    }

    let _term_guard = TermiosGuard(orig_termios);
    let mut raw = orig_termios;
    raw.c_lflag &= !(ICANON | ECHO);
    raw.c_cc[VMIN_IDX] = 0;
    raw.c_cc[VTIME_IDX] = 1;
    unsafe {
        tcsetattr(0, TCSANOW, &raw);
    }

    let mut selected = initial_idx.min(items.len() - 1);
    let max_vis_items = items.len().min(12);
    let total_lines = max_vis_items + 2;

    print!("\n\x1b[?25l");
    for _ in 0..total_lines {
        println!();
    }

    let mut stdin = io::stdin();
    let mut kbuf = [0u8; 8];
    let mut result: Option<String> = None;
    let _ = crate::take_resized();
    let (mut last_rows, mut last_cols) = term_size();
    let mut needs_redraw = true;

    loop {
        let (rows, cols) = term_size();
        if (rows, cols) != (last_rows, last_cols) || crate::take_resized() {
            thread::sleep(Duration::from_millis(16));
            let (r2, c2) = term_size();
            last_rows = r2;
            last_cols = c2;
            needs_redraw = true;
        }

        if needs_redraw {
            let max_w = last_cols.saturating_sub(2).max(20);
            let inner_w = max_w.saturating_sub(2);
            let mut out = format!("\x1b[?2026h\x1b[?7l\x1b[{}A\r\x1b[J", total_lines);
            let raw_hdr = format!(
                "{}╭─ {} {}(↑/↓ · Enter · Esc){} ",
                CYAN, title, YELLOW, CYAN
            );
            let (trunc_hdr, used_hdr) = truncate_ansi_to_width(&raw_hdr, max_w.saturating_sub(1));
            let hdr_pad = "─".repeat(max_w.saturating_sub(used_hdr + 1));
            out.push_str(&format!("{}{}{}╮{}\n", trunc_hdr, CYAN, hdr_pad, RESET));

            let key_col_w = items
                .iter()
                .map(|it| truncate_ansi_to_width(&it.key, 26).1)
                .max()
                .unwrap_or(10)
                .clamp(10, 22);
            let badge_col_w = items
                .iter()
                .map(|it| truncate_ansi_to_width(&it.badge, 30).1)
                .max()
                .unwrap_or(12)
                .clamp(10, 24);

            let win_start = if selected >= max_vis_items {
                selected + 1 - max_vis_items
            } else {
                0
            };

            let end = (win_start + max_vis_items).min(items.len());
            for (i, item) in items.iter().enumerate().take(end).skip(win_start) {
                let (k_str, k_used) = truncate_ansi_to_width(&item.key, key_col_w);
                let k_pad = " ".repeat(key_col_w.saturating_sub(k_used));
                let (b_str, b_used) = truncate_ansi_to_width(&item.badge, badge_col_w);
                let b_pad = " ".repeat(badge_col_w.saturating_sub(b_used));
                let fixed_used = 4 + key_col_w + 3 + badge_col_w + 3;
                let desc_budget = inner_w.saturating_sub(fixed_used);

                let row_inner = if desc_budget >= 8 && !item.desc.is_empty() {
                    let (d_str, _) = truncate_ansi_to_width(&item.desc, desc_budget);
                    if i == selected {
                        format!(
                            " \x1b[1;36m❯\x1b[0m \x1b[1;7;36m {}{} \x1b[0m {}│{} {}{} {}│{} {}{}{}",
                            k_str, k_pad, DIM, RESET, b_str, b_pad, DIM, RESET, BOLD, d_str, RESET
                        )
                    } else {
                        format!(
                            "    {}{} {}│{} {}{} {}│{} {}{}{}",
                            k_str, k_pad, DIM, RESET, b_str, b_pad, DIM, RESET, DIM, d_str, RESET
                        )
                    }
                } else if i == selected {
                    format!(
                        " \x1b[1;36m❯\x1b[0m \x1b[1;7;36m {}{} \x1b[0m {}│{} {}{}",
                        k_str, k_pad, DIM, RESET, b_str, b_pad
                    )
                } else {
                    format!(
                        "    {}{} {}│{} {}{}",
                        k_str, k_pad, DIM, RESET, b_str, b_pad
                    )
                };

                let (trunc_inner, used_inner) = truncate_ansi_to_width(&row_inner, inner_w);
                let right_pad = " ".repeat(inner_w.saturating_sub(used_inner));
                out.push_str(&format!(
                    "{}│{}{}{}{}│{}\n",
                    CYAN, RESET, trunc_inner, right_pad, CYAN, RESET
                ));
            }
            let bot_pad = "─".repeat(inner_w);
            out.push_str(&format!(
                "{}╰{}╯{}\n\x1b[?7h\x1b[?2026l",
                CYAN, bot_pad, RESET
            ));
            print!("{}", out);
            let _ = io::stdout().flush();
            needs_redraw = false;
        }

        let n = match stdin.read(&mut kbuf) {
            Ok(0) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                needs_redraw = true;
                continue;
            }
            Err(_) => break,
            Ok(mut n) => {
                if n == 1 && kbuf[0] == 27 {
                    thread::sleep(Duration::from_millis(25));
                    if let Ok(n2) = stdin.read(&mut kbuf[1..]) {
                        n += n2;
                    }
                }
                needs_redraw = true;
                n
            }
        };

        if n == 1 && kbuf[0] == 27 {
            break;
        }
        if kbuf[0] == 27 && n >= 3 && (kbuf[1] == b'[' || kbuf[1] == b'O') {
            match kbuf[2] {
                b'A' | b'Z' => {
                    selected = if selected == 0 {
                        items.len() - 1
                    } else {
                        selected - 1
                    };
                }
                b'B' => {
                    selected = (selected + 1) % items.len();
                }
                _ => {}
            }
            continue;
        }

        match kbuf[0] {
            b'\r' | b'\n' => {
                result = Some(items[selected].key.clone());
                break;
            }
            3 | b'q' | b'Q' => {
                break;
            }
            b'k' | b'K' => {
                selected = if selected == 0 {
                    items.len() - 1
                } else {
                    selected - 1
                };
            }
            b'j' | b'J' | b'\t' => {
                selected = (selected + 1) % items.len();
            }
            _ => {}
        }
    }

    print!("\x1b[{}A\r\x1b[J\x1b[?7h\x1b[?25h", total_lines);
    let _ = io::stdout().flush();
    unsafe {
        tcsetattr(0, TCSANOW, &orig_termios);
    }
    result
}

pub fn interactive_select_slash_command() -> Option<String> {
    let items = vec![
        MenuItem {
            key: "/model".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("🧠 Sağlayıcı & Model", "🧠 Provider & Model"),
                RESET
            ),
            desc: tr_en(
                "Sağlayıcı (←/→) ve AI modelini (↑/↓) iki sütunda seç",
                "Select provider (←/→) and AI model (↑/↓) in dual columns",
            )
            .to_string(),
        },
        MenuItem {
            key: "/byok".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🔑 BYOK Anahtar/URL", "🔑 BYOK Key/URL"),
                RESET
            ),
            desc: tr_en(
                "OpenAI/Anthropic/Gemini/DeepSeek/Özel API anahtarı ve URL ekle",
                "Add OpenAI/Anthropic/Gemini/DeepSeek/Custom API key & URL",
            )
            .to_string(),
        },
        MenuItem {
            key: "/local".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🟢 Yerel Modeller", "🟢 Local Models"),
                RESET
            ),
            desc: tr_en(
                "Yerel GPU/llama-swap/Ollama modellerini doğrudan aç",
                "Open local GPU/llama-swap/Ollama models directly",
            )
            .to_string(),
        },
        MenuItem {
            key: "/queue".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("📥 Mesaj Kuyruğu", "📥 Message Queue"),
                RESET
            ),
            desc: tr_en(
                "Bekleyen mesajları düzenle/sırala ve Anında(Steer)/Kuyruk modunu seç",
                "Manage queued messages and toggle Live Steer / Queue mode",
            )
            .to_string(),
        },
        MenuItem {
            key: "/lang".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("🌐 Dil / Language", "🌐 Language / Dil"),
                RESET
            ),
            desc: tr_en(
                "CLI arayüz dilini değiştir (Türkçe ⇄ English)",
                "Switch CLI interface language (English ⇄ Türkçe)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/resume".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("🗂️  Oturumlar", "🗂️  Sessions"),
                RESET
            ),
            desc: tr_en(
                "Geçmiş konuşmaları önizlemeli olarak seç ve yükle",
                "Browse and resume recent sessions with preview",
            )
            .to_string(),
        },
        MenuItem {
            key: "/recap".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("※ Oturum Özeti", "※ Session Recap"),
                RESET
            ),
            desc: tr_en(
                "Oturumun son adım özetini göster veya otomatik boşta kalma (idle) özetini aç/kapat",
                "Show current session recap or toggle automatic idle recap line",
            )
            .to_string(),
        },
        MenuItem {
            key: "/new".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("✨ Yeni Oturum", "✨ New Session"),
                RESET
            ),
            desc: tr_en("Temiz yeni bir oturum başlat", "Start a fresh session").to_string(),
        },
        MenuItem {
            key: "/fork".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("🌿 Dallandır", "🌿 Fork Session"),
                RESET
            ),
            desc: tr_en(
                "Mevcut konuşmayı yeni oturum dosyasına klonla",
                "Clone current conversation into a new session branch",
            )
            .to_string(),
        },
        MenuItem {
            key: "/role".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("🎭 Uzmanlık Rolü", "🎭 Agent Role"),
                RESET
            ),
            desc: "Auto / Coder / Reviewer / Security / Test / Docs / Refactor".to_string(),
        },
        MenuItem {
            key: "/plan".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("📐 PLAN Fazı", "📐 PLAN Phase"),
                RESET
            ),
            desc: tr_en(
                "Salt-okunur mimari tasarım fazına geç (write/edit/bash kilitli, kısayol: \\)",
                "Switch to read-only planning phase (write/edit/bash locked, shortcut: \\)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/code".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("💻 CODE Fazı", "💻 CODE Phase"),
                RESET
            ),
            desc: tr_en(
                "Tam uygulama fazına geç (tüm araçlar aktif, kısayol: \\)",
                "Switch to coding/implementation phase (all tools active, shortcut: \\)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/janitor".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🧹 Kalite Kapıları", "🧹 Quality Gates"),
                RESET
            ),
            desc: tr_en(
                "Spavn Janitor (cargo check / tsc / lint / test) çalıştır",
                "Run Spavn Janitor (cargo check / tsc / lint / test)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/audit".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("⛓️  HMAC Audit İzi", "⛓️  HMAC Audit Ledger"),
                RESET
            ),
            desc: tr_en(
                "Hangi prompt ve model hangi dosyayı değiştirdi (zincir doğrulamalı)",
                "Inspect cryptographic HMAC chain of file edits by prompt & model",
            )
            .to_string(),
        },
        MenuItem {
            key: "/wiki".to_string(),
            badge: format!("{}📖 Repo Wiki{}", CYAN, RESET),
            desc: tr_en(
                "Proje modül ve dosya mimarisi haritasını çıkar",
                "Generate repository module & architecture map",
            )
            .to_string(),
        },
        MenuItem {
            key: "/pin".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("📌 Bağlam Sabitle", "📌 Pin Context"),
                RESET
            ),
            desc: tr_en(
                "Proje için kalıcı mimari kural/not sabitle (/pins)",
                "Pin persistent architectural rules/notes for this repo (/pins)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/goal".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🎯 Proje Hedefleri", "🎯 Project Goals"),
                RESET
            ),
            desc: tr_en(
                "Aktif proje hedeflerini görüntüle ve yönet (/goals)",
                "View and manage active project goals (/goals)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/host".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("🖥️  Hedef Cihaz", "🖥️  Target Host"),
                RESET
            ),
            desc: tr_en(
                "Varsayılan çalışma hedefini seç",
                "Select default execution host",
            )
            .to_string(),
        },
        MenuItem {
            key: "/skills".to_string(),
            badge: format!("{}{}{}", YELLOW, tr_en("🧩 Yetenekler", "🧩 Skills"), RESET),
            desc: tr_en(
                "Uzman yeteneklerden (@skills) seç ve oturuma yükle",
                "Select and activate specialized @skills into session",
            )
            .to_string(),
        },
        MenuItem {
            key: "/access".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("🛡️  Yetki Modu", "🛡️  Access Mode"),
                RESET
            ),
            desc: tr_en(
                "SAFE / AUTO / SOVEREIGN erişim modunu seç",
                "Select SAFE / AUTO / SOVEREIGN execution mode",
            )
            .to_string(),
        },
        MenuItem {
            key: "/compact".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("🗜️  Sıkıştır", "🗜️  Compact"),
                RESET
            ),
            desc: tr_en(
                "Bağlamı özetleyerek token kullanımını düşür",
                "Compact conversation context to reduce token usage",
            )
            .to_string(),
        },
        MenuItem {
            key: "/undo".to_string(),
            badge: format!("{}{}{}", YELLOW, tr_en("↩️  Geri Al", "↩️  Undo"), RESET),
            desc: tr_en(
                "Son dosya değişikliğini anında geri al",
                "Revert the most recent file modification",
            )
            .to_string(),
        },
        MenuItem {
            key: "/reload".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🔄 Canlı Reload", "🔄 Live Reload"),
                RESET
            ),
            desc: tr_en(
                "Yeni derlenen binary ve yetenekleri oturumu koruyarak yükle",
                "Hot-reload binary and skills while preserving current session",
            )
            .to_string(),
        },
        MenuItem {
            key: "/exit".to_string(),
            badge: format!("{}{}{}", RED, tr_en("🚪 Çıkış", "🚪 Exit"), RESET),
            desc: tr_en("Oturumu kaydederek çık", "Save session and exit").to_string(),
        },
    ];
    select_interactive_menu(
        tr_en(
            "⚡ SPAVN v0.1 KOMUT MENÜSÜ (spavn.dev)",
            "⚡ SPAVN v0.1 COMMAND MENU (spavn.dev)",
        ),
        &items,
        0,
    )
}

/// Dual-Column Master-Detail Model Picker (`←`/`→` Provider, `↑`/`↓` Model, `Enter` Select, `Esc` Cancel).
/// Combines user-configured providers (`~/.spavn/config.json`), dynamically discovered local models,
/// and the built-in 10-Provider BYOK Catalog!
pub fn interactive_select_model(current_key: &str, local_first: bool) -> Option<String> {
    let cfg = load_user_config();
    let mut providers: Vec<(String, Vec<MenuItem>)> = Vec::new();

    // 1. User-configured provider groups from ~/.spavn/config.json
    for group in &cfg.providers {
        let items: Vec<MenuItem> = group
            .models
            .iter()
            .map(|m| MenuItem {
                key: m.key.clone(),
                badge: m.badge.clone(),
                desc: m.desc.clone(),
            })
            .collect();
        if !items.is_empty() {
            providers.push((format!("{} ({})", group.title, items.len()), items));
        }
    }

    // 2. Dynamic Local Model Discovery (if `local_discovery_url` is configured)
    let mut local_idx_in_providers = None;
    if let Some(ref disc_url) = cfg.local_discovery_url {
        let mut spin = LiveSpinner::start(tr_en(
            "Yerel model sunucusu taranıyor",
            "Scanning local model server",
        ));
        let discovered = crate::router::discover_local_models(disc_url, true);
        spin.stop();

        let mut local_models: Vec<MenuItem> = Vec::new();
        for m in discovered {
            let badge = if m.loaded {
                format!(
                    "{}{}🟢 {}{}",
                    BOLD,
                    GREEN,
                    tr_en("VRAM'DE AKTİF", "ACTIVE IN VRAM"),
                    RESET
                )
            } else {
                format!(
                    "{}⚪ {}{}",
                    DIM,
                    tr_en("Hazır (Yüklenir)", "Ready (On-Demand)"),
                    RESET
                )
            };
            let ctx_label = format_token_short(m.context_window.unwrap_or(131_072));
            local_models.push(MenuItem {
                key: format!("local:{}", m.id),
                badge,
                desc: format!(
                    "{} · {} · {} ctx",
                    m.spec_badge,
                    tr_en("Yerel GPU", "Local GPU"),
                    ctx_label
                ),
            });
        }
        local_models.sort_by_key(|m| if m.badge.contains("🟢") { 0 } else { 1 });
        if !local_models.is_empty() {
            local_idx_in_providers = Some(providers.len());
            providers.push((
                format!(
                    "🟢 {} ({})",
                    tr_en("Yerel GPU Sunucusu", "Local GPU Server"),
                    local_models.len()
                ),
                local_models,
            ));
        }
    }

    // 3. Built-in BYOK Catalog Providers (OpenAI, Anthropic, Gemini, OpenRouter, DeepSeek, MiniMax, GLM, Groq, xAI, Ollama)
    for byok_group in builtin_byok_catalog() {
        if byok_group.id == "ollama" && local_idx_in_providers.is_some() {
            continue;
        }
        let has_key = byok_group.is_local || resolve_api_key(&byok_group.api_key_env).is_some();
        let key_status = if has_key {
            format!("{}🔑 Hazır{}", GREEN, RESET)
        } else {
            format!("{}🔒 /byok ile anahtar gir{}", YELLOW, RESET)
        };
        let items: Vec<MenuItem> = byok_group
            .models
            .iter()
            .map(|m| MenuItem {
                key: m.key.clone(),
                badge: format!("{} · {}", m.badge, key_status),
                desc: m.desc.clone(),
            })
            .collect();
        providers.push((format!("{} ({})", byok_group.title, items.len()), items));
    }

    if providers.is_empty() {
        return None;
    }

    let mut orig_termios = empty_termios();
    let has_tty = unsafe { tcgetattr(0, &mut orig_termios) == 0 };
    if !has_tty {
        return Some(current_key.to_string());
    }

    let _term_guard = TermiosGuard(orig_termios);
    let mut raw = orig_termios;
    raw.c_lflag &= !(ICANON | ECHO);
    raw.c_cc[VMIN_IDX] = 0;
    raw.c_cc[VTIME_IDX] = 1;
    unsafe {
        tcsetattr(0, TCSANOW, &raw);
    }

    let mut prov_idx = if local_first {
        local_idx_in_providers.unwrap_or(0)
    } else {
        0
    };
    let mut model_indices = vec![0usize; providers.len()];
    for (pi, (_, list)) in providers.iter().enumerate() {
        if let Some(mi) = list
            .iter()
            .position(|m| m.key == current_key || m.key.strip_prefix("local:") == Some(current_key))
        {
            if !local_first {
                prov_idx = pi;
            }
            model_indices[pi] = mi;
        }
    }
    let mut focus_right = local_first;

    let body_rows = 10usize;
    let total_lines = body_rows + 4;

    print!("\n\x1b[?25l");
    for _ in 0..total_lines {
        println!();
    }

    let mut stdin = io::stdin();
    let mut kbuf = [0u8; 8];
    let mut result: Option<String> = None;
    let _ = crate::take_resized();
    let (mut last_rows, mut last_cols) = term_size();
    let mut needs_redraw = true;

    loop {
        let (rows, cols) = term_size();
        if (rows, cols) != (last_rows, last_cols) || crate::take_resized() {
            thread::sleep(Duration::from_millis(16));
            let (r2, c2) = term_size();
            last_rows = r2;
            last_cols = c2;
            needs_redraw = true;
        }

        if needs_redraw {
            let max_w = last_cols.saturating_sub(2).max(20);
            let right_inner_w = max_w.saturating_sub(33);
            let mut out = format!("\x1b[?2026h\x1b[?7l\x1b[{}A\r\x1b[J", total_lines);
            let raw_hdr = format!(
                "{}╭─ {} {}(←/→: Sütun · ↑/↓: Seç · Enter · Esc){} ",
                CYAN,
                tr_en("🧠 MODEL SEÇİMİ", "🧠 SELECT MODEL"),
                YELLOW,
                CYAN
            );
            let (trunc_hdr, used_hdr) = truncate_ansi_to_width(&raw_hdr, max_w.saturating_sub(1));
            let hdr_pad = "─".repeat(max_w.saturating_sub(used_hdr + 1));
            out.push_str(&format!("{}{}{}╮{}\n", trunc_hdr, CYAN, hdr_pad, RESET));

            let left_hdr_style = if !focus_right {
                tr_en(
                    "\x1b[1;36m❯ SAĞLAYICI (PROVIDER)\x1b[0m      ",
                    "\x1b[1;36m❯ PROVIDER            \x1b[0m      ",
                )
            } else {
                tr_en(
                    "\x1b[2m  SAĞLAYICI (← Geri)\x1b[0m        ",
                    "\x1b[2m  PROVIDER (← Back)   \x1b[0m      ",
                )
            };
            let right_hdr_style = if focus_right {
                tr_en(
                    "\x1b[1;36m❯ MODELLER (↑/↓ Seç · Enter: Uygula)\x1b[0m",
                    "\x1b[1;36m❯ MODELS (↑/↓ Select · Enter: Apply)\x1b[0m",
                )
            } else {
                tr_en(
                    "\x1b[2m  MODELLER (→ Sağ ok ile geç)\x1b[0m",
                    "\x1b[2m  MODELS (→ Right arrow to focus)\x1b[0m",
                )
            };
            let (r_hdr_trunc, r_hdr_used) =
                truncate_ansi_to_width(right_hdr_style, right_inner_w.saturating_sub(1));
            let r_hdr_pad = " ".repeat(right_inner_w.saturating_sub(r_hdr_used + 1));
            let col_hdr = format!(
                "{}│{} {} {}│{} {}{}{}│{}",
                CYAN, RESET, left_hdr_style, CYAN, RESET, r_hdr_trunc, r_hdr_pad, CYAN, RESET
            );
            out.push_str(&format!("{}\n", col_hdr));

            let sep_right = "─".repeat(right_inner_w);
            let raw_sep = format!(
                "{}├──────────────────────────────┼{}┤{}",
                CYAN, sep_right, RESET
            );
            out.push_str(&format!("{}\n", raw_sep));

            let p_win_start = if prov_idx >= body_rows {
                prov_idx + 1 - body_rows
            } else {
                0
            };

            let cur_models = &providers[prov_idx].1;
            let cur_m_idx = model_indices[prov_idx].min(cur_models.len().saturating_sub(1));
            let m_win_start = if cur_m_idx >= body_rows {
                cur_m_idx + 1 - body_rows
            } else {
                0
            };

            for r in 0..body_rows {
                let p_idx = p_win_start + r;
                let left_cell = if let Some((pname, _)) = providers.get(p_idx) {
                    let (trunc_p, used_p) = truncate_ansi_to_width(pname, 25);
                    let pad_p = " ".repeat(25usize.saturating_sub(used_p));
                    if p_idx == prov_idx {
                        if !focus_right {
                            format!(
                                " \x1b[1;36m❯\x1b[0m \x1b[1;7;36m {}{} \x1b[0m",
                                trunc_p, pad_p
                            )
                        } else {
                            format!(
                                " \x1b[1;32m▸\x1b[0m \x1b[1;36m {}{} \x1b[0m",
                                trunc_p, pad_p
                            )
                        }
                    } else {
                        format!("    {}{}{}{} ", DIM, trunc_p, pad_p, RESET)
                    }
                } else {
                    " ".repeat(30)
                };

                let m_idx = m_win_start + r;
                let right_cell = if let Some(m) = cur_models.get(m_idx) {
                    let (trunc_k, used_k) = truncate_ansi_to_width(&m.key, 24);
                    let pad_k = " ".repeat(24usize.saturating_sub(used_k));
                    let (trunc_b, used_b) = truncate_ansi_to_width(&m.badge, 22);
                    let pad_b = " ".repeat(22usize.saturating_sub(used_b));
                    if m_idx == cur_m_idx {
                        if focus_right {
                            format!(
                                "\x1b[1;36m❯\x1b[0m \x1b[1;7;36m {}{} \x1b[0m {}│{} {}{} {}│{} {}{}{}",
                                trunc_k, pad_k, DIM, RESET, trunc_b, pad_b, DIM, RESET, BOLD, m.desc, RESET
                            )
                        } else {
                            format!(
                                "\x1b[1;36m▸\x1b[0m  {}{}{}{}  {}│{} {}{} {}│{} {}{}{}",
                                BOLD,
                                trunc_k,
                                pad_k,
                                RESET,
                                DIM,
                                RESET,
                                trunc_b,
                                pad_b,
                                DIM,
                                RESET,
                                DIM,
                                m.desc,
                                RESET
                            )
                        }
                    } else {
                        format!(
                            "   {}{}{}  {}│{} {}{} {}│{} {}{}{}",
                            trunc_k,
                            pad_k,
                            RESET,
                            DIM,
                            RESET,
                            trunc_b,
                            pad_b,
                            DIM,
                            RESET,
                            DIM,
                            m.desc,
                            RESET
                        )
                    }
                } else {
                    String::new()
                };

                let (r_trunc, r_used) =
                    truncate_ansi_to_width(&right_cell, right_inner_w.saturating_sub(1));
                let r_pad = " ".repeat(right_inner_w.saturating_sub(r_used + 1));
                let full_row = format!(
                    "{}│{}{}{}│{} {}{}{}│{}",
                    CYAN, RESET, left_cell, CYAN, RESET, r_trunc, r_pad, CYAN, RESET
                );
                out.push_str(&format!("{}\n", full_row));
            }

            let bot_right = "─".repeat(right_inner_w);
            let raw_bot = format!(
                "{}╰──────────────────────────────┴{}╯{}",
                CYAN, bot_right, RESET
            );
            out.push_str(&format!("{}\n\x1b[?7h\x1b[?2026l", raw_bot));
            print!("{}", out);
            let _ = io::stdout().flush();
            needs_redraw = false;
        }

        let n = match stdin.read(&mut kbuf) {
            Ok(0) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                needs_redraw = true;
                continue;
            }
            Err(_) => break,
            Ok(mut n) => {
                if n == 1 && kbuf[0] == 27 {
                    thread::sleep(Duration::from_millis(25));
                    if let Ok(n2) = stdin.read(&mut kbuf[1..]) {
                        n += n2;
                    }
                }
                needs_redraw = true;
                n
            }
        };

        if n == 1 && kbuf[0] == 27 {
            break;
        }

        if kbuf[0] == 27 && n >= 3 && (kbuf[1] == b'[' || kbuf[1] == b'O') {
            match kbuf[2] {
                b'A' => {
                    if !focus_right {
                        prov_idx = if prov_idx == 0 {
                            providers.len() - 1
                        } else {
                            prov_idx - 1
                        };
                    } else {
                        let len = providers[prov_idx].1.len();
                        if len > 0 {
                            let cur = model_indices[prov_idx];
                            model_indices[prov_idx] = if cur == 0 { len - 1 } else { cur - 1 };
                        }
                    }
                }
                b'B' => {
                    if !focus_right {
                        prov_idx = (prov_idx + 1) % providers.len();
                    } else {
                        let len = providers[prov_idx].1.len();
                        if len > 0 {
                            model_indices[prov_idx] = (model_indices[prov_idx] + 1) % len;
                        }
                    }
                }
                b'C' => {
                    focus_right = true;
                }
                b'D' => {
                    focus_right = false;
                }
                b'Z' => {
                    focus_right = !focus_right;
                }
                _ => {}
            }
            continue;
        }

        match kbuf[0] {
            b'\r' | b'\n' => {
                if !focus_right {
                    focus_right = true;
                } else {
                    let cur_models = &providers[prov_idx].1;
                    let cur_m_idx = model_indices[prov_idx].min(cur_models.len().saturating_sub(1));
                    if let Some(m) = cur_models.get(cur_m_idx) {
                        result = Some(m.key.clone());
                    }
                    break;
                }
            }
            b'\t' => {
                focus_right = !focus_right;
            }
            b'l' | b'L' => {
                focus_right = true;
            }
            b'h' | b'H' => {
                focus_right = false;
            }
            b'k' | b'K' => {
                if !focus_right {
                    prov_idx = if prov_idx == 0 {
                        providers.len() - 1
                    } else {
                        prov_idx - 1
                    };
                } else {
                    let len = providers[prov_idx].1.len();
                    if len > 0 {
                        let cur = model_indices[prov_idx];
                        model_indices[prov_idx] = if cur == 0 { len - 1 } else { cur - 1 };
                    }
                }
            }
            b'j' | b'J' => {
                if !focus_right {
                    prov_idx = (prov_idx + 1) % providers.len();
                } else {
                    let len = providers[prov_idx].1.len();
                    if len > 0 {
                        model_indices[prov_idx] = (model_indices[prov_idx] + 1) % len;
                    }
                }
            }
            3 | b'q' | b'Q' => {
                break;
            }
            _ => {}
        }
    }

    print!("\x1b[{}A\r\x1b[J\x1b[?7h\x1b[?25h", total_lines);
    let _ = io::stdout().flush();
    unsafe {
        tcsetattr(0, TCSANOW, &orig_termios);
    }
    result
}

fn prompt_inline_line(prompt_label: &str) -> Option<String> {
    print!("{}{}{}: {}", CYAN, BOLD, prompt_label, RESET);
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).ok()? == 0 {
        return None;
    }
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

fn provider_portal_url(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "openai" => Some("https://platform.openai.com/api-keys"),
        "anthropic" => Some("https://console.anthropic.com/settings/keys"),
        "gemini" => Some("https://aistudio.google.com/app/apikey"),
        "openrouter" => Some("https://openrouter.ai/keys"),
        "deepseek" => Some("https://platform.deepseek.com/api_keys"),
        "minimax" => Some("https://platform.minimax.io"),
        "glm" => Some("https://open.bigmodel.cn/usercenter/apikeys"),
        "groq" => Some("https://console.groq.com/keys"),
        "xai" => Some("https://console.x.ai"),
        "microsoft-github" => Some("https://github.com/settings/tokens"),
        _ => None,
    }
}

fn detect_cli_token_for_provider(provider_id: &str) -> Option<(String, &'static str)> {
    match provider_id {
        "microsoft-github" => {
            let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
            if out.status.success() {
                let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !tok.is_empty() {
                    return Some((tok, "GitHub CLI (`gh auth token`)"));
                }
            }
            None
        }
        "gemini" => {
            let out = Command::new("gcloud")
                .args(["auth", "print-access-token"])
                .output()
                .ok()?;
            if out.status.success() {
                let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !tok.is_empty() {
                    return Some((tok, "Google Cloud CLI (`gcloud auth`)"));
                }
            }
            None
        }
        _ => None,
    }
}

/// Interactive BYOK / Login / Provider Connection Manager (`/login`, `/provider`, `/connect`, `/byok`):
/// Allows any user to connect providers via auto-detected CLI auth (`gh`, `gcloud`), one-click browser
/// portal launch, API key entry (saved in `~/.spavn/secrets.env` with 0600), local server discovery,
/// OR custom OpenAI-compatible Base URL + Model + API Key!
pub fn interactive_provider_login(preselected_provider: Option<&str>) -> Option<String> {
    let catalog = builtin_byok_catalog();
    let chosen = if let Some(pre) = preselected_provider
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let low = pre.to_ascii_lowercase();
        if matches!(low.as_str(), "local" | "m3" | "llama" | "vllm" | "lmstudio") {
            "local-server".to_string()
        } else if matches!(low.as_str(), "custom" | "url") {
            "custom-provider".to_string()
        } else if matches!(low.as_str(), "github" | "copilot" | "microsoft" | "azure") {
            "microsoft-github".to_string()
        } else if matches!(low.as_str(), "claude" | "anthropic") {
            "anthropic".to_string()
        } else if matches!(low.as_str(), "google" | "gemini") {
            "gemini".to_string()
        } else if let Some(g) = catalog.iter().find(|g| g.id.eq_ignore_ascii_case(&low)) {
            g.id.clone()
        } else {
            low
        }
    } else {
        let cfg = load_user_config();
        let mut items: Vec<MenuItem> = vec![MenuItem {
            key: "local-server".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🟢 Yerel Sunucu", "🟢 Local Server"),
                RESET
            ),
            desc: tr_en(
                "llama-swap / Ollama / LM Studio / vLLM adresini tara ve bağlan",
                "Scan & connect llama-swap / Ollama / LM Studio / vLLM endpoint",
            )
            .to_string(),
        }];
        items.extend(
            catalog
                .iter()
                .filter(|g| g.id != "ollama" || cfg.local_discovery_url.is_none())
                .map(|g| {
                    let has_key = g.is_local || resolve_api_key(&g.api_key_env).is_some();
                    let badge = if g.is_local {
                        format!(
                            "{}{}{}",
                            GREEN,
                            tr_en("🟢 Yerel (Hazır)", "🟢 Local (Ready)"),
                            RESET
                        )
                    } else if has_key {
                        format!("{}{}{}", GREEN, tr_en("🟢 Bağlı", "🟢 Connected"), RESET)
                    } else {
                        format!(
                            "{}{}{}",
                            YELLOW,
                            tr_en("🔗 Bağlan / Giriş", "🔗 Connect / Login"),
                            RESET
                        )
                    };
                    let clean_title = g.title.replace(" (BYOK)", "");
                    MenuItem {
                        key: g.id.clone(),
                        badge,
                        desc: format!("{} · {}", clean_title, g.base_url),
                    }
                }),
        );

        items.push(MenuItem {
            key: "custom-provider".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("➕ Özel Sağlayıcı Ekle", "➕ Add Custom Provider"),
                RESET
            ),
            desc: tr_en(
                "Kendi OpenAI/Azure-uyumlu Base URL, Model ve API Anahtarınızı bağlayın",
                "Connect your own OpenAI/Azure-compatible Base URL, Model ID, and API Key",
            )
            .to_string(),
        });

        select_interactive_menu(
            tr_en(
                "🔐 SPAVN HIZLI BAĞLANTI & SAĞLAYICI MERKEZİ (/login · /provider · /byok)",
                "🔐 SPAVN QUICK CONNECT & PROVIDER HUB (/login · /provider · /byok)",
            ),
            &items,
            0,
        )?
    };

    if chosen == "local-server" {
        let cfg = load_user_config();
        return interactive_select_model(&cfg.default_model, true);
    }

    if chosen == "custom-provider" {
        println!(
            "\n{}╭─ {} (~/.spavn/config.json) ───────────────────────────{}",
            CYAN,
            tr_en("➕ ÖZEL SAĞLAYICI EKLE", "➕ ADD CUSTOM PROVIDER"),
            RESET
        );
        let prov_name = prompt_inline_line(tr_en(
            "│ Sağlayıcı Kısa Adı (ör. my-vllm, fireworks, together)",
            "│ Provider Short ID (e.g. my-vllm, fireworks, together)",
        ))?;
        let base_url = prompt_inline_line(tr_en(
            "│ Base URL (ör. https://api.together.xyz/v1 veya http://127.0.0.1:8000/v1)",
            "│ Base URL (e.g. https://api.together.xyz/v1 or http://127.0.0.1:8000/v1)",
        ))?;
        let model_id = prompt_inline_line(tr_en(
            "│ Model Kimliği (ör. meta-llama/Llama-3.3-70B-Instruct-Turbo)",
            "│ Model ID (e.g. meta-llama/Llama-3.3-70B-Instruct-Turbo)",
        ))?;
        let api_key_opt = prompt_inline_line(tr_en(
            "│ API Anahtarı (Yerel ise boş bırakıp Enter'a basın)",
            "│ API Key (Leave empty and press Enter if local)",
        ));

        let env_var_name = format!(
            "{}_API_KEY",
            prov_name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                })
                .collect::<String>()
        );
        if let Some(ref secret_val) = api_key_opt {
            let _ = upsert_spavn_secret(&env_var_name, secret_val);
        }

        let mut cfg = load_user_config();
        let entry_key = format!("{}:{}", prov_name, model_id);
        cfg.providers.push(ProviderGroupConfig {
            id: prov_name.clone(),
            title: format!("🔌 {} (Custom BYOK)", prov_name),
            base_url,
            api_key_env: if api_key_opt.is_some() {
                env_var_name
            } else {
                String::new()
            },
            is_local: api_key_opt.is_none(),
            context_window: None,
            models: vec![ModelConfigEntry {
                key: entry_key.clone(),
                model_id: model_id.clone(),
                label: format!("{} ({})", model_id, prov_name),
                badge: "🔌 Custom".to_string(),
                desc: tr_en("Kullanıcı tanımlı sağlayıcı", "User-defined provider").to_string(),
                context_window: None,
            }],
        });
        let _ = save_user_config(&cfg);
        println!(
            "{}╰─ ✅ {}: {}{}{}",
            CYAN,
            tr_en(
                "Özel sağlayıcı kaydedildi ve aktif edildi",
                "Custom provider saved and activated"
            ),
            GREEN,
            entry_key,
            RESET
        );
        return Some(entry_key);
    }

    let group = catalog.into_iter().find(|g| g.id == chosen)?;
    if !group.is_local {
        let mut key_configured = false;
        if let Some((auto_tok, source_label)) = detect_cli_token_for_provider(&group.id) {
            let _ = upsert_spavn_secret(&group.api_key_env, &auto_tok);
            println!(
                "\n{}╭─ 🔐 {} ───────────────────────────────────────────{}\n{}│{} {}⚡ {} üzerinden oturum otomatik algılandı ve bağlandı!{}\n{}╰────────────────────────────────────────────────────────────────────────────{}",
                CYAN,
                group.title,
                RESET,
                CYAN,
                RESET,
                GREEN,
                source_label,
                RESET,
                CYAN,
                RESET
            );
            key_configured = true;
        }
        if !key_configured {
            let portal = provider_portal_url(&group.id);
            println!(
                "\n{}╭─ 🔐 {} {} (~/.spavn/secrets.env — 0600) ───────────{}",
                CYAN,
                group.title,
                tr_en("BAĞLANTI & GİRİŞ", "LOGIN & CONNECT"),
                RESET
            );
            if let Some(u) = portal {
                println!(
                    "{}│{} {}🌐 {}: {}{}{} {}",
                    CYAN,
                    RESET,
                    DIM,
                    tr_en("Anahtar Sayfası", "Key Portal"),
                    CYAN,
                    u,
                    RESET,
                    tr_en(
                        "(Tarayıcıda açmak için 'o' yazıp Enter'a basın)",
                        "(Type 'o' + Enter to open in browser)"
                    )
                );
            }
            let prompt_msg = format!(
                "│ {} {}",
                group.api_key_env,
                tr_en(
                    "anahtarını yapıştırın ('o' = Tarayıcıda Aç, boş Enter = Mevcutu Koru)",
                    "paste key ('o' = Open Browser, empty Enter = Keep Existing)"
                )
            );
            if let Some(first_in) = prompt_inline_line(&prompt_msg) {
                let final_key = if first_in.eq_ignore_ascii_case("o")
                    || first_in.eq_ignore_ascii_case("open")
                {
                    if let Some(u) = portal {
                        open_url_in_browser(u);
                        println!(
                            "{}│{} {}🌐 {}{}",
                            CYAN,
                            RESET,
                            GREEN,
                            tr_en(
                                "Tarayıcı açıldı — anahtarı kopyalayıp aşağıya yapıştırın:",
                                "Browser opened — copy your key and paste below:"
                            ),
                            RESET
                        );
                    }
                    prompt_inline_line(&format!("│ {} ", group.api_key_env))
                } else {
                    Some(first_in)
                };
                if let Some(new_key) = final_key {
                    match upsert_spavn_secret(&group.api_key_env, &new_key) {
                        Ok(()) => println!(
                            "{}│{} {}✅ {} {}{}",
                            CYAN,
                            RESET,
                            GREEN,
                            group.api_key_env,
                            tr_en(
                                "güvenli olarak ~/.spavn/secrets.env dosyasına kaydedildi ve bağlandı.",
                                "securely saved to ~/.spavn/secrets.env and connected."
                            ),
                            RESET
                        ),
                        Err(e) => println!(
                            "{}│{} {}✗ {}: {}{}",
                            CYAN,
                            RESET,
                            RED,
                            tr_en("Hata", "Error"),
                            e,
                            RESET
                        ),
                    }
                }
            }
            println!(
                "{}╰────────────────────────────────────────────────────────────────────────────{}",
                CYAN, RESET
            );
        }
    }

    let model_items: Vec<MenuItem> = group
        .models
        .iter()
        .map(|m| MenuItem {
            key: m.key.clone(),
            badge: m.badge.clone(),
            desc: m.desc.clone(),
        })
        .collect();
    select_interactive_menu(
        &format!(
            "🧠 {} — {}",
            group.title,
            tr_en("MODEL SEÇ", "SELECT MODEL")
        ),
        &model_items,
        0,
    )
}

pub enum QueueAction {
    None,
    EditMessage(String),
}

/// Interactive Queue & Busy-Turn Mode Manager (`/queue`):
/// Allows the user to:
/// 1) Switch the default behavior when typing during a running turn (`⚡ Anında Araya Gir (Steer)` vs `📥 Sıraya Al (Queue)`),
/// 2) Inspect, Edit (`✏️ Düzenle`), Promote (`⬆️ En Üste Al`), or Delete (`🗑️ Sil`) queued messages!
pub fn interactive_queue_manager() -> QueueAction {
    loop {
        let mut cfg = load_user_config();
        let queued = crate::list_queued_messages();
        let mode_label = if cfg.busy_input_mode == "queue" {
            format!(
                "{}{}{}",
                GREEN,
                tr_en(
                    "📥 SIRAYA AL (QUEUE — İşlem bitince sırayla çalıştır)",
                    "📥 QUEUE (Run sequentially after active turn finishes)"
                ),
                RESET
            )
        } else {
            format!(
                "{}{}{}",
                YELLOW,
                tr_en(
                    "⚡ ANINDA MÜDAHALE (STEER — Aktif adımda araya gir)",
                    "⚡ LIVE STEER (Inject immediately into active turn)"
                ),
                RESET
            )
        };

        let mut items = vec![
            MenuItem {
                key: "__toggle_mode__".to_string(),
                badge: format!(
                    "{}{}{}",
                    CYAN,
                    tr_en("⚙️  Varsayılan Mod", "⚙️  Default Mode"),
                    RESET
                ),
                desc: format!(
                    "{}: {} ({})",
                    tr_en("Aktif", "Active"),
                    mode_label,
                    tr_en("Değiştirmek için Enter", "Press Enter to toggle")
                ),
            },
            MenuItem {
                key: "__add_queue__".to_string(),
                badge: format!(
                    "{}{}{}",
                    GREEN,
                    tr_en("➕ Kuyruğa Ekle", "➕ Add to Queue"),
                    RESET
                ),
                desc: tr_en(
                    "Sıraya yeni bir görev/mesaj ekle",
                    "Enqueue a new task/message",
                )
                .to_string(),
            },
        ];

        for (i, msg) in queued.iter().enumerate() {
            let short: String = msg.chars().take(52).collect();
            items.push(MenuItem {
                key: format!("item:{}", i),
                badge: format!(
                    "{}{} #{}{}",
                    YELLOW,
                    tr_en("📥 Sıra", "📥 Queue"),
                    i + 1,
                    RESET
                ),
                desc: format!(
                    "\"{}\" ({})",
                    short,
                    tr_en("Düzenle / Üste Al / Sil", "Edit / Promote / Delete")
                ),
            });
        }

        if !queued.is_empty() {
            items.push(MenuItem {
                key: "__clear_queue__".to_string(),
                badge: format!(
                    "{}{}{}",
                    RED,
                    tr_en("🗑️  Tümünü Temizle", "🗑️  Clear All"),
                    RESET
                ),
                desc: format!(
                    "{} ({})",
                    tr_en("Kuyruktaki tüm mesajları sil", "Remove all queued messages"),
                    queued.len()
                ),
            });
        }

        let Some(chosen) = select_interactive_menu(
            tr_en(
                "📥 SPAVN MESAJ KUYRUĞU & CANLI MÜDAHALE AYARLARI (/queue)",
                "📥 SPAVN MESSAGE QUEUE & LIVE STEERING SETTINGS (/queue)",
            ),
            &items,
            0,
        ) else {
            return QueueAction::None;
        };

        if chosen == "__toggle_mode__" {
            cfg.busy_input_mode = if cfg.busy_input_mode == "queue" {
                "steer".to_string()
            } else {
                "queue".to_string()
            };
            let _ = save_user_config(&cfg);
            continue;
        }

        if chosen == "__add_queue__" {
            if let Some(new_msg) = prompt_inline_line(tr_en(
                "📥 Kuyruğa eklenecek mesajı yazın",
                "📥 Enter message to add to queue",
            )) {
                crate::push_queued_message(new_msg);
            }
            continue;
        }

        if chosen == "__clear_queue__" {
            crate::clear_queued_messages();
            continue;
        }

        if let Some(idx_str) = chosen.strip_prefix("item:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if let Some(msg_text) = queued.get(idx).cloned() {
                    let sub_items = vec![
                        MenuItem {
                            key: "edit".to_string(),
                            badge: format!(
                                "{}{}{}",
                                CYAN,
                                tr_en("✏️  Metni Düzenle", "✏️  Edit Text"),
                                RESET
                            ),
                            desc: tr_en(
                                "Mesajı ana giriş kutusuna çekip düzenle",
                                "Pull message into main input box to edit",
                            )
                            .to_string(),
                        },
                        MenuItem {
                            key: "promote".to_string(),
                            badge: format!(
                                "{}{}{}",
                                GREEN,
                                tr_en("⬆️  1. Sıraya Al", "⬆️  Promote to Top"),
                                RESET
                            ),
                            desc: tr_en("Kuyruğun en başına taşı", "Move to top of queue")
                                .to_string(),
                        },
                        MenuItem {
                            key: "delete".to_string(),
                            badge: format!(
                                "{}{}{}",
                                RED,
                                tr_en("🗑️  Kuyruktan Sil", "🗑️  Remove"),
                                RESET
                            ),
                            desc: tr_en(
                                "Bu mesajı kuyruktan kaldır",
                                "Delete this message from queue",
                            )
                            .to_string(),
                        },
                    ];
                    if let Some(act) = select_interactive_menu(
                        &format!(
                            "📥 {} #{}",
                            tr_en("KUYRUK MESAJI İŞLEMİ", "QUEUE ITEM ACTION"),
                            idx + 1
                        ),
                        &sub_items,
                        0,
                    ) {
                        match act.as_str() {
                            "edit" => {
                                crate::remove_queued_message(idx);
                                return QueueAction::EditMessage(msg_text);
                            }
                            "promote" => {
                                crate::promote_queued_message(idx);
                            }
                            "delete" => {
                                crate::remove_queued_message(idx);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

pub fn interactive_select_host(current_host: &str) -> Option<String> {
    let cfg = load_user_config();
    let items: Vec<MenuItem> = cfg
        .hosts
        .into_iter()
        .map(|h| MenuItem {
            key: h.key,
            badge: format!("{}{}{}", CYAN, h.badge, RESET),
            desc: h.desc,
        })
        .collect();
    let initial_idx = items
        .iter()
        .position(|h| h.key == current_host)
        .unwrap_or(0);
    select_interactive_menu(
        tr_en("🖥️  HEDEF SUNUCU (HOST) SEÇİMİ", "🖥️  SELECT TARGET HOST"),
        &items,
        initial_idx,
    )
}

pub fn interactive_select_access(current_access: AccessMode) -> Option<AccessMode> {
    let items = vec![
        MenuItem {
            key: "safe".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("🔒 SAFE (Salt-Okunur)", "🔒 SAFE (Read-Only)"),
                RESET
            ),
            desc: tr_en(
                "Her dosya yazma ve kabuk komutu için onay ister",
                "Prompts for approval before every file write or shell command",
            )
            .to_string(),
        },
        MenuItem {
            key: "auto".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("⚡ AUTO (Dengeli Otonom)", "⚡ AUTO (Smart Autonomous)"),
                RESET
            ),
            desc: tr_en(
                "Geliştirme serbest; kritik komutlar onay ister",
                "Development edits allowed; critical commands require approval",
            )
            .to_string(),
        },
        MenuItem {
            key: "sovereign".to_string(),
            badge: format!(
                "{}{}{}",
                RED,
                tr_en("🔥 SOVEREIGN (Tam Yetki)", "🔥 SOVEREIGN (Full Authority)"),
                RESET
            ),
            desc: tr_en(
                "Tüm hedeflerde otonom icra",
                "Fully autonomous execution on all hosts",
            )
            .to_string(),
        },
    ];
    let cur_key = match current_access {
        AccessMode::Safe => "safe",
        AccessMode::Auto => "auto",
        AccessMode::Sovereign => "sovereign",
    };
    let initial_idx = items.iter().position(|a| a.key == cur_key).unwrap_or(1);
    select_interactive_menu(
        tr_en("🛡️  YETKİ MODU SEÇİMİ", "🛡️  SELECT ACCESS MODE"),
        &items,
        initial_idx,
    )
    .and_then(|k| AccessMode::from_str(&k))
}

pub fn interactive_select_language() -> Option<String> {
    let items = vec![
        MenuItem {
            key: "tr".to_string(),
            badge: format!("{}🇹🇷 Türkçe (TR){}", CYAN, RESET),
            desc: "CLI arayüzünü, menüleri ve durum çubuklarını Türkçe kullan".to_string(),
        },
        MenuItem {
            key: "en".to_string(),
            badge: format!("{}🇬🇧 English (EN){}", GREEN, RESET),
            desc: "Use English for CLI interface, menus, and status bars".to_string(),
        },
    ];
    let initial_idx = if is_en() { 1 } else { 0 };
    if let Some(chosen) = select_interactive_menu(
        tr_en(
            "🌐 DİL SEÇİMİ / LANGUAGE SELECTION (/lang)",
            "🌐 LANGUAGE SELECTION / DİL SEÇİMİ (/lang)",
        ),
        &items,
        initial_idx,
    ) {
        let mut cfg = load_user_config();
        cfg.language = chosen.clone();
        let _ = save_user_config(&cfg);
        return Some(chosen);
    }
    None
}

pub fn interactive_select_role(
    current_role: crate::spavn_engine::AgentRoleKey,
) -> Option<crate::spavn_engine::AgentRoleKey> {
    use crate::spavn_engine::AgentRoleKey;
    let roles = [
        (
            AgentRoleKey::Auto,
            tr_en("🎯 Otomatik Dispatcher", "🎯 Auto Dispatcher"),
            tr_en(
                "Prompt içeriğine göre en uygun uzmanlık rolünü otomatik seçer",
                "Automatically selects the best specialist role based on prompt",
            ),
        ),
        (
            AgentRoleKey::Coder,
            "💻 Coder",
            tr_en(
                "Minimum kapsamda, mevcut kod stiline uygun temiz uygulama",
                "Clean implementation with minimal blast radius adhering to code style",
            ),
        ),
        (
            AgentRoleKey::Reviewer,
            "🔍 Reviewer",
            tr_en(
                "Kenar durumları, yarış koşulları ve hataları dosya:satır ile bulur",
                "Finds edge cases, race conditions, and bugs with exact file:line citations",
            ),
        ),
        (
            AgentRoleKey::Security,
            "🛡️  Security",
            tr_en(
                "Secret sızıntısı, yetki atlama ve injection zafiyetlerini denetler",
                "Audits secret exposure, auth bypass, and injection vulnerabilities",
            ),
        ),
        (
            AgentRoleKey::Test,
            "🧪 Test",
            tr_en(
                "Deterministik, hızlı doğrulama ve sınır durumu testleri yazar",
                "Writes deterministic, fast verification and boundary condition tests",
            ),
        ),
        (
            AgentRoleKey::Docs,
            "📖 Docs",
            tr_en(
                "Yalnızca koda dayalı, sıfır şişirmeli teknik dokümantasyon",
                "Zero-fluff technical documentation grounded strictly in code",
            ),
        ),
        (
            AgentRoleKey::Refactor,
            "🧹 Refactor",
            tr_en(
                "Davranışı bozmadan mimari sadeleştirme ve ölü kod temizliği",
                "Behavior-preserving structural simplification and dead-code removal",
            ),
        ),
    ];
    let items: Vec<MenuItem> = roles
        .iter()
        .map(|(r, badge, desc)| MenuItem {
            key: r.as_str().to_string(),
            badge: format!("{}{}{}", CYAN, badge, RESET),
            desc: (*desc).to_string(),
        })
        .collect();
    let initial_idx = roles
        .iter()
        .position(|(r, _, _)| *r == current_role)
        .unwrap_or(0);
    select_interactive_menu(
        tr_en(
            "🎭 SPAVN UZMANLIK ROLÜ SEÇİMİ (/role)",
            "🎭 SELECT SPAVN SPECIALIST ROLE (/role)",
        ),
        &items,
        initial_idx,
    )
    .and_then(|k| AgentRoleKey::from_str(&k))
}

pub fn interactive_select_skill() -> Option<String> {
    let skills = crate::skills::discover_skills();
    if skills.is_empty() {
        return None;
    }
    let items: Vec<MenuItem> = skills
        .into_iter()
        .map(|s| MenuItem {
            key: s.name,
            badge: format!("{}{}{}", MAGENTA, s.source, RESET),
            desc: s.summary.chars().take(42).collect(),
        })
        .collect();
    select_interactive_menu(
        tr_en(
            "🧩 YETENEK (SKILL) SEÇİM MENÜSÜ",
            "🧩 SELECT SPECIALIST SKILL (@skills)",
        ),
        &items,
        0,
    )
}

pub fn interactive_select_session(current_path: &Path) -> Option<PathBuf> {
    let sessions = crate::session::list_recent_sessions(20);
    if sessions.is_empty() {
        return None;
    }
    let mut initial_idx = 0usize;
    let items: Vec<MenuItem> = sessions
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            if s.path == current_path {
                initial_idx = idx;
            }
            let short_id = s
                .filename
                .trim_start_matches("session-")
                .trim_end_matches(".jsonl");
            let active_tag = if s.path == current_path {
                format!(" {}[AKTİF]{}", GREEN, RESET)
            } else {
                String::new()
            };
            MenuItem {
                key: format!("#{:02} · {}", idx + 1, short_id),
                badge: format!(
                    "{}🕒 {:<11}{} · 💬 {} msj{}",
                    CYAN, s.age_label, RESET, s.message_count, active_tag
                ),
                desc: s.preview.clone(),
            }
        })
        .collect();

    let chosen_key =
        select_interactive_menu("🗂️  GEÇMİŞ OTURUMLAR (/resume)", &items, initial_idx)?;
    let chosen_idx = items.iter().position(|it| it.key == chosen_key)?;
    sessions.get(chosen_idx).map(|s| s.path.clone())
}

pub fn render_assistant_header(model_key: &str) {
    close_thinking_box_if_open();
    spinner_print(&format!(
        "\n{}╭─ {}⚡ SPAVN{} {}· {}{}{}\n",
        CYAN, BOLD, RESET, DIM, CYAN, model_key, RESET
    ));
}

#[allow(dead_code)]
pub fn supports_inline_terminal_images() -> bool {
    let term_prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let lc_term = std::env::var("LC_TERMINAL").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    term_prog == "iTerm.app"
        || term_prog == "WezTerm"
        || lc_term == "iTerm2"
        || term_prog.eq_ignore_ascii_case("ghostty")
        || term.contains("kitty")
}

/// Renders an inline image preview in iTerm2 / WezTerm via OSC 1337 (`\x1b]1337;File=...`).
pub fn render_inline_image_preview(img: &crate::router::DraggedImage) {
    pause_active_spinner();
    let kb = (img.size_bytes as f64 / 1024.0).max(0.1);
    println!(
        "{}│{} {}🖼️  [Görsel Eklendi: {}{}{} ({:.1} KB)]{}",
        CYAN, RESET, GREEN, BOLD, img.filename, RESET, kb, RESET
    );
    let term_prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let lc_term = std::env::var("LC_TERMINAL").unwrap_or_default();
    if term_prog == "iTerm.app" || term_prog == "WezTerm" || lc_term == "iTerm2" {
        println!(
            "{}│{} \x1b]1337;File=inline=1;width=46;height=12;preserveAspectRatio=1:{}\x07",
            CYAN, RESET, img.base64
        );
        let _ = io::stdout().flush();
    }
    resume_active_spinner();
}

pub fn render_assistant_footer(
    model_key: &str,
    access_badge: &str,
    ttft_ms: u128,
    total_sec: f64,
    tok_per_sec: f64,
) {
    let speed_color = if tok_per_sec >= 40.0 {
        GREEN
    } else if tok_per_sec >= 18.0 {
        CYAN
    } else {
        DIM
    };
    spinner_print(&format!(
        "{}╰─{} {}✓ {}{} {}· {} · TTFT {}ms · {:.1}s · {}{:.1} tok/s{}\n",
        CYAN,
        RESET,
        GREEN,
        RESET,
        model_key,
        DIM,
        access_badge,
        ttft_ms,
        total_sec,
        speed_color,
        tok_per_sec,
        RESET
    ));
}

pub use crate::markdown::MarkdownStreamRenderer;

#[cfg(test)]
mod recap_tests {
    use super::*;
    use crate::markdown::render_markdown_table;
    use crate::router::ChatMessage;

    #[test]
    fn recap_line_summarizes_latest_turn_and_ignores_system_only() {
        let sys_only = vec![ChatMessage::system("sys prompt")];
        assert!(build_session_recap_line(&sys_only, 100).is_none());

        let msgs = vec![
            ChatMessage::system("sys prompt"),
            ChatMessage::user("[SPAVN ROLE: CODER] Implement\n\nrecap özelliğini kodlayalım"),
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(
                    "### Durum\n- **Idle Recap**: 6 saniye sonra alt dock'ta gösterilir."
                        .to_string(),
                ),
                reasoning_content: None,
                thought_signature: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];
        let line = build_session_recap_line(&msgs, 120).expect("recap should be generated");
        assert!(line.contains("recap:"));
        assert!(line.contains("recap özelliğini kodlayalım"));
        assert!(line.contains("Durum"));
    }

    #[test]
    fn markdown_table_renders_box_borders_and_wraps_wide_cells() {
        let rows = vec![
            "| # | Dosya | Tespit Edilen Kusur | Uygulanan Çözüm |".to_string(),
            "|---|---|---|---|".to_string(),
            "| **1** | `src/hud.rs` | **Hızlı Düşünce Akışı:** `stream_thinking_delta` ve `spinner_print_locked` güncellendi. | Ortak `rebuild_spinner_dock_lines()` fonksiyonu çıkarıldı. |".to_string(),
            "| **2** | `src/router.rs` | Anthropic native akışında araç bildirimi eklendi. | `notify_tool_call_streaming` devreye alındı. |".to_string(),
        ];
        let rendered = render_markdown_table(&rows, 100);
        assert!(rendered.contains('╭'));
        assert!(rendered.contains('┬'));
        assert!(rendered.contains('┼'));
        assert!(rendered.contains('╰'));
        assert!(!rendered.contains("|---|"));
        for line in rendered.lines() {
            let (_, vw) = truncate_ansi_to_width(line, usize::MAX);
            assert!(
                vw <= 100,
                "table line visual width {} exceeded 100 cols: {}",
                vw,
                line
            );
        }
    }

    #[test]
    fn atomic_image_token_deletes_and_navigates_as_single_unit() {
        let s = "merhaba [🖼️ Screenshot 2026-11-04.png (136KB)] test";
        let mut chars: Vec<char> = s.chars().collect();
        let close_pos = chars.iter().position(|&c| c == ']').unwrap() + 1;
        let span = super::image_token_span_before_cursor(&chars, close_pos)
            .expect("should detect atomic image token before cursor");
        assert_eq!(
            super::image_token_span_at_cursor(&chars, span.0),
            Some(span)
        );
        chars.drain(span.0..span.1);
        let after: String = chars.into_iter().collect();
        assert_eq!(after, "merhaba  test");
    }
}
