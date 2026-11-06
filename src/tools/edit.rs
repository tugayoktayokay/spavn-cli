use super::*;

pub(crate) fn strip_copied_line_numbers(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }
    let mut stripped = Vec::with_capacity(lines.len());
    let mut stripped_any = false;
    for line in lines {
        let trimmed = line.trim_start();
        let digits_len = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits_len > 0 && digits_len <= 6 {
            let rest = &trimmed[digits_len..];
            if let Some(after_colon) = rest.strip_prefix(": ") {
                stripped.push(after_colon);
                stripped_any = true;
                continue;
            } else if let Some(after_colon) = rest.strip_prefix(':') {
                stripped.push(after_colon);
                stripped_any = true;
                continue;
            }
        }
        stripped.push(line);
    }
    if stripped_any {
        Some(stripped.join("\n"))
    } else {
        None
    }
}

pub(crate) fn normalize_curly_quotes(s: &str) -> String {
    s.replace(['“', '”'], "\"")
        .replace(['‘', '’'], "'")
        .replace("\r\n", "\n")
}

pub(crate) fn leading_ws_len(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ' || b == b'\t').count()
}

pub(crate) fn reindent_replacement_lines(
    src_matched_lines: &[&str],
    old_text: &str,
    new_text: &str,
) -> Vec<String> {
    let src_indent = src_matched_lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .map(|l| &l[..leading_ws_len(l)])
        .unwrap_or("");
    let old_indent_len = old_text
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(leading_ws_len)
        .unwrap_or(0);
    let new_min_indent_len = new_text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(leading_ws_len)
        .min()
        .unwrap_or(0);

    // If the model dropped or shifted base indentation relative to the file's matched block,
    // re-anchor every non-empty line in `new_text` to `src_indent` while preserving relative nesting!
    let base_strip = if old_indent_len == 0 && new_min_indent_len == 0 && !src_indent.is_empty() {
        Some(0usize)
    } else if old_indent_len != src_indent.len() && old_indent_len == new_min_indent_len {
        Some(old_indent_len)
    } else {
        None
    };

    new_text
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else if let Some(strip_n) = base_strip {
                let take_from = strip_n.min(leading_ws_len(l));
                format!("{}{}", src_indent, &l[take_from..])
            } else {
                l.to_string()
            }
        })
        .collect()
}

pub(crate) fn find_project_manifest_dir(file_path: &Path, manifest_name: &str) -> Option<PathBuf> {
    let start_dir = if file_path.is_dir() {
        file_path.to_path_buf()
    } else {
        file_path
            .parent()
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok())?
    };
    let mut cur = start_dir.as_path();
    for _ in 0..5 {
        if cur.join(manifest_name).is_file() {
            return Some(cur.to_path_buf());
        }
        match cur.parent() {
            Some(parent) if parent != cur => cur = parent,
            _ => break,
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(manifest_name).is_file() {
            return Some(cwd);
        }
    }
    None
}

pub(crate) fn first_n_diagnostic_lines(out: &[u8], err: &[u8], max_lines: usize) -> String {
    let err_str = String::from_utf8_lossy(err);
    let out_str = String::from_utf8_lossy(out);
    let combined = if err_str.trim().is_empty() {
        out_str.trim().to_string()
    } else if out_str.trim().is_empty() {
        err_str.trim().to_string()
    } else {
        format!("{}\n{}", err_str.trim(), out_str.trim())
    };
    combined
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn build_live_project_signatures(cwd: &str) -> String {
    let root = expand_tilde(cwd);
    if !root.is_dir() {
        return String::new();
    }
    let mut files: Vec<PathBuf> = Vec::new();
    fn collect_src_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 3 || out.len() >= 24 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= 24 {
                break;
            }
            let p = entry.path();
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            if name.starts_with('.')
                || matches!(
                    name.as_ref(),
                    "target" | "node_modules" | "dist" | "build" | "vendor" | "__pycache__"
                )
            {
                continue;
            }
            if p.is_dir() {
                collect_src_files(&p, depth + 1, out);
            } else if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                if matches!(ext, "rs" | "ts" | "tsx" | "py" | "go" | "js") {
                    out.push(p);
                }
            }
        }
    }
    collect_src_files(&root, 0, &mut files);
    files.sort();
    let mut summary = String::new();
    let mut total_sigs = 0usize;
    for f in files {
        if total_sigs >= 65 || summary.len() >= 2400 {
            break;
        }
        let Ok(text) = fs::read_to_string(&f) else {
            continue;
        };
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(&f)
            .to_string_lossy()
            .to_string();
        let mut file_sigs = Vec::new();
        for (idx, line) in text.lines().enumerate() {
            if is_structural_outline_line(line) {
                let clean = line.trim().trim_end_matches('{').trim();
                file_sigs.push(format!("  L{}: {}", idx + 1, clean));
                total_sigs += 1;
                if file_sigs.len() >= 12 || total_sigs >= 65 {
                    break;
                }
            }
        }
        if !file_sigs.is_empty() {
            summary.push_str(&format!("{}:\n{}\n", rel, file_sigs.join("\n")));
        }
    }
    summary
}

pub fn check_unfinished_placeholders(modified_files: &[String]) -> Option<String> {
    for raw in modified_files {
        if split_remote_prefix(raw).is_some() {
            continue;
        }
        let path = expand_tilde(raw);
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if !matches!(ext, "rs" | "ts" | "tsx" | "py" | "go" | "js") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") || trimmed.starts_with('#') {
                    continue;
                }
                if (trimmed.contains("todo!()") || trimmed.contains("unimplemented!()"))
                    && !trimmed.contains('"')
                    && !trimmed.contains('\'')
                {
                    return Some(format!(
                        "[SPAVN ZERO-STOP GATE]: File `{}` (line {}) still contains an unfinished placeholder (`{}`). Implement the complete production logic before finishing.",
                        raw,
                        idx + 1,
                        trimmed
                    ));
                }
            }
        }
    }
    None
}

pub(crate) fn find_manifest_dir_for_gate(
    cwd_root: &Path,
    modified_files: &[String],
    manifest_name: &str,
) -> Option<PathBuf> {
    for raw in modified_files {
        if split_remote_prefix(raw).is_some() {
            continue;
        }
        let expanded = expand_tilde(raw);
        let candidate = if expanded.is_absolute() {
            expanded
        } else {
            cwd_root.join(expanded)
        };
        if let Some(dir) = find_project_manifest_dir(&candidate, manifest_name) {
            return Some(dir);
        }
    }
    find_project_manifest_dir(cwd_root, manifest_name)
}

pub fn has_compiler_or_test_error_signature(text: &str) -> bool {
    let clean = strip_ansi_codes_for_diag(text);
    clean.contains("error[E")
        || clean.contains("\nerror:")
        || clean.starts_with("error:")
        || clean.contains(": error:")
        || clean.contains("): error TS")
        || clean.contains("Traceback (most recent call last)")
        || clean.contains("SyntaxError:")
        || clean.contains("test result: FAILED")
        || clean.contains("--- FAIL:")
        || clean.contains("\nFAIL\t")
        || clean.contains("thread 'main' panicked")
}

pub fn is_verification_shell_command(raw_args: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(raw_args) else {
        return false;
    };
    let cmd = get_str_alias(&v, &["command", "cmd", "script", "shell"]).to_ascii_lowercase();
    cmd.contains("cargo check")
        || cmd.contains("cargo test")
        || cmd.contains("cargo build")
        || cmd.contains("cargo clippy")
        || cmd.contains("pytest")
        || cmd.contains("py_compile")
        || cmd.contains("go test")
        || cmd.contains("go build")
        || cmd.contains("go vet")
        || cmd.contains("tsc")
        || cmd.contains("npm test")
        || cmd.contains("npm run build")
        || cmd.contains("pnpm test")
        || cmd.contains("bun test")
        || cmd.contains("jest")
        || cmd.contains("vitest")
}

pub fn run_project_verification_gate(cwd: &str, modified_files: &[String]) -> Option<String> {
    if let Some(placeholder_err) = check_unfinished_placeholders(modified_files) {
        return Some(placeholder_err);
    }
    let root = expand_tilde(cwd);
    if let Some(cargo_dir) = find_manifest_dir_for_gate(&root, modified_files, "Cargo.toml") {
        let mut cmd = Command::new("cargo");
        cmd.args(["check", "--message-format=short", "-q"])
            .current_dir(&cargo_dir);
        if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 15) {
            if !timed_out && code != 0 {
                let diag = first_n_diagnostic_lines(&out, &err, 20);
                if !diag.is_empty() {
                    let snippets = extract_compiler_error_snippets(&cargo_dir, &diag, None);
                    return Some(format!(
                        "[SPAVN ZERO-STOP COMPILER GATE]: `cargo check` failed with errors. Fix these compilation errors immediately before finishing:\n{}{}",
                        diag, snippets
                    ));
                }
            }
        }
    } else if let Some(ts_dir) = find_manifest_dir_for_gate(&root, modified_files, "tsconfig.json")
    {
        let mut cmd = Command::new("tsc");
        cmd.args(["--noEmit", "--pretty", "false"])
            .current_dir(&ts_dir);
        if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 12) {
            if !timed_out && code != 0 {
                let diag = first_n_diagnostic_lines(&out, &err, 20);
                if !diag.is_empty() {
                    let snippets = extract_compiler_error_snippets(&ts_dir, &diag, None);
                    return Some(format!(
                        "[SPAVN ZERO-STOP COMPILER GATE]: `tsc --noEmit` failed with errors. Fix these type/syntax errors immediately before finishing:\n{}{}",
                        diag, snippets
                    ));
                }
            }
        }
    } else if let Some(go_dir) = find_manifest_dir_for_gate(&root, modified_files, "go.mod") {
        let mut cmd = Command::new("go");
        cmd.args(["vet", "./..."]).current_dir(&go_dir);
        if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 12) {
            if !timed_out && code != 0 {
                let diag = first_n_diagnostic_lines(&out, &err, 20);
                if !diag.is_empty() {
                    let snippets = extract_compiler_error_snippets(&go_dir, &diag, None);
                    return Some(format!(
                        "[SPAVN ZERO-STOP COMPILER GATE]: `go vet` failed with errors. Fix these errors immediately before finishing:\n{}{}",
                        diag, snippets
                    ));
                }
            }
        }
    } else {
        let py_files: Vec<String> = modified_files
            .iter()
            .filter(|raw| {
                split_remote_prefix(raw).is_none()
                    && expand_tilde(raw)
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.eq_ignore_ascii_case("py"))
                        .unwrap_or(false)
            })
            .cloned()
            .collect();
        for py in &py_files {
            let p = expand_tilde(py);
            if p.is_file() {
                let mut cmd = Command::new("python3");
                cmd.args(["-m", "py_compile", &p.to_string_lossy()]);
                if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 8) {
                    if !timed_out && code != 0 {
                        let diag = first_n_diagnostic_lines(&out, &err, 15);
                        if !diag.is_empty() {
                            let snippets = extract_compiler_error_snippets(&root, &diag, None);
                            return Some(format!(
                                "[SPAVN ZERO-STOP PYTHON GATE]: `py_compile` failed on `{}`:\n{}{}",
                                py, diag, snippets
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Parses compiler/test error lines (`src/x.rs:248:12`, `app.ts(42,10): error`, `File "a.py", line 84`)
/// and extracts ±5 lines of source code around up to 3 distinct error locations (`<auto_error_context>`).
pub fn extract_compiler_error_snippets(
    base_dir: &Path,
    diag: &str,
    mut read_mtimes: Option<&mut std::collections::HashMap<String, std::time::SystemTime>>,
) -> String {
    let mut locations: Vec<(PathBuf, String, usize)> = Vec::new();
    let mut seen_buckets: std::collections::HashSet<(String, usize)> =
        std::collections::HashSet::new();

    for raw_line in diag.lines() {
        if locations.len() >= 3 {
            break;
        }
        for (file_str, line_no) in parse_diag_file_line_candidates(raw_line) {
            if line_no == 0 {
                continue;
            }
            let candidate = Path::new(&file_str);
            let resolved = if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                let from_base = base_dir.join(candidate);
                if from_base.is_file() {
                    from_base
                } else {
                    expand_tilde(&file_str)
                }
            };
            if !resolved.is_file() {
                continue;
            }
            let canon_key = resolved
                .canonicalize()
                .unwrap_or_else(|_| resolved.clone())
                .to_string_lossy()
                .to_string();
            let bucket = (canon_key.clone(), line_no / 6);
            if seen_buckets.insert(bucket) {
                locations.push((resolved, file_str, line_no));
                if locations.len() >= 3 {
                    break;
                }
            }
        }
    }

    if locations.is_empty() {
        return String::new();
    }

    let mut out = String::from(
        "\n<auto_error_context>\n[SPAVN Zero-Turn Error Pinpointing — source lines at failure locations (already marked as read; call `edit` directly without `read`)]:\n",
    );
    let mut injected = 0usize;
    for (resolved, display_path, err_line) in locations {
        let Ok(content) = fs::read_to_string(&resolved) else {
            continue;
        };
        if let Some(ref mut mtimes) = read_mtimes {
            if let Ok(meta) = fs::metadata(&resolved) {
                if let Ok(mtime) = meta.modified() {
                    let key = resolved
                        .canonicalize()
                        .unwrap_or_else(|_| resolved.clone())
                        .to_string_lossy()
                        .to_string();
                    mtimes.insert(key, mtime);
                }
            }
        }
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() || err_line > lines.len() {
            continue;
        }
        let start = err_line.saturating_sub(5).max(1);
        let end = (err_line + 5).min(lines.len());
        out.push_str(&format!(
            "--- {}:{}-{} (error at line {}) ---\n",
            display_path, start, end, err_line
        ));
        for idx in start..=end {
            let marker = if idx == err_line { ">" } else { " " };
            out.push_str(&format!("{}{:>4}: {}\n", marker, idx, lines[idx - 1]));
        }
        injected += 1;
    }
    if injected == 0 {
        return String::new();
    }
    out.push_str("</auto_error_context>");
    out
}

pub(crate) fn strip_ansi_codes_for_diag(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            crate::ansi::skip_escape(&mut chars);
            continue;
        }
        out.push(ch);
    }
    out
}

fn parse_diag_file_line_candidates(line: &str) -> Vec<(String, usize)> {
    let mut results = Vec::new();
    let stripped_ansi = strip_ansi_codes_for_diag(line);
    let trimmed = stripped_ansi.trim();
    // 1. Python traceback: `File "path/to/file.py", line 84`
    if let Some(f_idx) = trimmed.find("File \"") {
        let rest = &trimmed[f_idx + 6..];
        if let Some(q_end) = rest.find('"') {
            let path_part = &rest[..q_end];
            let after_q = &rest[q_end + 1..];
            if let Some(l_idx) = after_q.find("line ") {
                let num_str: String = after_q[l_idx + 5..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(n) = num_str.parse::<usize>() {
                    results.push((path_part.to_string(), n));
                }
            }
        }
    }

    // 2. Rust / Go / TS / GCC tokens: `--> src/a.rs:248:12`, `src/a.rs:248:12:`, `src/a.ts(42,10):`
    for raw_tok in trimmed.split_whitespace() {
        let tok =
            raw_tok.trim_matches(|c: char| matches!(c, '\'' | '"' | '`' | '[' | ']' | ',' | ';'));
        // Check `file.ext(line,col)`
        if let Some(p_open) = tok.find('(') {
            let file_cand = &tok[..p_open];
            if has_source_extension(file_cand) {
                let num_str: String = tok[p_open + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(n) = num_str.parse::<usize>() {
                    results.push((file_cand.to_string(), n));
                    continue;
                }
            }
        }
        // Check `file.ext:line(:col)`
        let mut parts = tok.split(':');
        if let (Some(file_cand), Some(line_cand)) = (parts.next(), parts.next()) {
            let clean_file = file_cand.trim_start_matches("-->");
            if has_source_extension(clean_file) {
                let num_str: String = line_cand
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(n) = num_str.parse::<usize>() {
                    results.push((clean_file.to_string(), n));
                }
            }
        }
    }
    results
}

fn has_source_extension(path: &str) -> bool {
    if path.is_empty() || path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "rs" | "go"
                    | "ts"
                    | "tsx"
                    | "js"
                    | "jsx"
                    | "py"
                    | "c"
                    | "cc"
                    | "cpp"
                    | "h"
                    | "hpp"
                    | "swift"
                    | "kt"
                    | "java"
                    | "rb"
                    | "zig"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn count_code_braces(src: &str) -> (usize, usize) {
    let mut open = 0usize;
    let mut close = 0usize;
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        // Line comment `// ...`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Block comment `/* ... */`
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        // Rust raw string literal `r#"..."#` or `r"..."`
        if b == b'r' && i + 1 < len && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#') {
            let mut hashes = 0usize;
            let mut j = i + 1;
            while j < len && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < len && bytes[j] == b'"' {
                j += 1;
                while j < len {
                    if bytes[j] == b'"'
                        && j + hashes < len
                        && bytes[j + 1..j + 1 + hashes].iter().all(|&x| x == b'#')
                    {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        // Double-quoted string `"..."`
        if b == b'"' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Character literal `'x'` or `'\x'`
        if b == b'\'' {
            if i + 2 < len && bytes[i + 1] != b'\\' && bytes[i + 2] == b'\'' {
                i += 3;
                continue;
            }
            if i + 3 < len && bytes[i + 1] == b'\\' && bytes[i + 3] == b'\'' {
                i += 4;
                continue;
            }
        }
        if b == b'{' {
            open += 1;
        } else if b == b'}' {
            close += 1;
        }
        i += 1;
    }
    (open, close)
}

pub(crate) fn run_instant_syntax_check(raw_path: &str, content: &str) -> String {
    if split_remote_prefix(raw_path).is_some() {
        return String::new();
    }
    let path = expand_tilde(raw_path);
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return String::new();
    };
    match ext {
        "json" => match serde_json::from_str::<Value>(content) {
            Ok(_) => " [Syntax Check: Valid JSON]".to_string(),
            Err(e) => format!(
                "\n[AUTO-DIAGNOSTIC WARNING: Invalid JSON syntax in {}: {}]",
                raw_path, e
            ),
        },
        "py" => {
            let mut cmd = Command::new("python3");
            cmd.args(["-m", "py_compile", &path.to_string_lossy()]);
            if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 8) {
                if timed_out {
                    String::new()
                } else if code == 0 {
                    " [Syntax Check: Python OK]".to_string()
                } else {
                    format!(
                        "\n[AUTO-DIAGNOSTIC ERROR in {}]:\n{}",
                        raw_path,
                        first_n_diagnostic_lines(&out, &err, 15)
                    )
                }
            } else {
                String::new()
            }
        }
        "js" | "mjs" | "cjs" => {
            let mut cmd = Command::new("node");
            cmd.args(["--check", &path.to_string_lossy()]);
            if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 8) {
                if timed_out {
                    String::new()
                } else if code == 0 {
                    " [Syntax Check: JS OK]".to_string()
                } else {
                    format!(
                        "\n[AUTO-DIAGNOSTIC ERROR in {}]:\n{}",
                        raw_path,
                        first_n_diagnostic_lines(&out, &err, 15)
                    )
                }
            } else {
                String::new()
            }
        }
        "sh" | "zsh" => {
            if let Ok(out) = Command::new(crate::config::shell_bin())
                .args(["-n", &path.to_string_lossy()])
                .output()
            {
                if out.status.success() {
                    " [Syntax Check: Shell OK]".to_string()
                } else {
                    format!(
                        "\n[AUTO-DIAGNOSTIC ERROR in {}]:\n{}",
                        raw_path,
                        first_n_diagnostic_lines(&out.stdout, &out.stderr, 15)
                    )
                }
            } else {
                String::new()
            }
        }
        "toml" => {
            for (idx, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if trimmed.starts_with('[') && !trimmed.ends_with(']') && !trimmed.contains('#') {
                    return format!(
                        "\n[AUTO-DIAGNOSTIC WARNING in {}:{}: Unclosed TOML table header `{}`]",
                        raw_path,
                        idx + 1,
                        trimmed
                    );
                }
            }
            " [Syntax Check: TOML OK]".to_string()
        }
        "rs" | "ts" | "tsx" | "go" | "swift" | "jsx" | "c" | "cpp" => {
            let (open_braces, close_braces) = count_code_braces(content);
            if open_braces != close_braces {
                let scope_hint = extract_first_unbalanced_ast_scope(content);
                format!(
                    "\n[AUTO-DIAGNOSTIC WARNING in {}: Brace count mismatch ('{{': {}, '}}': {}) — verify block closure right now without calling `read`]{}",
                    raw_path, open_braces, close_braces, scope_hint
                )
            } else {
                format!(" [Syntax Check: {} Braces Balanced]", ext.to_uppercase())
            }
        }
        _ => String::new(),
    }
}

/// True if `line` (outside line comments) references `ident` on exact identifier boundaries.
pub(crate) fn contains_word_boundary_ident(line: &str, ident: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with('#') || ident.is_empty() {
        return false;
    }
    let code_part = line.split("//").next().unwrap_or(line);
    let bytes = code_part.as_bytes();
    let pat = ident.as_bytes();
    if pat.len() > bytes.len() {
        return false;
    }
    let mut pos = 0;
    while let Some(rel) = code_part[pos..].find(ident) {
        let start = pos + rel;
        let end = start + pat.len();
        let prev_ok = start == 0 || {
            let b = bytes[start - 1];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        let next_ok = end == bytes.len() || {
            let b = bytes[end];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        if prev_ok && next_ok {
            return true;
        }
        pos = start + 1;
    }
    false
}

/// Extracts parameter count `(a, b, c)` -> `Some(3)` for a declared function `sym` in `text`.
pub(crate) fn extract_function_arity(text: &str, sym: &str) -> Option<usize> {
    for line in text.lines() {
        if !contains_word_boundary_ident(line, sym) || !is_structural_outline_line(line) {
            continue;
        }
        if let Some(pos) = line.find(sym) {
            let after = &line[pos + sym.len()..];
            let open_paren = after.find('(')?;
            let params_slice = &after[open_paren + 1..];
            let close_paren = params_slice.find(')')?;
            let inside = params_slice[..close_paren].trim();
            if inside.is_empty() {
                return Some(0);
            }
            let count = inside
                .split(',')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty() && !matches!(*p, "self" | "&self" | "&mut self" | "cls"))
                .count();
            return Some(count);
        }
    }
    None
}

/// Extracts the primary declared identifier from a structural signature line
/// (e.g. `pub fn foo(`, `const handleSend = async () =>`, `export function bar(`).
fn extract_declared_symbol_name(line: &str) -> Option<String> {
    if !is_structural_outline_line(line) {
        return None;
    }
    let trimmed = line.trim();
    for kw in [
        "fn ",
        "def ",
        "func ",
        "function ",
        "class ",
        "struct ",
        "enum ",
        "trait ",
        "interface ",
        "const ",
        "let ",
    ] {
        if let Some(pos) = trimmed.find(kw) {
            let after = trimmed[pos + kw.len()..].trim_start();
            let ident: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if ident.len() >= 4
                && !matches!(
                    ident.as_str(),
                    "main" | "test" | "new" | "default" | "from" | "into" | "drop"
                )
            {
                return Some(ident);
            }
        }
    }
    None
}

/// Scans sibling source files in the workspace for external call-sites of symbols modified in
/// `old_text` / `new_text`, returning an Impact Graph hint (and explicit arity-shift alert when
/// function parameter counts change).
fn compute_caller_impact_hint(
    ctx: &ToolContext,
    raw_path: &str,
    old_text: &str,
    new_text: &str,
) -> String {
    if split_remote_prefix(raw_path).is_some() {
        return String::new();
    }
    let mut modified_syms: Vec<String> = Vec::new();
    for line in old_text.lines().chain(new_text.lines()) {
        if let Some(sym) = extract_declared_symbol_name(line) {
            if !modified_syms.contains(&sym) {
                modified_syms.push(sym);
                if modified_syms.len() >= 3 {
                    break;
                }
            }
        }
    }
    if modified_syms.is_empty() {
        return String::new();
    }
    let mut arity_shifts: Vec<String> = Vec::new();
    for sym in &modified_syms {
        if let (Some(old_n), Some(new_n)) = (
            extract_function_arity(old_text, sym),
            extract_function_arity(new_text, sym),
        ) {
            if old_n != new_n {
                arity_shifts.push(format!("`{}` ({} -> {} params)", sym, old_n, new_n));
            }
        }
    }
    let edited_canon = expand_tilde(raw_path);
    let root = &ctx.policy.cwd;
    let mut callers: Vec<String> = Vec::new();
    fn scan_dir(
        base: &Path,
        cur: &Path,
        depth: usize,
        edited: &Path,
        syms: &[String],
        out: &mut Vec<String>,
    ) {
        if depth > 4 || out.len() >= 6 {
            return;
        }
        let Ok(rd) = fs::read_dir(cur) else {
            return;
        };
        for entry in rd.flatten() {
            if out.len() >= 6 {
                break;
            }
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if is_ignored_dir_name(&name, false) {
                continue;
            }
            if p.is_dir() {
                scan_dir(base, &p, depth + 1, edited, syms, out);
            } else if p != edited {
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !matches!(
                    ext,
                    "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "cs" | "java"
                ) {
                    continue;
                }
                let Ok(meta) = fs::metadata(&p) else {
                    continue;
                };
                if meta.len() > 256 * 1024 {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&p) else {
                    continue;
                };
                for (idx, line) in text.lines().enumerate() {
                    if syms.iter().any(|s| contains_word_boundary_ident(line, s)) {
                        let rel = p
                            .strip_prefix(base)
                            .unwrap_or(&p)
                            .to_string_lossy()
                            .to_string();
                        out.push(format!("{}:{}", rel, idx + 1));
                        break;
                    }
                }
            }
        }
    }
    scan_dir(root, root, 0, &edited_canon, &modified_syms, &mut callers);
    if callers.is_empty() {
        String::new()
    } else if !arity_shifts.is_empty() {
        format!(
            "\n[SPAVN Impact Graph — SIGNATURE ARITY SHIFT: {} — MUST update external call sites in: {}]",
            arity_shifts.join(", "),
            callers.join(", ")
        )
    } else {
        format!(
            "\n[SPAVN Impact Graph: Modified symbol(s) `{}` referenced in: {} — verify callers match the updated contract]",
            modified_syms.join(", "),
            callers.join(", ")
        )
    }
}

pub(crate) fn exec_edit(ctx: &mut ToolContext, args: &Value) -> String {
    if let Some(files_arr) = args.get("files").and_then(|v| v.as_array()) {
        if !files_arr.is_empty() {
            let mut snapshots: Vec<(String, String)> = Vec::new();
            let mut applied_outputs: Vec<String> = Vec::new();
            let mut touched_paths: Vec<String> = Vec::new();
            for (idx, item) in files_arr.iter().enumerate() {
                let f_path = get_str_alias(item, &["path", "file_path", "file", "filepath"]);
                if f_path.is_empty() {
                    for (p, orig) in snapshots.iter().rev() {
                        let _ = write_target_file(p, orig);
                    }
                    return format!(
                        "Cross-file atomic edit failed at entry #{}: missing `path` (all prior edits rolled back)",
                        idx + 1
                    );
                }
                if let Err(blocked) = ctx.enforce_path(f_path, true, "edit") {
                    for (p, orig) in snapshots.iter().rev() {
                        let _ = write_target_file(p, orig);
                    }
                    return blocked;
                }
                let resolved_f = ctx.resolve_target_arg(f_path);
                if !snapshots.iter().any(|(p, _)| p == &resolved_f) {
                    if let Ok(orig) = read_target_file(&resolved_f) {
                        snapshots.push((resolved_f.clone(), orig));
                    }
                }
                let out = exec_edit_single(ctx, item);
                if !out.starts_with("Successfully ") {
                    for (p, orig) in snapshots.iter().rev() {
                        let _ = write_target_file(p, orig);
                    }
                    for _ in 0..idx {
                        let _ = ctx.undo_stack.pop();
                    }
                    return format!(
                        "Cross-file atomic edit failed at entry #{} ({}) — atomic rollback restored all {} modified file(s) to pre-edit state: {}",
                        idx + 1,
                        f_path,
                        snapshots.len(),
                        out
                    );
                }
                if !touched_paths.iter().any(|p| p == f_path) {
                    touched_paths.push(f_path.to_string());
                }
                applied_outputs.push(format!("- {}", out));
            }
            return format!(
                "Successfully edited {} file(s) atomically ({}):\n{}",
                touched_paths.len(),
                touched_paths.join(", "),
                applied_outputs.join("\n")
            );
        }
    }
    let raw_input_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
    let (raw_path, inline_range) = split_inline_line_range(raw_input_path);
    if let Err(blocked) = ctx.enforce_path(raw_path, true, "edit") {
        return blocked;
    }
    if let Some(edits_arr) = args.get("edits").and_then(|v| v.as_array()) {
        if !edits_arr.is_empty() {
            for item in edits_arr {
                let sub_new = item
                    .get("new_text")
                    .or_else(|| item.get("new_string"))
                    .or_else(|| item.get("new_str"))
                    .or_else(|| item.get("replace"))
                    .or_else(|| item.get("replacement"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let leaked_secrets = crate::spavn_engine::detect_secrets_in_text(sub_new);
                if !leaked_secrets.is_empty() && ctx.access != AccessMode::Sovereign {
                    let secret_msg = format!(
                        "🛡️  [SPAVN Secret Guard]: {} içinde hassas anahtar ({}) tespit edildi — onaylıyor musunuz?",
                        raw_path,
                        leaked_secrets.join(", ")
                    );
                    if !prompt_user_approval(ctx, &secret_msg, sub_new) {
                        return format!(
                            "BLOCKED by SPAVN Secret Guard: {} contains potential secret(s): {}. Use environment variables instead.",
                            raw_path,
                            leaked_secrets.join(", ")
                        );
                    }
                }
            }
            if ctx.access == AccessMode::Safe
                && !prompt_user_approval(
                    ctx,
                    &format!(
                        "Çoklu Dosya Düzenleme (edit {} hunk): {}",
                        edits_arr.len(),
                        raw_path
                    ),
                    raw_path,
                )
            {
                return "User rejected edit in SAFE mode.".to_string();
            }
            let mtime_warn = ctx.check_external_mtime_shift(raw_path);
            let resolved_target = ctx.resolve_target_arg(raw_path);
            let original_content = match read_target_file(&resolved_target) {
                Ok(c) => c,
                Err(e) => return e,
            };
            let mut working = original_content.clone();
            let mut line_delta: isize = 0;
            let mut combined_old = String::new();
            let mut combined_new = String::new();
            for (idx, item) in edits_arr.iter().enumerate() {
                let sub_old = get_str_alias(
                    item,
                    &["old_text", "old_string", "old_str", "search", "target"],
                );
                let sub_new = item
                    .get("new_text")
                    .or_else(|| item.get("new_string"))
                    .or_else(|| item.get("new_str"))
                    .or_else(|| item.get("replace"))
                    .or_else(|| item.get("replacement"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let sub_range = extract_hunk_line_range(item, None);
                let sub_insert_after = extract_insert_after_line(item);
                match apply_edit_hunk_in_memory(
                    &working,
                    raw_path,
                    sub_old,
                    sub_new,
                    sub_range,
                    sub_insert_after,
                    &mut line_delta,
                ) {
                    Ok((updated, _desc, eff_old, eff_new)) => {
                        working = updated;
                        if !eff_old.is_empty() {
                            combined_old.push_str(&eff_old);
                            combined_old.push('\n');
                        }
                        if !eff_new.is_empty() {
                            combined_new.push_str(&eff_new);
                            combined_new.push('\n');
                        }
                    }
                    Err(err_msg) => {
                        return format!(
                            "Multi-edit failed at hunk #{} (atomic rollback restored {} to pre-edit state): {}",
                            idx + 1,
                            raw_path,
                            err_msg
                        );
                    }
                }
            }
            ctx.undo_stack.push(UndoSnapshot {
                target: resolved_target.clone(),
                previous_content: Some(original_content),
                description: format!("edit ({} hunks)", edits_arr.len()),
            });
            if let Err(e) = write_target_file(&resolved_target, &working) {
                let _ = ctx.undo_stack.pop();
                return e;
            }
            ctx.record_file_mtime(raw_path);
            crate::spavn_engine::record_audit_action(
                "file_edit",
                &ctx.current_model,
                &ctx.default_host,
                raw_path,
                combined_new.lines().count(),
                combined_old.lines().count(),
                &ctx.current_prompt,
            );
            let diag = run_instant_syntax_check(&resolved_target, &working);
            let impact = compute_caller_impact_hint(ctx, raw_path, &combined_old, &combined_new);
            return format!(
                "Successfully edited {} ({} hunks applied in-memory — file state is current in context, no need to re-read){}{}{}",
                raw_path,
                edits_arr.len(),
                mtime_warn,
                diag,
                impact
            );
        }
    }
    exec_edit_single_with_range(ctx, args, raw_path, inline_range)
}

pub(crate) fn split_inline_line_range(raw: &str) -> (&str, Option<(usize, usize)>) {
    if Path::new(raw).exists() || expand_tilde(raw).exists() {
        return (raw, None);
    }
    if let Some((base, suffix)) = raw.rsplit_once(':') {
        let is_bare_ssh_host = !base.contains(':') && split_remote_prefix(raw).is_some();
        if !base.is_empty() && !base.ends_with(':') && !is_bare_ssh_host {
            let s = suffix.trim().trim_start_matches(['L', 'l']);
            if let Some((a, b)) = s.split_once('-').or_else(|| s.split_once("..")) {
                let b_clean = b.trim().trim_start_matches(['=', 'L', 'l']);
                if let (Ok(start), Ok(end)) = (a.trim().parse::<usize>(), b_clean.parse::<usize>())
                {
                    if start > 0 && end >= start {
                        return (base, Some((start, end)));
                    }
                }
            } else if let Ok(single) = s.parse::<usize>() {
                if single > 0 {
                    return (base, Some((single, single)));
                }
            }
        }
    }
    (raw, None)
}

pub(crate) fn extract_hunk_line_range(
    args: &Value,
    inline_fallback: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let s = args
        .get("start_line")
        .or_else(|| args.get("start"))
        .or_else(|| args.get("line"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let e = args
        .get("end_line")
        .or_else(|| args.get("end"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    match (s, e) {
        (Some(start), Some(end)) if start >= 1 && end >= start => Some((start, end)),
        (Some(start), None) if start >= 1 => Some((start, start)),
        _ => inline_fallback,
    }
}

pub(crate) fn extract_insert_after_line(args: &Value) -> Option<usize> {
    args.get("insert_after_line")
        .or_else(|| args.get("insert_after"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
}

pub(crate) fn exec_edit_single(ctx: &mut ToolContext, args: &Value) -> String {
    let raw_input_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
    let (raw_path, inline_range) = split_inline_line_range(raw_input_path);
    exec_edit_single_with_range(ctx, args, raw_path, inline_range)
}

fn exec_edit_single_with_range(
    ctx: &mut ToolContext,
    args: &Value,
    raw_path: &str,
    inline_range: Option<(usize, usize)>,
) -> String {
    let old_text = get_str_alias(
        args,
        &["old_text", "old_string", "old_str", "search", "target"],
    );
    let new_text = args
        .get("new_text")
        .or_else(|| args.get("new_string"))
        .or_else(|| args.get("new_str"))
        .or_else(|| args.get("replace"))
        .or_else(|| args.get("replacement"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let line_range = extract_hunk_line_range(args, inline_range);
    let insert_after = extract_insert_after_line(args);
    if old_text.is_empty() && line_range.is_none() && insert_after.is_none() {
        return "Edit error: old_text cannot be empty".to_string();
    }

    let leaked_secrets = crate::spavn_engine::detect_secrets_in_text(new_text);
    if !leaked_secrets.is_empty() && ctx.access != AccessMode::Sovereign {
        let secret_msg = format!(
            "🛡️  [SPAVN Secret Guard]: {} içinde hassas anahtar ({}) tespit edildi — onaylıyor musunuz?",
            raw_path,
            leaked_secrets.join(", ")
        );
        if !prompt_user_approval(ctx, &secret_msg, new_text) {
            return format!(
                "BLOCKED by SPAVN Secret Guard: {} contains potential secret(s): {}. Use environment variables instead.",
                raw_path,
                leaked_secrets.join(", ")
            );
        }
    }

    if ctx.access == AccessMode::Safe
        && !prompt_user_approval(
            ctx,
            &format!("Dosya Düzenleme (edit): {}", raw_path),
            new_text,
        )
    {
        return "User rejected edit in SAFE mode.".to_string();
    }

    if old_text.is_empty() && ctx.is_external_stale_version(raw_path) {
        return format!(
            "Edit error [FS_STALE_VERSION]: {} was modified externally since your last read/edit. Re-read the file first before applying a line-range edit.",
            raw_path
        );
    }
    let mtime_warn = ctx.check_external_mtime_shift(raw_path);
    let resolved_target = ctx.resolve_target_arg(raw_path);

    let content = match read_target_file(&resolved_target) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let mut delta: isize = 0;
    match apply_edit_hunk_in_memory(
        &content,
        raw_path,
        old_text,
        new_text,
        line_range,
        insert_after,
        &mut delta,
    ) {
        Ok((updated, match_label, eff_old, eff_new)) => {
            ctx.undo_stack.push(UndoSnapshot {
                target: resolved_target.clone(),
                previous_content: Some(content),
                description: "edit".to_string(),
            });
            if let Err(e) = write_target_file(&resolved_target, &updated) {
                let _ = ctx.undo_stack.pop();
                return e;
            }
            ctx.record_file_mtime(raw_path);
            crate::spavn_engine::record_audit_action(
                "file_edit",
                &ctx.current_model,
                &ctx.default_host,
                raw_path,
                eff_new.lines().count(),
                eff_old.lines().count(),
                &ctx.current_prompt,
            );
            let diag = run_instant_syntax_check(&resolved_target, &updated);
            let impact = compute_caller_impact_hint(ctx, raw_path, &eff_old, &eff_new);
            format!(
                "Successfully edited {} ({}){}{}{}",
                raw_path, match_label, mtime_warn, diag, impact
            )
        }
        Err(err_msg) => err_msg,
    }
}

pub(crate) fn apply_edit_hunk_in_memory(
    content: &str,
    raw_path: &str,
    old_text: &str,
    new_text: &str,
    line_range: Option<(usize, usize)>,
    insert_after: Option<usize>,
    line_delta: &mut isize,
) -> Result<(String, String, String, String), String> {
    let use_crlf = content.contains("\r\n") && !old_text.contains("\r\n");
    let line_sep = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };

    if old_text.is_empty() {
        let clean_new = strip_copied_line_numbers(new_text).unwrap_or_else(|| new_text.to_string());
        let src_lines: Vec<&str> = content.lines().collect();
        if let Some(after_raw) = insert_after {
            let eff_after = if after_raw == 0 {
                0
            } else {
                ((after_raw as isize) + *line_delta).max(0) as usize
            };
            if eff_after > src_lines.len() {
                return Err(format!(
                    "Edit error: insert_after_line {} out of bounds for {} ({} lines)",
                    after_raw,
                    raw_path,
                    src_lines.len()
                ));
            }
            let ins_lines: Vec<&str> = clean_new.lines().collect();
            let mut new_lines: Vec<String> = Vec::with_capacity(src_lines.len() + ins_lines.len());
            for l in &src_lines[..eff_after] {
                new_lines.push((*l).to_string());
            }
            for l in &ins_lines {
                new_lines.push((*l).to_string());
            }
            for l in &src_lines[eff_after..] {
                new_lines.push((*l).to_string());
            }
            *line_delta += ins_lines.len() as isize;
            let mut joined = new_lines.join(line_sep);
            if content.ends_with('\n') || content.is_empty() {
                joined.push_str(line_sep);
            }
            return Ok((
                joined,
                format!(
                    "inserted {} line(s) after line {} — file state is current in context, no need to re-read",
                    ins_lines.len(),
                    eff_after
                ),
                String::new(),
                clean_new,
            ));
        }
        if let Some((s_raw, e_raw)) = line_range {
            let s = ((s_raw as isize) + *line_delta).max(1) as usize;
            let e = ((e_raw as isize) + *line_delta).max(s as isize) as usize;
            if s < 1 || e < s || e > src_lines.len() {
                return Err(format!(
                    "Edit error: line range {}-{} (adjusted {}-{}) out of bounds for {} ({} lines)",
                    s_raw,
                    e_raw,
                    s,
                    e,
                    raw_path,
                    src_lines.len()
                ));
            }
            let replaced_old = src_lines[s - 1..e].join("\n");
            let rep_lines: Vec<&str> = if clean_new.is_empty() {
                Vec::new()
            } else {
                clean_new.lines().collect()
            };
            let mut new_lines: Vec<String> = Vec::with_capacity(src_lines.len() + rep_lines.len());
            for l in &src_lines[..s - 1] {
                new_lines.push((*l).to_string());
            }
            for l in &rep_lines {
                new_lines.push((*l).to_string());
            }
            for l in &src_lines[e..] {
                new_lines.push((*l).to_string());
            }
            let removed_count = e - s + 1;
            *line_delta += (rep_lines.len() as isize) - (removed_count as isize);
            let mut joined = new_lines.join(line_sep);
            if content.ends_with('\n') && !joined.is_empty() {
                joined.push_str(line_sep);
            }
            return Ok((
                joined,
                format!(
                    "line-range match at lines {}-{} — file state is current in context, no need to re-read",
                    s, e
                ),
                replaced_old,
                clean_new,
            ));
        }
        return Err("Edit error: old_text cannot be empty".to_string());
    }

    let mut effective_old = if use_crlf {
        old_text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        old_text.to_string()
    };
    let mut effective_new = if use_crlf {
        new_text.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        new_text.to_string()
    };
    if !content.contains(&effective_old) {
        if let Some(stripped_old) = strip_copied_line_numbers(&effective_old) {
            if content.contains(&stripped_old) {
                effective_old = stripped_old;
                if let Some(stripped_new) = strip_copied_line_numbers(&effective_new) {
                    effective_new = stripped_new;
                }
            }
        }
    }
    if !content.contains(&effective_old) {
        let norm_old = normalize_curly_quotes(&effective_old);
        if norm_old != effective_old && content.contains(&norm_old) {
            effective_old = norm_old;
            effective_new = normalize_curly_quotes(&effective_new);
        }
    }
    let old_text = effective_old.as_str();
    let new_text = effective_new.as_str();

    let exact_matches: Vec<usize> = content
        .match_indices(old_text)
        .map(|(idx, _)| idx)
        .collect();
    if exact_matches.len() > 1 {
        let match_lines: Vec<String> = exact_matches
            .iter()
            .map(|&pos| (content[..pos].lines().count().max(1)).to_string())
            .collect();
        return Err(format!(
            "Edit error: old_text matches {} locations in {} (lines: {}). Include 2-3 surrounding lines in old_text to disambiguate.",
            exact_matches.len(),
            raw_path,
            match_lines.join(", ")
        ));
    }

    if let Some(&pos) = exact_matches.first() {
        let mut updated = String::with_capacity(content.len() + new_text.len());
        updated.push_str(&content[..pos]);
        updated.push_str(new_text);
        updated.push_str(&content[pos + old_text.len()..]);
        *line_delta += (new_text.lines().count() as isize) - (old_text.lines().count() as isize);
        return Ok((
            updated,
            "exact match — file state is current in context, no need to re-read".to_string(),
            effective_old,
            effective_new,
        ));
    }

    let src_lines: Vec<&str> = content.lines().collect();
    let needle_lines: Vec<&str> = old_text.lines().map(|l| l.trim()).collect();
    if !needle_lines.is_empty() && needle_lines.len() <= src_lines.len() {
        let mut ws_hits: Vec<usize> = Vec::new();
        for start_idx in 0..=(src_lines.len() - needle_lines.len()) {
            let matches = src_lines[start_idx..start_idx + needle_lines.len()]
                .iter()
                .zip(needle_lines.iter())
                .all(|(a, b)| a.trim() == *b);
            if matches {
                ws_hits.push(start_idx);
            }
        }
        if ws_hits.len() > 1 {
            let hit_lines: Vec<String> = ws_hits.iter().map(|&idx| (idx + 1).to_string()).collect();
            return Err(format!(
                "Edit error: old_text matches {} locations in {} (lines: {}). Include more surrounding context.",
                ws_hits.len(),
                raw_path,
                hit_lines.join(", ")
            ));
        }
        if let Some(&start_idx) = ws_hits.first() {
            let mut new_lines: Vec<String> = Vec::new();
            for l in &src_lines[..start_idx] {
                new_lines.push((*l).to_string());
            }
            let matched_slice = &src_lines[start_idx..start_idx + needle_lines.len()];
            let reindented = reindent_replacement_lines(matched_slice, old_text, new_text);
            let reindented_len = reindented.len();
            for l in reindented {
                new_lines.push(l);
            }
            for l in &src_lines[start_idx + needle_lines.len()..] {
                new_lines.push((*l).to_string());
            }
            *line_delta += (reindented_len as isize) - (needle_lines.len() as isize);
            let mut joined = new_lines.join("\n");
            if content.ends_with('\n') {
                joined.push('\n');
            }
            return Ok((
                joined,
                format!(
                    "whitespace-tolerant match at line {} — file state is current in context, no need to re-read",
                    start_idx + 1
                ),
                effective_old,
                effective_new,
            ));
        }
    }

    let non_empty_needle: Vec<&str> = old_text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if !non_empty_needle.is_empty() {
        let non_empty_src: Vec<(usize, &str)> = src_lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i, l.trim()))
            .filter(|(_, l)| !l.is_empty())
            .collect();
        if non_empty_needle.len() <= non_empty_src.len() {
            let mut fuzzy_hits: Vec<(usize, usize)> = Vec::new();
            for win_idx in 0..=(non_empty_src.len() - non_empty_needle.len()) {
                let matches = non_empty_src[win_idx..win_idx + non_empty_needle.len()]
                    .iter()
                    .zip(non_empty_needle.iter())
                    .all(|((_, a), b)| *a == *b);
                if matches {
                    let first_src_line = non_empty_src[win_idx].0;
                    let last_src_line = non_empty_src[win_idx + non_empty_needle.len() - 1].0;
                    fuzzy_hits.push((first_src_line, last_src_line));
                }
            }
            if fuzzy_hits.len() > 1 {
                let hit_lines: Vec<String> = fuzzy_hits
                    .iter()
                    .map(|(s, _)| (s + 1).to_string())
                    .collect();
                return Err(format!(
                    "Edit error: old_text matches {} locations in {} (lines: {}). Include more surrounding context.",
                    fuzzy_hits.len(),
                    raw_path,
                    hit_lines.join(", ")
                ));
            }
            if let Some(&(first_src_line, last_src_line)) = fuzzy_hits.first() {
                let mut new_lines: Vec<String> = Vec::new();
                for l in &src_lines[..first_src_line] {
                    new_lines.push((*l).to_string());
                }
                let matched_slice = &src_lines[first_src_line..=last_src_line];
                let reindented = reindent_replacement_lines(matched_slice, old_text, new_text);
                let reindented_len = reindented.len();
                for l in reindented {
                    new_lines.push(l);
                }
                for l in &src_lines[last_src_line + 1..] {
                    new_lines.push((*l).to_string());
                }
                *line_delta +=
                    (reindented_len as isize) - ((last_src_line - first_src_line + 1) as isize);
                let mut joined = new_lines.join("\n");
                if content.ends_with('\n') {
                    joined.push('\n');
                }
                return Ok((
                    joined,
                    format!(
                        "fuzzy blank-line-tolerant match at lines {}-{} — file state is current in context, no need to re-read",
                        first_src_line + 1,
                        last_src_line + 1
                    ),
                    effective_old,
                    effective_new,
                ));
            }

            // Layer A: Intra-line whitespace collapse (tolerates extra internal spaces/tabs on any line)
            let collapsed_needle: Vec<String> = non_empty_needle
                .iter()
                .map(|l| collapse_inline_whitespace(l))
                .collect();
            let mut collapsed_hits: Vec<(usize, usize)> = Vec::new();
            for win_idx in 0..=(non_empty_src.len() - non_empty_needle.len()) {
                let matches = non_empty_src[win_idx..win_idx + non_empty_needle.len()]
                    .iter()
                    .zip(collapsed_needle.iter())
                    .all(|((_, a), b)| collapse_inline_whitespace(a) == *b);
                if matches {
                    let first_src_line = non_empty_src[win_idx].0;
                    let last_src_line = non_empty_src[win_idx + non_empty_needle.len() - 1].0;
                    collapsed_hits.push((first_src_line, last_src_line));
                }
            }
            if collapsed_hits.len() == 1 {
                let (first_src_line, last_src_line) = collapsed_hits[0];
                let mut new_lines: Vec<String> = Vec::new();
                for l in &src_lines[..first_src_line] {
                    new_lines.push((*l).to_string());
                }
                let matched_slice = &src_lines[first_src_line..=last_src_line];
                let reindented = reindent_replacement_lines(matched_slice, old_text, new_text);
                let reindented_len = reindented.len();
                for l in reindented {
                    new_lines.push(l);
                }
                for l in &src_lines[last_src_line + 1..] {
                    new_lines.push((*l).to_string());
                }
                *line_delta +=
                    (reindented_len as isize) - ((last_src_line - first_src_line + 1) as isize);
                let mut joined = new_lines.join(line_sep);
                if content.ends_with('\n') {
                    joined.push_str(line_sep);
                }
                return Ok((
                    joined,
                    format!(
                        "intra-line whitespace-healed match at lines {}-{} — file state is current in context, no need to re-read",
                        first_src_line + 1,
                        last_src_line + 1
                    ),
                    effective_old,
                    effective_new,
                ));
            }

            // Layer B: Anchor-Locked Fuzzy Self-Heal (>= 88% bigram similarity with >= 0.15 margin)
            if non_empty_needle.len() >= 3 {
                let needle_joined = collapsed_needle.join("\n");
                let mut best_sim = 0.0f64;
                let mut second_best_sim = 0.0f64;
                let mut best_span: Option<(usize, usize)> = None;
                for win_idx in 0..=(non_empty_src.len() - non_empty_needle.len()) {
                    let win = &non_empty_src[win_idx..win_idx + non_empty_needle.len()];
                    let first_ok = collapse_inline_whitespace(win[0].1) == collapsed_needle[0]
                        || char_bigram_similarity(
                            &collapse_inline_whitespace(win[0].1),
                            &collapsed_needle[0],
                        ) >= 0.85;
                    let last_idx = non_empty_needle.len() - 1;
                    let last_ok = collapse_inline_whitespace(win[last_idx].1)
                        == collapsed_needle[last_idx]
                        || char_bigram_similarity(
                            &collapse_inline_whitespace(win[last_idx].1),
                            &collapsed_needle[last_idx],
                        ) >= 0.85;
                    let exact_line_matches = win
                        .iter()
                        .zip(collapsed_needle.iter())
                        .filter(|((_, a), b)| collapse_inline_whitespace(a) == **b)
                        .count();
                    let ratio_ok = exact_line_matches * 5 >= non_empty_needle.len() * 4; // >= 80%
                    if !(first_ok && last_ok) && !ratio_ok {
                        continue;
                    }
                    let cand_joined: String = win
                        .iter()
                        .map(|(_, l)| collapse_inline_whitespace(l))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let sim = char_bigram_similarity(&needle_joined, &cand_joined);
                    if sim > best_sim {
                        second_best_sim = best_sim;
                        best_sim = sim;
                        best_span = Some((win[0].0, win[last_idx].0));
                    } else if sim > second_best_sim {
                        second_best_sim = sim;
                    }
                }
                if best_sim >= 0.88 && (best_sim - second_best_sim) >= 0.15 {
                    if let Some((first_src_line, last_src_line)) = best_span {
                        let mut new_lines: Vec<String> = Vec::new();
                        for l in &src_lines[..first_src_line] {
                            new_lines.push((*l).to_string());
                        }
                        let matched_slice = &src_lines[first_src_line..=last_src_line];
                        let reindented =
                            reindent_replacement_lines(matched_slice, old_text, new_text);
                        let reindented_len = reindented.len();
                        for l in reindented {
                            new_lines.push(l);
                        }
                        for l in &src_lines[last_src_line + 1..] {
                            new_lines.push((*l).to_string());
                        }
                        *line_delta += (reindented_len as isize)
                            - ((last_src_line - first_src_line + 1) as isize);
                        let mut joined = new_lines.join(line_sep);
                        if content.ends_with('\n') {
                            joined.push_str(line_sep);
                        }
                        return Ok((
                            joined,
                            format!(
                                "fuzzy self-healed match ({:.0}% similarity) at lines {}-{} — file state is current in context, no need to re-read",
                                best_sim * 100.0,
                                first_src_line + 1,
                                last_src_line + 1
                            ),
                            effective_old,
                            effective_new,
                        ));
                    }
                }
            }
        }
    }
    if let Some((best_start, best_end, best_line)) =
        find_closest_mismatch_window(&src_lines, &non_empty_needle)
    {
        let mut snippet = String::new();
        for (idx, line) in src_lines[best_start..best_end].iter().enumerate() {
            snippet.push_str(&format!("{:>4}: {}\n", best_start + idx + 1, line));
        }
        return Err(format!(
            "Edit error: old_text did not match exactly in {}, but the closest target block is around line {} (showing lines {}-{} below — retry `edit` directly using these exact lines or `start_line: {}, end_line: {}` without calling `read`):\n<auto_mismatch_context lines=\"{}-{}\">\n{}</auto_mismatch_context>",
            raw_path,
            best_line,
            best_start + 1,
            best_end,
            best_start + 1,
            best_end,
            best_start + 1,
            best_end,
            snippet
        ));
    }

    Err(format!(
        "Edit error: old_text not found in {} (file has {} lines). Read the target lines first to get the exact current text.",
        raw_path,
        src_lines.len()
    ))
}

/// Locates the first AST scope (fn/impl/struct/class/func) where brace balance diverges,
/// or the tail scope of the file, returning a numbered `<ast_scope_context>` snippet.
pub(crate) fn extract_first_unbalanced_ast_scope(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let mut last_decl_line = 0usize;
    let mut suspect_decl_line: Option<usize> = None;
    let mut depth: isize = 0;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_ast_scope_header(trimmed) {
            if depth == 0 {
                last_decl_line = idx;
            } else if suspect_decl_line.is_none() {
                // New top-level declaration encountered while depth > 0 -> previous scope wasn't closed!
                suspect_decl_line = Some(last_decl_line);
            }
        }
        let (o, c) = count_code_braces(line);
        depth += (o as isize) - (c as isize);
        if depth < 0 && suspect_decl_line.is_none() {
            suspect_decl_line = Some(last_decl_line);
        }
    }

    let anchor = suspect_decl_line.unwrap_or(last_decl_line);
    let start = anchor.saturating_sub(2);
    let end = (anchor + 35).min(lines.len());
    let mut out = format!("\n<ast_scope_context lines=\"{}-{}\">\n", start + 1, end);
    for (idx, l) in lines[start..end].iter().enumerate() {
        out.push_str(&format!("{:>4}: {}\n", start + idx + 1, l));
    }
    out.push_str("</ast_scope_context>");
    out
}

fn is_ast_scope_header(trimmed: &str) -> bool {
    let stripped = trimmed
        .trim_start_matches("pub(crate) ")
        .trim_start_matches("pub(super) ")
        .trim_start_matches("pub ")
        .trim_start_matches("export ")
        .trim_start_matches("default ")
        .trim_start_matches("async ")
        .trim_start_matches("unsafe ")
        .trim_start_matches("const ");
    stripped.starts_with("fn ")
        || stripped.starts_with("func ")
        || stripped.starts_with("function ")
        || stripped.starts_with("def ")
        || stripped.starts_with("impl ")
        || stripped.starts_with("impl<")
        || stripped.starts_with("struct ")
        || stripped.starts_with("enum ")
        || stripped.starts_with("trait ")
        || stripped.starts_with("class ")
        || stripped.starts_with("interface ")
}

/// Multi-anchor & token-overlap window locator so even when line 1 of `old_text` was edited/wrong,
/// the closest matching source window is returned with exact line numbers.
fn find_closest_mismatch_window(
    src_lines: &[&str],
    non_empty_needle: &[&str],
) -> Option<(usize, usize, usize)> {
    if src_lines.is_empty() || non_empty_needle.is_empty() {
        return None;
    }
    // 1. Check if ANY non-empty needle line's prefix or full trimmed content appears in `src_lines`
    for needle in non_empty_needle {
        if needle.len() < 4 {
            continue;
        }
        let probe = if needle.len() > 20 {
            let mut end = 20;
            while end > 0 && !needle.is_char_boundary(end) {
                end -= 1;
            }
            &needle[..end]
        } else {
            *needle
        };
        if let Some((hit_idx, _)) = src_lines
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains(probe))
        {
            let ctx_start = hit_idx.saturating_sub(5);
            let ctx_end = (hit_idx + non_empty_needle.len() + 12).min(src_lines.len());
            return Some((ctx_start, ctx_end, hit_idx + 1));
        }
    }

    // 2. Token-overlap scoring across sliding windows of `src_lines`
    let mut needle_tokens: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for line in non_empty_needle {
        for tok in line.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if tok.len() >= 3 {
                needle_tokens.insert(tok);
            }
        }
    }
    if needle_tokens.is_empty() {
        return None;
    }

    let win_len = non_empty_needle.len().clamp(3, 20).min(src_lines.len());
    let mut best_score = 0usize;
    let mut best_idx = 0usize;
    for start in 0..=(src_lines.len() - win_len) {
        let mut score = 0usize;
        for line in &src_lines[start..start + win_len] {
            for tok in line.split(|c: char| !c.is_alphanumeric() && c != '_') {
                if needle_tokens.contains(tok) {
                    score += 1;
                }
            }
        }
        if score > best_score {
            best_score = score;
            best_idx = start;
        }
    }

    if best_score >= 2 {
        let ctx_start = best_idx.saturating_sub(4);
        let ctx_end = (best_idx + win_len + 8).min(src_lines.len());
        Some((ctx_start, ctx_end, best_idx + 1))
    } else {
        None
    }
}

pub(crate) fn exec_write(ctx: &mut ToolContext, args: &Value) -> String {
    let raw_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
    if let Err(blocked) = ctx.enforce_path(raw_path, true, "write") {
        return blocked;
    }
    let content = get_str_alias(args, &["content", "text", "body", "code"]);

    let leaked_secrets = crate::spavn_engine::detect_secrets_in_text(content);
    if !leaked_secrets.is_empty() && ctx.access != AccessMode::Sovereign {
        let secret_msg = format!(
            "🛡️  [SPAVN Secret Guard]: {} içinde hassas anahtar ({}) tespit edildi — onaylıyor musunuz?",
            raw_path,
            leaked_secrets.join(", ")
        );
        if !prompt_user_approval(ctx, &secret_msg, content) {
            return format!(
                "BLOCKED by SPAVN Secret Guard: {} contains potential secret(s): {}. Use environment variables instead.",
                raw_path,
                leaked_secrets.join(", ")
            );
        }
    }

    if ctx.access == AccessMode::Safe
        && !prompt_user_approval(ctx, &format!("Dosya Yazma (write): {}", raw_path), content)
    {
        return "User rejected write in SAFE mode.".to_string();
    }

    if ctx.is_external_stale_version(raw_path) {
        return format!(
            "Write error [FS_STALE_VERSION]: {} was modified externally since your last read/edit. Re-read the file before overwriting so external changes are not lost.",
            raw_path
        );
    }

    let resolved_target = ctx.resolve_target_arg(raw_path);
    let prev = read_target_file(&resolved_target).ok();
    let prev_lines = prev.as_ref().map(|p| p.lines().count()).unwrap_or(0);
    ctx.undo_stack.push(UndoSnapshot {
        target: resolved_target.clone(),
        previous_content: prev,
        description: "write".to_string(),
    });

    match write_target_file(&resolved_target, content) {
        Ok(()) => {
            ctx.record_file_mtime(raw_path);
            crate::spavn_engine::record_audit_action(
                "file_write",
                &ctx.current_model,
                &ctx.default_host,
                raw_path,
                content.lines().count(),
                prev_lines,
                &ctx.current_prompt,
            );
            let diag = run_instant_syntax_check(&resolved_target, content);
            format!(
                "Successfully wrote {} bytes to {} (file state is current in context, no need to re-read){}",
                content.len(),
                raw_path,
                diag
            )
        }
        Err(e) => e,
    }
}

pub(crate) fn collapse_inline_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.trim().chars() {
        if ch == ' ' || ch == '\t' {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out
}

pub(crate) fn char_bigram_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    if a_chars.len() < 2 || b_chars.len() < 2 {
        return 0.0;
    }
    let mut a_counts: std::collections::HashMap<(char, char), usize> =
        std::collections::HashMap::new();
    for w in a_chars.windows(2) {
        *a_counts.entry((w[0], w[1])).or_insert(0) += 1;
    }
    let mut b_counts: std::collections::HashMap<(char, char), usize> =
        std::collections::HashMap::new();
    for w in b_chars.windows(2) {
        *b_counts.entry((w[0], w[1])).or_insert(0) += 1;
    }
    let mut intersection = 0usize;
    for (bg, &cnt_a) in &a_counts {
        if let Some(&cnt_b) = b_counts.get(bg) {
            intersection += cnt_a.min(cnt_b);
        }
    }
    let total = (a_chars.len() - 1) + (b_chars.len() - 1);
    (2.0 * intersection as f64) / (total as f64)
}

#[cfg(test)]
mod cognitive_harness_tests {
    use super::*;

    #[test]
    fn ansi_colored_compiler_diag_extracts_file_and_line() {
        let colored = "\x1b[1m\x1b[38;5;9merror[E0308]\x1b[0m: mismatched types\n  \x1b[38;5;12m-->\x1b[0m src/turn.rs:248:12";
        assert!(has_compiler_or_test_error_signature(colored));
        let mut cands = Vec::new();
        for l in colored.lines() {
            cands.extend(parse_diag_file_line_candidates(l));
        }
        assert_eq!(cands, vec![("src/turn.rs".to_string(), 248)]);
    }

    #[test]
    fn word_boundary_and_arity_detection_work_for_impact_radar() {
        assert!(contains_word_boundary_ident(
            "    let x = compute_total(&items);",
            "compute_total"
        ));
        assert!(!contains_word_boundary_ident(
            "    let x = compute_total_v2(&items);",
            "compute_total"
        ));
        assert!(!contains_word_boundary_ident(
            "    // compute_total(&items);",
            "compute_total"
        ));
        assert_eq!(
            extract_function_arity(
                "pub fn compute_total(&self, a: i32, b: bool) -> i32 {",
                "compute_total"
            ),
            Some(2)
        );
        assert_eq!(
            extract_function_arity(
                "pub fn compute_total(&self, a: i32, b: bool, c: &str) -> i32 {",
                "compute_total"
            ),
            Some(3)
        );
    }

    #[test]
    fn fuzzy_self_heal_handles_intra_line_whitespace_and_minor_drift() {
        let src = "fn compute_total(items: &[i32]) -> i32 {\n    let mut sum = 0;\n    for item in items {\n        sum += *item;\n    }\n    sum\n}\n";
        // Layer A: extra internal whitespace (`let  mut   sum = 0;`)
        let mut delta = 0isize;
        let (updated_a, label_a, _, _) = apply_edit_hunk_in_memory(
            src,
            "src/lib.rs",
            "    let  mut   sum = 0;\n    for item in items {",
            "    let mut sum: i32 = 0;\n    for item in items {",
            None,
            None,
            &mut delta,
        )
        .expect("Layer A intra-line whitespace collapse should succeed");
        assert!(label_a.contains("intra-line whitespace-healed"));
        assert!(updated_a.contains("let mut sum: i32 = 0;"));

        // Layer B: 5-line block where 1 middle line has minor drift (`sum += item;` vs `sum += *item;`)
        let mut delta_b = 0isize;
        let (updated_b, label_b, _, _) = apply_edit_hunk_in_memory(
            src,
            "src/lib.rs",
            "    let mut sum = 0;\n    for item in items {\n        sum += item;\n    }\n    sum",
            "    items.iter().sum()",
            None,
            None,
            &mut delta_b,
        )
        .expect("Layer B anchor-locked fuzzy self-heal should succeed");
        assert!(label_b.contains("fuzzy self-healed match"));
        assert!(updated_b.contains("items.iter().sum()"));
    }
}
