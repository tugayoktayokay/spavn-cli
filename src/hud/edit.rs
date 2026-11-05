//! Pure buffer-editing primitives for the dock prompt editor.
//!
//! Every function here is deterministic and I/O-free so cursor/word/selection behaviour can
//! be pinned down by tests without a terminal. `input.rs` owns the state machine; this module
//! owns the arithmetic it drives.

use super::{image_token_span_at_cursor, image_token_span_before_cursor};

/// Start of the word to the left of `pos`: skip trailing whitespace, then the word itself.
pub fn word_start_before(chars: &[char], pos: usize) -> usize {
    let mut i = pos.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

/// End of the word to the right of `pos`: skip leading whitespace, then the word itself.
pub fn word_end_after(chars: &[char], pos: usize) -> usize {
    let mut i = pos.min(chars.len());
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Option+Left: an inline `[🖼️ …]` token is one unit, otherwise jump a whole word back.
pub fn jump_left(chars: &[char], pos: usize) -> usize {
    if let Some((start, _)) = image_token_span_before_cursor(chars, pos) {
        return start;
    }
    word_start_before(chars, pos)
}

/// Option+Right: mirror of [`jump_left`].
pub fn jump_right(chars: &[char], pos: usize) -> usize {
    if let Some((_, end)) = image_token_span_at_cursor(chars, pos) {
        return end;
    }
    word_end_after(chars, pos)
}

/// Normalises an anchor/cursor pair into an ordered, non-empty range.
pub fn selection_range(anchor: Option<usize>, cursor: usize) -> Option<(usize, usize)> {
    let a = anchor?;
    let (s, e) = (a.min(cursor), a.max(cursor));
    (s < e).then_some((s, e))
}

/// Deletes the selection when one exists, otherwise the word before the cursor.
///
/// Returns the new cursor position. An inline image token is deleted atomically.
pub fn delete_word_backward(chars: &mut Vec<char>, cursor: usize) -> usize {
    let cursor = cursor.min(chars.len());
    if let Some((start, end)) = image_token_span_before_cursor(chars, cursor) {
        chars.drain(start..end);
        return start;
    }
    let start = word_start_before(chars, cursor);
    if start < cursor {
        chars.drain(start..cursor);
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn word_start_skips_trailing_space_then_the_word() {
        let c = v("git commit  ");
        assert_eq!(word_start_before(&c, c.len()), 4);
        assert_eq!(word_start_before(&c, 10), 4);
        assert_eq!(word_start_before(&c, 3), 0);
    }

    #[test]
    fn word_bounds_are_safe_at_both_edges() {
        let empty: Vec<char> = Vec::new();
        assert_eq!(word_start_before(&empty, 0), 0);
        assert_eq!(word_end_after(&empty, 0), 0);

        let c = v("abc");
        assert_eq!(word_start_before(&c, 0), 0);
        assert_eq!(word_end_after(&c, c.len()), c.len());
        // Out-of-range cursors clamp instead of panicking.
        assert_eq!(word_start_before(&c, 99), 0);
        assert_eq!(word_end_after(&c, 99), 3);
    }

    #[test]
    fn word_end_stops_at_whitespace_boundary() {
        let c = v("  ls -la foo");
        assert_eq!(word_end_after(&c, 0), 4);
        // Cursor on the space after `ls` lands past `-la`, i.e. back on a word boundary.
        assert_eq!(word_end_after(&c, 4), 8);
        assert_eq!(word_end_after(&c, 8), c.len());
    }

    #[test]
    fn jump_left_is_idempotent_on_a_word_start() {
        let c = v("a b c");
        let mut p = c.len();
        p = jump_left(&c, p);
        assert_eq!(p, 4);
        // Already at a word boundary: the next press still moves, onto the previous word.
        assert_eq!(jump_left(&c, 2), 0);
        assert_eq!(jump_left(&c, 0), 0);
    }

    #[test]
    fn multibyte_input_never_splits_a_code_point() {
        let c = v("şu dosyayı aç");
        let p = jump_left(&c, c.len());
        assert_eq!(p, 11);
        assert_eq!(c[p], 'a');

        let mut buf = c.clone();
        let len = buf.len();
        let p = delete_word_backward(&mut buf, len);
        assert_eq!(p, 11);
        assert_eq!(buf.iter().collect::<String>(), "şu dosyayı ");
    }

    #[test]
    fn image_token_is_deleted_and_skipped_as_one_unit() {
        let mut buf = v("bak [🖼️ logo.png] son");
        let close = buf.iter().position(|&c| c == ']').unwrap() + 1;

        // Jumping left from just after the token lands on its opening bracket.
        let p = jump_left(&buf, close);
        assert_eq!(buf[p], '[');

        let p = delete_word_backward(&mut buf, close);
        assert_eq!(p, 4);
        assert_eq!(buf.iter().collect::<String>(), "bak  son");
    }

    #[test]
    fn selection_range_orders_and_discards_empty_ranges() {
        assert_eq!(selection_range(Some(5), 2), Some((2, 5)));
        assert_eq!(selection_range(Some(2), 5), Some((2, 5)));
        assert_eq!(selection_range(Some(3), 3), None);
        assert_eq!(selection_range(None, 3), None);
    }

    #[test]
    fn delete_word_backward_at_origin_is_a_noop() {
        let mut buf = v("abc");
        assert_eq!(delete_word_backward(&mut buf, 0), 0);
        assert_eq!(buf.iter().collect::<String>(), "abc");
    }

    #[test]
    fn delete_word_backward_consumes_leading_whitespace_too() {
        let mut buf = v("foo   bar   ");
        let len = buf.len();
        let p = delete_word_backward(&mut buf, len);
        assert_eq!(p, 6);
        assert_eq!(buf.iter().collect::<String>(), "foo   ");
    }
}
