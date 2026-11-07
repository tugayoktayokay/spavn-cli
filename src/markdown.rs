use crate::hud::{char_display_width, spinner_print, term_size};

const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

fn parse_table_row_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or(trimmed);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut chars = inner.chars().peekable();
    let push_cell = |cur: &mut String, cells: &mut Vec<String>| {
        let raw = cur.trim();
        // Code spans display raw markdown by design; plain cells collapse
        // `[label](url)` / `![alt](url)` to their visible label so column
        // widths and wrapping measure what the reader actually sees.
        let cell = if raw.contains('`') {
            raw.to_string()
        } else {
            strip_md_links(raw)
        };
        cells.push(cell.trim().to_string());
        cur.clear();
    };
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next_c) = chars.peek() {
                if next_c == '|' {
                    cur.push('|');
                    let _ = chars.next();
                    continue;
                }
            }
            cur.push(ch);
            continue;
        }
        if ch == '`' {
            in_code = !in_code;
            cur.push(ch);
            continue;
        }
        if ch == '|' && !in_code {
            push_cell(&mut cur, &mut cells);
        } else {
            cur.push(ch);
        }
    }
    push_cell(&mut cur, &mut cells);
    cells
}

/// Collapses markdown image/link syntax to the label the reader sees:
/// `![alt](url)` and `[label](url)` become `alt` / `label`. Unmatched
/// brackets (a literal `[` or an unclosed link) pass through untouched.
fn strip_md_links(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        let is_image = chars[i] == '!' && i + 1 < chars.len() && chars[i + 1] == '[';
        let open = if is_image {
            i + 1
        } else if chars[i] == '[' {
            i
        } else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        let mut close = open + 1;
        // Nested-label aware: `[![img](imgurl)](linkurl)` must close at the
        // outermost `]`, not the image one.
        let mut depth = 0usize;
        while close < chars.len() {
            match chars[close] {
                '[' => depth += 1,
                ']' if depth == 0 => break,
                ']' => depth -= 1,
                _ => {}
            }
            close += 1;
        }
        if close < chars.len() && close + 1 < chars.len() && chars[close + 1] == '(' {
            let mut end = close + 2;
            // Balanced parens: `[a](b_(c))` closes at the final `)`, so a
            // wiki-style URL never leaks a stray `)` into the cell.
            let mut paren_depth = 0usize;
            while end < chars.len() {
                match chars[end] {
                    '(' => paren_depth += 1,
                    ')' if paren_depth == 0 => break,
                    ')' => paren_depth -= 1,
                    _ => {}
                }
                end += 1;
            }
            if end < chars.len() {
                let label: String = chars[open + 1..close].iter().collect();
                // The label may itself carry `[![img](u)]` pieces — collapse them too.
                out.push_str(&strip_md_links(label.trim()));
                i = end + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_table_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|c| {
            let t = c.trim();
            !t.is_empty()
                && t.chars().any(|ch| ch == '-')
                && t.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
        })
}

fn parse_cell_styled_chars(raw: &str, is_header: bool) -> Vec<(char, &'static str)> {
    let base_style: &'static str = if is_header { "\x1b[1;36m" } else { "\x1b[0m" };
    let bold_style: &'static str = "\x1b[1;97m";
    let code_style: &'static str = if is_header { "\x1b[1;97m" } else { "\x1b[36m" };
    let italic_style: &'static str = if is_header { "\x1b[1;36m" } else { "\x1b[3m" };

    let mut out: Vec<(char, &'static str)> = Vec::with_capacity(raw.len());
    let mut in_bold = false;
    let mut in_italic = false;
    let mut in_code = false;
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if !in_code && ch == '*' {
            if i + 1 < chars.len() && chars[i + 1] == '*' {
                in_bold = !in_bold;
                i += 2;
                continue;
            } else {
                in_italic = !in_italic;
                i += 1;
                continue;
            }
        }
        let style = if in_code {
            code_style
        } else if in_bold {
            bold_style
        } else if in_italic {
            italic_style
        } else {
            base_style
        };
        out.push((ch, style));
        i += 1;
    }
    out
}

fn styled_visual_width(styled: &[(char, &'static str)]) -> usize {
    styled
        .iter()
        .map(|(ch, _)| char_display_width(*ch).max(1))
        .sum()
}

fn styled_max_word_width(styled: &[(char, &'static str)]) -> usize {
    let mut max_w = 0usize;
    let mut cur_w = 0usize;
    for &(ch, _) in styled {
        if ch.is_whitespace() {
            max_w = max_w.max(cur_w);
            cur_w = 0;
        } else {
            cur_w += char_display_width(ch).max(1);
        }
    }
    max_w.max(cur_w)
}

fn render_styled_slice_padded(slice: &[(char, &'static str)], target_width: usize) -> String {
    let mut out = String::with_capacity(target_width + 24);
    let mut cur_style = "";
    let mut used_w = 0usize;
    for &(ch, style) in slice {
        let w = char_display_width(ch).max(1);
        if used_w + w > target_width {
            break;
        }
        if style != cur_style {
            out.push_str(style);
            cur_style = style;
        }
        out.push(ch);
        used_w += w;
    }
    out.push_str(RESET);
    if used_w < target_width {
        out.push_str(&" ".repeat(target_width - used_w));
    }
    out
}

fn wrap_styled_cell(styled: &[(char, &'static str)], col_width: usize) -> Vec<String> {
    let width = col_width.max(2);
    if styled.is_empty() {
        return vec![" ".repeat(width)];
    }
    let mut lines: Vec<Vec<(char, &'static str)>> = Vec::new();
    let mut cur_line: Vec<(char, &'static str)> = Vec::new();
    let mut cur_w = 0usize;

    let mut idx = 0usize;
    while idx < styled.len() {
        if styled[idx].0.is_whitespace() {
            while idx < styled.len() && styled[idx].0.is_whitespace() {
                idx += 1;
            }
            if !cur_line.is_empty() && cur_w < width {
                let space_style = cur_line.last().map(|x| x.1).unwrap_or("\x1b[0m");
                cur_line.push((' ', space_style));
                cur_w += 1;
            }
            continue;
        }
        let word_start = idx;
        let mut word_w = 0usize;
        while idx < styled.len() && !styled[idx].0.is_whitespace() {
            word_w += char_display_width(styled[idx].0).max(1);
            idx += 1;
        }
        let word = &styled[word_start..idx];

        if !cur_line.is_empty() && cur_w + word_w > width {
            while cur_line.last().map(|(c, _)| *c == ' ').unwrap_or(false) {
                cur_line.pop();
            }
            lines.push(std::mem::take(&mut cur_line));
            cur_w = 0;
        }

        for &(ch, st) in word {
            let cw = char_display_width(ch).max(1);
            if cur_w + cw > width && !cur_line.is_empty() {
                lines.push(std::mem::take(&mut cur_line));
                cur_w = 0;
            }
            cur_line.push((ch, st));
            cur_w += cw;
        }
    }

    while cur_line.last().map(|(c, _)| *c == ' ').unwrap_or(false) {
        cur_line.pop();
    }
    if !cur_line.is_empty() || lines.is_empty() {
        lines.push(cur_line);
    }

    lines
        .into_iter()
        .map(|ln| render_styled_slice_padded(&ln, width))
        .collect()
}

pub fn render_markdown_table(raw_rows: &[String], term_cols: usize) -> String {
    let parsed_rows: Vec<Vec<String>> = raw_rows
        .iter()
        .map(|r| parse_table_row_cells(r))
        .filter(|cells| cells.iter().any(|c| !c.trim().is_empty()))
        .collect();
    if parsed_rows.is_empty() {
        return String::new();
    }

    let mut header_cells: Option<Vec<String>> = None;
    let mut body_rows: Vec<Vec<String>> = Vec::new();

    if parsed_rows.len() >= 2 && is_table_separator_row(&parsed_rows[1]) {
        header_cells = Some(parsed_rows[0].clone());
        for row in parsed_rows.into_iter().skip(2) {
            if !is_table_separator_row(&row) {
                body_rows.push(row);
            }
        }
    } else {
        for (idx, row) in parsed_rows.into_iter().enumerate() {
            if is_table_separator_row(&row) {
                continue;
            }
            if idx == 0 && header_cells.is_none() {
                header_cells = Some(row);
            } else {
                body_rows.push(row);
            }
        }
    }

    let num_cols = header_cells
        .as_ref()
        .map(|h| h.len())
        .into_iter()
        .chain(body_rows.iter().map(|r| r.len()))
        .max()
        .unwrap_or(0);
    if num_cols == 0 {
        return String::new();
    }

    let styled_header: Option<Vec<Vec<(char, &'static str)>>> = header_cells.map(|mut h| {
        h.resize(num_cols, String::new());
        h.into_iter()
            .map(|c| parse_cell_styled_chars(&c, true))
            .collect()
    });
    let styled_body: Vec<Vec<Vec<(char, &'static str)>>> = body_rows
        .into_iter()
        .map(|mut r| {
            r.resize(num_cols, String::new());
            r.into_iter()
                .map(|c| parse_cell_styled_chars(&c, false))
                .collect()
        })
        .collect();

    let mut nat_w = vec![2usize; num_cols];
    let mut min_w = vec![2usize; num_cols];
    if let Some(ref hdr) = styled_header {
        for c in 0..num_cols {
            let w = styled_visual_width(&hdr[c]).max(2);
            let mw = styled_max_word_width(&hdr[c]).clamp(2, 16);
            nat_w[c] = nat_w[c].max(w);
            min_w[c] = min_w[c].max(mw.min(w));
        }
    }
    for row in &styled_body {
        for c in 0..num_cols {
            let w = styled_visual_width(&row[c]).max(2);
            let mw = styled_max_word_width(&row[c]).clamp(2, 18);
            nat_w[c] = nat_w[c].max(w);
            min_w[c] = min_w[c].max(mw.min(w));
        }
    }

    let overhead = 3 * num_cols + 5;
    // Responsive stacked card layout when a multi-column table on a narrow terminal
    // would squeeze columns below 8 characters on average.
    if num_cols >= 3
        && !styled_body.is_empty()
        && term_cols.saturating_sub(overhead) / num_cols < 8
    {
        let card_inner_w = term_cols.saturating_sub(6).max(4);
        let draw_card_rule = |left: &str, right: &str| -> String {
            format!(
                "  {}{}{}{}{}\n",
                DIM,
                CYAN,
                left,
                "─".repeat(card_inner_w + 2),
                format!("{}{}", right, RESET)
            )
        };
        let bdr = format!("{}{}│{}", DIM, CYAN, RESET);
        let mut out = String::new();
        out.push_str(&draw_card_rule("╭", "╮"));
        for (r_idx, row) in styled_body.iter().enumerate() {
            for c in 0..num_cols {
                if row[c].is_empty() {
                    continue;
                }
                let mut combined: Vec<(char, &'static str)> = Vec::new();
                if let Some(ref hdr) = styled_header {
                    if !hdr[c].is_empty() {
                        combined.extend_from_slice(&hdr[c]);
                        combined.push((':', "\x1b[2;36m"));
                        combined.push((' ', ""));
                    }
                }
                combined.extend_from_slice(&row[c]);
                for line in wrap_styled_cell(&combined, card_inner_w) {
                    out.push_str("  ");
                    out.push_str(&bdr);
                    out.push(' ');
                    out.push_str(&line);
                    out.push(' ');
                    out.push_str(&bdr);
                    out.push('\n');
                }
            }
            if r_idx + 1 < styled_body.len() {
                out.push_str(&draw_card_rule("├", "┤"));
            }
        }
        out.push_str(&draw_card_rule("╰", "╯"));
        return out;
    }

    let avail_w = term_cols.saturating_sub(overhead).max(num_cols * 4);
    let total_nat: usize = nat_w.iter().sum();

    let mut col_w = nat_w.clone();
    if total_nat > avail_w {
        let mut locked = vec![false; num_cols];
        let mut locked_sum = 0usize;
        let mut unlocked_count = num_cols;
        let short_limit = (avail_w / num_cols).clamp(8, 22);
        for c in 0..num_cols {
            if nat_w[c] <= short_limit
                && locked_sum + nat_w[c] + unlocked_count.saturating_sub(1) * 10 <= avail_w
            {
                locked[c] = true;
                locked_sum += nat_w[c];
                unlocked_count = unlocked_count.saturating_sub(1);
            }
        }
        let rem_avail = avail_w.saturating_sub(locked_sum);
        let weight_sum: f64 = (0..num_cols)
            .filter(|&c| !locked[c])
            .map(|c| (nat_w[c] as f64).sqrt())
            .sum::<f64>()
            .max(1.0);
        let mut assigned = 0usize;
        let mut last_unlocked = None;
        for c in 0..num_cols {
            if !locked[c] {
                let share =
                    ((rem_avail as f64) * ((nat_w[c] as f64).sqrt() / weight_sum)).floor() as usize;
                let w = share
                    .max(min_w[c].min(rem_avail / unlocked_count.max(1)))
                    .max(4);
                col_w[c] = w.min(nat_w[c]);
                assigned += col_w[c];
                last_unlocked = Some(c);
            }
        }
        if let Some(last_c) = last_unlocked {
            if assigned < rem_avail {
                let extra = rem_avail - assigned;
                col_w[last_c] = (col_w[last_c] + extra).min(nat_w[last_c]);
            }
        }
    }

    // Hard responsive budget: narrow terminals must never see a torn table.
    // `avail_w` and the per-column floors above can still overflow the real
    // width when the column count runs high, so squeeze proportionally here.
    let budget = term_cols.saturating_sub(overhead).max(2);
    let mut excess = col_w.iter().sum::<usize>().saturating_sub(budget);
    while excess > 0 {
        let max_w = *col_w.iter().max().unwrap_or(&2);
        let mut touched = false;
        for w in col_w.iter_mut() {
            if excess == 0 {
                break;
            }
            if *w == max_w && *w > 2 {
                *w -= 1;
                excess -= 1;
                touched = true;
            }
        }
        if !touched {
            break;
        }
    }

    let draw_rule = |left: &str, mid: &str, right: &str| -> String {
        let segs: Vec<String> = col_w.iter().map(|&w| "─".repeat(w + 2)).collect();
        format!(
            "  {}{}{}{}{}{}\n",
            DIM,
            CYAN,
            left,
            segs.join(mid),
            right,
            RESET
        )
    };

    let mut out = String::new();
    out.push_str(&draw_rule("╭", "┬", "╮"));

    let bdr = format!("{}{}│{}", DIM, CYAN, RESET);
    if let Some(ref hdr) = styled_header {
        let wrapped_cols: Vec<Vec<String>> = (0..num_cols)
            .map(|c| wrap_styled_cell(&hdr[c], col_w[c]))
            .collect();
        let h_height = wrapped_cols.iter().map(|v| v.len()).max().unwrap_or(1);
        for line_idx in 0..h_height {
            out.push_str("  ");
            out.push_str(&bdr);
            for (c, col_lines) in wrapped_cols.iter().enumerate() {
                let cell_str = col_lines
                    .get(line_idx)
                    .cloned()
                    .unwrap_or_else(|| " ".repeat(col_w[c]));
                out.push(' ');
                out.push_str(&cell_str);
                out.push(' ');
                out.push_str(&bdr);
            }
            out.push('\n');
        }
        if !styled_body.is_empty() {
            out.push_str(&draw_rule("├", "┼", "┤"));
        }
    }

    let wrapped_body: Vec<Vec<Vec<String>>> = styled_body
        .iter()
        .map(|row| {
            (0..num_cols)
                .map(|c| wrap_styled_cell(&row[c], col_w[c]))
                .collect()
        })
        .collect();
    let any_multiline = wrapped_body
        .iter()
        .any(|row| row.iter().any(|cell_lines| cell_lines.len() > 1));

    for (r_idx, wrapped_cols) in wrapped_body.iter().enumerate() {
        let r_height = wrapped_cols.iter().map(|v| v.len()).max().unwrap_or(1);
        for line_idx in 0..r_height {
            out.push_str("  ");
            out.push_str(&bdr);
            for (c, col_lines) in wrapped_cols.iter().enumerate() {
                let cell_str = col_lines
                    .get(line_idx)
                    .cloned()
                    .unwrap_or_else(|| " ".repeat(col_w[c]));
                out.push(' ');
                out.push_str(&cell_str);
                out.push(' ');
                out.push_str(&bdr);
            }
            out.push('\n');
        }
        if any_multiline && r_idx + 1 < wrapped_body.len() {
            out.push_str(&draw_rule("├", "┼", "┤"));
        }
    }

    out.push_str(&draw_rule("╰", "┴", "╯"));
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CodeLang {
    Generic,
    Json,
    Yaml,
    Toml,
    Sql,
    Diff,
    Markup,
}

impl CodeLang {
    fn from_fence(tag: &str) -> Self {
        let lower = tag
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match lower.as_str() {
            "json" | "jsonc" | "jsonl" => Self::Json,
            "yaml" | "yml" => Self::Yaml,
            "toml" | "ini" => Self::Toml,
            "sql" | "postgres" | "postgresql" | "sqlite" | "mysql" => Self::Sql,
            "diff" | "patch" => Self::Diff,
            "html" | "xml" | "svg" => Self::Markup,
            _ => Self::Generic,
        }
    }
}

fn highlight_code_line_lang(line: &str, lang: CodeLang) -> String {
    let trimmed = line.trim_start();
    match lang {
        CodeLang::Diff => {
            if trimmed.starts_with("+++") || trimmed.starts_with("---") || trimmed.starts_with("@@")
            {
                return format!("\x1b[36m{}{}", line, RESET);
            } else if line.starts_with('+') {
                return format!("\x1b[32m{}{}", line, RESET);
            } else if line.starts_with('-') {
                return format!("\x1b[31m{}{}", line, RESET);
            }
        }
        CodeLang::Yaml | CodeLang::Toml => {
            if trimmed.starts_with('#') {
                return format!("{}{}{}", DIM, line, RESET);
            }
            if lang == CodeLang::Toml && trimmed.starts_with('[') && trimmed.ends_with(']') {
                return format!("\x1b[1;36m{}{}", line, RESET);
            }
            if let Some(sep_idx) = line.find(if lang == CodeLang::Yaml { ':' } else { '=' }) {
                let key_part = &line[..sep_idx];
                if !key_part.contains('"') && !key_part.contains('\'') {
                    let sep = &line[sep_idx..sep_idx + 1];
                    let val_part = &line[sep_idx + 1..];
                    return format!(
                        "\x1b[36m{}{}{}{}{}",
                        key_part,
                        DIM,
                        sep,
                        RESET,
                        highlight_code_line(val_part)
                    );
                }
            }
        }
        CodeLang::Json => {
            if let Some(colon_idx) = line.find("\":") {
                let key_part = &line[..colon_idx + 1];
                let rest = &line[colon_idx + 1..];
                return format!("\x1b[36m{}{}{}", key_part, RESET, highlight_code_line(rest));
            }
        }
        CodeLang::Sql => {
            if trimmed.starts_with("--") {
                return format!("{}{}{}", DIM, line, RESET);
            }
        }
        CodeLang::Markup | CodeLang::Generic => {}
    }
    highlight_code_line(line)
}

fn highlight_code_line(line: &str) -> String {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || (trimmed.starts_with('#') && !trimmed.starts_with("#[")) {
        return format!("{}{}{}", DIM, line, RESET);
    }

    const KEYWORDS: &[&str] = &[
        "fn",
        "pub",
        "let",
        "mut",
        "const",
        "static",
        "struct",
        "enum",
        "impl",
        "trait",
        "for",
        "in",
        "while",
        "loop",
        "if",
        "else",
        "match",
        "return",
        "break",
        "continue",
        "use",
        "mod",
        "crate",
        "super",
        "self",
        "Self",
        "async",
        "await",
        "where",
        "type",
        "unsafe",
        "extern",
        "ref",
        "move",
        "def",
        "class",
        "import",
        "from",
        "as",
        "try",
        "except",
        "finally",
        "with",
        "yield",
        "lambda",
        "pass",
        "function",
        "var",
        "export",
        "default",
        "interface",
        "extends",
        "implements",
        "new",
        "switch",
        "case",
        "throw",
        "catch",
        "package",
        "func",
        "defer",
        "go",
        "select",
        "chan",
    ];
    const LITERALS_AND_TYPES: &[&str] = &[
        "true",
        "false",
        "None",
        "Some",
        "Ok",
        "Err",
        "null",
        "undefined",
        "True",
        "False",
        "String",
        "str",
        "usize",
        "isize",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "f32",
        "f64",
        "bool",
        "char",
        "Vec",
        "Option",
        "Result",
        "Box",
        "Arc",
        "Mutex",
        "Path",
        "PathBuf",
        "Value",
    ];

    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut out = String::with_capacity(line.len() + 32);

    while i < n {
        let ch = chars[i];
        if ch == '/' && i + 1 < n && chars[i + 1] == '/' {
            let comment: String = chars[i..].iter().collect();
            out.push_str(DIM);
            out.push_str(&comment);
            out.push_str(RESET);
            break;
        }
        if ch == '"' || (ch == '\'' && (i + 2 < n && chars[i + 2] == '\'')) {
            let quote = ch;
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let lit: String = chars[start..i].iter().collect();
            out.push_str("\x1b[32m");
            out.push_str(&lit);
            out.push_str(RESET);
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if i < n && chars[i] == '!' {
                i += 1;
                out.push_str("\x1b[36m");
                out.push_str(&word);
                out.push('!');
                out.push_str(RESET);
            } else if KEYWORDS.contains(&word.as_str()) {
                out.push_str("\x1b[1;35m");
                out.push_str(&word);
                out.push_str(RESET);
            } else if LITERALS_AND_TYPES.contains(&word.as_str())
                || word
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_uppercase())
                    .unwrap_or(false)
            {
                out.push_str("\x1b[33m");
                out.push_str(&word);
                out.push_str(RESET);
            } else if i < n && chars[i] == '(' {
                out.push_str("\x1b[36m");
                out.push_str(&word);
                out.push_str(RESET);
            } else {
                out.push_str(&word);
            }
            continue;
        }
        if ch.is_ascii_digit() {
            let start = i;
            while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            let num: String = chars[start..i].iter().collect();
            out.push_str("\x1b[33m");
            out.push_str(&num);
            out.push_str(RESET);
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

pub struct MarkdownStreamRenderer {
    at_line_start: bool,
    prefix_buf: String,
    in_code_block: bool,
    code_lang: CodeLang,
    in_heading: bool,
    in_quote: bool,
    in_bold: bool,
    in_italic: bool,
    in_inline_code: bool,
    pending_underscore: bool,
    last_word: bool,
    star_count: usize,
    table_rows: Vec<String>,
    out_buf: String,
}

impl Default for MarkdownStreamRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownStreamRenderer {
    pub fn new() -> Self {
        Self {
            at_line_start: true,
            prefix_buf: String::new(),
            in_code_block: false,
            code_lang: CodeLang::Generic,
            in_heading: false,
            in_quote: false,
            in_bold: false,
            in_italic: false,
            in_inline_code: false,
            pending_underscore: false,
            last_word: false,
            star_count: 0,
            table_rows: Vec::new(),
            out_buf: String::new(),
        }
    }

    fn flush_table(&mut self) {
        if self.table_rows.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.table_rows);
        let cols = term_size().1;
        let rendered = render_markdown_table(&rows, cols);
        self.out_buf.push_str(&rendered);
    }

    pub fn push(&mut self, chunk: &str) {
        crate::hud::record_raw_output_fragment(chunk);
        self.push_replay(chunk);
    }

    pub fn push_replay(&mut self, chunk: &str) {
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
            if self.in_code_block {
                let line = pending.trim_end_matches(['\r', '\n']);
                if line.trim().starts_with("```") {
                    self.in_code_block = false;
                    self.code_lang = CodeLang::Generic;
                    let rule_w = term_size().1.saturating_sub(8).clamp(16, 60);
                    self.out_buf
                        .push_str(&format!("  {}╰{}{}\n", DIM, "─".repeat(rule_w), RESET));
                } else {
                    self.out_buf.push_str(&format!(
                        "  {}│{} {}\n",
                        DIM,
                        RESET,
                        highlight_code_line_lang(line, self.code_lang)
                    ));
                }
            } else if pending.trim_start().starts_with('|') {
                self.table_rows.push(pending.trim().to_string());
            } else {
                self.flush_table();
                self.at_line_start = false;
                self.out_buf.push_str("  ");
                for ch in pending.chars() {
                    self.emit_inline_char(ch);
                }
            }
        }
        if self.in_code_block {
            self.in_code_block = false;
            self.code_lang = CodeLang::Generic;
            let rule_w = term_size().1.saturating_sub(8).clamp(16, 60);
            self.out_buf
                .push_str(&format!("\r  {}╰{}{}\n", DIM, "─".repeat(rule_w), RESET));
        }
        self.flush_table();
        if self.pending_underscore {
            self.pending_underscore = false;
            self.out_buf.push('_');
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
        self.last_word = false;
        if self.pending_underscore {
            self.pending_underscore = false;
            self.out_buf.push('_');
        }
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
            self.last_word = false;
            return;
        }

        if self.star_count == 1 && ch == ' ' {
            self.star_count = 0;
            self.out_buf.push_str("* ");
            self.last_word = false;
            return;
        }
        self.flush_stars(false);

        if ch == '_' && !self.in_inline_code {
            if self.pending_underscore {
                // "__" in a row: literal underscores, intraword per CommonMark
                self.pending_underscore = false;
                self.out_buf.push_str("__");
                self.last_word = true;
            } else {
                // hold: resolve on next char (prev/next word boundary decides)
                self.pending_underscore = true;
            }
            return;
        }
        if self.pending_underscore {
            self.pending_underscore = false;
            let prev_word = self.last_word;
            let next_word = ch.is_alphanumeric() || ch == '_';
            if self.in_italic {
                if prev_word && !next_word {
                    // closing `_` (word char before, boundary after)
                    self.in_italic = false;
                    self.out_buf.push_str("\x1b[23m");
                } else {
                    self.out_buf.push('_');
                }
            } else if !prev_word && next_word {
                // opening `_` (boundary before, word char after)
                self.in_italic = true;
                self.out_buf.push_str("\x1b[3m");
            } else {
                // intraword or isolated `_`: literal
                self.out_buf.push('_');
            }
        }

        if ch == '`' {
            self.in_inline_code = !self.in_inline_code;
            self.last_word = false;
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
            self.last_word = false;
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
        self.last_word = ch.is_alphanumeric();
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
                    self.code_lang = CodeLang::Generic;
                    let rule_w = term_size().1.saturating_sub(8).clamp(16, 60);
                    self.out_buf
                        .push_str(&format!("  {}╰{}{}\n", DIM, "─".repeat(rule_w), RESET));
                } else {
                    self.out_buf.push_str(&format!(
                        "  {}│{} {}\n",
                        DIM,
                        RESET,
                        highlight_code_line_lang(line, self.code_lang)
                    ));
                }
                self.prefix_buf.clear();
                self.at_line_start = true;
            }
            return;
        }

        self.prefix_buf.push(ch);

        if ch == '\n' {
            let full_line = std::mem::take(&mut self.prefix_buf);
            let trimmed = full_line.trim();
            if trimmed.starts_with('|') {
                self.table_rows.push(trimmed.to_string());
                self.at_line_start = true;
                return;
            }
            self.flush_table();
            if trimmed.is_empty() {
                self.out_buf.push('\n');
                self.at_line_start = true;
                return;
            }
            if trimmed.starts_with("```") {
                let lang = trimmed.trim_start_matches('`').trim();
                let label = if lang.is_empty() { "kod" } else { lang };
                self.in_code_block = true;
                self.code_lang = CodeLang::from_fence(lang);
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
        if self.prefix_buf[leading_spaces..].is_empty() {
            if leading_spaces >= 8 {
                self.flush_table();
                let pending = std::mem::take(&mut self.prefix_buf);
                self.at_line_start = false;
                self.out_buf.push_str(&format!("  {}", pending));
            }
            return;
        }

        if self.prefix_buf[leading_spaces..].starts_with('|') {
            return;
        }
        self.flush_table();
        let rest = &self.prefix_buf[leading_spaces..];

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

#[cfg(test)]
mod markdown_tests {
    use super::*;
    use super::{parse_table_row_cells, strip_md_links};

    #[test]
    fn table_cells_collapse_links_and_images_to_labels() {
        // Screenshot case: a contributors table with avatar image + profile link
        // must measure the visible labels, not the raw markdown URLs.
        let row = "| ![av](https://x/y.png) | [tugayokayokay](https://github.com/tugayokayokay) | Tugay Oktay Okay |";
        let cells = parse_table_row_cells(row);
        assert_eq!(cells, vec!["av", "tugayokayokay", "Tugay Oktay Okay"]);

        // Code spans keep their raw markdown by design.
        assert_eq!(
            parse_table_row_cells("| `[a](b)` | plain |"),
            vec!["`[a](b)`", "plain"]
        );
        // Unmatched brackets pass through untouched.
        assert_eq!(
            parse_table_row_cells("| a [literal | b |"),
            vec!["a [literal", "b"]
        );
        assert_eq!(strip_md_links("[](empty) label"), " label");
        // Nested image-in-link closes at the outermost bracket and recurses into the label.
        assert_eq!(
            strip_md_links("[![img](https://a/i.png)](https://b/u)"),
            "img"
        );
        // Wiki-style URL with balanced parens leaks no stray `)`.
        assert_eq!(strip_md_links("[a](https://b/x_(y)) ab"), "a ab");
        assert_eq!(strip_md_links("no links here"), "no links here");
    }

    #[test]
    fn parse_row_cells_splits_on_plain_pipes_only() {
        let cells = parse_table_row_cells("| a | b | c |");
        assert_eq!(cells, vec!["a", "b", "c"]);

        // Escape'li pipe hücre içinde kalmalı
        let cells = parse_table_row_cells("| a \\| b | c |");
        assert_eq!(cells, vec!["a | b", "c"]);

        // Inline code içindeki pipe bölmemeli
        let cells = parse_table_row_cells("| `x|y` | z |");
        assert_eq!(cells, vec!["`x|y`", "z"]);

        // Dış pipe'lar opsiyonel
        let cells = parse_table_row_cells("a | b");
        assert_eq!(cells, vec!["a", "b"]);
    }

    #[test]
    fn separator_row_detection() {
        assert!(is_table_separator_row(&[
            "---".to_string(),
            ":--:".to_string(),
            " - ".to_string()
        ]));
        assert!(!is_table_separator_row(&[
            "---".to_string(),
            "abc".to_string()
        ]));
        assert!(!is_table_separator_row(&["".to_string()]));
        assert!(!is_table_separator_row(&[]));
    }

    #[test]
    fn render_table_empty_input_returns_empty() {
        assert_eq!(render_markdown_table(&[], 80), "");
        // Boş hücreli satırlar (örn. tek başına "|") çöp kutu çizdirmemeli
        assert_eq!(
            render_markdown_table(&["".to_string(), "|".to_string()], 80),
            ""
        );
    }

    #[test]
    fn render_table_without_separator_uses_first_row_as_header() {
        let rows = vec!["| A | B |".to_string(), "| 1 | 2 |".to_string()];
        let out = render_markdown_table(&rows, 80);
        assert!(out.contains('╭'));
        assert!(out.contains('╰'));
        assert!(out.contains('A'));
        assert!(out.contains('1'));
    }

    #[test]
    fn render_table_resizes_ragged_rows() {
        let rows = vec![
            "| A | B | C |".to_string(),
            "|---|---|---|".to_string(),
            "| only-one |".to_string(),
        ];
        let out = render_markdown_table(&rows, 100);
        assert!(out.contains("only-one"));
        for line in out.lines() {
            assert!(
                line.contains('│')
                    || line.contains('╭')
                    || line.contains('╰')
                    || line.contains('├')
                    || line.contains('┼')
                    || line.contains('┤'),
                "unexpected table line: {line}"
            );
        }
    }

    #[test]
    fn stream_renderer_buffers_table_until_newline() {
        let mut r = MarkdownStreamRenderer::new();
        // push stdout'a (spinner_print) yazar; testte yalnızca panik/veri kaybı yokluğunu doğruluyoruz.
        r.push("| A | B |\n|---|---|\n| 1 | 2 |\n");
        r.finish();
    }
}

#[cfg(test)]
mod robustness_tests {
    use super::*;

    #[test]
    fn probe_narrow_terminal_width_overflow() {
        let rows = vec![
            "| # | Sorun | Etki | Kanıt | Durum | Not |".to_string(),
            "|---|---|---|---|---|---|".to_string(),
            "| 1 | format_string_trap | derleme kırılması | session.rs:727 | fixlendi | tr_en çifti |".to_string(),
            "| 2 | envoy_long_cell_content_that_never_stops | görsel taşma | turn.rs:564 | fixlendi | wrap |".to_string(),
            "| 3 | compact_marker_chain | dedup kırılması | turn.rs:523 | korundu | tasarım |".to_string(),
        ];
        for cols in [40usize, 50, 70, 120] {
            let out = render_markdown_table(&rows, cols);
            for line in out.lines() {
                let w = crate::ansi::visible_width(line);
                assert!(
                    w <= cols,
                    "cols={cols}: line width {w} > terminal {cols}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn table_and_stream_never_panic_on_odd_input() {
        let cells = [
            "",
            "x",
            "çğüşöı İ",
            "日本語テキスト",
            "😀😀😀",
            "a\tb",
            "**b**",
            "`c`",
            "\u{200b}",
            "|",
            "\\|",
        ];
        let widths = [0usize, 1, 2, 3, 5, 20, 80, 500];
        for w in widths {
            for a in cells {
                for b in cells {
                    let rows = vec![
                        format!("| {a} | {b} |"),
                        "|---|---|".to_string(),
                        format!("| {b} | {a} | extra |"),
                        format!("|{a}"),
                    ];
                    let _ = render_markdown_table(&rows, w);
                }
            }
        }
        let mut r = MarkdownStreamRenderer::new();
        for chunk in [
            "# Başlık ğ\n",
            "  ## x",
            "|a|b|\n|-|-|\n|1",
            "|2|\n",
            "```rs\nfn é(){}\n",
            "```\n",
            "####### 7\n",
            "**uns",
            "`tick",
            "日本",
            "\n",
            "- [ ] ",
            "1. ",
            "> q\n",
        ] {
            r.push(chunk);
        }
        r.finish();
    }

    #[test]
    fn code_line_highlighter_colors_keywords_strings_and_comments() {
        let h = super::highlight_code_line("pub fn compute(x: usize) -> String { // note");
        assert!(h.contains("\x1b[1;35mpub\x1b[0m"));
        assert!(h.contains("\x1b[1;35mfn\x1b[0m"));
        assert!(h.contains("\x1b[36mcompute\x1b[0m"));
        assert!(h.contains("\x1b[33mString\x1b[0m"));
        assert!(h.contains("// note"));
    }

    #[test]
    fn code_line_highlighter_supports_json_yaml_diff_and_unclosed_fence() {
        let j = super::highlight_code_line_lang("  \"model\": \"qwen\",", super::CodeLang::Json);
        assert!(j.contains("\x1b[36m  \"model\"\x1b[0m"));

        let y = super::highlight_code_line_lang("host: localhost", super::CodeLang::Yaml);
        assert!(y.contains("\x1b[36mhost"));

        let d_add = super::highlight_code_line_lang("+added line", super::CodeLang::Diff);
        let d_del = super::highlight_code_line_lang("-removed line", super::CodeLang::Diff);
        assert!(d_add.starts_with("\x1b[32m+added line"));
        assert!(d_del.starts_with("\x1b[31m-removed line"));

        let mut r = MarkdownStreamRenderer::new();
        r.push("```json\n{\"ok\": true}");
        assert!(r.in_code_block);
        r.finish();
        assert!(!r.in_code_block);
    }
}
