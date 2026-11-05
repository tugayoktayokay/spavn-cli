use crate::config::{
    byok_catalog, expand_user_path, is_en, load_user_config, resolve_api_key, save_user_config,
    tr_en, upsert_spavn_secret, ModelConfigEntry, ProviderGroupConfig,
};
use crate::router::{ChatMessage, ModelEndpoint};
use crate::tools::AccessMode;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod edit;
mod input;
mod keys;
mod menus;
pub mod toolcard;

pub use input::{read_bottom_dock_input, LiveSpinner};
pub use menus::{
    interactive_provider_login, interactive_queue_manager, interactive_select_access,
    interactive_select_host, interactive_select_language, interactive_select_model,
    interactive_select_role, interactive_select_session, interactive_select_skill,
    interactive_select_slash_command, select_interactive_menu, MenuItem, QueueAction,
};

static LAST_TERM_ROWS: AtomicUsize = AtomicUsize::new(0);
static LAST_TERM_COLS: AtomicUsize = AtomicUsize::new(0);
static STICKY_DOCK_STATUS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static SPINNER_PAUSED: AtomicBool = AtomicBool::new(false);
static IDLE_RECAP_ENABLED: AtomicBool = AtomicBool::new(true);
static USED_CONTEXT_TOKENS: AtomicUsize = AtomicUsize::new(1_200);
static MAX_CONTEXT_TOKENS: AtomicUsize = AtomicUsize::new(200_000);
static STREAM_EXTRA_TOKENS: AtomicUsize = AtomicUsize::new(0);
type AtFileCache = Mutex<Option<(PathBuf, Instant, Arc<Vec<String>>)>>;
static AT_FILE_CACHE: AtFileCache = Mutex::new(None);
const AT_FILE_CACHE_TTL: Duration = Duration::from_secs(15);
const AT_FILE_RESULT_LIMIT: usize = 12;
const AT_SKILL_RESULT_LIMIT: usize = 15;
/// Fixed row count of the `@`/`/` completion popup, so the dock height never oscillates.
const SUGGESTION_ROWS: usize = 3;

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
    if let Some(task_bar) = crate::format_live_tasks_compact_bar(max_w) {
        mid_lines.push(task_bar);
    }
    let pending_list = crate::list_pending_messages();
    if !pending_list.is_empty() {
        let total_p = pending_list.len();
        let show_slice = &pending_list[total_p.saturating_sub(2)..];
        for (idx_in_slice, (is_steer, pending_txt)) in show_slice.iter().enumerate() {
            let abs_idx = total_p.saturating_sub(show_slice.len()) + idx_in_slice + 1;
            let short_p: String = pending_txt
                .chars()
                .take(max_w.saturating_sub(42).max(16))
                .collect();
            let badge = if *is_steer {
                format!(
                    "{}⚡ #{}: {}{}",
                    YELLOW,
                    abs_idx,
                    tr_en("Müdahale", "Steer"),
                    RESET
                )
            } else {
                format!(
                    "{}📥 #{}: {}{}",
                    GREEN,
                    abs_idx,
                    tr_en("Sırada", "Queued"),
                    RESET
                )
            };
            let is_last_pending = abs_idx == total_p;
            let action_tag = if is_last_pending && !has_typed {
                format!(
                    " {}[↑: {} · Ctrl+X: {}]{}",
                    DIM,
                    tr_en("Düzenle", "Edit"),
                    tr_en("İptal", "Cancel"),
                    RESET
                )
            } else if is_last_pending {
                format!(" {}[Ctrl+X: {}]{}", DIM, tr_en("İptal", "Cancel"), RESET)
            } else {
                String::new()
            };
            let raw_row = format!(
                "{}│{}  {} {}·{} {}{}{}{}",
                CYAN,
                RESET,
                badge,
                DIM,
                RESET,
                BOLD,
                style_inline_image_tokens(&short_p),
                RESET,
                action_tag
            );
            let (trunc_row, _) = truncate_ansi_to_width(&raw_row, max_w);
            mid_lines.push(trunc_row);
        }
    }

    if has_typed {
        let wrap_cols = max_w.saturating_sub(7).max(12);
        let c_pos = state.typing_cursor.min(state.typing_buf.len());
        let cur_sel = state.typing_sel_anchor.and_then(|a| {
            let a_clamped = a.min(state.typing_buf.len());
            let s = a_clamped.min(c_pos);
            let e = a_clamped.max(c_pos);
            (s < e).then_some((s, e))
        });
        let (wrapped, _, _) =
            wrap_input_lines_with_cursor(&state.typing_buf, c_pos, cur_sel, wrap_cols);
        let total_w = wrapped.len();
        let win_start = total_w.saturating_sub(4);
        let visible_slice = &wrapped[win_start..];
        for (i, line) in visible_slice.iter().enumerate() {
            let styled = style_inline_image_tokens(line);
            let raw_row = if i == 0 {
                format!("{}│{} {}❯{} {}", CYAN, RESET, BOLD, RESET, styled)
            } else {
                format!("{}│{}   {}", CYAN, RESET, styled)
            };
            let (trunc_row, _) = truncate_ansi_to_width(&raw_row, max_w);
            mid_lines.push(trunc_row);
        }
    } else if pending_list.is_empty() {
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
        let c_pos = state.typing_cursor.min(state.typing_buf.len());
        let sel_len = state.typing_sel_anchor.and_then(|a| {
            let a_clamped = a.min(state.typing_buf.len());
            let s = a_clamped.min(c_pos);
            let e = a_clamped.max(c_pos);
            (s < e).then_some(e - s)
        });
        if let Some(len) = sel_len {
            if is_en() {
                format!("Selected: {} chars (Backspace: Delete)", len)
            } else {
                format!("Seçili: {} krk (Backspace: Sil)", len)
            }
        } else {
            tr_en(
                "Tab: Mod · Shift+←/→: Seç · Esc: Temizle",
                "Tab: Mode · Shift+←/→: Select · Esc: Clear",
            )
            .to_string()
        }
    } else if s_cnt > 0 || q_cnt > 0 {
        tr_en(
            "↑: Kuyruktakini Düzenle · Ctrl+X: Kuyruğu İptal Et · Esc: Durdur",
            "↑: Edit Queued · Ctrl+X: Cancel Queued · Esc: Stop",
        )
        .to_string()
    } else {
        tr_en(
            "Tab: Mod Değiştir · Esc: İptal",
            "Tab: Toggle Mode · Esc: Cancel",
        )
        .to_string()
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

pub use crate::ansi::{
    char_display_width, compute_dock_geometry, flex_wrap_items, truncate_ansi_to_width,
    truncate_ansi_to_width_ellipsis, truncate_ansi_to_width_middle, visible_width,
    wrap_input_lines_with_cursor, BOLD, CYAN, DIM, GREEN, MAGENTA, RED, RESET, YELLOW,
};

/// Switches the controlling tty to non-canonical, no-echo mode with the given read timeout.
///
/// Callers must own a `TermiosGuard` built from the *original* termios so the tty is
/// always restored, including on panic or early return.
fn enter_raw_mode(orig_termios: &Termios, vtime: u8) {
    let mut raw = *orig_termios;
    raw.c_lflag &= !(ICANON | ECHO);
    raw.c_cc[VMIN_IDX] = 0;
    raw.c_cc[VTIME_IDX] = vtime;
    unsafe {
        tcsetattr(0, TCSANOW, &raw);
    }
}

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

/// True when stdin is an interactive terminal.
///
/// Approval prompts read a line from stdin. Without a TTY that read can block forever on an
/// inherited pipe and can never be answered, so callers must fail closed instead of waiting.
pub fn stdin_is_tty() -> bool {
    // Test binaries inherit whatever stdin the developer ran `cargo test` from, so a suite started
    // in a terminal used to sit on the approval prompt until it was answered by hand. The gate
    // fails closed, so a test build reports "no TTY" and never reaches the blocking read.
    #[cfg(test)]
    {
        false
    }
    #[cfg(all(not(test), unix))]
    unsafe {
        let mut term = empty_termios();
        tcgetattr(0, &mut term) == 0
    }
    #[cfg(all(not(test), windows))]
    unsafe {
        let h_in = GetStdHandle((-10i32) as u32);
        let mut mode: u32 = 0;
        GetConsoleMode(h_in, &mut mode) != 0
    }
    #[cfg(all(not(test), not(any(unix, windows))))]
    false
}

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
        print!("\x1b[?2004l\x1b[?7h\x1b[?25h");
        let _ = io::stdout().flush();
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

/// Project file list for `@` completion. Cached briefly so typing never spawns git per keystroke.
fn project_file_list() -> Arc<Vec<String>> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Ok(guard) = AT_FILE_CACHE.lock() {
        if let Some((cached_cwd, fetched_at, files)) = guard.as_ref() {
            if *cached_cwd == cwd && fetched_at.elapsed() < AT_FILE_CACHE_TTL {
                return Arc::clone(files);
            }
        }
    }

    let mut files: Vec<String> = Vec::new();
    if let Ok(out) = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(&cwd)
        .output()
    {
        if out.status.success() {
            files = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .take(5_000)
                .collect();
        }
    }

    let shared = Arc::new(files);
    if let Ok(mut guard) = AT_FILE_CACHE.lock() {
        *guard = Some((cwd, Instant::now(), Arc::clone(&shared)));
    }
    shared
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
            ("/todo", tr_en("/todo (Görev Takibi)", "/todo (Live Tasks)")),
            (
                "/reload",
                tr_en("/reload (Yenile)", "/reload (Live Reload)"),
            ),
            (
                "/clear",
                tr_en("/clear (Temizle)", "/clear (Clear Session)"),
            ),
            (
                "/diff",
                tr_en("/diff (Git Değişiklikleri)", "/diff (Git Changes)"),
            ),
            (
                "/commit",
                tr_en("/commit (Değişiklikleri İşle)", "/commit (Commit Changes)"),
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
                    || (q == "/d" && *k == "/diff")
                    || (q == "/cm" && *k == "/commit")
            })
            .map(|(k, l)| (k.to_string(), l.to_string()))
            .collect();
    }
    if let Some(at_idx) = input_buf.rfind('@') {
        let tail = &input_buf[at_idx..];
        if !tail.contains(' ') {
            let q = tail.trim_start_matches('@').to_lowercase();
            let mut out: Vec<(String, String)> = Vec::new();
            if !q.starts_with('/') {
                for path in project_file_list().iter() {
                    if q.is_empty() || path.to_lowercase().contains(&q) {
                        let token = format!("@{}", path);
                        out.push((token.clone(), token));
                    }
                    if out.len() >= AT_FILE_RESULT_LIMIT {
                        break;
                    }
                }
            }
            for skill in crate::skills::discover_skills() {
                let lower = skill.name.to_lowercase();
                if q.is_empty() || lower.starts_with(&q) || lower.contains(&q) {
                    let token = format!("@{}", skill.name);
                    out.push((
                        token.clone(),
                        format!("{} {}", token, tr_en("(Yetenek)", "(Skill)")),
                    ));
                }
                if out.len() >= AT_SKILL_RESULT_LIMIT {
                    break;
                }
            }
            return out;
        }
    }
    Vec::new()
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
    if let Some(task_bar) = crate::format_live_tasks_compact_bar(full_w) {
        top_lines.push(task_bar);
    }
    let pending_idle = crate::list_pending_messages();
    if !pending_idle.is_empty() {
        let total_p = pending_idle.len();
        for (idx_p, (is_steer, pending_txt)) in pending_idle
            .iter()
            .enumerate()
            .skip(total_p.saturating_sub(2))
        {
            let short_p: String = pending_txt
                .chars()
                .take(full_w.saturating_sub(42).max(16))
                .collect();
            let badge = if *is_steer {
                format!(
                    "{}⚡ #{}: {}{}",
                    YELLOW,
                    idx_p + 1,
                    tr_en("Müdahale", "Steer"),
                    RESET
                )
            } else {
                format!(
                    "{}📥 #{}: {}{}",
                    GREEN,
                    idx_p + 1,
                    tr_en("Sırada", "Queued"),
                    RESET
                )
            };
            let hint = if idx_p + 1 == total_p && input_chars.is_empty() {
                format!(" {}[↑: {}]{}", DIM, tr_en("Düzenle/Al", "Edit/Pop"), RESET)
            } else {
                String::new()
            };
            let row = format!(
                "{}│{}  {} {}·{} {}{}{}{}",
                CYAN,
                RESET,
                badge,
                DIM,
                RESET,
                BOLD,
                style_inline_image_tokens(&short_p),
                RESET,
                hint
            );
            let (trunc_row, _) = truncate_ansi_to_width(&row, full_w);
            top_lines.push(trunc_row);
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
                        "{}│{} {}❯{} {}█{} {}{}{}",
                        CYAN,
                        RESET,
                        BOLD,
                        RESET,
                        CYAN,
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
    let mut h_items = vec![
        format_context_usage_badge(),
        format!("{}{}{}", DIM, tr_en("/: Menü", "/: Menu"), RESET),
        format!("{}@: {}{}", DIM, tr_en("Dosya/Skill", "File/Skill"), RESET),
        format!("{}/model{}", DIM, RESET),
        format!("{}/diff{}", DIM, RESET),
        format!("{}/commit{}", DIM, RESET),
    ];
    if suggestions.is_empty() {
        h_items.push(format!(
            "{}{}{}",
            DIM,
            tr_en("Shift+Tab: Yetki", "Shift+Tab: Access"),
            RESET
        ));
        h_items.push(format!("{}{}{}", DIM, tr_en("\\: Faz", "\\: Phase"), RESET));
        if !input_chars.is_empty() {
            if let Some((s, e)) = sel_range {
                if s < e {
                    h_items.push(format!(
                        "{}{}{}",
                        YELLOW,
                        tr_en(
                            &format!("Seçili: {} krk (Backspace: Sil)", e - s),
                            &format!("Selected: {} chars (Backspace: Delete)", e - s)
                        ),
                        RESET
                    ));
                }
            } else {
                h_items.push(format!(
                    "{}{}{}",
                    YELLOW,
                    tr_en(
                        "Shift+←/→: Seç · Esc: Temizle",
                        "Shift+←/→: Select · Esc: Clear"
                    ),
                    RESET
                ));
            }
        }
    } else {
        let sel = selected_idx
            .unwrap_or(0)
            .min(suggestions.len().saturating_sub(1));
        h_items.push(format!("{}{}/{}{}", DIM, sel + 1, suggestions.len(), RESET));
        h_items.push(format!("{}(↑/↓ · Tab · Enter · Esc){}", YELLOW, RESET));
    }

    // Single-row hint bar. It is truncated (never wrapped) and the popup below always
    // reserves SUGGESTION_ROWS, so the dock height is constant while typing and the
    // prompt row can never drift into the scrollback.
    let (hint_row, _) = truncate_ansi_to_width(
        &format!(
            "{}{}",
            bot_prefix_first,
            h_items.join(&format!(" {}·{} ", DIM, RESET))
        ),
        full_w,
    );
    let mut bottom_lines: Vec<String> = vec![hint_row];
    if !suggestions.is_empty() {
        let sel = selected_idx
            .unwrap_or(0)
            .min(suggestions.len().saturating_sub(1));
        let window_start = sel.saturating_sub(SUGGESTION_ROWS - 1);
        for (i, (_, label)) in suggestions
            .iter()
            .enumerate()
            .skip(window_start)
            .take(SUGGESTION_ROWS)
        {
            let raw = if i == sel {
                format!("\x1b[1;7;36m ❯ {} \x1b[0m", label)
            } else {
                format!("{}  {}{}", DIM, label, RESET)
            };
            let (trunc, _) = truncate_ansi_to_width(&raw, full_w.saturating_sub(6));
            bottom_lines.push(format!("{}│{}  {}", CYAN, RESET, trunc));
        }
    }
    while bottom_lines.len() < SUGGESTION_ROWS + 1 {
        bottom_lines.push(String::new());
    }

    let bottom_count = bottom_lines.len().max(1);
    let bottom_joined = bottom_lines.join("\n");

    let geometry = compute_dock_geometry(
        *prev_top_w,
        top_count,
        input_line_count,
        bottom_count,
        cursor_row,
    );
    let cursor_col = (5 + cursor_col_offset).min(full_w);
    print!(
        "\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[{}A\r\x1b[J{}\n{}\n{}\x1b[?7h\x1b[{}A\r\x1b[{}G\x1b[?2026l",
        geometry.up_to_start,
        top_joined,
        input_joined,
        bottom_joined,
        geometry.lines_below_cursor,
        cursor_col
    );
    let _ = io::stdout().flush();
    *prev_top_w = geometry.next_top_w;
    *prev_cursor_col = cursor_col;
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

pub enum PromptEvent {
    Submit(String),
    CycleAccess,
    CyclePhase,
    Reload,
    ShowSkills,
    Exit,
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

pub fn render_assistant_header(model_key: &str) {
    close_thinking_box_if_open();
    spinner_print(&format!(
        "\n{}╭─ {}⚡ SPAVN{} {}· {}{}{}\n",
        CYAN, BOLD, RESET, DIM, CYAN, model_key, RESET
    ));
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

pub fn render_session_resume_preview(session_path: &Path, messages: &[ChatMessage]) {
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
                    .map(|s| !strip_internal_tags_for_display(s).is_empty())
                    .unwrap_or(false)
        })
        .collect();
    let last_user_preview = text_msgs
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_deref())
        .map(strip_internal_tags_for_display)
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

    #[test]
    fn zero_cost_live_tasks_sync_and_advance() {
        let _g = crate::live_task_test_lock();
        crate::clear_live_tasks();
        let plan =
            "Here is the plan:\n- [ ] Update config.rs\n- [ ] Refactor hud.rs\n- [ ] Run cargo test";
        crate::sync_live_tasks_from_text(plan);
        let tasks = crate::list_live_tasks();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].status, crate::LiveTaskStatus::InProgress);

        crate::advance_live_tasks_on_tool("edit", "src/config.rs");
        let tasks2 = crate::list_live_tasks();
        assert_eq!(tasks2[0].status, crate::LiveTaskStatus::Done);
        assert_eq!(tasks2[1].status, crate::LiveTaskStatus::InProgress);

        let bar = crate::format_live_tasks_compact_bar(100).expect("should render task bar");
        assert!(bar.contains("[1/3]"));
        crate::clear_live_tasks();
    }
}
