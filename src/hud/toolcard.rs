//! Pure renderer for the inline tool cards printed during a turn.
//!
//! A single turn can fire dozens of tools, so every card has to earn its rows. The body shows a
//! head+tail slice of the output plus an explicit elision count (never a silent cut), and
//! `edit`/`write` show the lines that actually changed instead of a byte tally. Everything here
//! is a total function over its inputs — no terminal access, no globals, no I/O — so the layout
//! is unit-testable without a tty.
//!
//! Shape (deliberately the same rail as the rest of the HUD, one row per body line, no extra
//! chrome):
//!
//! ```text
//! ╭─ ⚡ bash  [local] git status --short
//! │  M install.sh
//! │  M src/http.rs
//! │  … (37 lines hidden)
//! ╰─ ✓ 39 lines · 14ms
//! ```

use crate::ansi::{DIM, GREEN, RED, RESET, YELLOW};

/// Body rows a card may spend before the middle is elided.
pub const BODY_MAX_ROWS: usize = 8;

/// Body rows for an `edit`/`write` change preview.
pub const CHANGE_MAX_ROWS: usize = 10;

/// Hunks of a batched `edit` worth previewing. A model that packs ten hunks into one call
/// would otherwise push the diff past the card and hide the replacements entirely.
pub const CHANGE_MAX_HUNKS: usize = 3;

/// Strips ANSI escapes and control characters, expands tabs and normalizes CRLF.
///
/// Tool output routinely carries SGR colors, `\r` progress redraws, C1 control codes,
/// UTF-8 replacement characters (`\u{FFFD}`) and NUL bytes from binary/process output;
/// any of those would desync or garble a row that is drawn after the `│ ` rail.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    // OSC / DCS / SOS / PM / APC: terminated by BEL or ESC \.
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\r' => {}
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            '\x00'..='\x1f'
            | '\x7f'..='\u{009f}'
            | '\u{fffd}'
            | '\u{feff}'
            | '\u{fffe}'..='\u{ffff}'
            | '\u{e000}'..='\u{f8ff}' => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Detects whether a raw output line is corrupted binary data (e.g. Mach-O / SQLite /
/// compressed stream bytes decoded via `from_utf8_lossy` or Python `errors='replace'`).
pub fn is_binary_noise_line(raw_line: &str) -> bool {
    if raw_line.contains('\u{fffd}') || raw_line.contains('\0') {
        return true;
    }
    let mut bad_ctrl = 0usize;
    for ch in raw_line.chars() {
        if matches!(
            ch,
            '\x01'..='\x08'
                | '\x0e'..='\x1a'
                | '\x1c'..='\x1f'
                | '\x7f'..='\u{009f}'
                | '\u{e000}'..='\u{f8ff}'
        ) {
            bad_ctrl += 1;
            if bad_ctrl >= 2 {
                return true;
            }
        }
    }
    false
}

/// `… (N satır gizlendi)` marker for rows a card could not fit.
///
/// Kept in one place so the plain body and the change preview never drift into two spellings of
/// the same cut, and so it follows the user's language like the rest of the HUD.
fn hidden_marker(hidden: usize) -> String {
    format!(
        "… ({})",
        crate::config::tr_en(
            &format!("{} satır gizlendi", hidden),
            &format!(
                "{} line{} hidden",
                hidden,
                if hidden == 1 { "" } else { "s" }
            )
        )
    )
}

/// Head+tail elision. Returns at most `max_rows + 1` rows: the survivors plus one
/// `… (N lines hidden)` marker when anything was dropped, so the reader always knows a cut
/// happened and how big it was.
pub fn elide(rows: &[String], max_rows: usize) -> Vec<String> {
    let budget = max_rows.max(1);
    if rows.len() <= budget {
        return rows.to_vec();
    }
    let head = budget.div_ceil(2).max(1);
    let tail = budget.saturating_sub(head);
    let hidden = rows.len() - head - tail;
    let mut out: Vec<String> = rows[..head].to_vec();
    out.push(hidden_marker(hidden));
    out.extend_from_slice(&rows[rows.len() - tail..]);
    out
}

/// Splits tool output into `│ `-ready rows, dropping leading/trailing blank lines
/// and filtering out binary/garbled noise rows (`\u{FFFD}`, raw control bytes).
pub fn body_rows(output: &str, max_rows: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut dropped_binary = 0usize;
    for raw_line in output.lines() {
        if is_binary_noise_line(raw_line) {
            dropped_binary += 1;
            continue;
        }
        rows.push(sanitize(raw_line).trim_end().to_string());
    }
    while rows.first().is_some_and(|r| r.is_empty()) {
        rows.remove(0);
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    if rows.is_empty() && dropped_binary > 0 {
        return vec![format!(
            "… ({})",
            crate::config::tr_en(
                &format!("{} satır ikili/bozuk veri gizlendi", dropped_binary),
                &format!(
                    "{} binary line{} hidden",
                    dropped_binary,
                    if dropped_binary == 1 { "" } else { "s" }
                )
            )
        )];
    }
    elide(&rows, max_rows)
}

fn alias<'a>(val: &'a serde_json::Value, keys: &[&str]) -> &'a str {
    for k in keys {
        if let Some(s) = val.get(*k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s;
            }
        }
    }
    ""
}

const OLD_KEYS: &[&str] = &["old_text", "old_string", "old_str", "search", "target"];
const NEW_KEYS: &[&str] = &[
    "new_text",
    "new_string",
    "new_str",
    "replace",
    "replacement",
];
const CONTENT_KEYS: &[&str] = &["content", "text", "body", "code"];

/// Pairs of `(old, new)` text in a payload: batched hunks first, then the single-hunk shape.
///
/// One place decides what a payload means, so the change preview, the diffstat and the row
/// count can never disagree about the same call.
fn parse_inline_line_range(raw_path: &str) -> (&str, Option<(usize, usize)>) {
    if let Some(colon) = raw_path.rfind(':') {
        let suffix = &raw_path[colon + 1..];
        let first_seg = suffix.split(',').next().unwrap_or("");
        let (s_part, e_part) = first_seg.split_once('-').unwrap_or((first_seg, first_seg));
        if !s_part.is_empty()
            && s_part.bytes().all(|b| b.is_ascii_digit())
            && !e_part.is_empty()
            && e_part.bytes().all(|b| b.is_ascii_digit())
        {
            if let (Ok(s), Ok(e)) = (s_part.parse::<usize>(), e_part.parse::<usize>()) {
                if s > 0 && e >= s {
                    return (&raw_path[..colon], Some((s, e)));
                }
            }
        }
    }
    (raw_path, None)
}

fn parse_inline_start_line(raw_path: &str) -> (&str, Option<usize>) {
    let (clean, range) = parse_inline_line_range(raw_path);
    (clean, range.map(|(s, _)| s))
}

fn resolve_range_old_text(
    h: &serde_json::Value,
    root: &serde_json::Value,
    default_clean_path: &str,
    default_inline_range: Option<(usize, usize)>,
) -> String {
    let h_raw = alias(h, &["path", "file_path", "filepath", "file"]);
    let (h_clean, h_inline_range) = if h_raw.is_empty() {
        (default_clean_path, default_inline_range)
    } else {
        parse_inline_line_range(h_raw)
    };
    let start = h
        .get("start_line")
        .or_else(|| root.get("start_line"))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .or_else(|| h_inline_range.map(|(s, _)| s));
    let end = h
        .get("end_line")
        .or_else(|| root.get("end_line"))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .or_else(|| h_inline_range.map(|(_, e)| e))
        .or(start);
    let (Some(s), Some(e)) = (start, end) else {
        return String::new();
    };
    if s == 0 || e < s || h_clean.is_empty() {
        return String::new();
    }
    let Ok(content) = std::fs::read_to_string(h_clean) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    if s > lines.len() {
        return String::new();
    }
    let end_clamped = e.min(lines.len());
    lines[s - 1..end_clamped].join("\n")
}

fn hunk_pairs(tool_name: &str, val: &serde_json::Value) -> Vec<(String, String)> {
    let top_raw_path = alias(val, &["path", "file_path", "filepath", "file"]);
    let (top_clean_path, top_inline_range) = parse_inline_line_range(top_raw_path);
    if let Some(files) = val.get("files").and_then(|v| v.as_array()) {
        return files
            .iter()
            .map(|h| {
                let raw_o = alias(h, OLD_KEYS);
                let o = if raw_o.is_empty() && tool_name != "write" {
                    resolve_range_old_text(h, val, top_clean_path, top_inline_range)
                } else {
                    raw_o.to_string()
                };
                (o, alias(h, NEW_KEYS).to_string())
            })
            .filter(|(o, n)| !o.is_empty() || !n.is_empty())
            .collect();
    }
    if let Some(hunks) = val.get("edits").and_then(|v| v.as_array()) {
        return hunks
            .iter()
            .map(|h| {
                let raw_o = alias(h, OLD_KEYS);
                let o = if raw_o.is_empty() && tool_name != "write" {
                    resolve_range_old_text(h, val, top_clean_path, top_inline_range)
                } else {
                    raw_o.to_string()
                };
                (o, alias(h, NEW_KEYS).to_string())
            })
            .filter(|(o, n)| !o.is_empty() || !n.is_empty())
            .collect();
    }
    let raw_old = alias(val, OLD_KEYS);
    let old_text = if raw_old.is_empty() && tool_name != "write" {
        resolve_range_old_text(val, val, top_clean_path, top_inline_range)
    } else {
        raw_old.to_string()
    };
    // `write` replaces the whole file, so everything it carries is an addition; an `edit`
    // with no anchor is not a change we can show.
    let new_text = if old_text.is_empty() && tool_name == "write" {
        alias(val, CONTENT_KEYS).to_string()
    } else {
        alias(val, NEW_KEYS).to_string()
    };
    if old_text.is_empty() && new_text.is_empty() {
        Vec::new()
    } else {
        vec![(old_text, new_text)]
    }
}

fn find_snippet_start_line(file_content: &str, snippet: &str) -> Option<usize> {
    let trimmed = snippet.trim_matches('\n');
    if trimmed.is_empty() {
        return None;
    }
    if let Some(pos) = file_content.find(trimmed) {
        return Some(file_content[..pos].bytes().filter(|&b| b == b'\n').count() + 1);
    }
    let first_non_empty = trimmed.lines().find(|l| !l.trim().is_empty())?.trim();
    if first_non_empty.len() >= 4 {
        if let Some(pos) = file_content.find(first_non_empty) {
            return Some(file_content[..pos].bytes().filter(|&b| b == b'\n').count() + 1);
        }
    }
    None
}

/// Resolves the 1-based starting file line number for each hunk in an `edit`/`write` call.
pub fn resolve_hunk_start_lines(tool_name: &str, args_json: &str) -> Vec<usize> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return Vec::new();
    };
    if tool_name == "write" {
        return vec![1];
    }
    let top_raw_path = alias(&val, &["path", "file_path", "filepath", "file"]);
    let (top_clean_path, top_inline_line) = parse_inline_start_line(top_raw_path);
    let top_file_content = (!top_clean_path.is_empty())
        .then(|| std::fs::read_to_string(top_clean_path).ok())
        .flatten();

    let resolve_one = |h: &serde_json::Value, old_s: &str, new_s: &str| -> usize {
        if let Some(s) = h
            .get("start_line")
            .or_else(|| val.get("start_line"))
            .and_then(|v| v.as_u64())
            .filter(|&n| n > 0)
        {
            return s as usize;
        }
        if let Some(ins) = h
            .get("insert_after_line")
            .or_else(|| val.get("insert_after_line"))
            .and_then(|v| v.as_u64())
        {
            return (ins as usize) + 1;
        }
        let h_raw = alias(h, &["path", "file_path", "filepath", "file"]);
        let (h_clean, h_inline) = parse_inline_start_line(h_raw);
        if let Some(l) = h_inline.or(top_inline_line) {
            return l;
        }
        let content_buf;
        let content_ref = if !h_clean.is_empty() && h_clean != top_clean_path {
            content_buf = std::fs::read_to_string(h_clean).ok();
            content_buf.as_deref()
        } else {
            top_file_content.as_deref()
        };
        if let Some(fc) = content_ref {
            if let Some(l) =
                find_snippet_start_line(fc, new_s).or_else(|| find_snippet_start_line(fc, old_s))
            {
                return l;
            }
        }
        1
    };

    if let Some(files) = val.get("files").and_then(|v| v.as_array()) {
        return files
            .iter()
            .filter_map(|h| {
                let (o, n) = (alias(h, OLD_KEYS), alias(h, NEW_KEYS));
                (!o.is_empty() || !n.is_empty()).then(|| resolve_one(h, o, n))
            })
            .collect();
    }
    if let Some(hunks) = val.get("edits").and_then(|v| v.as_array()) {
        return hunks
            .iter()
            .filter_map(|h| {
                let (o, n) = (alias(h, OLD_KEYS), alias(h, NEW_KEYS));
                (!o.is_empty() || !n.is_empty()).then(|| resolve_one(h, o, n))
            })
            .collect();
    }
    let old_text = alias(&val, OLD_KEYS);
    let new_text = alias(&val, NEW_KEYS);
    if old_text.is_empty() && new_text.is_empty() {
        Vec::new()
    } else {
        vec![resolve_one(&val, old_text, new_text)]
    }
}

/// `(added, removed)` line counts for the card header, or `None` when nothing is previewable.
pub fn diff_stat(tool_name: &str, args_json: &str) -> Option<(usize, usize)> {
    let val = serde_json::from_str::<serde_json::Value>(args_json).ok()?;
    let mut added = 0usize;
    let mut removed = 0usize;
    for (old, new) in hunk_pairs(tool_name, &val) {
        added += new.lines().count();
        removed += old.lines().count();
    }
    if added == 0 && removed == 0 {
        None
    } else {
        Some((added, removed))
    }
}

/// Renders the change preview for an `edit`/`write` call from its arguments.
///
/// `-` rows are the replaced text, `+` rows the replacement, mirroring how the edit will land
/// so the reader can spot a wrong anchor without opening the file. Returns empty when the
/// payload has no recognizable text (nothing worth showing beats a misleading blank card).
pub fn change_rows(tool_name: &str, args_json: &str, max_rows: usize) -> Vec<String> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return Vec::new();
    };
    let mut rows: Vec<String> = Vec::new();
    let pairs = hunk_pairs(tool_name, &val);
    for (i, (old_text, new_text)) in pairs.iter().take(CHANGE_MAX_HUNKS).enumerate() {
        if i > 0 {
            rows.push("  ⋯".to_string());
        }
        push_hunk(&mut rows, old_text, new_text, max_rows);
    }
    if pairs.len() > CHANGE_MAX_HUNKS {
        rows.push(format!(
            "  ⋯ ({} more hunks)",
            pairs.len() - CHANGE_MAX_HUNKS
        ));
    }
    elide(&rows, max_rows)
}

fn push_hunk(rows: &mut Vec<String>, old_text: &str, new_text: &str, keep: usize) {
    let keep = keep.max(1);
    let mut old_rows: Vec<&str> = old_text.lines().collect();
    let mut new_rows: Vec<&str> = new_text.lines().collect();
    while old_rows.last().is_some_and(|r| r.trim().is_empty()) {
        old_rows.pop();
    }
    while new_rows.last().is_some_and(|r| r.trim().is_empty()) {
        new_rows.pop();
    }
    let shown_old = old_rows.len().min(keep);
    let shown_new = new_rows.len().min(keep);
    for r in &old_rows[..shown_old] {
        rows.push(format!("- {}", sanitize(r)));
    }
    if old_rows.len() > shown_old {
        rows.push(format!("- {}", hidden_marker(old_rows.len() - shown_old)));
    }
    if !old_rows.is_empty() {
        rows.push("  ⋯".to_string());
    }
    for r in &new_rows[..shown_new] {
        rows.push(format!("+ {}", sanitize(r)));
    }
    if new_rows.len() > shown_new {
        rows.push(format!("+ {}", hidden_marker(new_rows.len() - shown_new)));
    }
}

const BG_REMOVED: &str = "\x1b[48;5;52m";
const BG_ADDED: &str = "\x1b[48;5;22m";
const BG_REMOVED_WORD: &str = "\x1b[1;48;5;88m\x1b[97m";
const BG_ADDED_WORD: &str = "\x1b[1;48;5;28m\x1b[97m";

/// Highlights the changed intra-line span between `row` (`- ...` or `+ ...`) and its paired
/// counterpart line (`peer`), keeping the outer line on `base_bg + base_fg` and painting only
/// the modified middle token(s) with `word_bg`.
fn style_word_diff_line(
    row: &str,
    peer: Option<&str>,
    base_bg: &str,
    base_fg: &str,
    word_bg: &str,
) -> String {
    let Some(peer_raw) = peer else {
        return format!("{}{}{}{}{}", base_bg, base_fg, row, RESET, RESET);
    };
    let (sign, body) = if let Some(b) = row.strip_prefix("- ") {
        ("- ", b)
    } else if let Some(b) = row.strip_prefix("+ ") {
        ("+ ", b)
    } else {
        return format!("{}{}{}{}{}", base_bg, base_fg, row, RESET, RESET);
    };
    let peer_body = peer_raw
        .strip_prefix("- ")
        .or_else(|| peer_raw.strip_prefix("+ "))
        .unwrap_or(peer_raw);
    if body == peer_body || body.is_empty() || peer_body.is_empty() {
        return format!("{}{}{}{}{}", base_bg, base_fg, row, RESET, RESET);
    }
    let a_chars: Vec<char> = body.chars().collect();
    let b_chars: Vec<char> = peer_body.chars().collect();
    let mut pref = 0usize;
    while pref < a_chars.len() && pref < b_chars.len() && a_chars[pref] == b_chars[pref] {
        pref += 1;
    }
    let mut suff = 0usize;
    while suff < (a_chars.len() - pref)
        && suff < (b_chars.len() - pref)
        && a_chars[a_chars.len() - 1 - suff] == b_chars[b_chars.len() - 1 - suff]
    {
        suff += 1;
    }
    // Only use intra-line highlight when the two lines share a meaningful anchor (prefix or suffix)
    // and the changed span is a sub-segment of the line.
    if pref + suff < 2 || (pref == 0 && suff == 0) || pref + suff >= a_chars.len() {
        return format!("{}{}{}{}{}", base_bg, base_fg, row, RESET, RESET);
    }
    let prefix_str: String = a_chars[..pref].iter().collect();
    let mid_str: String = a_chars[pref..a_chars.len() - suff].iter().collect();
    let suffix_str: String = a_chars[a_chars.len() - suff..].iter().collect();
    format!(
        "{}{}{}{}{}{}\x1b[22m{}{}{}{}{}",
        base_bg,
        base_fg,
        sign,
        prefix_str,
        word_bg,
        mid_str,
        base_bg,
        base_fg,
        suffix_str,
        RESET,
        RESET
    )
}

/// Colors `- `/`+ ` change rows, leaving everything else dim.
pub fn style_body_row(row: &str) -> String {
    let trimmed = row.trim_start();
    if trimmed.starts_with("- ") {
        format!("{}{}{}", RED, row, RESET)
    } else if trimmed.starts_with("+ ") {
        format!("{}{}{}", GREEN, row, RESET)
    } else if trimmed.starts_with("  ⋯") || trimmed.starts_with("… (") {
        format!("{}{}", DIM, row)
    } else {
        row.to_string()
    }
}

/// Prefixes a row with the `│ ` rail, truncating it to `cols` printable cells first.
pub fn rail_row(row: &str, cols: usize) -> String {
    let styled = style_body_row(row);
    let (trunc, _) = crate::ansi::truncate_ansi_to_width(&styled, cols.saturating_sub(4).max(8));
    format!("{}│{} {}", DIM, RESET, trunc)
}

/// True for the `…`/`⋯` markers that stand in for rows a card could not show.
///
/// A truncated side of a hunk is written `- … (N …)` so it stays inside the coloured block it
/// belongs to; the sign is stripped before matching so it still takes a blank gutter.
pub fn is_marker_row(row: &str) -> bool {
    let t = row
        .trim_start()
        .strip_prefix("- ")
        .or_else(|| row.trim_start().strip_prefix("+ "))
        .unwrap_or(row.trim_start());
    t.starts_with('⋯') || t.starts_with("… (")
}

/// One preview row with a line-number gutter and the changed text on a solid block.
///
/// Plain `│ - text` in dim red read as a faint paragraph rather than a diff; the block makes
/// an insertion and a deletion distinguishable at a glance, which is the whole point of the
/// preview. `number` is `None` for elision markers so the gutter never numbers a non-row.
pub fn diff_row(number: Option<usize>, row: &str, cols: usize) -> String {
    diff_row_sized(number, row, cols, 2)
}

/// `diff_row` with an explicit gutter width, so a 12-line hunk does not spend four cells on
/// padding while a 1 200-line file would otherwise lose its number.
pub fn diff_row_sized(number: Option<usize>, row: &str, cols: usize, gutter_w: usize) -> String {
    diff_row_sized_with_peer(number, row, None, cols, gutter_w)
}

pub fn diff_row_sized_with_peer(
    number: Option<usize>,
    row: &str,
    peer: Option<&str>,
    cols: usize,
    gutter_w: usize,
) -> String {
    let gutter_w = gutter_w.max(2);
    let gutter = match number {
        Some(n) => format!("{}│{:>width$} {}", DIM, n, RESET, width = gutter_w),
        None => format!("{}│{} {}", DIM, " ".repeat(gutter_w), RESET),
    };
    let styled = if number.is_none() {
        let t = row.trim_start();
        let bare = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("+ "))
            .unwrap_or(t);
        format!("{}{}", DIM, bare)
    } else {
        let t = row.trim_start();
        if t.starts_with("- ") {
            style_word_diff_line(row, peer, BG_REMOVED, RED, BG_REMOVED_WORD)
        } else if t.starts_with("+ ") {
            style_word_diff_line(row, peer, BG_ADDED, GREEN, BG_ADDED_WORD)
        } else {
            row.to_string()
        }
    };
    let budget = cols.saturating_sub(gutter_w + 5).max(8);
    let (trunc, _) = crate::ansi::truncate_ansi_to_width(&styled, budget);
    format!("{}{}", gutter, trunc)
}

/// Numbers each run of preview rows from 1 (or from the resolved file starting lines in
/// `hunk_starts`), restarting after any marker and pairing `-`/`+` lines within each hunk for
/// word-level intra-line diff emphasis.
pub fn numbered_diff_rows(rows: &[String], cols: usize) -> Vec<String> {
    numbered_diff_rows_at(rows, cols, &[])
}

pub fn numbered_diff_rows_for_call(
    tool_name: &str,
    args_json: &str,
    rows: &[String],
    cols: usize,
) -> Vec<String> {
    let starts = resolve_hunk_start_lines(tool_name, args_json);
    numbered_diff_rows_at(rows, cols, &starts)
}

pub fn numbered_diff_rows_at(rows: &[String], cols: usize, hunk_starts: &[usize]) -> Vec<String> {
    // Split into contiguous non-marker runs so we can:
    // 1. Know whether a `+` run is the replacement half of the preceding `-` run (same hunk),
    //    so both sides share the same file starting line and pair line-by-line for word diff.
    // 2. Size the gutter width for the highest line number across all runs.
    let mut blocks: Vec<Vec<usize>> = Vec::new();
    let mut cur_block: Vec<usize> = Vec::new();
    for (idx, row) in rows.iter().enumerate() {
        if is_marker_row(row) {
            if !cur_block.is_empty() {
                blocks.push(std::mem::take(&mut cur_block));
            }
        } else {
            cur_block.push(idx);
        }
    }
    if !cur_block.is_empty() {
        blocks.push(cur_block);
    }

    let mut block_start_lines: Vec<usize> = Vec::with_capacity(blocks.len());
    let mut peer_map: Vec<Option<usize>> = vec![None; rows.len()];
    let mut hunk_idx = 0usize;
    let mut bi = 0usize;
    while bi < blocks.len() {
        let base_line = hunk_starts.get(hunk_idx).copied().unwrap_or(1).max(1);
        let b_is_minus = blocks[bi]
            .first()
            .is_some_and(|&r| rows[r].trim_start().starts_with("- "));
        let next_is_plus = blocks.get(bi + 1).is_some_and(|nb| {
            nb.first()
                .is_some_and(|&r| rows[r].trim_start().starts_with("+ "))
        });
        block_start_lines.push(base_line);
        if b_is_minus && next_is_plus {
            // Pair corresponding lines between the `-` block and `+` block of the same hunk.
            block_start_lines.push(base_line);
            let pair_len = blocks[bi].len().min(blocks[bi + 1].len());
            for (&r_minus, &r_plus) in blocks[bi].iter().zip(blocks[bi + 1].iter()).take(pair_len) {
                peer_map[r_minus] = Some(r_plus);
                peer_map[r_plus] = Some(r_minus);
            }
            bi += 2;
        } else {
            bi += 1;
        }
        hunk_idx += 1;
    }

    let max_line_num = blocks
        .iter()
        .zip(block_start_lines.iter())
        .map(|(b, &start)| start + b.len().saturating_sub(1))
        .max()
        .unwrap_or(1);
    let gutter_w = max_line_num.max(1).to_string().len().max(2);

    let mut line_numbers: Vec<Option<usize>> = vec![None; rows.len()];
    for (b, &start) in blocks.iter().zip(block_start_lines.iter()) {
        for (offset, &row_idx) in b.iter().enumerate() {
            line_numbers[row_idx] = Some(start + offset);
        }
    }

    rows.iter()
        .enumerate()
        .map(|(idx, row)| {
            if is_marker_row(row) {
                diff_row_sized(None, row, cols, gutter_w)
            } else {
                let peer = peer_map[idx].map(|p| rows[p].as_str());
                diff_row_sized_with_peer(line_numbers[idx], row, peer, cols, gutter_w)
            }
        })
        .collect()
}

/// Compact footer: status mark, line count, wall time and an optional note.
pub fn footer_line(ok: bool, preview: &str, lines: usize, millis: u128) -> String {
    let mark = if ok {
        format!("{}✓{}", GREEN, YELLOW)
    } else {
        format!("{}✗{}", RED, YELLOW)
    };
    let lines = lines.max(1);
    let count = if lines == 1 {
        "1 line".to_string()
    } else {
        format!("{} lines", lines)
    };
    let body = if preview.is_empty() {
        count
    } else {
        format!("{} · {}", preview, count)
    };
    format!(
        "{}╰─ {} {}{} · {}ms{}",
        YELLOW, mark, DIM, body, millis, RESET
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::visible_width;

    fn rows(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sanitize_drops_escapes_cr_and_control_bytes() {
        assert_eq!(sanitize("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(sanitize("a\r\nb"), "a\nb");
        assert_eq!(sanitize("a\tb"), "a    b");
        assert_eq!(sanitize("a\x00\x07b"), "ab");
    }

    #[test]
    fn sanitize_handles_osc_titles_and_hyperlinks() {
        assert_eq!(sanitize("\x1b]0;title\x07visible"), "visible");
        assert_eq!(sanitize("\x1b]8;;http://x\x1b\\link"), "link");
    }

    #[test]
    fn elide_keeps_everything_that_fits() {
        let r = rows(&["a", "b", "c"]);
        assert_eq!(elide(&r, 8), r);
    }

    #[test]
    fn elide_reports_the_exact_hidden_count_between_head_and_tail() {
        let r: Vec<String> = (0..20).map(|i| format!("l{}", i)).collect();
        let out = elide(&r, 4);
        assert_eq!(out.len(), 5, "4 survivors + 1 marker");
        assert_eq!(out[0], "l0");
        assert_eq!(out[1], "l1");
        assert!(
            out[2] == "… (16 satır gizlendi)" || out[2] == "… (16 lines hidden)",
            "unexpected marker: {}",
            out[2]
        );
        assert_eq!(out[3], "l18");
        assert_eq!(out[4], "l19");
    }

    #[test]
    fn elide_never_exceeds_budget_with_an_odd_row_count() {
        let r: Vec<String> = (0..9).map(|i| format!("l{}", i)).collect();
        let out = elide(&r, 3);
        assert_eq!(out.len(), 4);
        assert!(
            out.iter()
                .any(|r| r == "… (6 satır gizlendi)" || r == "… (6 lines hidden)"),
            "{out:?}"
        );
    }

    #[test]
    fn body_rows_strips_blank_edges_and_never_returns_a_trailing_blank() {
        let out = body_rows("\n\nfirst\nsecond\n\n\n", 8);
        assert_eq!(out, rows(&["first", "second"]));
    }

    #[test]
    fn body_rows_filters_binary_replacement_noise_from_tail() {
        let input = "=== model-hub.ts ===\nif (a > 0) {\n  o.push(n);\n}\n\u{fffd}\u{fffd}\u{fffd} files in \u{fffd}\"ҵ sessions\n\u{fffd}\u{fffd}M\u{fffd}referenced\u{fffd}\u{fffd}\u{fffd}Xarchive\n";
        let out = body_rows(input, 8);
        assert_eq!(
            out,
            rows(&["=== model-hub.ts ===", "if (a > 0) {", "  o.push(n);", "}"])
        );
    }

    #[test]
    fn change_rows_pairs_batch_hunks_with_minus_and_plus() {
        let args = r#"{"path":"a.rs","edits":[{"old_text":"let x = 1;","new_text":"let x = 2;"}]}"#;
        let out = change_rows("edit", args, 10);
        assert_eq!(out[0], "- let x = 1;");
        assert!(out.contains(&"+ let x = 2;".to_string()));
        assert!(out.contains(&"  ⋯".to_string()));
    }

    #[test]
    fn change_rows_treats_write_as_pure_additions() {
        let args = r#"{"path":"new.rs","content":"fn main() {}\n"}"#;
        let out = change_rows("write", args, 10);
        assert_eq!(out, rows(&["+ fn main() {}"]));
    }

    #[test]
    fn change_rows_is_empty_on_unparsable_arguments() {
        assert!(change_rows("edit", "{not json", 10).is_empty());
        assert!(change_rows("edit", r#"{"path":"a.rs"}"#, 10).is_empty());
    }

    #[test]
    fn rail_row_truncates_to_the_rail_width_and_stays_color_safe() {
        let long = "x".repeat(400);
        let line = rail_row(&long, 40);
        assert!(visible_width(&line) <= 40, "{}", visible_width(&line));
        assert!(line.starts_with(&format!("{}│", DIM)));
    }

    #[test]
    fn rail_row_paints_minus_red_and_plus_green() {
        assert!(rail_row("- gone", 60).contains(RED));
        assert!(rail_row("+ added", 60).contains(GREEN));
    }

    #[test]
    fn footer_line_reports_error_mark_and_plural_lines() {
        let ok = footer_line(true, "first row", 40, 12);
        assert!(ok.contains('✓') && ok.contains("40 lines") && ok.contains("12ms"));
        let bad = footer_line(false, "", 1, 3);
        assert!(bad.contains('✗') && bad.contains("1 line"));
        assert!(!footer_line(false, "", 1, 3).contains("1 lines"));
    }

    #[test]
    fn diff_stat_counts_both_hunk_shapes() {
        let single = r#"{"path":"a.rs","old_text":"one\ntwo","new_text":"one\ntwo\nthree"}"#;
        assert_eq!(diff_stat("edit", single), Some((3, 2)));

        let batched = r#"{"path":"a.rs","edits":[{"old_text":"a","new_text":"b\nc"},{"old_text":"d\ne","new_text":"f"}]}"#;
        assert_eq!(diff_stat("edit", batched), Some((3, 3)));

        let write = r#"{"path":"n.rs","content":"fn main() {}\n"}"#;
        assert_eq!(diff_stat("write", write), Some((1, 0)));
        assert_eq!(diff_stat("edit", r#"{"path":"a.rs"}"#), None);
    }

    #[test]
    fn diff_stat_and_change_rows_agree() {
        let args = r#"{"path":"a.rs","edits":[{"old_text":"let x = 1;","new_text":"let x = 2;"}]}"#;
        let (added, removed) = diff_stat("edit", args).unwrap();
        let rows = change_rows("edit", args, 10);
        assert_eq!(rows.iter().filter(|r| r.starts_with("+ ")).count(), added);
        assert_eq!(rows.iter().filter(|r| r.starts_with("- ")).count(), removed);
    }

    #[test]
    fn numbered_diff_rows_put_a_gutter_on_changes_and_skip_markers() {
        let rows = rows(&["- old", "+ new", "  ⋯"]);
        let out = numbered_diff_rows(&rows, 80);
        assert!(
            out[0].contains("│ 1 ") && out[0].contains(BG_REMOVED),
            "{:?}",
            out[0]
        );
        assert!(
            out[1].contains("│ 2 ") && out[1].contains(BG_ADDED),
            "{:?}",
            out[1]
        );
        assert!(!out[2].contains("│ 3 "), "markers take no number");
        assert!(out[2].contains('⋯'));
    }

    #[test]
    fn numbered_diff_rows_restart_each_block_and_size_the_gutter() {
        let mut r = rows(&["- a", "- b", "  ⋯"]);
        r.extend(rows(&["+ a", "+ b"]));
        let out = numbered_diff_rows(&r, 80);
        assert!(out[0].contains("│ 1 "), "{:?}", out[0]);
        assert!(out[1].contains("│ 2 "), "{:?}", out[1]);
        assert!(
            out[3].contains("│ 1 "),
            "the green side restarts at 1: {:?}",
            out[3]
        );
        assert!(out[4].contains("│ 2 "), "{:?}", out[4]);

        let many: Vec<String> = (0..12).map(|i| format!("- l{i}")).collect();
        let wide = numbered_diff_rows(&many, 80);
        assert!(wide[11].contains("│12 "), "{:?}", wide[11]);
    }

    #[test]
    fn change_line_count_ignores_elision_markers() {
        // The footer reports the rows a reader can actually see, so a `⋯` gap between two
        // hunks must not be counted as a changed line.
        let body = rows(&["- old", "- old2", "⋯", "+ new"]);
        let shown = body.iter().filter(|r| !is_marker_row(r)).count();
        assert_eq!(shown, 3);
        assert!(is_marker_row("- … (12 more)"), "signed elision marker");
        assert!(!is_marker_row("+ new"));
    }

    #[test]
    fn numbered_diff_rows_fit_the_card_width() {
        let long = format!("+ {}", "x".repeat(400));
        let out = numbered_diff_rows(&rows(&[long.as_str()]), 48);
        assert!(
            visible_width(&out[0]) <= 48,
            "{} cells",
            visible_width(&out[0])
        );
    }

    #[test]
    fn numbered_diff_rows_at_uses_real_file_line_numbers_and_word_level_highlights() {
        let r = rows(&["- let count = 10;", "  ⋯", "+ let count = 20;"]);
        let out = numbered_diff_rows_at(&r, 80, &[420]);
        assert!(out[0].contains("│420 "), "expected line 420: {:?}", out[0]);
        assert!(out[2].contains("│420 "), "expected line 420: {:?}", out[2]);
        assert!(
            out[0].contains(BG_REMOVED_WORD),
            "expected intra-line removed word highlight: {:?}",
            out[0]
        );
        assert!(
            out[2].contains(BG_ADDED_WORD),
            "expected intra-line added word highlight: {:?}",
            out[2]
        );
    }

    #[test]
    fn osc8_links_have_zero_display_width_and_auto_close_on_truncation() {
        let linked = crate::ansi::osc8_link("file:///tmp/demo.rs", "src/demo.rs:42");
        assert_eq!(visible_width(&linked), "src/demo.rs:42".len());
        let (trunc, w) = crate::ansi::truncate_ansi_to_width(&linked, 7);
        assert_eq!(w, 7);
        assert!(
            trunc.contains("\x1b]8;;\x07"),
            "truncated OSC 8 link must auto-close: {:?}",
            trunc
        );
    }
}
