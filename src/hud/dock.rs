use super::*;

/// Switches the controlling tty to non-canonical, no-echo mode with the given read timeout.
///
/// Callers must own a `TermiosGuard` built from the *original* termios so the tty is
/// always restored, including on panic or early return.
pub(crate) fn enter_raw_mode(orig_termios: &Termios, vtime: u8) {
    let mut raw = *orig_termios;
    raw.c_lflag &= !(ICANON | ECHO);
    raw.c_cc[VMIN_IDX] = 0;
    raw.c_cc[VTIME_IDX] = vtime;
    unsafe {
        tcsetattr(0, TCSANOW, &raw);
    }
}

#[repr(C)]
pub(crate) struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Termios {
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
pub(crate) struct Termios {
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
pub(crate) const NCCS: usize = 20;
#[cfg(not(target_os = "macos"))]
pub(crate) const NCCS: usize = 32;

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
pub(crate) fn empty_termios() -> Termios {
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
pub(crate) fn empty_termios() -> Termios {
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
    pub(crate) fn ioctl(fd: i32, request: u64, ...) -> i32;
    pub(crate) fn tcgetattr(fd: i32, termios_p: *mut Termios) -> i32;
    pub(crate) fn tcsetattr(fd: i32, optional_actions: i32, termios_p: *const Termios) -> i32;
    pub(crate) fn read(fd: i32, buf: *mut std::ffi::c_void, count: usize) -> isize;
}

#[cfg(windows)]
#[repr(C)]
pub(crate) struct Coord {
    x: i16,
    y: i16,
}

#[cfg(windows)]
#[repr(C)]
pub(crate) struct SmallRect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

#[cfg(windows)]
#[repr(C)]
pub(crate) struct ConsoleScreenBufferInfo {
    dw_size: Coord,
    dw_cursor_position: Coord,
    w_attributes: u16,
    sr_window: SmallRect,
    dw_maximum_window_size: Coord,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    pub(crate) fn GetStdHandle(n_std_handle: u32) -> *mut std::ffi::c_void;
    pub(crate) fn GetConsoleMode(h_console_handle: *mut std::ffi::c_void, lp_mode: *mut u32)
        -> i32;
    pub(crate) fn SetConsoleMode(h_console_handle: *mut std::ffi::c_void, dw_mode: u32) -> i32;
    pub(crate) fn GetConsoleScreenBufferInfo(
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
pub(crate) unsafe fn tcgetattr(_fd: i32, termios_p: *mut Termios) -> i32 {
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
pub(crate) unsafe fn tcsetattr(_fd: i32, _optional_actions: i32, termios_p: *const Termios) -> i32 {
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
pub(crate) unsafe fn read(_fd: i32, buf: *mut std::ffi::c_void, count: usize) -> isize {
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
pub(crate) type TcFlag = u64;
#[cfg(not(target_os = "macos"))]
pub(crate) type TcFlag = u32;

#[cfg(target_os = "macos")]
pub(crate) const TIOCGWINSZ: u64 = 0x4008_7468;
#[cfg(not(target_os = "macos"))]
pub(crate) const TIOCGWINSZ: u64 = 0x5413;

#[cfg(target_os = "macos")]
pub(crate) const ICANON: TcFlag = 0x0000_0100;
#[cfg(not(target_os = "macos"))]
pub(crate) const ICANON: TcFlag = 0x0000_0002;

pub(crate) const ECHO: TcFlag = 0x0000_0008;
pub(crate) const TCSANOW: i32 = 0;

/// Restores the saved termios on drop, so a missed return path or a panic never leaves the tty raw.
pub(crate) struct TermiosGuard(pub(crate) Termios);

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
pub(crate) const VMIN_IDX: usize = 16;
#[cfg(target_os = "macos")]
pub(crate) const VTIME_IDX: usize = 17;
#[cfg(not(target_os = "macos"))]
pub(crate) const VMIN_IDX: usize = 6;
#[cfg(not(target_os = "macos"))]
pub(crate) const VTIME_IDX: usize = 5;
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
            let rows = (ws.ws_row as usize).max(12);
            let cols = (ws.ws_col as usize).max(20);
            LAST_TERM_COLS.store(cols, Ordering::Relaxed);
            return (rows, cols);
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
pub(crate) fn project_file_list() -> Arc<Vec<String>> {
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
            (
                "/copy",
                tr_en(
                    "/copy (Son Çıktıdan Kopyalanabilir Alan Seç)",
                    "/copy (Pick Copyable Region From Last Output)",
                ),
            ),
            (
                "/update",
                tr_en("/update (Sürüm Denetimi)", "/update (Check for Updates)"),
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

pub(crate) fn image_token_span_before_cursor(
    chars: &[char],
    cursor_pos: usize,
) -> Option<(usize, usize)> {
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

pub(crate) fn image_token_span_at_cursor(
    chars: &[char],
    cursor_pos: usize,
) -> Option<(usize, usize)> {
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

pub(crate) fn style_inline_image_tokens(line: &str) -> String {
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
pub(crate) fn render_inline_dock(
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
    prev_top_w: &mut Option<usize>,
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

    const MAX_VISIBLE_INPUT_ROWS: usize = 4;
    let max_input_cols = full_w.saturating_sub(5).max(8);
    let (wrapped_input_lines, cursor_row, cursor_col_offset) =
        wrap_input_lines_with_cursor(input_chars, cursor_pos, sel_range, max_input_cols);
    let total_wrapped_rows = wrapped_input_lines.len().max(1);
    let win_start = if total_wrapped_rows <= MAX_VISIBLE_INPUT_ROWS {
        0
    } else {
        cursor_row
            .saturating_add(1)
            .saturating_sub(MAX_VISIBLE_INPUT_ROWS)
            .min(total_wrapped_rows - MAX_VISIBLE_INPUT_ROWS)
    };
    let win_end = (win_start + MAX_VISIBLE_INPUT_ROWS).min(total_wrapped_rows);
    let visible_input_slice = &wrapped_input_lines[win_start..win_end];
    let visible_cursor_row = cursor_row.saturating_sub(win_start);
    let formatted_input_lines: Vec<String> = visible_input_slice
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            let abs_idx = win_start + idx;
            let raw = if abs_idx == 0 {
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
        format!("{}{}{}", DIM, tr_en("/copy: Kopyala", "/copy: Copy"), RESET),
        format!("{}@: {}{}", DIM, tr_en("Dosya/Skill", "File/Skill"), RESET),
        format!("{}/model{}", DIM, RESET),
        format!("{}/diff{}", DIM, RESET),
        format!("{}/commit{}", DIM, RESET),
    ];
    if total_wrapped_rows > MAX_VISIBLE_INPUT_ROWS {
        h_items.insert(
            1,
            format!(
                "{}📋 {} {}{}",
                CYAN,
                total_wrapped_rows,
                tr_en("satır", "lines"),
                RESET
            ),
        );
    }
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

    let bottom_count = bottom_lines.len().max(1);
    let bottom_joined = bottom_lines.join("\n");

    // If the suggestion popup just closed (shrinking rows below the cursor), clear from the
    // old top anchor and re-pin the dock flush to the bottom terminal row (`\x1b[999B`).
    let prev_bottom_count = PREV_DOCK_BOTTOM_COUNT.swap(bottom_count, Ordering::SeqCst);
    if prev_top_w.is_some() && bottom_count < prev_bottom_count {
        if let Some(prev) = *prev_top_w {
            print!("\x1b[{}A\r\x1b[J\x1b[999B", prev.max(1));
        }
        *prev_top_w = None;
    }

    let geometry = compute_dock_geometry(
        *prev_top_w,
        top_count,
        input_line_count,
        bottom_count,
        visible_cursor_row,
    );
    let pin_bottom = if prev_top_w.is_none() {
        "\x1b[999B"
    } else {
        ""
    };
    let cursor_col = (5 + cursor_col_offset).min(full_w);
    print!(
        "\x1b[?2026h\x1b[?25l\x1b[?7l{}\x1b[{}A\r\x1b[J{}\n{}\n{}\x1b[?7h\x1b[{}A\r\x1b[{}G\x1b[?2026l",
        pin_bottom,
        geometry.up_to_start,
        top_joined,
        input_joined,
        bottom_joined,
        geometry.lines_below_cursor,
        cursor_col
    );
    let _ = io::stdout().flush();
    *prev_top_w = Some(geometry.next_top_w);
    *prev_cursor_col = cursor_col;
}
