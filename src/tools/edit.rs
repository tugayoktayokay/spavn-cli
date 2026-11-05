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

pub fn run_project_verification_gate(cwd: &str, modified_files: &[String]) -> Option<String> {
    if let Some(placeholder_err) = check_unfinished_placeholders(modified_files) {
        return Some(placeholder_err);
    }

    let root = expand_tilde(cwd);
    if let Some(cargo_dir) = find_project_manifest_dir(&root, "Cargo.toml") {
        let mut cmd = Command::new("cargo");
        cmd.args(["check", "--message-format=short", "-q"])
            .current_dir(&cargo_dir);
        if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 15) {
            if !timed_out && code != 0 {
                let diag = first_n_diagnostic_lines(&out, &err, 20);
                if !diag.is_empty() {
                    return Some(format!(
                        "[SPAVN ZERO-STOP COMPILER GATE]: `cargo check` failed with errors. Fix these compilation errors immediately before finishing:\n{}",
                        diag
                    ));
                }
            }
        }
    } else if let Some(ts_dir) = find_project_manifest_dir(&root, "tsconfig.json") {
        let mut cmd = Command::new("tsc");
        cmd.args(["--noEmit", "--pretty", "false"])
            .current_dir(&ts_dir);
        if let Ok((code, out, err, timed_out)) = run_command_with_timeout(cmd, 12) {
            if !timed_out && code != 0 {
                let diag = first_n_diagnostic_lines(&out, &err, 20);
                if !diag.is_empty() {
                    return Some(format!(
                        "[SPAVN ZERO-STOP COMPILER GATE]: `tsc --noEmit` failed with errors. Fix these type/syntax errors immediately before finishing:\n{}",
                        diag
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
                            return Some(format!(
                                "[SPAVN ZERO-STOP PYTHON GATE]: `py_compile` failed on `{}`:\n{}",
                                py, diag
                            ));
                        }
                    }
                }
            }
        }
    }
    None
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
        "rs" | "ts" | "tsx" | "go" | "swift" => {
            let (open_braces, close_braces) = count_code_braces(content);
            if open_braces != close_braces {
                format!(
                    "\n[AUTO-DIAGNOSTIC WARNING in {}: Brace count mismatch ('{{': {}, '}}': {}) — verify block closure]",
                    raw_path, open_braces, close_braces
                )
            } else {
                format!(" [Syntax Check: {} Braces Balanced]", ext.to_uppercase())
            }
        }
        _ => String::new(),
    }
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
/// `old_text` / `new_text`, returning a 1-line Impact Graph hint when external callers exist.
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
                return;
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
                    if syms.iter().any(|s| line.contains(s.as_str())) {
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
                if !snapshots.iter().any(|(p, _)| p == f_path) {
                    if let Ok(orig) = read_target_file(f_path) {
                        snapshots.push((f_path.to_string(), orig));
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
    let raw_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
    if let Err(blocked) = ctx.enforce_path(raw_path, true, "edit") {
        return blocked;
    }
    if let Some(edits_arr) = args.get("edits").and_then(|v| v.as_array()) {
        if !edits_arr.is_empty() {
            let original_snapshot = read_target_file(raw_path).ok();
            let mut results = Vec::new();
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
                let single_arg = json!({
                    "path": raw_path,
                    "old_text": sub_old,
                    "new_text": sub_new,
                });
                let out = exec_edit_single(ctx, &single_arg);
                if !out.starts_with("Successfully ") {
                    if idx > 0 {
                        if let Some(ref orig) = original_snapshot {
                            let _ = write_target_file(raw_path, orig);
                            for _ in 0..idx {
                                let _ = ctx.undo_stack.pop();
                            }
                        }
                    }
                    return format!(
                        "Multi-edit failed at hunk #{} (atomic rollback restored {} to pre-edit state): {}",
                        idx + 1,
                        raw_path,
                        out
                    );
                }
                results.push(out);
            }
            return results.last().cloned().unwrap_or_else(|| {
                format!(
                    "Successfully edited {} ({} hunks applied)",
                    raw_path,
                    edits_arr.len()
                )
            });
        }
    }
    exec_edit_single(ctx, args)
}

pub(crate) fn exec_edit_single(ctx: &mut ToolContext, args: &Value) -> String {
    let raw_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
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
    if old_text.is_empty() {
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

    let mtime_warn = ctx.check_external_mtime_shift(raw_path);

    let content = match read_target_file(raw_path) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let use_crlf = content.contains("\r\n") && !old_text.contains("\r\n");
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
        return format!(
            "Edit error: old_text matches {} locations in {} (lines: {}). Include 2-3 surrounding lines in old_text to disambiguate.",
            exact_matches.len(),
            raw_path,
            match_lines.join(", ")
        );
    }

    if let Some(&pos) = exact_matches.first() {
        ctx.undo_stack.push(UndoSnapshot {
            target: raw_path.to_string(),
            previous_content: Some(content.clone()),
            description: "edit".to_string(),
        });
        let mut updated = String::with_capacity(content.len() + new_text.len());
        updated.push_str(&content[..pos]);
        updated.push_str(new_text);
        updated.push_str(&content[pos + old_text.len()..]);
        if let Err(e) = write_target_file(raw_path, &updated) {
            return e;
        }
        ctx.record_file_mtime(raw_path);
        crate::spavn_engine::record_audit_action(
            "file_edit",
            &ctx.current_model,
            &ctx.default_host,
            raw_path,
            new_text.lines().count(),
            old_text.lines().count(),
            &ctx.current_prompt,
        );
        let diag = run_instant_syntax_check(raw_path, &updated);
        let impact = compute_caller_impact_hint(ctx, raw_path, old_text, new_text);
        return format!(
            "Successfully edited {} (exact match — file state is current in context, no need to re-read){}{}{}",
            raw_path, mtime_warn, diag, impact
        );
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
            return format!(
                "Edit error: old_text matches {} locations in {} (lines: {}). Include more surrounding context.",
                ws_hits.len(),
                raw_path,
                hit_lines.join(", ")
            );
        }
        if let Some(&start_idx) = ws_hits.first() {
            ctx.undo_stack.push(UndoSnapshot {
                target: raw_path.to_string(),
                previous_content: Some(content.clone()),
                description: format!("edit line {}", start_idx + 1),
            });
            let mut new_lines: Vec<String> = Vec::new();
            for l in &src_lines[..start_idx] {
                new_lines.push((*l).to_string());
            }
            let matched_slice = &src_lines[start_idx..start_idx + needle_lines.len()];
            for l in reindent_replacement_lines(matched_slice, old_text, new_text) {
                new_lines.push(l);
            }
            for l in &src_lines[start_idx + needle_lines.len()..] {
                new_lines.push((*l).to_string());
            }
            let mut joined = new_lines.join("\n");
            if content.ends_with('\n') {
                joined.push('\n');
            }
            if let Err(e) = write_target_file(raw_path, &joined) {
                return e;
            }
            ctx.record_file_mtime(raw_path);
            crate::spavn_engine::record_audit_action(
                "file_edit",
                &ctx.current_model,
                &ctx.default_host,
                raw_path,
                new_text.lines().count(),
                old_text.lines().count(),
                &ctx.current_prompt,
            );
            let diag = run_instant_syntax_check(raw_path, &joined);
            return format!(
                "Successfully edited {} (whitespace-tolerant match at line {} — file state is current in context, no need to re-read){}{}",
                raw_path,
                start_idx + 1,
                mtime_warn,
                diag
            );
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
                return format!(
                    "Edit error: old_text matches {} locations in {} (lines: {}). Include more surrounding context.",
                    fuzzy_hits.len(),
                    raw_path,
                    hit_lines.join(", ")
                );
            }
            if let Some(&(first_src_line, last_src_line)) = fuzzy_hits.first() {
                ctx.undo_stack.push(UndoSnapshot {
                    target: raw_path.to_string(),
                    previous_content: Some(content.clone()),
                    description: format!(
                        "edit lines {}..{}",
                        first_src_line + 1,
                        last_src_line + 1
                    ),
                });
                let mut new_lines: Vec<String> = Vec::new();
                for l in &src_lines[..first_src_line] {
                    new_lines.push((*l).to_string());
                }
                let matched_slice = &src_lines[first_src_line..=last_src_line];
                for l in reindent_replacement_lines(matched_slice, old_text, new_text) {
                    new_lines.push(l);
                }
                for l in &src_lines[last_src_line + 1..] {
                    new_lines.push((*l).to_string());
                }
                let mut joined = new_lines.join("\n");
                if content.ends_with('\n') {
                    joined.push('\n');
                }
                if let Err(e) = write_target_file(raw_path, &joined) {
                    return e;
                }
                ctx.record_file_mtime(raw_path);
                crate::spavn_engine::record_audit_action(
                    "file_edit",
                    &ctx.current_model,
                    &ctx.default_host,
                    raw_path,
                    new_text.lines().count(),
                    old_text.lines().count(),
                    &ctx.current_prompt,
                );
                let diag = run_instant_syntax_check(raw_path, &joined);
                return format!(
                    "Successfully edited {} (fuzzy blank-line-tolerant match at lines {}-{} — file state is current in context, no need to re-read){}{}",
                    raw_path,
                    first_src_line + 1,
                    last_src_line + 1,
                    mtime_warn,
                    diag
                );
            }
        }
    }

    if let Some(first_anchor) = non_empty_needle.first() {
        let anchor_probe = if first_anchor.len() > 16 {
            let mut end = 16;
            while end > 0 && !first_anchor.is_char_boundary(end) {
                end -= 1;
            }
            &first_anchor[..end]
        } else {
            *first_anchor
        };
        if !anchor_probe.is_empty() {
            if let Some((hit_idx, _)) = src_lines
                .iter()
                .enumerate()
                .find(|(_, l)| l.contains(anchor_probe))
            {
                let ctx_start = hit_idx.saturating_sub(5);
                let ctx_end = (hit_idx + 18).min(src_lines.len());
                let mut snippet = String::new();
                for (idx, line) in src_lines[ctx_start..ctx_end].iter().enumerate() {
                    snippet.push_str(&format!("{:>4}: {}\n", ctx_start + idx + 1, line));
                }
                return format!(
                    "Edit error: exact old_text not found in {}. However, a similar anchor was found around line {}. Here is the exact current file content at lines {}-{} (copy old_text verbatim from here and retry immediately):\n\n{}",
                    raw_path,
                    hit_idx + 1,
                    ctx_start + 1,
                    ctx_end,
                    snippet
                );
            }
        }
    }

    format!(
        "Edit error: old_text not found in {} (file has {} lines). Read the target lines first to get the exact current text.",
        raw_path,
        src_lines.len()
    )
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

    let prev = read_target_file(raw_path).ok();
    let prev_lines = prev.as_ref().map(|p| p.lines().count()).unwrap_or(0);
    ctx.undo_stack.push(UndoSnapshot {
        target: raw_path.to_string(),
        previous_content: prev,
        description: "write".to_string(),
    });

    match write_target_file(raw_path, content) {
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
            let diag = run_instant_syntax_check(raw_path, content);
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
