//! Pure text/ANSI geometry primitives.
//!
//! Everything in this module is a total function over its inputs: no terminal access, no
//! global state, no I/O. That keeps the width math and the dock geometry unit-testable,
//! which is exactly where the "prompt jumps into the scrollback" class of bugs used to hide.

pub const CYAN: &str = "\x1b[36m";
pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED: &str = "\x1b[31m";
pub const MAGENTA: &str = "\x1b[35m";
pub const DIM: &str = "\x1b[2m";
pub const BOLD: &str = "\x1b[1m";
pub const RESET: &str = "\x1b[0m";

/// Terminal cell width of a single char: 0 for combining/zero-width, 2 for CJK/emoji, else 1.
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

/// Printable cell width of `s`, ignoring ANSI escape sequences.
pub fn visible_width(s: &str) -> usize {
    let mut used = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            skip_escape(&mut chars);
            continue;
        }
        used += char_display_width(ch);
    }
    used
}

pub(crate) fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    if let Some(&next_c) = chars.peek() {
        if next_c == '[' {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else if next_c == ']' {
            // OSC sequence (\x1b]...\x07 or \x1b]...\x1b\\): zero printable width.
            chars.next();
            while let Some(c) = chars.next() {
                if c == '\x07' {
                    break;
                }
                if c == '\x1b' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
}

/// Wraps `display` in an arbitrary OSC 8 hyperlink (`\x1b]8;;<url>\x07<display>\x1b]8;;\x07`).
pub fn osc8_link(url: &str, display: &str) -> String {
    let clean = url.trim();
    if clean.is_empty() || clean.contains('\x1b') || clean.contains('\x07') {
        return display.to_string();
    }
    format!("\x1b]8;;{}\x07{}\x1b]8;;\x07", clean, display)
}

/// Wraps `display` in an OSC 8 terminal hyperlink (`file:///abs/path#L<line>`) when `path`
/// resolves to a local path. Because `skip_escape` and `truncate_ansi_to_width` treat OSC
/// sequences as zero-width, wrapping does not alter layout math.
pub fn osc8_file_link(path: &str, line: Option<usize>, display: &str) -> String {
    let clean = path.trim();
    if clean.is_empty() || clean.contains("://") || clean.contains('\x1b') {
        return display.to_string();
    }
    let abs = if clean.starts_with('/') {
        std::path::PathBuf::from(clean)
    } else if let Some(rest) = clean.strip_prefix("~/") {
        std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("/"))
            .join(rest)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(clean)
    };
    let frag = line
        .filter(|&l| l > 0)
        .map(|l| format!("#L{}", l))
        .unwrap_or_default();
    format!(
        "\x1b]8;;file://{}{}\x07{}\x1b]8;;\x07",
        abs.display(),
        frag,
        display
    )
}

/// Truncates `s` to at most `max_cols` printable cells, preserving any escape sequences it
/// walks past and always terminating the result with a `RESET`.
pub fn truncate_ansi_to_width(s: &str, max_cols: usize) -> (String, usize) {
    let mut out = String::with_capacity(s.len());
    let mut used = 0usize;
    let mut chars = s.chars().peekable();
    let mut in_osc8 = false;
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
                } else if next_c == ']' {
                    if let Some(c) = chars.next() {
                        out.push(c);
                    }
                    let mut osc_body = String::new();
                    while let Some(c) = chars.next() {
                        out.push(c);
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            if let Some(st) = chars.next() {
                                out.push(st);
                            }
                            break;
                        }
                        osc_body.push(c);
                    }
                    if osc_body.starts_with("8;;") {
                        in_osc8 = osc_body.len() > 3;
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
    if in_osc8 {
        out.push_str("\x1b]8;;\x07");
    }
    out.push_str(RESET);
    (out, used)
}

/// Like `truncate_ansi_to_width`, but reserves one cell for a trailing `…` whenever anything
/// is actually cut, so a clipped card header reads as clipped instead of as a line that simply
/// stops mid-word (a `bash` command showed as a bare `cd` looked like a failed tool call).
pub fn truncate_ansi_to_width_ellipsis(s: &str, max_cols: usize) -> (String, usize) {
    if max_cols < 2 || visible_width(s) <= max_cols {
        return truncate_ansi_to_width(s, max_cols);
    }
    let (head, used) = truncate_ansi_to_width(s, max_cols - 1);
    let mut out = head;
    if out.ends_with(RESET) {
        out.truncate(out.len() - RESET.len());
    }
    out.push('…');
    out.push_str(RESET);
    (out, used + 1)
}

/// Like `truncate_ansi_to_width_ellipsis`, but keeps both ends of the line.
///
/// A tool header is `… ⚡ bash [local] cd ~/apps/spavn-cli && cargo test`: cutting from the
/// right hides the only part that says what the command does, leaving a bare `cd <path>`.
/// So the middle goes and the tail survives. Colour codes make a backwards scan unsafe, so an
/// escaped string falls back to head-only truncation.
pub fn truncate_ansi_to_width_middle(s: &str, max_cols: usize) -> (String, usize) {
    let total = visible_width(s);
    if total <= max_cols || s.contains('\x1b') {
        return truncate_ansi_to_width_ellipsis(s, max_cols);
    }
    if max_cols <= 1 {
        return (format!("…{RESET}"), 1);
    }
    let budget = max_cols - 1;
    let (head, head_w) = plain_head(s, budget / 2);
    let (tail, tail_w) = plain_tail(s, budget - budget / 2);
    (format!("{head}…{tail}{RESET}"), head_w + 1 + tail_w)
}

/// Longest prefix of `s` that fits in `max_cols` printable cells, plus the width it uses.
fn plain_head(s: &str, max_cols: usize) -> (&str, usize) {
    let mut used = 0usize;
    for (i, ch) in s.char_indices() {
        let w = char_display_width(ch);
        if used + w > max_cols {
            return (&s[..i], used);
        }
        used += w;
    }
    (s, used)
}

/// Longest suffix of `s` that fits in `max_cols` printable cells, plus the width it uses.
fn plain_tail(s: &str, max_cols: usize) -> (&str, usize) {
    let mut used = 0usize;
    let mut start = s.len();
    for (i, ch) in s.char_indices().rev() {
        let w = char_display_width(ch);
        if used + w > max_cols {
            break;
        }
        used += w;
        start = i;
    }
    (&s[start..], used)
}

/// Greedy flex-wrap of `items` under `max_w`, continuing wrapped rows with `prefix_cont`.
/// Every returned line fits within `max_w` printable cells.
pub fn flex_wrap_items(
    prefix_first: &str,
    prefix_cont: &str,
    sep: &str,
    items: &[String],
    max_w: usize,
) -> Vec<String> {
    let pf_w = visible_width(prefix_first);
    let pc_w = visible_width(prefix_cont);
    let sep_w = visible_width(sep);

    let mut lines: Vec<String> = Vec::new();
    let mut cur_line = String::from(prefix_first);
    let mut cur_w = pf_w;
    let mut first_in_line = true;

    for item in items {
        let item_w = visible_width(item);
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

/// Wraps the input buffer into display rows and reports where the cursor landed.
/// Returns `(rows, cursor_row, cursor_col_offset)`.
pub fn wrap_input_lines_with_cursor(
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
        if i == cursor_pos && !chars.is_empty() {
            if in_sel {
                cur_line.push_str("\x1b[27m");
            }
            cur_line.push_str("\x1b[46;30m");
            cur_line.push(ch);
            cur_line.push_str(RESET);
            if in_sel {
                cur_line.push_str("\x1b[7m");
            }
        } else {
            cur_line.push(ch);
        }
        cur_w += w;
    }

    if in_sel {
        cur_line.push_str("\x1b[27m");
    }
    if !cursor_recorded {
        cursor_row = lines.len();
        cursor_col_offset = cur_w;
        if !chars.is_empty() {
            cur_line.push_str(&format!("{}█{}", CYAN, RESET));
        }
    }
    lines.push(cur_line);
    if lines.is_empty() {
        lines.push(String::new());
    }
    (lines, cursor_row, cursor_col_offset)
}

/// Vertical bookkeeping for the bottom input dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockGeometry {
    /// Rows to move up from the cursor row to land on the first dock row.
    pub up_to_start: usize,
    /// Rows between the cursor row and the last dock row.
    pub lines_below_cursor: usize,
    /// Value to carry into the next frame's `prev_top_w`.
    pub next_top_w: usize,
}

/// Computes the dock's vertical geometry.
///
/// Steady-state invariant: `up_to_start + lines_below_cursor == total_height - 1`, i.e. the
/// cursor row is always exactly between the rows above and below it. Breaking this invariant
/// is what used to push the prompt up into the scrollback.
pub fn compute_dock_geometry(
    prev_top_w: usize,
    top_count: usize,
    input_line_count: usize,
    bottom_count: usize,
    cursor_row: usize,
) -> DockGeometry {
    let next_top_w = top_count + cursor_row;
    DockGeometry {
        // The very first frame has no previous anchor; land on the last dock row instead.
        up_to_start: prev_top_w.max(1),
        lines_below_cursor: bottom_count + input_line_count.saturating_sub(1 + cursor_row),
        next_top_w,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_width_handles_ascii_wide_and_zero_width() {
        assert_eq!(char_display_width('a'), 1);
        assert_eq!(char_display_width('🟢'), 2);
        assert_eq!(char_display_width('\u{FE0F}'), 0);
        assert_eq!(char_display_width('\n'), 0);
    }

    #[test]
    fn truncate_keeps_escapes_and_counts_cells() {
        let (out, used) = truncate_ansi_to_width("\x1b[32mabcdef\x1b[0m", 3);
        assert_eq!(used, 3);
        assert!(out.starts_with("\x1b[32m"));
        assert!(out.contains("abc"));
        assert!(!out.contains('d'));
        assert!(out.ends_with(RESET));
    }

    #[test]
    fn truncate_ellipsis_marks_a_clipped_line() {
        let (out, used) = truncate_ansi_to_width_ellipsis("\x1b[32mabcdefghij\x1b[0m", 5);
        assert_eq!(used, 5);
        assert!(out.trim_end_matches(RESET).ends_with("abcd…"), "{out:?}");
        assert!(out.ends_with(RESET));
    }

    #[test]
    fn truncate_ellipsis_leaves_a_fitting_line_untouched() {
        let (out, used) = truncate_ansi_to_width_ellipsis("abc", 5);
        assert_eq!(used, 3);
        assert!(!out.contains('…'), "{out:?}");
    }

    #[test]
    fn truncate_middle_keeps_both_ends() {
        let long = "[local] cd ~/apps/spavn-cli && cargo test memory::";
        let (out, used) = truncate_ansi_to_width_middle(long, 30);
        assert_eq!(used, 30);
        assert!(out.starts_with("[local] cd"), "{out:?}");
        assert!(out.trim_end_matches(RESET).ends_with("memory::"), "{out:?}");
        assert!(out.ends_with(RESET));
    }

    #[test]
    fn truncate_middle_degrades_to_head_for_escaped_text() {
        let (out, used) = truncate_ansi_to_width_middle("\x1b[32mabcdefghij\x1b[0m", 5);
        assert_eq!(used, 5);
        assert!(out.trim_end_matches(RESET).ends_with("abcd…"), "{out:?}");
    }

    #[test]
    fn truncate_is_wide_char_aware() {
        let (_, used) = truncate_ansi_to_width("🟢🟢🟢", 4);
        assert_eq!(used, 4);
    }

    #[test]
    fn flex_wrap_never_exceeds_max_width() {
        let items: Vec<String> = ["alpha", "beta", "gamma", "delta", "epsilon"]
            .iter()
            .map(|s| format!("{}{}{}", DIM, s, RESET))
            .collect();
        for max_w in [12usize, 20, 33, 80] {
            for line in flex_wrap_items("head: ", "      ", " · ", &items, max_w) {
                assert!(
                    visible_width(&line) <= max_w,
                    "line `{line}` wider than {max_w}"
                );
            }
        }
    }

    #[test]
    fn flex_wrap_truncates_a_single_oversized_item() {
        let lines = flex_wrap_items("", "", " ", &["0123456789".to_string()], 4);
        assert_eq!(lines.len(), 1);
        assert_eq!(visible_width(&lines[0]), 4);
    }

    #[test]
    fn wrap_reports_cursor_at_end_of_wrapped_buffer() {
        let chars: Vec<char> = "abcdefghij".chars().collect();
        let (lines, cursor_row, cursor_col) = wrap_input_lines_with_cursor(&chars, 10, None, 4);
        assert_eq!(lines.len(), 2);
        assert_eq!(cursor_row, 1);
        assert_eq!(cursor_col, 2);
    }

    #[test]
    fn wrap_reports_cursor_row_for_mid_buffer_position() {
        let chars: Vec<char> = "abc".chars().collect();
        let (lines, cursor_row, cursor_col) = wrap_input_lines_with_cursor(&chars, 1, None, 8);
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor_row, 0);
        assert_eq!(cursor_col, 1);
    }

    #[test]
    fn wrap_starts_a_new_row_on_explicit_newline() {
        let chars: Vec<char> = "ab\ncd".chars().collect();
        let (lines, cursor_row, _) = wrap_input_lines_with_cursor(&chars, 5, None, 40);
        assert_eq!(lines.len(), 2);
        assert_eq!(cursor_row, 1);
    }

    #[test]
    fn dock_geometry_round_trips_to_a_cursor_centred_block() {
        // (top_count, input_line_count, bottom_count, cursor_row). `top_count` is `max(1)` at
        // every call site, so `top + row >= 1` always holds — the `.max(1)` clamp below is then a
        // no-op and the steady-state invariant is exact.
        let cases = [
            (2usize, 1usize, 4usize, 0usize),
            (1, 3, 4, 2),
            (1, 1, 1, 0),
            (3, 5, 4, 4),
            (2, 1, 4, 0),
        ];
        for (top, input, bottom, row) in cases {
            assert!(top + row >= 1, "case must stay production-reachable");
            let first = compute_dock_geometry(0, top, input, bottom, row);
            let steady = compute_dock_geometry(first.next_top_w, top, input, bottom, row);
            let total = top + input + bottom;
            assert_eq!(
                steady.up_to_start + steady.lines_below_cursor,
                total - 1,
                "geometry drifted for top={top} input={input} bottom={bottom} row={row}"
            );
            assert_eq!(steady.up_to_start, top + row);
        }
    }

    #[test]
    fn dock_geometry_first_frame_never_emits_zero_row_move() {
        // With no previous anchor the escape sequence must at least move one row (\x1b[1A).
        let first = compute_dock_geometry(0, 2, 1, 4, 0);
        assert_eq!(first.up_to_start, 1);
    }

    #[test]
    fn dock_geometry_reserves_rows_below_the_last_input_row() {
        let g = compute_dock_geometry(5, 2, 2, 3, 1);
        assert_eq!(g.lines_below_cursor, 3);
        assert_eq!(g.up_to_start, 5);
        assert_eq!(g.next_top_w, 3);
    }
}
