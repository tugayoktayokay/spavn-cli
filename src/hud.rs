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
static USED_CONTEXT_TOKENS: AtomicUsize = AtomicUsize::new(1_200);
static MAX_CONTEXT_TOKENS: AtomicUsize = AtomicUsize::new(200_000);
static STREAM_EXTRA_TOKENS: AtomicUsize = AtomicUsize::new(0);

pub fn set_context_window_usage(used_tokens: usize, max_tokens: usize) {
    USED_CONTEXT_TOKENS.store(used_tokens, Ordering::Relaxed);
    MAX_CONTEXT_TOKENS.store(max_tokens.max(1), Ordering::Relaxed);
    STREAM_EXTRA_TOKENS.store(0, Ordering::Relaxed);
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
        "128k".to_string()
    } else if tokens == 262_144 {
        "256k".to_string()
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
    if is_en() {
        format!(
            "📊 {} {}{}{} / {}{} ({}%){}",
            bar,
            color,
            format_token_short(used),
            RESET,
            DIM,
            format_token_short(max),
            pct_display,
            RESET
        )
    } else {
        format!(
            "📊 {} {}{}{} / {}{} (%{}){}",
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
}

pub fn effective_quota_badge(endpoint: &ModelEndpoint, quota_badge: &str) -> String {
    if endpoint.is_local() {
        format!(
            "{}🟢 {}{}",
            GREEN,
            tr_en("Yerel VRAM (Sınırsız)", "Local VRAM (Unlimited)"),
            RESET
        )
    } else if endpoint.is_smart_pool_proxy() {
        format!("🔋 {}{}{}", DIM, quota_badge, RESET)
    } else {
        format!("{}🔑 BYOK API{}", YELLOW, RESET)
    }
}

struct SpinnerScreenState {
    active: bool,
    paused: bool,
    label: String,
    dock_lines: [String; 3],
    partial_line: String,
    partial_prefix: String,
    partial_shown: bool,
    thinking_box_open: bool,
    thinking_chars: usize,
    typing_buf: Vec<char>,
    is_typing: bool,
    steer_mode: Option<bool>,
}

static SPINNER_SCREEN: std::sync::Mutex<SpinnerScreenState> =
    std::sync::Mutex::new(SpinnerScreenState {
        active: false,
        paused: false,
        label: String::new(),
        dock_lines: [String::new(), String::new(), String::new()],
        partial_line: String::new(),
        partial_prefix: String::new(),
        partial_shown: false,
        thinking_box_open: false,
        thinking_chars: 0,
        typing_buf: Vec::new(),
        is_typing: false,
        steer_mode: None,
    });

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
    let max_w = cols.saturating_sub(4).max(20);
    let prev_lines = if state.partial_shown { 4 } else { 3 };
    let mut out = format!("\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[{}A\r\x1b[J", prev_lines);

    for ch in text.chars() {
        if ch == '\n' {
            out.push_str("\x1b[?7h");
            out.push_str(&state.partial_line);
            out.push_str("\x1b[0m\n");
            state.partial_line.clear();
        } else {
            state.partial_line.push(ch);
            if !in_incomplete_ansi(&state.partial_line) {
                let (_, vw) = truncate_ansi_to_width(&state.partial_line, usize::MAX);
                if (ch == ' ' && vw >= max_w.saturating_sub(10)) || vw >= max_w {
                    out.push_str("\x1b[?7h");
                    out.push_str(&state.partial_line);
                    out.push('\n');
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
    if !state.partial_line.is_empty() {
        let (preview, _) = truncate_ansi_to_width(&state.partial_line, max_w);
        out.push_str(&preview);
        out.push_str("\x1b[0m\n");
        state.partial_shown = true;
    } else {
        state.partial_shown = false;
    }
    out.push_str(&format!(
        "{}\n{}\n{}\n\x1b[?7h\x1b[?2026l",
        state.dock_lines[0], state.dock_lines[1], state.dock_lines[2]
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
            spinner_print_locked(
                state,
                &format!(
                    "{}╰────────────────────────────────────────────────────────────{}\n",
                    DIM, RESET
                ),
            );
        } else {
            spinner_print_locked(
                state,
                &format!(
                    "\n{}╰────────────────────────────────────────────────────────────{}\n",
                    DIM, RESET
                ),
            );
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
                    "{}╭─ 💭 {} ─────────────────────────────────────────{}\n{}{}│ ",
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
                let prev_lines = if state.partial_shown { 4 } else { 3 };
                let mut out = format!("\x1b[?2026h\x1b[?7l\x1b[{}A\r\x1b[J", prev_lines);
                if !state.partial_line.is_empty() {
                    out.push_str("\x1b[?7h");
                    out.push_str(&state.partial_line);
                    out.push_str("\x1b[0m\n");
                    state.partial_line.clear();
                }
                state.partial_shown = false;
                out.push_str("\x1b[?7h\x1b[?25h\x1b[?2026l");
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
                print!(
                    "\x1b[?2026h\x1b[?25l\x1b[?7l{}\n{}\n{}\n\x1b[?7h\x1b[?2026l",
                    state.dock_lines[0], state.dock_lines[1], state.dock_lines[2]
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
    quota_badge: &str,
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
    let eff_quota = effective_quota_badge(endpoint, quota_badge);
    let line = format!(
        "{}╭─{} {}{}{}{} {}│{} {}{}{} {}│{} {} {}│{} 🖥️  {}{}{} {}│{} {}",
        CYAN,
        RESET,
        BOLD,
        access_color,
        access.short_badge(),
        RESET,
        CYAN,
        RESET,
        phase_color,
        phase.short_badge(),
        RESET,
        CYAN,
        RESET,
        model_badge,
        CYAN,
        RESET,
        MAGENTA,
        default_host,
        RESET,
        CYAN,
        RESET,
        eff_quota
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

extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn tcgetattr(fd: i32, termios_p: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, optional_actions: i32, termios_p: *const Termios) -> i32;
    fn read(fd: i32, buf: *mut std::ffi::c_void, count: usize) -> isize;
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

#[cfg(target_os = "macos")]
const VMIN_IDX: usize = 16;
#[cfg(target_os = "macos")]
const VTIME_IDX: usize = 17;
#[cfg(not(target_os = "macos"))]
const VMIN_IDX: usize = 6;
#[cfg(not(target_os = "macos"))]
const VTIME_IDX: usize = 5;
pub fn term_size() -> (usize, usize) {
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
                "/local",
                tr_en("/local (Yerel Modeller)", "/local (Local Models)"),
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

#[allow(clippy::too_many_arguments)]
fn render_inline_dock(
    endpoint: &ModelEndpoint,
    access: AccessMode,
    phase: &crate::spavn_engine::AgentPhase,
    default_host: &str,
    quota_badge: &str,
    visible_input: &str,
    cursor_display_offset: usize,
    suggestions: &[(String, String)],
    selected_idx: Option<usize>,
    queued_count: usize,
    busy_mode: &str,
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
        format!(" · 📥:{}", queued_count)
    } else {
        String::new()
    };
    let mode_tag = if busy_mode == "queue" {
        tr_en("📥Sırala", "📥Queue")
    } else {
        tr_en("⚡Anında", "⚡Steer")
    };

    let lang_badge = if is_en() { "🌐EN" } else { "🌐TR" };
    let model_badge = if endpoint.is_local() {
        format!("🟢 {}{}{}", GREEN, endpoint.key, RESET)
    } else {
        format!("🧠 {}{}{}", GREEN, endpoint.key, RESET)
    };
    let status_items = vec![
        format!("{}{}{}{}", BOLD, access_color, access.short_badge(), RESET),
        format!("{}{}{}", phase_color, phase.short_badge(), RESET),
        model_badge,
        format!("🖥️  {}{}{}", MAGENTA, default_host, RESET),
        effective_quota_badge(endpoint, quota_badge),
        format!("{}{}{}", CYAN, lang_badge, RESET),
    ];
    set_sticky_dock_status(endpoint, access, phase, default_host, quota_badge);
    let top_prefix_first = format!("{}╭─{} ", CYAN, RESET);
    let top_prefix_cont = format!("{}│{}  ", CYAN, RESET);
    let top_sep = format!(" {}│{} ", CYAN, RESET);
    let top_lines = flex_wrap_items(
        &top_prefix_first,
        &top_prefix_cont,
        &top_sep,
        &status_items,
        full_w,
    );
    let top_count = top_lines.len().max(1);
    let top_joined = top_lines.join("\n");

    let raw_input = format!("{}│{} {}❯{} {}", CYAN, RESET, BOLD, RESET, visible_input);
    let (trunc_input, _) = truncate_ansi_to_width(&raw_input, full_w);

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
        let h_items = vec![
            format_context_usage_badge(),
            format!("{}{}{}", DIM, tr_en("Enter: Gönder", "Enter: Send"), RESET),
            format!("{}{}{}", DIM, tr_en("/: Menü", "/: Menu"), RESET),
            format!("{}/model{}", DIM, RESET),
            format!("{}/byok{}", DIM, RESET),
            format!("{}/queue({}{}){}", DIM, mode_tag, queue_tag, RESET),
            format!("{}/lang{}", DIM, RESET),
            format!(
                "{}{}{}",
                DIM,
                tr_en("Shift+Tab: Yetki", "Shift+Tab: Access"),
                RESET
            ),
            format!("{}{}{}", DIM, tr_en("Esc: Sil", "Esc: Clear"), RESET),
        ];
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
    let cursor_col = (5 + cursor_display_offset).min(full_w);
    print!(
        "\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[{}A\r\x1b[J{}\n{}\n{}\x1b[?7h\x1b[{}A\r\x1b[{}G\x1b[?25h\x1b[?2026l",
        up_to_start,
        top_joined,
        trunc_input,
        bottom_joined,
        bottom_count,
        cursor_col
    );
    let _ = io::stdout().flush();
    *prev_top_w = top_count;
    *prev_cursor_col = cursor_col;
}

fn erase_inline_dock(prev_top_w: usize, _prev_cursor_col: usize) {
    let up_to_start = prev_top_w.max(1);
    print!("\x1b[{}A\r\x1b[J\x1b[?7h\x1b[?25h", up_to_start);
    let _ = io::stdout().flush();
}

pub fn strip_internal_tags_for_display(raw: &str) -> String {
    let mut s = raw.trim();
    while let Some(start) = s.find("<spavn_knowledge_artifacts>") {
        if let Some(end) = s[start..].find("</spavn_knowledge_artifacts>") {
            s = s[start + end + "</spavn_knowledge_artifacts>".len()..].trim();
        } else {
            break;
        }
    }
    while let Some(start) = s.find("<active_skill") {
        if let Some(end) = s[start..].find("</active_skill>") {
            s = s[start + end + "</active_skill>".len()..].trim();
        } else {
            break;
        }
    }
    if s.starts_with("[SPAVN ROLE:") {
        if let Some(nl) = s.find("\n\n") {
            s = s[nl + 2..].trim();
        } else if let Some(nl) = s.find('\n') {
            s = s[nl + 1..].trim();
        }
    }
    s.to_string()
}

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
    cwd: &str,
    session_name: &str,
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

    let mut raw = orig_termios;
    raw.c_lflag &= !(ICANON | ECHO);
    raw.c_cc[VMIN_IDX] = 0;
    raw.c_cc[VTIME_IDX] = 1; // 100ms non-blocking timeout for resize detection
    unsafe {
        tcsetattr(0, TCSANOW, &raw);
    }
    print!("\x1b[?2004l\n\n\n\n\x1b[2A\r");
    let _ = io::stdout().flush();

    let mut chars: Vec<char> = initial_prefill.unwrap_or("").chars().collect();
    let mut cursor_pos: usize = chars.len();
    let mut hist_idx = history.len();
    let mut suggestion_idx: Option<usize> = None;
    let mut kbuf = [0u8; 1024];
    let (mut last_rows, mut last_cols) = term_size();
    let mut needs_redraw = true;
    let mut full_resize_pending = false;
    let busy_mode = load_user_config().busy_input_mode;
    let mut prev_top_w: usize = 0;
    let mut prev_cursor_col: usize = 1;

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

        let max_vis_cols = last_cols.saturating_sub(8).max(4);
        let buf: String = chars.iter().collect();
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

        if needs_redraw {
            if full_resize_pending {
                redraw_full_viewport(endpoint, *access, default_host, cwd, session_name, messages);
                print!("\n\n\n\n\x1b[2A\r");
                prev_top_w = 0;
                prev_cursor_col = 1;
                full_resize_pending = false;
            }

            let mut win_start = cursor_pos;
            let mut used_cols = 0usize;
            while win_start > 0 {
                let w = char_display_width(chars[win_start - 1]);
                if used_cols + w > max_vis_cols {
                    break;
                }
                used_cols += w;
                win_start -= 1;
            }
            let cursor_display_offset = used_cols;
            let mut win_end = cursor_pos;
            while win_end < chars.len() {
                let w = char_display_width(chars[win_end]);
                if used_cols + w > max_vis_cols {
                    break;
                }
                used_cols += w;
                win_end += 1;
            }
            let visible_slice: String = chars[win_start..win_end].iter().collect();

            render_inline_dock(
                endpoint,
                *access,
                phase,
                default_host,
                quota_badge,
                &visible_slice,
                cursor_display_offset,
                &suggestions,
                suggestion_idx,
                crate::queued_message_count(),
                &busy_mode,
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
            needs_redraw = true;
            n_read as usize
        };

        if n == 1 && kbuf[0] == 27 {
            if !chars.is_empty() {
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
                if rem.starts_with(b"\x1b[200~") || rem.starts_with(b"\x1b[201~") {
                    idx += 5;
                    continue;
                }
                if idx + 1 < n && (kbuf[idx] == b'[' || kbuf[idx] == b'O') {
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
                            if !cur_suggestions.is_empty() {
                                let cur = suggestion_idx.unwrap_or(0);
                                let prev = if cur == 0 {
                                    cur_suggestions.len() - 1
                                } else {
                                    cur - 1
                                };
                                suggestion_idx = Some(prev);
                            } else if !history.is_empty() && hist_idx > 0 {
                                hist_idx -= 1;
                                chars = history[hist_idx].chars().collect();
                                cursor_pos = chars.len();
                            }
                        }
                        b'B' => {
                            if !cur_suggestions.is_empty() {
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
                            if cursor_pos < chars.len() {
                                cursor_pos += 1;
                            }
                        }
                        b'D' => {
                            cursor_pos = cursor_pos.saturating_sub(1);
                        }
                        b'H' => {
                            cursor_pos = 0;
                        }
                        b'F' => {
                            cursor_pos = chars.len();
                        }
                        b'3' => {
                            if idx < n && kbuf[idx] == b'~' {
                                idx += 1;
                            }
                            if cursor_pos < chars.len() {
                                chars.remove(cursor_pos);
                            }
                        }
                        b'1' | b'7' => {
                            if idx < n && kbuf[idx] == b'~' {
                                idx += 1;
                            }
                            cursor_pos = 0;
                        }
                        b'4' | b'8' => {
                            if idx < n && kbuf[idx] == b'~' {
                                idx += 1;
                            }
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
            if b == b'['
                && idx + 3 < n
                && (&kbuf[idx..idx + 4] == b"200~" || &kbuf[idx..idx + 4] == b"201~")
            {
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
                    // If there are more printable bytes in this same pasted chunk, convert newline to space
                    if idx < n && kbuf[idx..n].iter().any(|&c| c >= 32) {
                        chars.insert(cursor_pos, ' ');
                        cursor_pos += 1;
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
                    erase_inline_dock(prev_top_w, prev_cursor_col);
                    unsafe {
                        tcsetattr(0, TCSANOW, &orig_termios);
                    }
                    return PromptEvent::Submit(submitted);
                }
                1 => cursor_pos = 0,
                5 => cursor_pos = chars.len(),
                2 => cursor_pos = cursor_pos.saturating_sub(1),
                6 => {
                    if cursor_pos < chars.len() {
                        cursor_pos += 1;
                    }
                }
                23 => {
                    while cursor_pos > 0 && chars[cursor_pos - 1].is_whitespace() {
                        chars.remove(cursor_pos - 1);
                        cursor_pos -= 1;
                    }
                    while cursor_pos > 0 && !chars[cursor_pos - 1].is_whitespace() {
                        chars.remove(cursor_pos - 1);
                        cursor_pos -= 1;
                    }
                }
                127 | 8 => {
                    if cursor_pos > 0 {
                        chars.remove(cursor_pos - 1);
                        cursor_pos -= 1;
                    }
                }
                21 => {
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
            state.partial_line.clear();
            state.partial_prefix.clear();
            state.partial_shown = false;
            state.thinking_box_open = false;
            state.thinking_chars = 0;
            if state.steer_mode.is_none() {
                state.steer_mode = Some(load_user_config().busy_input_mode != "queue");
            }
            let (_, cols) = term_size();
            let max_w = cols.saturating_sub(2).max(20);
            let steer_mode = state.steer_mode.unwrap_or(true);
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
            let top_raw = format!(
                "{} {}│{} {}⠋{} {} (0.0s)",
                status_hdr_init, CYAN, RESET, CYAN, RESET, state.label
            );
            let (top_line, _) = truncate_ansi_to_width(&top_raw, max_w);
            let typed: String = state.typing_buf.iter().collect();
            let mid_raw = if state.is_typing || !typed.is_empty() {
                format!(
                    "{}│{} {}❯{} {}{}█{}",
                    CYAN, RESET, BOLD, RESET, typed, CYAN, RESET
                )
            } else {
                format!(
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
                )
            };
            let (mid_line, _) = truncate_ansi_to_width(&mid_raw, max_w);
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
                tr_en(
                    "Tab: Mod Değiştir · Esc: İptal",
                    "Tab: Toggle Mode · Esc: Cancel"
                ),
                RESET
            );
            let (bot_line, _) = truncate_ansi_to_width(&bot_raw, max_w);
            state.dock_lines = [top_line, mid_line, bot_line];
            print!(
                "\x1b[?2026h\x1b[?25l\x1b[?7l{}\n{}\n{}\n\x1b[?7h\x1b[?2026l",
                state.dock_lines[0], state.dock_lines[1], state.dock_lines[2]
            );
            let _ = io::stdout().flush();
        }

        let handle = thread::spawn(move || {
            let mut orig_termios = empty_termios();
            let has_tty = unsafe { tcgetattr(0, &mut orig_termios) == 0 };
            if has_tty {
                let mut raw = orig_termios;
                raw.c_lflag &= !(ICANON | ECHO);
                raw.c_cc[VMIN_IDX] = 0;
                raw.c_cc[VTIME_IDX] = 0;
                unsafe {
                    tcsetattr(0, TCSANOW, &raw);
                }
            }
            let mut kbuf = [0u8; 32];
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let start = Instant::now();
            let mut idx = 0usize;
            let status_hdr = get_sticky_dock_status();

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
                            if n == 1 && kbuf[0] == 27 {
                                if state.is_typing || !state.typing_buf.is_empty() {
                                    state.typing_buf.clear();
                                    state.is_typing = false;
                                } else {
                                    crate::trigger_interrupt();
                                    should_break = true;
                                }
                            } else if kbuf[..n].contains(&3) {
                                crate::trigger_interrupt();
                                should_break = true;
                            } else if kbuf[0] == 27 {
                                // Ignore ANSI escape sequences (arrow keys, etc.) while busy
                            } else {
                                let mut bi = 0usize;
                                while bi < n {
                                    let b = kbuf[bi];
                                    match b {
                                        b'\t' => {
                                            let cur = state.steer_mode.unwrap_or(true);
                                            state.steer_mode = Some(!cur);
                                            bi += 1;
                                        }
                                        b'\r' | b'\n' => {
                                            let msg: String = state.typing_buf.iter().collect();
                                            let trimmed = msg.trim().to_string();
                                            state.typing_buf.clear();
                                            state.is_typing = false;
                                            if !trimmed.is_empty() {
                                                if trimmed.starts_with('/') {
                                                    crate::push_queued_message(trimmed.clone());
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
                                                    }
                                                } else if state.steer_mode.unwrap_or(true) {
                                                    crate::push_steer_message(trimmed);
                                                } else {
                                                    crate::push_queued_message(trimmed);
                                                }
                                            }
                                            bi += 1;
                                        }
                                        127 | 8 => {
                                            state.typing_buf.pop();
                                            if state.typing_buf.is_empty() {
                                                state.is_typing = false;
                                            }
                                            bi += 1;
                                        }
                                        21 => {
                                            state.typing_buf.clear();
                                            state.is_typing = false;
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
                                                for ch in s.chars() {
                                                    state.is_typing = true;
                                                    state.typing_buf.push(ch);
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
                let elapsed = start.elapsed().as_secs_f32();
                let (_, cols) = term_size();
                let max_w = cols.saturating_sub(2).max(20);

                if let Ok(mut state) = SPINNER_SCREEN.lock() {
                    if state.active && !state.paused {
                        let steer_mode = state.steer_mode.unwrap_or(true);
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
                            status_hdr,
                            CYAN,
                            RESET,
                            CYAN,
                            frames[idx % frames.len()],
                            RESET,
                            state.label,
                            elapsed,
                            queue_status
                        );
                        let (top_line, _) = truncate_ansi_to_width(&top_raw, max_w);

                        let typed: String = state.typing_buf.iter().collect();
                        let mid_raw = if state.is_typing || !typed.is_empty() {
                            format!(
                                "{}│{} {}❯{} {}{}█{}",
                                CYAN, RESET, BOLD, RESET, typed, CYAN, RESET
                            )
                        } else {
                            format!(
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
                            )
                        };
                        let (mid_line, _) = truncate_ansi_to_width(&mid_raw, max_w);

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
                            tr_en(
                                "Tab: Mod Değiştir · Esc: İptal",
                                "Tab: Toggle Mode · Esc: Cancel"
                            ),
                            RESET
                        );
                        let (bot_line, _) = truncate_ansi_to_width(&bot_raw, max_w);

                        state.dock_lines = [top_line, mid_line, bot_line];
                        let prev_lines = if state.partial_shown { 4 } else { 3 };
                        let mut out =
                            format!("\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[{}A\r\x1b[J", prev_lines);
                        if !state.partial_line.is_empty() {
                            let (preview, _) = truncate_ansi_to_width(
                                &state.partial_line,
                                max_w.saturating_sub(2),
                            );
                            out.push_str(&preview);
                            out.push_str("\x1b[0m\n");
                            state.partial_shown = true;
                        } else {
                            state.partial_shown = false;
                        }
                        out.push_str(&format!(
                            "{}\n{}\n{}\n\x1b[?7h\x1b[?2026l",
                            state.dock_lines[0], state.dock_lines[1], state.dock_lines[2]
                        ));
                        print!("{}", out);
                        let _ = io::stdout().flush();
                    }
                }
                idx += 1;
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
                    let prev_lines = if state.partial_shown { 4 } else { 3 };
                    let mut out = format!("\x1b[?2026h\x1b[?7l\x1b[{}A\r\x1b[J", prev_lines);
                    if !state.partial_line.is_empty() {
                        out.push_str("\x1b[?7h");
                        out.push_str(&state.partial_line);
                        out.push_str("\x1b[0m\n");
                        state.partial_line.clear();
                    }
                    state.partial_shown = false;
                    out.push_str("\x1b[?7h\x1b[?25h\x1b[?2026l");
                    print!("{}", out);
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
    endpoint: &ModelEndpoint,
    access: AccessMode,
    default_host: &str,
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

    let p_top = format!("{}╭─{} ", CYAN, RESET);
    let p_mid = format!("{}│{}  ", CYAN, RESET);
    let p_bot = format!("{}╰─{} ", CYAN, RESET);
    let sep = " · ".to_string();
    let row1_items = vec![
        format!("{}⚡ SPAVN v0.1 (spavn.dev){}", BOLD, RESET),
        format!("🧠 {}{}{}", GREEN, endpoint.label, RESET),
        format!("🛡️  {}{}{}", YELLOW, access.badge(), RESET),
    ];
    let row2_items = vec![
        format!("📂 {}{}{}", DIM, short_cwd, RESET),
        format!("🖥️  {}{}{}", MAGENTA, default_host, RESET),
        format!("💬 {}{}{}", DIM, short_session, RESET),
    ];

    println!();
    for line in flex_wrap_items(&p_top, &p_mid, &sep, &row1_items, full_w) {
        println!("{}", line);
    }
    let r2_lines = flex_wrap_items(&p_mid, &p_mid, &sep, &row2_items, full_w);
    let r2_len = r2_lines.len();
    for (i, line) in r2_lines.into_iter().enumerate() {
        if i + 1 == r2_len {
            println!("{}", line.replacen(&p_mid, &p_bot, 1));
        } else {
            println!("{}", line);
        }
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
    let (mut last_rows, mut last_cols) = term_size();
    let mut needs_redraw = true;

    let mut resize_clear = false;
    loop {
        let (rows, cols) = term_size();
        if (rows, cols) != (last_rows, last_cols) || crate::take_resized() {
            thread::sleep(Duration::from_millis(16));
            let (r2, c2) = term_size();
            last_rows = r2;
            last_cols = c2;
            resize_clear = true;
            needs_redraw = true;
        }

        if needs_redraw {
            let max_w = last_cols.saturating_sub(2).clamp(4, 78);
            if resize_clear {
                print!("\x1b[?25l\x1b[?7l\x1b[2J\x1b[3J\x1b[H");
                resize_clear = false;
            } else {
                print!("\x1b[?7l\x1b[{}A\r\x1b[J", total_lines);
            }
            let raw_hdr = format!(
                "{}╭─ {} {}(↑/↓/Tab: Gezin · Enter: Seç · Esc/q: İptal){} ",
                CYAN, title, YELLOW, CYAN
            );
            let (trunc_hdr, used_hdr) = truncate_ansi_to_width(&raw_hdr, max_w);
            let hdr_pad = "─".repeat(max_w.saturating_sub(used_hdr));
            println!("{}{}{}{}", trunc_hdr, CYAN, hdr_pad, RESET);

            let win_start = if selected >= max_vis_items {
                selected + 1 - max_vis_items
            } else {
                0
            };

            let end = (win_start + max_vis_items).min(items.len());
            for (i, item) in items.iter().enumerate().take(end).skip(win_start) {
                let row_str = if i == selected {
                    format!(
                        "{}│{} \x1b[1;36m❯\x1b[0m \x1b[1;7;36m {:<24} \x1b[0m │ {:<22} │ {}{}{}",
                        CYAN, RESET, item.key, item.badge, BOLD, item.desc, RESET
                    )
                } else {
                    format!(
                        "{}│{}   {:<24}   │ {:<22} │ {}{}{}",
                        CYAN, RESET, item.key, item.badge, DIM, item.desc, RESET
                    )
                };
                let (trunc_row, _) = truncate_ansi_to_width(&row_str, max_w);
                println!("{}", trunc_row);
            }
            let bot_pad = "─".repeat(max_w.saturating_sub(2).max(10));
            print!("{}╰{}{}\n\x1b[?7h", CYAN, bot_pad, RESET);
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
            Ok(n) => {
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
            let ctx_label = format_token_short(m.context_window.unwrap_or(32_768));
            local_models.push(MenuItem {
                key: format!("local:{}", m.id),
                badge,
                desc: format!(
                    "{} · {} ctx",
                    tr_en("Yerel GPU Sunucusu", "Local GPU Server"),
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
    let (mut last_rows, mut last_cols) = term_size();
    let mut needs_redraw = true;

    let mut resize_clear = false;
    loop {
        let (rows, cols) = term_size();
        if (rows, cols) != (last_rows, last_cols) || crate::take_resized() {
            thread::sleep(Duration::from_millis(16));
            let (r2, c2) = term_size();
            last_rows = r2;
            last_cols = c2;
            resize_clear = true;
            needs_redraw = true;
        }

        if needs_redraw {
            let max_w = last_cols.saturating_sub(2).clamp(20, 100);
            if resize_clear {
                print!("\x1b[?25l\x1b[?7l\x1b[2J\x1b[3J\x1b[H");
                resize_clear = false;
            } else {
                print!("\x1b[?7l\x1b[{}A\r\x1b[J", total_lines);
            }
            let raw_hdr = format!(
                "{}╭─ {} {}{} ",
                CYAN,
                tr_en("🧠 MODEL SEÇİMİ", "🧠 SELECT MODEL"),
                YELLOW,
                tr_en(
                    "(←/→: Sağlayıcı/Model Geçişi · ↑/↓: Seç · Enter: Onayla · Esc: İptal)",
                    "(←/→: Switch Column · ↑/↓: Select · Enter: Confirm · Esc: Cancel)"
                )
            );
            let raw_hdr = format!("{}{}", raw_hdr, CYAN);
            let (trunc_hdr, used_hdr) = truncate_ansi_to_width(&raw_hdr, max_w);
            let hdr_pad = "─".repeat(max_w.saturating_sub(used_hdr));
            println!("{}{}{}{}", trunc_hdr, CYAN, hdr_pad, RESET);

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
            let col_hdr = format!(
                "{}│{} {} {}│{} {}",
                CYAN, RESET, left_hdr_style, CYAN, RESET, right_hdr_style
            );
            let (trunc_col_hdr, _) = truncate_ansi_to_width(&col_hdr, max_w);
            println!("{}", trunc_col_hdr);

            let sep_right = "─".repeat(max_w.saturating_sub(32));
            let raw_sep = format!(
                "{}├──────────────────────────────┼{}{}",
                CYAN, sep_right, RESET
            );
            let (trunc_sep, _) = truncate_ansi_to_width(&raw_sep, max_w);
            println!("{}", trunc_sep);

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
                    if m_idx == cur_m_idx {
                        if focus_right {
                            format!(
                                "\x1b[1;36m❯\x1b[0m \x1b[1;7;36m {}{} \x1b[0m │ {} │ {}{}{}",
                                trunc_k, pad_k, m.badge, BOLD, m.desc, RESET
                            )
                        } else {
                            format!(
                                "\x1b[1;36m▸\x1b[0m  {}{}{}{}  │ {} │ {}{}{}",
                                BOLD, trunc_k, pad_k, RESET, m.badge, DIM, m.desc, RESET
                            )
                        }
                    } else {
                        format!(
                            "   {}{}{}  │ {} │ {}{}{}",
                            trunc_k, pad_k, RESET, m.badge, DIM, m.desc, RESET
                        )
                    }
                } else {
                    String::new()
                };

                let full_row = format!(
                    "{}│{}{}{}│{} {}",
                    CYAN, RESET, left_cell, CYAN, RESET, right_cell
                );
                let (trunc_row, _) = truncate_ansi_to_width(&full_row, max_w);
                println!("{}", trunc_row);
            }

            let bot_right = "─".repeat(max_w.saturating_sub(32));
            let raw_bot = format!(
                "{}╰──────────────────────────────┴{}{}",
                CYAN, bot_right, RESET
            );
            let (trunc_bot, _) = truncate_ansi_to_width(&raw_bot, max_w);
            print!("{}\n\x1b[?7h", trunc_bot);
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
            Ok(n) => {
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

/// Interactive BYOK Manager (`/byok`):
/// Allows any user to configure API keys for built-in providers (saved in `~/.spavn/secrets.env` with 0600)
/// OR add any custom OpenAI-compatible Base URL + Model + API Key into `~/.spavn/config.json`!
pub fn interactive_byok_manager() -> Option<String> {
    let catalog = builtin_byok_catalog();
    let mut items: Vec<MenuItem> = catalog
        .iter()
        .map(|g| {
            let has_key = g.is_local || resolve_api_key(&g.api_key_env).is_some();
            let badge = if g.is_local {
                format!(
                    "{}{}{}",
                    GREEN,
                    tr_en("🟢 Yerel (Anahtarsız)", "🟢 Local (Keyless)"),
                    RESET
                )
            } else if has_key {
                format!(
                    "{}{} ({}){}",
                    GREEN,
                    tr_en("🟢 Anahtar Kayıtlı", "🟢 Key Configured"),
                    g.api_key_env,
                    RESET
                )
            } else {
                format!(
                    "{}{} ({}){}",
                    YELLOW,
                    tr_en("⚪ Anahtar Gir", "⚪ Enter Key"),
                    g.api_key_env,
                    RESET
                )
            };
            MenuItem {
                key: g.id.clone(),
                badge,
                desc: format!("{} -> {}", g.title, g.base_url),
            }
        })
        .collect();

    items.push(MenuItem {
        key: "custom-provider".to_string(),
        badge: format!(
            "{}{}{}",
            CYAN,
            tr_en("➕ Özel Sağlayıcı Ekle", "➕ Add Custom Provider"),
            RESET
        ),
        desc: tr_en(
            "Kendi OpenAI-uyumlu Base URL, Model ve API Anahtarınızı ekleyin",
            "Add your own OpenAI-compatible Base URL, Model ID, and API Key",
        )
        .to_string(),
    });

    let chosen = select_interactive_menu(
        tr_en(
            "🔑 SPAVN BYOK SAĞLAYICI & ANAHTAR YÖNETİCİSİ (/byok)",
            "🔑 SPAVN BYOK PROVIDER & KEY MANAGER (/byok)",
        ),
        &items,
        0,
    )?;

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
        println!(
            "\n{}╭─ 🔑 {} {} (~/.spavn/secrets.env — 0600) ───────────{}",
            CYAN,
            group.title,
            tr_en("API ANAHTARI", "API KEY"),
            RESET
        );
        if let Some(new_key) = prompt_inline_line(&format!(
            "│ {} {}",
            group.api_key_env,
            tr_en(
                "değerini girin (Mevcut anahtarı korumak için boş Enter)",
                "value (Press Enter empty to keep existing key)"
            )
        )) {
            match upsert_spavn_secret(&group.api_key_env, &new_key) {
                Ok(()) => println!(
                    "{}│{} {}✅ {} {}{}",
                    CYAN,
                    RESET,
                    GREEN,
                    group.api_key_env,
                    tr_en(
                        "güvenli olarak ~/.spavn/secrets.env dosyasına kaydedildi.",
                        "securely saved to ~/.spavn/secrets.env."
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
        println!(
            "{}╰────────────────────────────────────────────────────────────────────────────{}",
            CYAN, RESET
        );
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
        "\n{}╭─ ⚡ SPAVN {}[{}]{}\n",
        CYAN, DIM, model_key, RESET
    ));
}

pub fn render_assistant_footer(
    model_key: &str,
    access_badge: &str,
    ttft_ms: u128,
    total_sec: f64,
    tok_per_sec: f64,
) {
    spinner_print(&format!(
        "{}╰─{} {}{} · {} · TTFT {}ms · {:.1}s · {:.0} tok/s{}\n",
        CYAN, RESET, DIM, model_key, access_badge, ttft_ms, total_sec, tok_per_sec, RESET
    ));
}

pub struct MarkdownStreamRenderer {
    at_line_start: bool,
    prefix_buf: String,
    in_code_block: bool,
    in_heading: bool,
    in_quote: bool,
    in_bold: bool,
    in_italic: bool,
    in_inline_code: bool,
    star_count: usize,
    out_buf: String,
}

impl MarkdownStreamRenderer {
    pub fn new() -> Self {
        Self {
            at_line_start: true,
            prefix_buf: String::new(),
            in_code_block: false,
            in_heading: false,
            in_quote: false,
            in_bold: false,
            in_italic: false,
            in_inline_code: false,
            star_count: 0,
            out_buf: String::new(),
        }
    }

    pub fn push(&mut self, chunk: &str) {
        for ch in chunk.chars() {
            self.push_char(ch);
        }
        if !self.out_buf.is_empty() {
            let text = std::mem::take(&mut self.out_buf);
            spinner_print(&text);
        }
    }

    pub fn finish(&mut self) {
        if self.at_line_start && !self.prefix_buf.is_empty() {
            let pending = std::mem::take(&mut self.prefix_buf);
            self.at_line_start = false;
            self.out_buf.push_str("  ");
            for ch in pending.chars() {
                self.emit_inline_char(ch);
            }
        }
        self.flush_stars(true);
        if !self.at_line_start {
            self.out_buf.push_str(RESET);
            self.out_buf.push('\n');
            self.at_line_start = true;
        } else {
            self.out_buf.push_str(RESET);
        }
        if !self.out_buf.is_empty() {
            let text = std::mem::take(&mut self.out_buf);
            spinner_print(&text);
        }
    }

    fn reset_line_styles(&mut self) {
        self.flush_stars(true);
        self.in_heading = false;
        self.in_quote = false;
        self.in_bold = false;
        self.in_italic = false;
        self.in_inline_code = false;
        self.out_buf.push_str(RESET);
        self.out_buf.push('\n');
        self.at_line_start = true;
    }

    fn flush_stars(&mut self, at_eol: bool) {
        if self.star_count == 0 {
            return;
        }
        let count = self.star_count;
        self.star_count = 0;
        if count >= 2 {
            self.in_bold = !self.in_bold;
            if self.in_heading {
                self.out_buf.push_str("\x1b[1;36m");
            } else if self.in_bold {
                self.out_buf.push_str("\x1b[1;97m");
            } else {
                self.out_buf.push_str("\x1b[22;39m");
            }
        } else if count == 1 && !at_eol {
            self.in_italic = !self.in_italic;
            if self.in_italic {
                self.out_buf.push_str("\x1b[3m");
            } else {
                self.out_buf.push_str("\x1b[23m");
            }
        }
    }

    fn emit_inline_char(&mut self, ch: char) {
        if self.in_code_block {
            if ch == '\n' {
                self.out_buf.push_str(RESET);
                self.out_buf.push('\n');
                self.at_line_start = true;
            } else {
                self.out_buf.push(ch);
            }
            return;
        }

        if ch == '\n' {
            self.reset_line_styles();
            return;
        }

        if ch == '*' {
            self.star_count += 1;
            if self.star_count == 2 {
                self.flush_stars(false);
            }
            return;
        }

        if self.star_count == 1 && ch == ' ' {
            self.star_count = 0;
            self.out_buf.push_str("* ");
            return;
        }
        self.flush_stars(false);

        if ch == '`' {
            self.in_inline_code = !self.in_inline_code;
            if self.in_inline_code {
                self.out_buf.push_str("\x1b[36m");
            } else if self.in_heading {
                self.out_buf.push_str("\x1b[1;36m");
            } else if self.in_bold {
                self.out_buf.push_str("\x1b[1;97m");
            } else {
                self.out_buf.push_str("\x1b[39m");
            }
            return;
        }

        if ch == '|' && !self.in_inline_code {
            self.out_buf.push_str(&format!(
                "{}│{}",
                DIM,
                if self.in_heading {
                    "\x1b[1;36m"
                } else if self.in_bold {
                    "\x1b[1;97m"
                } else {
                    RESET
                }
            ));
            return;
        }

        self.out_buf.push(ch);
    }

    fn push_char(&mut self, ch: char) {
        if !self.at_line_start {
            self.emit_inline_char(ch);
            return;
        }

        if self.in_code_block {
            self.prefix_buf.push(ch);
            if ch == '\n' {
                let line = self.prefix_buf.trim_end_matches(['\r', '\n']);
                if line.trim().starts_with("```") {
                    self.in_code_block = false;
                    let rule_w = term_size().1.saturating_sub(8).clamp(16, 60);
                    self.out_buf
                        .push_str(&format!("  {}╰{}{}\n", DIM, "─".repeat(rule_w), RESET));
                } else {
                    self.out_buf
                        .push_str(&format!("  {}│{} {}{}{}\n", DIM, RESET, CYAN, line, RESET));
                }
                self.prefix_buf.clear();
                self.at_line_start = true;
            } else {
                let trimmed = self.prefix_buf.trim_start();
                if !trimmed.is_empty() && !trimmed.starts_with('`') {
                    let pending = std::mem::take(&mut self.prefix_buf);
                    self.at_line_start = false;
                    self.out_buf
                        .push_str(&format!("  {}│{} {}", DIM, RESET, CYAN));
                    for c in pending.chars() {
                        self.emit_inline_char(c);
                    }
                }
            }
            return;
        }

        self.prefix_buf.push(ch);

        if ch == '\n' {
            let full_line = std::mem::take(&mut self.prefix_buf);
            let trimmed = full_line.trim();
            if trimmed.is_empty() {
                self.out_buf.push('\n');
                self.at_line_start = true;
                return;
            }
            if trimmed.starts_with("```") {
                let lang = trimmed.trim_start_matches('`').trim();
                let label = if lang.is_empty() { "kod" } else { lang };
                self.in_code_block = true;
                let rule_w = term_size()
                    .1
                    .saturating_sub(14 + label.chars().count())
                    .clamp(8, 52);
                self.out_buf.push_str(&format!(
                    "  {}╭─ 💻 {} {}{}\n",
                    DIM,
                    label,
                    "─".repeat(rule_w),
                    RESET
                ));
                self.at_line_start = true;
                return;
            }
            if trimmed.len() >= 3
                && (trimmed.chars().all(|c| c == '-')
                    || trimmed.chars().all(|c| c == '*')
                    || trimmed.chars().all(|c| c == '─'))
            {
                let rule_w = term_size().1.saturating_sub(6).clamp(16, 60);
                self.out_buf
                    .push_str(&format!("  {}{}{}\n", DIM, "─".repeat(rule_w), RESET));
                self.at_line_start = true;
                return;
            }
            self.at_line_start = false;
            self.out_buf.push_str("  ");
            for c in full_line.chars() {
                self.emit_inline_char(c);
            }
            return;
        }

        let leading_spaces = self.prefix_buf.chars().take_while(|&c| c == ' ').count();
        let rest = &self.prefix_buf[leading_spaces..];
        if rest.is_empty() {
            if leading_spaces >= 8 {
                let pending = std::mem::take(&mut self.prefix_buf);
                self.at_line_start = false;
                self.out_buf.push_str(&format!("  {}", pending));
            }
            return;
        }

        if rest.starts_with('#') {
            let hashes = rest.chars().take_while(|&c| c == '#').count();
            if rest.len() > hashes {
                let next_c = rest[hashes..].chars().next().unwrap_or(' ');
                if next_c == ' ' && hashes <= 6 {
                    self.prefix_buf.clear();
                    self.at_line_start = false;
                    self.in_heading = true;
                    self.out_buf.push_str("\n  \x1b[1;36m▍ ");
                } else {
                    let pending = rest.to_string();
                    self.prefix_buf.clear();
                    self.at_line_start = false;
                    self.out_buf.push_str("  ");
                    for c in pending.chars() {
                        self.emit_inline_char(c);
                    }
                }
            }
            return;
        }

        if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("• ") {
            let indent = " ".repeat(leading_spaces.min(6));
            self.prefix_buf.clear();
            self.at_line_start = false;
            self.out_buf
                .push_str(&format!("  {}{}\x1b[36m•{} ", indent, BOLD, RESET));
            return;
        }

        if rest.starts_with("> ") {
            self.prefix_buf.clear();
            self.at_line_start = false;
            self.in_quote = true;
            self.out_buf.push_str("  \x1b[2;36m▎\x1b[0m \x1b[3m");
            return;
        }

        if rest.starts_with('-') || rest.starts_with('*') {
            let is_bold_start =
                rest.starts_with("**") && rest.len() >= 3 && !rest.chars().all(|c| c == '*');
            let is_bullet_text = !rest.chars().all(|c| c == '-' || c == '*');
            if is_bold_start || is_bullet_text {
                let indent = " ".repeat(leading_spaces.min(6));
                let pending = rest.to_string();
                self.prefix_buf.clear();
                self.at_line_start = false;
                self.out_buf.push_str(&format!("  {}", indent));
                for c in pending.chars() {
                    self.emit_inline_char(c);
                }
            }
            return;
        }

        if rest.starts_with('`') {
            if !rest.chars().all(|c| c == '`') && !rest.starts_with("```") {
                let indent = " ".repeat(leading_spaces.min(6));
                let pending = rest.to_string();
                self.prefix_buf.clear();
                self.at_line_start = false;
                self.out_buf.push_str(&format!("  {}", indent));
                for c in pending.chars() {
                    self.emit_inline_char(c);
                }
            }
            return;
        }

        if rest
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            let after = &rest[digits.len()..];
            if after == ". " || after == ") " {
                let indent = " ".repeat(leading_spaces.min(6));
                self.prefix_buf.clear();
                self.at_line_start = false;
                self.out_buf
                    .push_str(&format!("  {}\x1b[1;36m{}.\x1b[0m ", indent, digits));
            } else if after.len() >= 2 || (!after.is_empty() && after != "." && after != ")") {
                let indent = " ".repeat(leading_spaces.min(6));
                let pending = rest.to_string();
                self.prefix_buf.clear();
                self.at_line_start = false;
                self.out_buf.push_str(&format!("  {}", indent));
                for c in pending.chars() {
                    self.emit_inline_char(c);
                }
            }
            return;
        }

        if rest == ">" {
            return;
        }

        let indent = " ".repeat(leading_spaces.min(6));
        let pending = rest.to_string();
        self.prefix_buf.clear();
        self.at_line_start = false;
        self.out_buf.push_str(&format!("  {}", indent));
        for c in pending.chars() {
            self.emit_inline_char(c);
        }
    }
}
