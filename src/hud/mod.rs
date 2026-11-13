use crate::config::{
    byok_catalog, expand_user_path, load_user_config, resolve_api_key, tr_en, upsert_spavn_secret,
    ModelConfigEntry, ProviderGroupConfig,
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

mod cards;
mod dock;
mod edit;
mod input;
mod keys;
mod menus;
mod pickers;
mod title;
pub mod toolcard;

pub use cards::*;
pub use dock::*;
pub use pickers::*;
pub use title::{
    restore_window_title, set_title_state, set_window_topic, TitleState, WorkingGuard,
};

pub use input::{read_bottom_dock_input, LiveSpinner};
pub use menus::{
    interactive_select_model, interactive_select_slash_command, select_interactive_menu, MenuItem,
};

pub(crate) static LAST_TERM_ROWS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static LAST_TERM_COLS: AtomicUsize = AtomicUsize::new(0);
pub(crate) static STICKY_DOCK_STATUS: std::sync::Mutex<Option<String>> =
    std::sync::Mutex::new(None);
pub(crate) static SPINNER_PAUSED: AtomicBool = AtomicBool::new(false);
pub(crate) static IDLE_RECAP_ENABLED: AtomicBool = AtomicBool::new(true);
pub(crate) static USED_CONTEXT_TOKENS: AtomicUsize = AtomicUsize::new(1_200);
pub(crate) static MAX_CONTEXT_TOKENS: AtomicUsize = AtomicUsize::new(200_000);
pub(crate) static STREAM_EXTRA_TOKENS: AtomicUsize = AtomicUsize::new(0);
pub(crate) type AtFileCache = Mutex<Option<(PathBuf, Instant, Arc<Vec<String>>)>>;
pub(crate) static AT_FILE_CACHE: AtFileCache = Mutex::new(None);
pub(crate) const AT_FILE_CACHE_TTL: Duration = Duration::from_secs(15);
pub(crate) const AT_FILE_RESULT_LIMIT: usize = 12;
pub(crate) const AT_SKILL_RESULT_LIMIT: usize = 15;
/// Fixed row count of the `@`/`/` completion popup, so the dock height never oscillates.
pub(crate) const SUGGESTION_ROWS: usize = 3;
pub(crate) static PREV_DOCK_BOTTOM_COUNT: AtomicUsize = AtomicUsize::new(1);

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
    } else if pct_display < 75 {
        YELLOW
    } else {
        RED
    };
    let alert_badge = if pct_display >= 90 {
        format!(" {}\x1b[1;31m●{}", BOLD, RESET)
    } else if pct_display >= 75 {
        format!(" {}⚠{}", YELLOW, RESET)
    } else {
        String::new()
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
        "{} {}{}{} / {}{} ({}%){}{}",
        bar,
        color,
        format_token_short(used),
        RESET,
        DIM,
        format_token_short(max),
        pct_display,
        RESET,
        alert_badge
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

pub(crate) struct SpinnerScreenState {
    pub(crate) active: bool,
    pub(crate) paused: bool,
    pub(crate) label: String,
    pub(crate) started_at: Option<Instant>,
    pub(crate) dock_lines: Vec<String>,
    pub(crate) partial_line: String,
    /// O(1) memoized visible width of `partial_line` (kept in sync at every mutation).
    pub(crate) partial_vw: usize,
    /// 0 = none, 1 = bare ESC pending, 2 = inside CSI sequence.
    pub(crate) partial_esc: u8,
    /// Memoized "contains │╭├╰" for the table-box-line wrap decision.
    pub(crate) partial_has_box: bool,
    pub(crate) partial_prefix: String,
    pub(crate) partial_shown: bool,
    pub(crate) thinking_box_open: bool,
    pub(crate) thinking_chars: usize,
    pub(crate) thinking_pending: String,
    pub(crate) last_think_flush: Option<Instant>,
    pub(crate) last_render_at: Option<Instant>,
    pub(crate) typing_buf: Vec<char>,
    pub(crate) typing_cursor: usize,
    pub(crate) typing_sel_anchor: Option<usize>,
    pub(crate) is_typing: bool,
    pub(crate) steer_mode: Option<bool>,
}

pub(crate) static SPINNER_SCREEN: std::sync::Mutex<SpinnerScreenState> =
    std::sync::Mutex::new(SpinnerScreenState {
        active: false,
        paused: false,
        label: String::new(),
        started_at: None,
        dock_lines: Vec::new(),
        partial_line: String::new(),
        partial_vw: 0,
        partial_esc: 0,
        partial_has_box: false,
        partial_prefix: String::new(),
        partial_shown: false,
        thinking_box_open: false,
        thinking_chars: 0,
        thinking_pending: String::new(),
        last_think_flush: None,
        last_render_at: None,
        typing_buf: Vec::new(),
        typing_cursor: 0,
        typing_sel_anchor: None,
        is_typing: false,
        steer_mode: None,
    });

/// Draft typed into the busy spinner input but not submitted before the turn
/// ended. Transferred to the main prompt as prefill so the user's in-progress
/// text is never lost.
static SPINNER_DRAFT: Mutex<Option<String>> = Mutex::new(None);

pub fn take_spinner_draft() -> Option<String> {
    let mut from_state: Option<String> = None;
    if let Ok(mut state) = SPINNER_SCREEN.lock() {
        if !state.typing_buf.is_empty() {
            let live: String = state.typing_buf.iter().collect();
            if !live.trim().is_empty() {
                from_state = Some(live);
            }
        }
        state.typing_buf.clear();
        state.typing_cursor = 0;
        state.typing_sel_anchor = None;
        state.is_typing = false;
    }
    let from_slot = SPINNER_DRAFT.lock().ok().and_then(|mut s| s.take());
    from_state.or(from_slot)
}

pub(crate) fn rebuild_spinner_dock_lines(
    state: &mut SpinnerScreenState,
    status_hdr: &str,
    cols: usize,
) {
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
            let template = tr_en(
                "Seçili: {} karakter (Backspace: Sil)",
                "Selected: {} chars (Backspace: Delete)",
            )
            .replace("{}", &len.to_string());
            template
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

/// Pushes one char into the spinner's partial line, keeping the O(1) memoized
/// visible-width / escape-state / box-char flags in sync. Returns the new memo
/// values so the wrap decision needs no re-scan of the whole line.
fn partial_screen_push(state: &mut SpinnerScreenState, ch: char) -> (usize, bool, bool) {
    if state.partial_esc == 2 {
        state.partial_line.push(ch);
        if matches!(ch, '@'..='~') {
            state.partial_esc = 0;
        }
        return (
            state.partial_vw,
            state.partial_has_box,
            state.partial_esc != 0,
        );
    }
    if state.partial_esc == 3 {
        // Inside an OSC body: consume until BEL or ST (\x1b\\), mirroring
        // `skip_escape` so the memo counts zero width for the whole sequence.
        state.partial_line.push(ch);
        if ch == '\x07' {
            state.partial_esc = 0;
        } else if ch == '\x1b' {
            state.partial_esc = 4;
        }
        return (
            state.partial_vw,
            state.partial_has_box,
            state.partial_esc != 0,
        );
    }
    if state.partial_esc == 4 {
        state.partial_line.push(ch);
        state.partial_esc = if ch == '\\' { 0 } else { 3 };
        return (
            state.partial_vw,
            state.partial_has_box,
            state.partial_esc != 0,
        );
    }
    if state.partial_esc == 1 {
        state.partial_line.push(ch);
        if ch == '[' {
            state.partial_esc = 2;
        } else if ch == ']' {
            state.partial_esc = 3;
        } else {
            state.partial_esc = 0;
        }
        return (
            state.partial_vw,
            state.partial_has_box,
            state.partial_esc != 0,
        );
    }
    if ch == '\u{1b}' {
        state.partial_line.push(ch);
        state.partial_esc = 1;
        return (state.partial_vw, state.partial_has_box, true);
    }
    state.partial_line.push(ch);
    state.partial_vw += char_display_width(ch);
    if !state.partial_has_box && matches!(ch, '│' | '╭' | '├' | '╰') {
        state.partial_has_box = true;
    }
    (state.partial_vw, state.partial_has_box, false)
}

/// Full-reset of the partial line; recomputes memos from scratch.
pub(crate) fn partial_screen_reset(state: &mut SpinnerScreenState) {
    state.partial_line.clear();
    state.partial_vw = 0;
    state.partial_esc = 0;
    state.partial_has_box = false;
}

/// Recomputes the memos after an external mutation restores prefix/carry text.
fn partial_screen_recount(state: &mut SpinnerScreenState) {
    let mut vw = 0usize;
    let mut esc = 0u8;
    let mut has_box = false;
    for ch in state.partial_line.chars() {
        if esc == 2 {
            if matches!(ch, '@'..='~') {
                esc = 0;
            }
            continue;
        }
        if esc == 3 {
            if ch == '\x07' {
                esc = 0;
            } else if ch == '\x1b' {
                esc = 4;
            }
            continue;
        }
        if esc == 4 {
            esc = if ch == '\\' { 0 } else { 3 };
            continue;
        }
        if esc == 1 {
            esc = if ch == '[' {
                2
            } else if ch == ']' {
                3
            } else {
                0
            };
            continue;
        }
        if ch == '\u{1b}' {
            esc = 1;
            continue;
        }
        vw += char_display_width(ch);
        has_box = has_box || matches!(ch, '│' | '╭' | '├' | '╰');
    }
    state.partial_vw = vw;
    state.partial_esc = esc;
    state.partial_has_box = has_box;
}

pub(crate) fn spinner_print_locked(state: &mut SpinnerScreenState, text: &str) {
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
    let mut out = String::from("\u{1b}[?2026h\u{1b}[?25l\u{1b}[?7l\r\u{1b}[J");
    for ch in text.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            if state.thinking_box_open
                && !state.partial_prefix.is_empty()
                && state.partial_line == state.partial_prefix
            {
                partial_screen_reset(state);
                continue;
            }
            out.push_str("\u{1b}[?7h");
            out.push_str(&state.partial_line);
            out.push_str("\u{1b}[0m\n");
            partial_screen_reset(state);
        } else {
            let (vw, has_box, esc_open) = partial_screen_push(state, ch);
            if !esc_open {
                let is_table_box_line = !state.thinking_box_open && has_box;
                let should_wrap = if is_table_box_line {
                    vw >= cols.saturating_sub(2).max(20)
                } else {
                    (ch == ' ' && vw >= max_w.saturating_sub(8)) || vw >= max_w
                };
                if should_wrap {
                    let pfx_len = if state.thinking_box_open {
                        state.partial_prefix.len().min(state.partial_line.len())
                    } else {
                        0
                    };
                    // Word-boundary wrap: never slice a word in half when wrapping prose/thinking
                    if !is_table_box_line && ch != ' ' {
                        if let Some(rel_sp) = state.partial_line[pfx_len..].rfind(' ') {
                            if rel_sp > 8 {
                                let split_at = pfx_len + rel_sp;
                                let carry = state.partial_line[split_at + 1..].to_string();
                                out.push_str("\u{1b}[?7h");
                                out.push_str(&state.partial_line[..split_at]);
                                out.push_str("\u{1b}[0m\n");
                                state.partial_line.clear();
                                if !state.partial_prefix.is_empty() {
                                    state.partial_line.push_str(&state.partial_prefix);
                                } else {
                                    state.partial_line.push_str("  ");
                                }
                                state.partial_line.push_str(&carry);
                                partial_screen_recount(state);
                                continue;
                            }
                        }
                    }
                    out.push_str("\u{1b}[?7h");
                    out.push_str(state.partial_line.trim_end());
                    out.push_str("\u{1b}[0m\n");
                    state.partial_line.clear();
                    if !state.partial_prefix.is_empty() {
                        state.partial_line.push_str(&state.partial_prefix);
                    } else {
                        state.partial_line.push_str("  ");
                    }
                    partial_screen_recount(state);
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
    state.last_render_at = Some(Instant::now());
}

pub(crate) fn flush_pending_thinking_locked(state: &mut SpinnerScreenState) {
    if state.thinking_pending.is_empty() {
        return;
    }
    let pending = std::mem::take(&mut state.thinking_pending);
    state.last_think_flush = Some(Instant::now());
    let formatted = pending.replace('\n', &format!("\n{}{}│ ", DIM, CYAN));
    spinner_print_locked(state, &formatted);
}

pub(crate) fn close_thinking_box_locked(state: &mut SpinnerScreenState) {
    flush_pending_thinking_locked(state);
    state.thinking_chars = 0;
    state.last_think_flush = None;
    if state.thinking_box_open {
        state.thinking_box_open = false;
        let pfx = std::mem::take(&mut state.partial_prefix);
        if !pfx.is_empty() && state.partial_line.trim_end() == pfx.trim_end() {
            state.partial_line.clear();
            partial_screen_reset(state);
            spinner_print_locked(state, &format!("{}╰─{}\n", DIM, RESET));
        } else {
            spinner_print_locked(state, &format!("\n{}╰─{}\n", DIM, RESET));
        }
    }
}

pub fn spinner_print(text: &str) {
    record_output_fragment(text);
    if let Ok(mut state) = SPINNER_SCREEN.lock() {
        spinner_print_locked(&mut state, text);
    } else {
        print!("{}", text);
        let _ = io::stdout().flush();
    }
}

/// Rolling buffer fed by every HUD print; Ctrl+Y copies its ANSI-stripped content.
pub(crate) static LAST_OUTPUT: Mutex<String> = Mutex::new(String::new());

/// Raw model-text buffer fed by the markdown stream renderer: unrendered markdown
/// with zero HUD chrome (headers, footers, borders). Copy operations prefer this.
/// Marker-based capture: cleared at each turn's content start (`begin_turn_capture`),
/// filled by `MarkdownStreamRenderer::push`, so it holds exactly the last turn.
pub(crate) static LAST_RAW_OUTPUT: Mutex<String> = Mutex::new(String::new());

/// Opens a fresh raw-capture window at the start of an assistant turn's content.
/// Everything streamed afterwards is what `/copy` serves; HUD chrome never enters.
pub fn begin_turn_capture() {
    if let Ok(mut buf) = LAST_RAW_OUTPUT.lock() {
        buf.clear();
    }
}

/// Keeps `buf` bounded by dropping its head past 120k chars (down to 100k).
fn cap_rolling_buffer(buf: &mut String) {
    while buf.len() > 120_000 {
        let offset = buf.len() - 100_000;
        match buf.char_indices().find(|(i, _)| *i >= offset) {
            Some((i, _)) => {
                buf.drain(..i);
            }
            None => {
                buf.clear();
            }
        };
    }
}

pub(crate) fn record_output_fragment(fragment: &str) {
    if fragment.is_empty() {
        return;
    }
    if let Ok(mut buf) = LAST_OUTPUT.lock() {
        buf.push_str(fragment);
        cap_rolling_buffer(&mut buf);
    }
}

pub(crate) fn record_raw_output_fragment(fragment: &str) {
    if fragment.is_empty() {
        return;
    }
    if let Ok(mut buf) = LAST_RAW_OUTPUT.lock() {
        buf.push_str(fragment);
        cap_rolling_buffer(&mut buf);
    }
}

pub fn last_output_snapshot() -> String {
    // Prefer the raw model text: it carries no UI chrome (header/footer/borders),
    // so copies land as clean plain markdown.
    if let Ok(raw) = LAST_RAW_OUTPUT.lock() {
        if !raw.is_empty() {
            return raw.clone();
        }
    }
    let raw = LAST_OUTPUT.lock().map(|b| b.clone()).unwrap_or_default();
    crate::hud::menus::strip_ansi(&raw)
}

/// Prints the one-line copy footer shown right after a finished turn, so the
/// copy affordance is visible where the output just landed (not only in the dock).
pub fn print_copy_hint() {
    let regions = copy_regions();
    let Some(first) = regions.first() else {
        return;
    };
    if regions.len() == 1 && first.body.lines().count() <= 2 {
        return;
    }
    println!(
        "{}📋 {}: {} ({} {}){}",
        DIM,
        tr_en("Kopyala", "Copy"),
        tr_en("/copy", "/copy"),
        regions.len(),
        tr_en("alan", "regions"),
        RESET
    );
}

/// A copyable chunk of the last assistant output, listed by `/copy`.
pub struct CopyRegion {
    pub label: String,
    pub body: String,
}

/// Strips blockquote (`> `) markers and optional wrapping outer quotes (`"..."` / `“...”`)
/// from a block of lines, returning clean copy-ready text.
fn clean_blockquote_lines(lines: &[&str]) -> String {
    let mut cleaned: Vec<&str> = lines
        .iter()
        .map(|l| {
            let t = l.trim_start();
            if let Some(rest) = t.strip_prefix('>') {
                rest.strip_prefix(' ').unwrap_or(rest)
            } else {
                *l
            }
        })
        .collect();
    while cleaned
        .first()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        cleaned.remove(0);
    }
    while cleaned.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        cleaned.pop();
    }
    let joined = cleaned.join("\n");
    let trimmed = joined.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('“') && trimmed.ends_with('”')))
    {
        let first_len = trimmed.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        let last_len = trimmed.chars().last().map(|c| c.len_utf8()).unwrap_or(1);
        trimmed[first_len..trimmed.len() - last_len]
            .trim()
            .to_string()
    } else {
        joined
    }
}

/// Cleans the lines captured under a markdown heading so copying a section yields
/// paste-ready content without the `### Heading` prefix, ```` ``` ```` fences, or `> ` quote markers.
fn clean_section_body(lines: &[&str]) -> String {
    let mut slice = lines;
    while slice.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
        slice = &slice[1..];
    }
    while slice.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        slice = &slice[..slice.len() - 1];
    }
    if slice.is_empty() {
        return String::new();
    }

    // 1. If the section wraps a single fenced code block (optionally after a 1-line intro),
    // return the inner code block content directly without fences.
    let mut fence_blocks: Vec<Vec<&str>> = Vec::new();
    let mut in_fence = false;
    let mut cur_fence: Vec<&str> = Vec::new();
    let mut outside_non_empty = 0usize;
    for &line in slice {
        let t = line.trim_start();
        if t.starts_with("```") {
            if in_fence {
                fence_blocks.push(std::mem::take(&mut cur_fence));
                in_fence = false;
            } else {
                in_fence = true;
            }
        } else if in_fence {
            cur_fence.push(line);
        } else if !t.is_empty() {
            outside_non_empty += 1;
        }
    }
    if fence_blocks.len() == 1 && outside_non_empty <= 1 && !fence_blocks[0].is_empty() {
        return fence_blocks[0].join("\n");
    }

    // 2. If the section holds a blockquote draft/option (`> "..."`), extract and clean it.
    let quote_lines: Vec<&str> = slice
        .iter()
        .copied()
        .filter(|l| l.trim_start().starts_with('>'))
        .collect();
    let non_quote_non_empty = slice
        .iter()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('>')
        })
        .count();
    if !quote_lines.is_empty() && non_quote_non_empty <= 1 {
        return clean_blockquote_lines(&quote_lines);
    }

    // 3. General section: return trimmed body lines without the heading line.
    slice.join("\n")
}

/// Splits the rolling output buffer into copyable regions: every markdown
/// heading section, fenced code blocks, and standalone blockquotes. When the
/// output has no structured regions at all (plain chat), the whole output is
/// offered as a single region.
pub fn copy_regions() -> Vec<CopyRegion> {
    let text = last_output_snapshot();
    let mut regions: Vec<CopyRegion> = Vec::new();
    if text.trim().is_empty() {
        return regions;
    }
    let mut open: Option<(String, Vec<&str>)> = None;
    let flush = |region: &mut Vec<CopyRegion>, done: (String, Vec<&str>)| {
        if done.1.is_empty() {
            return;
        }
        let body = done.1.join("\n");
        if region.iter().any(|r| r.body == body) {
            return;
        }
        region.push(CopyRegion {
            label: format!(
                "{}{}{}: {} ({} {})",
                BOLD,
                tr_en("Kod bloğu", "Code block"),
                RESET,
                if done.0.is_empty() {
                    tr_en("kod", "code")
                } else {
                    done.0.as_str()
                },
                done.1.len(),
                tr_en("satır", "lines")
            ),
            body,
        });
    };
    let mut section: Option<(String, Vec<&str>)> = None;
    let flush_section = |region: &mut Vec<CopyRegion>, done: (String, Vec<&str>)| {
        let cleaned = clean_section_body(&done.1);
        if cleaned.is_empty() {
            return;
        }
        let line_count = cleaned.lines().count();
        // If a child code block inside this section was already pushed with the exact same
        // body, replace its generic "Code block" entry with this titled section.
        region.retain(|r| r.body != cleaned);
        region.push(CopyRegion {
            label: format!(
                "{}{}{}: {} ({} {})",
                BOLD,
                tr_en("Bölüm", "Section"),
                RESET,
                done.0.trim_start_matches('#').trim(),
                line_count,
                tr_en("satır", "lines")
            ),
            body: cleaned,
        });
    };
    let mut quote_open: Vec<&str> = Vec::new();
    let flush_quote = |region: &mut Vec<CopyRegion>, lines: &mut Vec<&str>| {
        if lines.is_empty() {
            return;
        }
        let raw = std::mem::take(lines);
        let cleaned = clean_blockquote_lines(&raw);
        if cleaned.is_empty() || region.iter().any(|r| r.body == cleaned) {
            return;
        }
        let line_count = cleaned.lines().count();
        region.push(CopyRegion {
            label: format!(
                "{}{}{} ({} {})",
                BOLD,
                tr_en("Alıntı", "Quote"),
                RESET,
                line_count,
                tr_en("satır", "lines")
            ),
            body: cleaned,
        });
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        // Inside an open fenced code block, only a closing fence ends it; headings or `>`
        // inside code must never split the section or trigger quote capture.
        if let Some(done) = open.take() {
            if let Some(sec) = section.as_mut() {
                sec.1.push(line);
            }
            if trimmed.starts_with("```") {
                flush(&mut regions, done);
            } else {
                let mut done = done;
                done.1.push(line);
                open = Some(done);
            }
            continue;
        }
        // Thematic break (`***` / `---` / `___`): models use it to close the body
        // before postamble chatter ("Buradayım Efendim..."); end the section there.
        let is_thematic = !trimmed.is_empty()
            && trimmed
                .chars()
                .all(|c| c == '*' || c == '-' || c == '_' || c == ' ')
            && trimmed.chars().filter(|c| !c.is_whitespace()).count() >= 3;
        if is_thematic {
            if let Some(done) = section.take() {
                flush_section(&mut regions, done);
            }
            flush_quote(&mut regions, &mut quote_open);
            continue;
        }
        // Heading opens a new copyable section (heading title becomes label, body is clean content).
        if trimmed.starts_with('#') && !trimmed.starts_with("#!/") {
            if let Some(done) = section.take() {
                flush_section(&mut regions, done);
            }
            flush_quote(&mut regions, &mut quote_open);
            section = Some((trimmed.to_string(), Vec::new()));
            continue;
        }
        if let Some(done) = section.as_mut() {
            done.1.push(line);
        }
        if trimmed.starts_with("```") {
            flush_quote(&mut regions, &mut quote_open);
            open = Some((
                trimmed.trim_start_matches('`').trim().to_string(),
                Vec::new(),
            ));
            continue;
        }
        if section.is_none() {
            if trimmed.starts_with('>') {
                quote_open.push(line);
            } else if !trimmed.is_empty() {
                flush_quote(&mut regions, &mut quote_open);
            }
        }
    }
    if let Some(done) = section.take() {
        flush_section(&mut regions, done);
    }
    if let Some(done) = open.take() {
        flush(&mut regions, done);
    }
    flush_quote(&mut regions, &mut quote_open);
    // Headings/sections are what the menu is for; only when none exist at all
    // (plain chat output) fall back to the whole output as one region.
    if regions.is_empty() {
        regions.push(CopyRegion {
            label: format!(
                "{}{} ({} {}, {} {})",
                BOLD,
                tr_en("Son çıktı — tamamı", "Last output — whole"),
                text.chars().count(),
                tr_en("krk", "ch"),
                text.lines().count(),
                tr_en("satır", "lines")
            ),
            body: text,
        });
    }
    regions
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
                state.last_think_flush = Some(Instant::now());
            }
            state.thinking_pending.push_str(think_delta);
            let should_flush = state.thinking_pending.contains('\n')
                || state.thinking_pending.len() >= 36
                || state
                    .last_think_flush
                    .map(|t| t.elapsed() >= Duration::from_millis(32))
                    .unwrap_or(true);
            if should_flush {
                flush_pending_thinking_locked(&mut state);
            }
        }
    }
}

pub fn close_thinking_box_if_open() {
    if let Ok(mut state) = SPINNER_SCREEN.lock() {
        close_thinking_box_locked(&mut state);
    }
}

pub fn notify_tool_call_streaming(tool_name: &str, arg_bytes: usize) {
    notify_tool_call_streaming_args(tool_name, "", arg_bytes);
}

fn extract_partial_json_preview(partial_args: &str) -> Option<String> {
    for key in [
        "\"path\"",
        "\"command\"",
        "\"pattern\"",
        "\"query\"",
        "\"url\"",
    ] {
        let Some(pos) = partial_args.find(key) else {
            continue;
        };
        let after_key = &partial_args[pos + key.len()..];
        let Some(colon) = after_key.find(':') else {
            continue;
        };
        let after_colon = after_key[colon + 1..].trim_start();
        let Some(rest) = after_colon.strip_prefix('"') else {
            continue;
        };
        let mut out = String::new();
        let mut escaped = false;
        for ch in rest.chars() {
            if escaped {
                match ch {
                    'n' | 'r' | 't' => out.push(' '),
                    other => out.push(other),
                }
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                break;
            } else {
                out.push(ch);
            }
            if out.chars().count() >= 38 {
                out.push('…');
                break;
            }
        }
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

pub fn notify_tool_call_streaming_args(tool_name: &str, partial_args: &str, arg_bytes: usize) {
    if let Ok(mut state) = SPINNER_SCREEN.lock() {
        close_thinking_box_locked(&mut state);
        let tname = if tool_name.is_empty() {
            tr_en("araç", "tool")
        } else {
            tool_name
        };
        let size_str = if arg_bytes >= 1024 {
            format!("{:.1} KB", arg_bytes as f64 / 1024.0)
        } else {
            format!("{} B", arg_bytes)
        };
        if let Some(preview) = extract_partial_json_preview(partial_args) {
            state.label = format!(
                "⚡ {} {} · {} ({})",
                tname,
                preview,
                tr_en("hazırlanıyor", "streaming"),
                size_str
            );
        } else {
            state.label = format!(
                "⚡ {} {} ({})",
                tname,
                tr_en("hazırlanıyor", "preparing"),
                size_str
            );
        }
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
        .unwrap_or_else(|| format!("{}╭─ ⚡ SPAVN LIVE INPUT{}", CYAN, RESET))
}

pub use crate::ansi::{
    char_display_width, compute_dock_geometry, flex_wrap_items, truncate_ansi_to_width,
    truncate_ansi_to_width_ellipsis, truncate_ansi_to_width_middle, visible_width,
    wrap_input_lines_with_cursor, BOLD, CYAN, DIM, GREEN, MAGENTA, RED, RESET, YELLOW,
};

#[cfg(test)]
mod copy_raw_tests {
    use super::*;

    /// Global copy buffers; serialize copy-snapshot tests.
    static COPY_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_copy_buffers() {
        begin_turn_capture();
        if let Ok(mut buf) = LAST_OUTPUT.lock() {
            buf.clear();
        }
    }

    #[test]
    fn begin_turn_capture_discards_stale_turn_text() {
        let _g = COPY_TEST_LOCK.lock().unwrap();
        reset_copy_buffers();
        record_raw_output_fragment("# Old turn poem\n");
        begin_turn_capture();
        record_raw_output_fragment("# New turn text\n");
        let snap = last_output_snapshot();
        assert_eq!(snap, "# New turn text\n");
        assert!(!snap.contains("Old turn"));
        reset_copy_buffers();
    }

    #[test]
    fn copy_region_per_heading_keeps_title_with_body() {
        let _g = COPY_TEST_LOCK.lock().unwrap();
        reset_copy_buffers();
        record_raw_output_fragment(
            "Efendim, buyrun:\n### Şiir\nbir dize\niki dize\n***\n### Kapanış\nsöz\n",
        );
        let regions = copy_regions();
        let section = regions
            .iter()
            .find(|r| r.label.contains("Şiir"))
            .expect("heading section must exist");
        assert_eq!(section.body, "bir dize\niki dize");
        assert_eq!(section.body.lines().count(), 2);
        assert!(!regions.is_empty());
        reset_copy_buffers();
    }

    #[test]
    fn thematic_break_ends_section_before_postamble_chatter() {
        let _g = COPY_TEST_LOCK.lock().unwrap();
        reset_copy_buffers();
        record_raw_output_fragment(
            "### Şiir\ndize bir\ndize iki\n***\nBuradayım Efendim. Ne yapalım?\n",
        );
        let regions = copy_regions();
        let section = regions.first().expect("section must exist");
        assert_eq!(section.body, "dize bir\ndize iki");
        assert!(section.body.contains("dize iki"));
        assert!(!section.body.contains("Buradayım"));
        // Postamble chatter belongs to no section and there are sections available,
        // so no whole-output fallback region must be listed.
    }

    #[test]
    fn blockquote_and_fenced_options_under_headings_yield_pure_copy_text() {
        let _g = COPY_TEST_LOCK.lock().unwrap();
        reset_copy_buffers();
        record_raw_output_fragment(
            "### Seçenek 1: En Doğal ve Güçlü Yanıt (Tavsiyem budur)\n> \"Spot on. That was the exact pain point.\n>\n> The human developer shouldn't have to act as the agent's compiler.\"\n\n---\n\n### Seçenek 2: Kısa\n```text\n100%. Deterministic gates win.\n```\n",
        );
        let regions = copy_regions();
        assert_eq!(regions.len(), 2);
        assert!(regions[0].label.contains("Seçenek 1"));
        assert_eq!(
            regions[0].body,
            "Spot on. That was the exact pain point.\n\nThe human developer shouldn't have to act as the agent's compiler."
        );
        assert!(regions[1].label.contains("Seçenek 2"));
        assert_eq!(regions[1].body, "100%. Deterministic gates win.");
        reset_copy_buffers();
    }

    #[test]
    fn snapshot_prefers_raw_model_text_over_rendered_chrome() {
        let _g = COPY_TEST_LOCK.lock().unwrap();
        reset_copy_buffers();
        record_output_fragment("\x1b[1m╭─ ⚡ SPAVN · gpt-x\x1b[0m\n");
        record_output_fragment("  \x1b[2;36m▍\x1b[0m Rendered heading\n");
        record_raw_output_fragment("# Raw heading\n");
        record_raw_output_fragment("\nPlain prose with `code`.\n");
        let snap = last_output_snapshot();
        assert_eq!(snap, "# Raw heading\n\nPlain prose with `code`.\n");
        assert!(!snap.contains('╭'));
        reset_copy_buffers();
    }

    #[test]
    fn empty_raw_buffer_falls_back_to_rendered_snapshot() {
        let _g = COPY_TEST_LOCK.lock().unwrap();
        reset_copy_buffers();
        record_output_fragment("\x1b[1m╭─ frame\x1b[0m\nproto-only line\n");
        let snap = last_output_snapshot();
        assert_eq!(snap, "╭─ frame\nproto-only line\n");
        reset_copy_buffers();
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;

    /// SPINNER_DRAFT is process-global; serialize draft-touching tests.
    static DRAFT_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_draft() {
        if let Ok(mut state) = SPINNER_SCREEN.lock() {
            state.typing_buf.clear();
            state.typing_cursor = 0;
            state.typing_sel_anchor = None;
            state.is_typing = false;
        }
        if let Ok(mut slot) = SPINNER_DRAFT.lock() {
            *slot = None;
        }
    }

    #[test]
    fn take_drains_live_spinner_typing_buf_and_preserves_trailing_space() {
        let _g = DRAFT_TEST_LOCK.lock().unwrap();
        reset_draft();
        if let Ok(mut state) = SPINNER_SCREEN.lock() {
            state.typing_buf = "merhaba ".chars().collect();
            state.typing_cursor = state.typing_buf.len();
            state.is_typing = true;
        }
        assert_eq!(take_spinner_draft(), Some("merhaba ".to_string()));
        assert_eq!(take_spinner_draft(), None);
        if let Ok(state) = SPINNER_SCREEN.lock() {
            assert!(state.typing_buf.is_empty());
            assert!(!state.is_typing);
        }
    }

    #[test]
    fn take_on_empty_slot_returns_none() {
        let _g = DRAFT_TEST_LOCK.lock().unwrap();
        reset_draft();
        assert_eq!(take_spinner_draft(), None);
    }

    #[test]
    fn take_returns_draft_once_then_none() {
        let _g = DRAFT_TEST_LOCK.lock().unwrap();
        reset_draft();
        if let Ok(mut slot) = SPINNER_DRAFT.lock() {
            *slot = Some("merhaba dünya".to_string());
        }
        assert_eq!(take_spinner_draft(), Some("merhaba dünya".to_string()));
        // Consume-once: second read must not duplicate the prefill.
        assert_eq!(take_spinner_draft(), None);
    }

    #[test]
    fn last_write_wins_on_overwrite() {
        let _g = DRAFT_TEST_LOCK.lock().unwrap();
        reset_draft();
        if let Ok(mut slot) = SPINNER_DRAFT.lock() {
            *slot = Some("eski taslak".to_string());
        }
        if let Ok(mut slot) = SPINNER_DRAFT.lock() {
            *slot = Some("yeni taslak".to_string());
        }
        assert_eq!(take_spinner_draft(), Some("yeni taslak".to_string()));
    }
}

#[cfg(test)]
mod partial_memo_tests {
    use super::*;

    fn fresh_state() -> SpinnerScreenState {
        SpinnerScreenState {
            active: false,
            paused: false,
            label: String::new(),
            started_at: None,
            dock_lines: Vec::new(),
            partial_line: String::new(),
            partial_vw: 0,
            partial_esc: 0,
            partial_has_box: false,
            partial_prefix: String::new(),
            partial_shown: false,
            thinking_box_open: false,
            thinking_chars: 0,
            thinking_pending: String::new(),
            last_think_flush: None,
            last_render_at: None,
            typing_buf: Vec::new(),
            typing_cursor: 0,
            typing_sel_anchor: None,
            is_typing: false,
            steer_mode: None,
        }
    }

    /// The memoized visible width must equal `visible_width` at every step.
    #[test]
    fn partial_push_memo_matches_visible_width() {
        let mut s = fresh_state();
        let cases: &[&str] = &[
            "merhaba dünya",
            "Şiir çıktası: ççğğ_",
            "\u{1b}[31mkırmızı\u{1b}[0m normal",
            "\u{1b}]8;;https://o.sc\u{1b}\\link\u{1b}\\ OSC8",
            "CJK 以字段 한국어 width=2",
            "│╭├╰ box chars",
            "emoji 📋✓ ok",
        ];
        for case in cases {
            partial_screen_reset(&mut s);
            for ch in case.chars() {
                partial_screen_push(&mut s, ch);
                assert_eq!(
                    s.partial_vw,
                    visible_width(&s.partial_line),
                    "memo drift after char {:?} in {:?}",
                    ch,
                    case
                );
            }
        }
    }

    /// Escape state: while inside a CSI run no char must count width, and the
    /// memo must reopen normal counting once the sequence terminates.
    #[test]
    fn partial_push_ansi_sequences_are_invisible_to_wrap() {
        let mut s = fresh_state();
        for ch in "\u{1b}[31mA".chars() {
            partial_screen_push(&mut s, ch);
        }
        assert_eq!(s.partial_vw, 1);
        assert!(!s.partial_has_box);
        for ch in "\u{1b}[0msütun │".chars() {
            partial_screen_push(&mut s, ch);
        }
        assert!(s.partial_has_box);
        assert_eq!(s.partial_vw, visible_width(&s.partial_line));
    }

    /// Reset clears both the line and every memo; recount restores them from
    /// a restored prefix+carry combo (the wrap-continuation state).
    #[test]
    fn partial_reset_and_recount_restore_memos() {
        let mut s = fresh_state();
        for ch in "önceki satır metni │".chars() {
            partial_screen_push(&mut s, ch);
        }
        partial_screen_reset(&mut s);
        assert!(s.partial_line.is_empty());
        assert_eq!(s.partial_vw, 0);
        assert!(!s.partial_has_box);
        s.partial_prefix = "\u{1b}[34m❯\u{1b}[0m".to_string();
        for ch in "  devam metni │".chars() {
            partial_screen_push(&mut s, ch);
        }
        partial_screen_recount(&mut s);
        assert_eq!(s.partial_vw, visible_width(&s.partial_line));
        assert!(s.partial_has_box);
    }

    /// OSC sequences (\x1b]…BEL or \x1b]…\x1b\\) must count zero width, exactly
    /// like `skip_escape` does, otherwise the memo drifts from visible_width.
    #[test]
    fn partial_push_osc_sequences_count_zero_width() {
        let mut s = fresh_state();
        for ch in "\u{1b}]8;;https://o.sc\u{1b}\\link\u{1b}\\".chars() {
            partial_screen_push(&mut s, ch);
        }
        assert_eq!(s.partial_vw, visible_width(&s.partial_line));
        assert_eq!(s.partial_vw, 4);
        assert_eq!(s.partial_esc, 0);
    }
}
