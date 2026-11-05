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
/// Tool output routinely carries SGR colors, `\r` progress redraws and NUL bytes from process
/// output; any of those would desync a row that is drawn after the `│ ` rail.
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
                Some(']') => {
                    // OSC (window title, hyperlink): terminated by BEL or ESC \.
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
            '\x00'..='\x1f' => {}
            _ => out.push(ch),
        }
    }
    out
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

/// Splits tool output into `│ `-ready rows, dropping leading/trailing blank lines.
pub fn body_rows(output: &str, max_rows: usize) -> Vec<String> {
    let clean = sanitize(output);
    let mut rows: Vec<String> = clean.lines().map(|l| l.trim_end().to_string()).collect();
    while rows.first().is_some_and(|r| r.is_empty()) {
        rows.remove(0);
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
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
fn hunk_pairs<'a>(tool_name: &str, val: &'a serde_json::Value) -> Vec<(&'a str, &'a str)> {
    if let Some(hunks) = val.get("edits").and_then(|v| v.as_array()) {
        return hunks
            .iter()
            .map(|h| (alias(h, OLD_KEYS), alias(h, NEW_KEYS)))
            .filter(|(o, n)| !o.is_empty() || !n.is_empty())
            .collect();
    }
    let old_text = alias(val, OLD_KEYS);
    // `write` replaces the whole file, so everything it carries is an addition; an `edit`
    // with no anchor is not a change we can show.
    let new_text = if old_text.is_empty() && tool_name == "write" {
        alias(val, CONTENT_KEYS)
    } else {
        alias(val, NEW_KEYS)
    };
    if old_text.is_empty() && new_text.is_empty() {
        Vec::new()
    } else {
        vec![(old_text, new_text)]
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
            format!("{}{}{}{}{}", BG_REMOVED, RED, row, RESET, RESET)
        } else if t.starts_with("+ ") {
            format!("{}{}{}{}{}", BG_ADDED, GREEN, row, RESET, RESET)
        } else {
            row.to_string()
        }
    };
    let (trunc, _) = crate::ansi::truncate_ansi_to_width(&styled, cols.saturating_sub(7).max(8));
    format!("{}{}", gutter, trunc)
}

/// Numbers each run of preview rows from 1, restarting after any marker.
///
/// The removed and added sides of a hunk are two different snapshots of the same lines, so a
/// single running counter would show `7 8 9` on the green side of a three-line replacement and
/// imply the file grew. A marker is a jump in the sequence, so it ends a run either way.
///
/// The number is a *row index inside the block*, not a file line number — the executor does not
/// carry original line numbers through the preview, and inventing them would be worse than
/// showing none.
pub fn numbered_diff_rows(rows: &[String], cols: usize) -> Vec<String> {
    let per_block = rows
        .split(|r| r.trim_start().starts_with('⋯'))
        .map(|b| b.iter().filter(|r| !is_marker_row(r)).count())
        .max()
        .unwrap_or(0);
    let gutter_w = per_block.max(1).to_string().len().max(2);
    let mut n = 0usize;
    rows.iter()
        .map(|row| {
            if is_marker_row(row) {
                n = 0;
                diff_row_sized(None, row, cols, gutter_w)
            } else {
                n += 1;
                diff_row_sized(Some(n), row, cols, gutter_w)
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
        assert_eq!(out[2], "… (16 satır gizlendi)");
        assert_eq!(out[3], "l18");
        assert_eq!(out[4], "l19");
    }

    #[test]
    fn elide_never_exceeds_budget_with_an_odd_row_count() {
        let r: Vec<String> = (0..9).map(|i| format!("l{}", i)).collect();
        let out = elide(&r, 3);
        assert_eq!(out.len(), 4);
        assert!(out.iter().any(|r| r == "… (6 satır gizlendi)"), "{out:?}");
    }

    #[test]
    fn body_rows_strips_blank_edges_and_never_returns_a_trailing_blank() {
        let out = body_rows("\n\nfirst\nsecond\n\n\n", 8);
        assert_eq!(out, rows(&["first", "second"]));
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
}
