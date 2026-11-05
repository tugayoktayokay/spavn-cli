//! Decoding of terminal navigation keys for the interactive pickers.
//!
//! Arrow/Home/End/Shift-Tab arrive as `ESC [ <final>` (xterm) or `ESC O <final>`
//! (application cursor mode). Every picker loop reads the same byte shapes, so the
//! classification lives here instead of being re-spelled per menu.

/// A navigation key recognised inside a raw read buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    /// `Shift+Tab` — reported as `ESC [ Z`.
    BackTab,
}

/// Classifies a raw `read(2)` buffer as a navigation key, or `None` when it is not one.
///
/// Callers treat `None` as "plain key": the buffer's first byte is then matched against
/// the usual control characters / printable range.
pub(super) fn decode_nav_key(buf: &[u8]) -> Option<NavKey> {
    if buf.len() < 3 || buf[0] != 27 || (buf[1] != b'[' && buf[1] != b'O') {
        return None;
    }
    match buf[2] {
        b'A' => Some(NavKey::Up),
        b'B' => Some(NavKey::Down),
        b'C' => Some(NavKey::Right),
        b'D' => Some(NavKey::Left),
        b'H' => Some(NavKey::Home),
        b'F' => Some(NavKey::End),
        b'Z' => Some(NavKey::BackTab),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_csi_arrows() {
        assert_eq!(decode_nav_key(b"\x1b[A"), Some(NavKey::Up));
        assert_eq!(decode_nav_key(b"\x1b[B"), Some(NavKey::Down));
        assert_eq!(decode_nav_key(b"\x1b[C"), Some(NavKey::Right));
        assert_eq!(decode_nav_key(b"\x1b[D"), Some(NavKey::Left));
    }

    #[test]
    fn decodes_application_cursor_mode_and_shift_tab() {
        assert_eq!(decode_nav_key(b"\x1bOA"), Some(NavKey::Up));
        assert_eq!(decode_nav_key(b"\x1b[Z"), Some(NavKey::BackTab));
    }

    #[test]
    fn rejects_lone_escape_plain_keys_and_truncated_sequences() {
        assert_eq!(decode_nav_key(b"\x1b"), None);
        assert_eq!(decode_nav_key(b"\x1b["), None);
        assert_eq!(decode_nav_key(b"a"), None);
        assert_eq!(decode_nav_key(b"\x1b[1~"), None);
        assert_eq!(decode_nav_key(b"\x1b[M"), None);
    }
}
