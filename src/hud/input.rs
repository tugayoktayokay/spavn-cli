use super::edit::{delete_word_backward, jump_left, jump_right, selection_range};
use super::*;

fn erase_inline_dock(prev_top_w: Option<usize>, _prev_cursor_col: usize) {
    if let Some(prev_top_w) = prev_top_w {
        let up_to_start = prev_top_w.max(1);
        print!("\x1b[{}A\r\x1b[J\x1b[?7h\x1b[?25h", up_to_start);
        let _ = io::stdout().flush();
    }
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

    #[cfg(unix)]
    crate::tools::reclaim_foreground_tty();
    let has_tty = unsafe { tcgetattr(0, &mut orig_termios) == 0 };
    if !has_tty {
        let mut line = String::new();
        if io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            return PromptEvent::Exit;
        }
        return PromptEvent::Submit(line.trim().to_string());
    }

    let _term_guard = TermiosGuard(orig_termios);
    // 100ms non-blocking timeout so resize events are picked up between keystrokes.
    enter_raw_mode(&orig_termios, 1);
    // Reserve 3 rows at the bottom (scrolling existing output up if already on the last row)
    // and then jump to the bottom-most row (`\x1b[999B`) so the 4-line dock always sits flush
    // at the bottom of the terminal without overwriting the last lines of assistant output.
    print!("\x1b[?2004h\n\n\n\x1b[999B");
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
    let mut prev_top_w: Option<usize> = None;
    PREV_DOCK_BOTTOM_COUNT.store(1, Ordering::SeqCst);
    let mut prev_cursor_col: usize = 1;
    let mut last_input_at = Instant::now();
    let mut idle_recap_shown = false;
    loop {
        let (rows, cols) = term_size();
        if (rows, cols) != (last_rows, last_cols) || crate::take_resized() {
            // Wait until active mouse-drag resize pauses for 50ms before redrawing,
            // and ignore SIGWINCH events from window moves where (rows, cols) did not change.
            let mut stable = (rows, cols);
            let mut quiet_ticks = 0u8;
            for _ in 0..24 {
                thread::sleep(Duration::from_millis(25));
                let _ = crate::take_resized();
                let next = term_size();
                if next == stable {
                    quiet_ticks += 1;
                    if quiet_ticks >= 2 {
                        break;
                    }
                } else {
                    stable = next;
                    quiet_ticks = 0;
                }
            }
            if stable != (last_rows, last_cols) {
                last_rows = stable.0;
                last_cols = stable.1;
                full_resize_pending = true;
                needs_redraw = true;
            }
        }

        // A finished update check parks its notice here; the dock is erased first so the advice
        // lands on a clean line instead of inside the prompt box.
        if let Some(info) = crate::update::take_pending_notice() {
            erase_inline_dock(prev_top_w, prev_cursor_col);
            print!("\n\r");
            prev_top_w = None;
            prev_cursor_col = 1;
            render_update_notice(&info);
            print!("\n\n\n\x1b[999B");
            let _ = io::stdout().flush();
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
                redraw_full_viewport(endpoint, *access, default_host, cwd, session_name, messages);
                print!("\n\n\n\x1b[999B\x1b[?2026l");
                let _ = io::stdout().flush();
                prev_top_w = None;
                prev_cursor_col = 1;
                full_resize_pending = false;
            }

            let recap_str = if idle_recap_shown {
                build_session_recap_line(messages, last_cols)
            } else {
                None
            };

            let cur_sel = selection_range(sel_anchor, cursor_pos);

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
                    if let Some((s, e)) = selection_range(sel_anchor.take(), cursor_pos) {
                        chars.drain(s..e);
                        cursor_pos = s;
                        continue;
                    }
                    cursor_pos = delete_word_backward(&mut chars, cursor_pos);
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
                    cursor_pos = jump_left(&chars, cursor_pos);
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
                    cursor_pos = jump_right(&chars, cursor_pos);
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
                                    cursor_pos =
                                        match image_token_span_before_cursor(&chars, cursor_pos) {
                                            Some((start, _)) => start,
                                            None if has_alt_or_ctrl => {
                                                jump_left(&chars, cursor_pos)
                                            }
                                            None => cursor_pos.saturating_sub(1),
                                        };
                                }
                                b'C' => {
                                    cursor_pos =
                                        match image_token_span_at_cursor(&chars, cursor_pos) {
                                            Some((_, end)) => end,
                                            None if has_alt_or_ctrl => {
                                                jump_right(&chars, cursor_pos)
                                            }
                                            None => (cursor_pos + 1).min(chars.len()),
                                        };
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
                    sel_anchor = None;
                    cursor_pos = 0;
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
                    if let Some((s, e)) = selection_range(sel_anchor.take(), cursor_pos) {
                        chars.drain(s..e);
                        cursor_pos = s;
                        continue;
                    }
                    cursor_pos = delete_word_backward(&mut chars, cursor_pos);
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
            partial_screen_reset(&mut state);
            state.partial_prefix.clear();
            state.partial_shown = false;
            state.thinking_box_open = false;
            state.thinking_chars = 0;
            state.thinking_pending.clear();
            state.last_think_flush = None;
            state.last_render_at = None;
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
                enter_raw_mode(&orig_termios, 0);
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
                        enter_raw_mode(&orig_termios, 0);
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
                                                let c = state.typing_cursor;
                                                state.typing_cursor =
                                                    delete_word_backward(&mut state.typing_buf, c);
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
                                            state.typing_cursor =
                                                jump_left(&state.typing_buf, state.typing_cursor);
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
                                            state.typing_cursor =
                                                jump_right(&state.typing_buf, state.typing_cursor);
                                            if state.typing_sel_anchor == Some(state.typing_cursor)
                                            {
                                                state.typing_sel_anchor = None;
                                            }
                                            bi += 2;
                                            continue;
                                        }
                                        bi += 1;
                                        if bi < n && (kbuf[bi] == b'[' || kbuf[bi] == b'O') {
                                            let bracket_ch = kbuf[bi];
                                            bi += 1;
                                            let csi_rem = &kbuf[bi..n];
                                            let parsed_mod_csi: Option<(usize, u8, u8)> = if csi_rem
                                                .starts_with(b"1;")
                                                && csi_rem.len() >= 4
                                            {
                                                Some((4, csi_rem[2], csi_rem[3]))
                                            } else if csi_rem.len() >= 2
                                                && matches!(csi_rem[0], b'2'..=b'9')
                                                && matches!(
                                                    csi_rem[1],
                                                    b'A' | b'B' | b'C' | b'D' | b'H' | b'F'
                                                )
                                            {
                                                Some((2, csi_rem[0], csi_rem[1]))
                                            } else if bracket_ch == b'['
                                                && !csi_rem.is_empty()
                                                && matches!(csi_rem[0], b'a' | b'b' | b'c' | b'd')
                                            {
                                                Some((1, b'2', csi_rem[0].to_ascii_uppercase()))
                                            } else {
                                                None
                                            };
                                            if let Some((adv, mod_byte, final_b)) = parsed_mod_csi {
                                                if matches!(mod_byte, b'2'..=b'9')
                                                    && matches!(
                                                        final_b,
                                                        b'A' | b'B' | b'C' | b'D' | b'H' | b'F'
                                                    )
                                                {
                                                    bi += adv;
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
                                                            if let Some((start, _)) =
                                                                image_token_span_before_cursor(
                                                                    &state.typing_buf,
                                                                    state.typing_cursor,
                                                                )
                                                            {
                                                                state.typing_cursor = start;
                                                            } else if has_alt_or_ctrl {
                                                                state.typing_cursor = jump_left(
                                                                    &state.typing_buf,
                                                                    state.typing_cursor,
                                                                );
                                                            } else {
                                                                state.typing_cursor = state
                                                                    .typing_cursor
                                                                    .saturating_sub(1);
                                                            }
                                                        }
                                                        b'C' => {
                                                            if let Some((_, end)) =
                                                                image_token_span_at_cursor(
                                                                    &state.typing_buf,
                                                                    state.typing_cursor,
                                                                )
                                                            {
                                                                state.typing_cursor = end;
                                                            } else if has_alt_or_ctrl {
                                                                state.typing_cursor = jump_right(
                                                                    &state.typing_buf,
                                                                    state.typing_cursor,
                                                                );
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
                                                        } else if let Some((_, end)) =
                                                            image_token_span_at_cursor(
                                                                &state.typing_buf,
                                                                state.typing_cursor,
                                                            )
                                                        {
                                                            state.typing_cursor = end;
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
                                                        } else if let Some((start, _)) =
                                                            image_token_span_before_cursor(
                                                                &state.typing_buf,
                                                                state.typing_cursor,
                                                            )
                                                        {
                                                            state.typing_cursor = start;
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
                                                        } else if let Some((start, end)) =
                                                            image_token_span_at_cursor(
                                                                &state.typing_buf,
                                                                state.typing_cursor,
                                                            )
                                                        {
                                                            state.typing_buf.drain(start..end);
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
                                            state.typing_sel_anchor = None;
                                            state.typing_cursor = 0;
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
                                                let c = state.typing_cursor;
                                                state.typing_cursor =
                                                    delete_word_backward(&mut state.typing_buf, c);
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
                            let draft_now: String = state.typing_buf.iter().collect();
                            if let Ok(mut slot) = super::SPINNER_DRAFT.lock() {
                                *slot = if draft_now.trim().is_empty() {
                                    None
                                } else {
                                    Some(draft_now)
                                };
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
                        if !state.thinking_pending.is_empty() {
                            flush_pending_thinking_locked(&mut state);
                        }
                        let recently_rendered = state
                            .last_render_at
                            .map(|t| t.elapsed() < Duration::from_millis(55))
                            .unwrap_or(false);
                        if !recently_rendered {
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
                            state.last_render_at = Some(Instant::now());
                        }
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
                // Keep `state.typing_buf` intact across mid-turn spinner restarts (tool calls,
                // multi-step LLM turns) so the user's in-progress typing never disappears while
                // the agent is still working. `take_spinner_draft()` drains `typing_buf` once
                // the turn finishes and hands it to `read_bottom_dock_input`.
                let draft: String = state.typing_buf.iter().collect();
                if let Ok(mut slot) = super::SPINNER_DRAFT.lock() {
                    *slot = if draft.trim().is_empty() {
                        None
                    } else {
                        Some(draft)
                    };
                }
                close_thinking_box_locked(&mut state);
                if state.active && !state.paused {
                    let mut out = String::from("\x1b[?2026h\x1b[?7l\r\x1b[J");
                    if !state.partial_line.is_empty() {
                        out.push_str("\x1b[?7h");
                        out.push_str(&state.partial_line);
                        out.push_str("\x1b[0m\n");
                        state.partial_line.clear();
                        partial_screen_reset(&mut state);
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
