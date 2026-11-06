use super::*;

/// Directory patterns that ripgrep must never descend into. Shared by `search` and `glob` so a
/// file-hunting call cannot accidentally walk a home directory into a multi-minute stall.
///
/// The leading `**/` is load-bearing: a bare `!target/**` is matched relative to the search root,
/// so it silently stops working the moment the caller passes an absolute path.
pub(crate) const RG_IGNORE_GLOBS: &[&str] = &[
    "!**/Library/**",
    "!**/Applications/**",
    "!**/Pictures/**",
    "!**/Movies/**",
    "!**/Music/**",
    "!**/*OneDrive*/**",
    "!**/.Trash/**",
    "!**/node_modules/**",
    "!**/target/**",
    "!**/.git/**",
    "!**/.omp/**",
    "!**/.cache/**",
    "!**/.cargo/**",
    "!**/.npm/**",
];

pub(crate) fn push_ignore_globs(cmd: &mut Command) {
    for pat in RG_IGNORE_GLOBS {
        cmd.args(["-g", pat]);
    }
}

/// Directory names skipped by the built-in Rust walker when `rg` is unavailable.
pub(crate) fn is_ignored_dir_name(name: &str, hidden: bool) -> bool {
    if !hidden && name.starts_with('.') {
        return true;
    }
    matches!(
        name,
        "Library"
            | "Applications"
            | "Pictures"
            | "Movies"
            | "Music"
            | ".Trash"
            | "node_modules"
            | "target"
            | ".git"
            | ".omp"
            | ".cache"
            | ".cargo"
            | ".npm"
            | "dist"
            | "build"
            | "Pods"
            | "__pycache__"
    ) || name.contains("OneDrive")
}

/// Matches a simple glob (`*`, `?`) against a single path segment or string.
pub(crate) fn wildcard_match_segment(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            pi += 1;
            star_ti = ti;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Matches a repository-relative path (forward-slash normalized) against a glob pattern
/// supporting `**/`, `/**`, `*`, and `?`.
pub(crate) fn glob_pattern_matches(pattern: &str, rel_path: &str) -> bool {
    let pat = pattern.trim_start_matches("./");
    let rel = rel_path.trim_start_matches("./");
    let file_name = rel.rsplit('/').next().unwrap_or(rel);
    if let Some(tail) = pat.strip_prefix("**/") {
        if !tail.contains('/') {
            return wildcard_match_segment(tail, file_name);
        }
        if glob_pattern_matches(tail, rel) {
            return true;
        }
        for (idx, ch) in rel.char_indices() {
            if ch == '/' && glob_pattern_matches(tail, &rel[idx + 1..]) {
                return true;
            }
        }
        return false;
    }
    if !pat.contains('/') {
        return wildcard_match_segment(pat, file_name) || wildcard_match_segment(pat, rel);
    }
    let pat_segs: Vec<&str> = pat.split('/').collect();
    let rel_segs: Vec<&str> = rel.split('/').collect();
    fn match_segs(p: &[&str], r: &[&str]) -> bool {
        if p.is_empty() {
            return r.is_empty();
        }
        if p[0] == "**" {
            if match_segs(&p[1..], r) {
                return true;
            }
            return !r.is_empty() && match_segs(p, &r[1..]);
        }
        if r.is_empty() {
            return false;
        }
        wildcard_match_segment(p[0], r[0]) && match_segs(&p[1..], &r[1..])
    }
    match_segs(&pat_segs, &rel_segs)
}

/// Native zero-dependency Rust file globber used when `rg` is not installed on the host.
pub(crate) fn native_glob_fallback(
    root: &Path,
    pattern: &str,
    limit: usize,
    hidden: bool,
) -> String {
    let mut matches: Vec<String> = Vec::new();
    let mut hit_limit = false;
    #[allow(clippy::too_many_arguments)]
    fn walk(
        base: &Path,
        cur: &Path,
        depth: usize,
        pattern: &str,
        limit: usize,
        hidden: bool,
        out: &mut Vec<String>,
        hit_limit: &mut bool,
    ) {
        if depth > 12 || *hit_limit {
            return;
        }
        let Ok(rd) = fs::read_dir(cur) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if *hit_limit {
                return;
            }
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            let p = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if is_ignored_dir_name(&name, hidden) {
                    continue;
                }
                walk(base, &p, depth + 1, pattern, limit, hidden, out, hit_limit);
            } else {
                if !hidden && name.starts_with('.') {
                    continue;
                }
                let rel = p
                    .strip_prefix(base)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                if glob_pattern_matches(pattern, &rel) {
                    if out.len() >= limit {
                        *hit_limit = true;
                        return;
                    }
                    out.push(p.to_string_lossy().to_string());
                }
            }
        }
    }
    if root.is_file() {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if glob_pattern_matches(pattern, &name) {
            matches.push(root.to_string_lossy().to_string());
        }
    } else {
        walk(
            root,
            root,
            0,
            pattern,
            limit,
            hidden,
            &mut matches,
            &mut hit_limit,
        );
    }
    if matches.is_empty() {
        return format!("(no files match `{}` in {})", pattern, root.display());
    }
    let mut out = matches.join("\n");
    if hit_limit {
        out.push_str(&format!(
            "\n[glob: limit {} reached — narrow the pattern or pass a higher `limit`]",
            limit
        ));
    }
    out
}

/// Simplifies common regex idioms (`\b`, `\s+`, `(fn|struct|...)\s+`) into literal search terms
/// when falling back to the built-in zero-dependency Rust scanner.
fn extract_native_search_terms(pattern: &str) -> Vec<String> {
    let mut p = pattern.replace("\\|", "|").replace("\\b", "");
    // Strip leading grouped keyword prefixes like `(fn|struct|...)\s+symbol` from `lsp` definition queries
    if let Some(idx) = p.find(")\\s+") {
        p = p[idx + 4..].to_string();
    } else if let Some(idx) = p.find(") ") {
        if p.starts_with('(') {
            p = p[idx + 2..].to_string();
        }
    }
    p = p.replace("\\s+", " ").replace("\\s*", "");
    p.split('|')
        .map(|s| {
            s.trim()
                .trim_start_matches('^')
                .trim_end_matches('$')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Native zero-dependency content search used when `rg` is not installed on the host.
/// Supports literal substrings, `|` / `\|` alternations, optional filename `glob`, and
/// `context` lines around matches, capping per-file matches at 10.
pub(crate) fn native_search_fallback(
    root: &Path,
    pattern: &str,
    file_glob: &str,
    context_lines: usize,
) -> String {
    let terms = extract_native_search_terms(pattern);
    if terms.is_empty() {
        return "(no matches)".to_string();
    }
    let mut results: Vec<String> = Vec::new();
    fn search_file(
        path: &Path,
        terms: &[String],
        file_glob: &str,
        ctx_n: usize,
        out: &mut Vec<String>,
    ) {
        if !file_glob.is_empty() {
            let fname = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if !glob_pattern_matches(file_glob, &fname) {
                return;
            }
        }
        let Ok(meta) = fs::metadata(path) else {
            return;
        };
        if meta.len() > 512 * 1024 {
            return;
        }
        let Ok(content) = fs::read_to_string(path) else {
            return;
        };
        let all_lines: Vec<&str> = content.lines().collect();
        let mut file_hits = 0usize;
        let disp = path.to_string_lossy();
        for (idx, &line) in all_lines.iter().enumerate() {
            if line.len() > 500 {
                continue;
            }
            if terms.iter().any(|t| line.contains(t.as_str())) {
                if ctx_n > 0 {
                    let start = idx.saturating_sub(ctx_n);
                    let end = (idx + ctx_n).min(all_lines.len().saturating_sub(1));
                    for (c_idx, &c_line) in all_lines[start..=end].iter().enumerate() {
                        let abs_idx = start + c_idx;
                        let sep = if abs_idx == idx { ':' } else { '-' };
                        let clipped: String = c_line.chars().take(220).collect();
                        out.push(format!("{}{}{}{}{}", disp, sep, abs_idx + 1, sep, clipped));
                    }
                } else {
                    let clipped: String = line.chars().take(220).collect();
                    out.push(format!("{}:{}:{}", disp, idx + 1, clipped));
                }
                file_hits += 1;
                if file_hits >= 10 || out.len() >= 80 {
                    break;
                }
            }
        }
    }
    fn walk_search(
        cur: &Path,
        depth: usize,
        terms: &[String],
        file_glob: &str,
        ctx_n: usize,
        out: &mut Vec<String>,
    ) {
        if depth > 6 || out.len() >= 80 {
            return;
        }
        let Ok(rd) = fs::read_dir(cur) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if out.len() >= 80 {
                return;
            }
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            let p = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if is_ignored_dir_name(&name, false) {
                    continue;
                }
                walk_search(&p, depth + 1, terms, file_glob, ctx_n, out);
            } else if !name.starts_with('.') {
                search_file(&p, terms, file_glob, ctx_n, out);
            }
        }
    }
    if root.is_file() {
        search_file(root, &terms, file_glob, context_lines, &mut results);
    } else {
        walk_search(root, 0, &terms, file_glob, context_lines, &mut results);
    }
    if results.is_empty() {
        bm25_ast_block_search(root, pattern, file_glob)
            .unwrap_or_else(|| "(no matches)".to_string())
    } else {
        results.join("\n")
    }
}

pub(crate) fn normalize_tr_ascii(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            'İ' | 'I' | 'ı' | 'i' => out.push('i'),
            'Ş' | 'ş' => out.push('s'),
            'Ğ' | 'ğ' => out.push('g'),
            'Ü' | 'ü' => out.push('u'),
            'Ö' | 'ö' => out.push('o'),
            'Ç' | 'ç' => out.push('c'),
            c => out.push(c.to_ascii_lowercase()),
        }
    }
    out
}

pub(crate) fn tokenize_identifier_and_query(raw: &str) -> Vec<String> {
    let mut expanded = String::with_capacity(raw.len() + 16);
    let mut prev_lower = false;
    for ch in raw.chars() {
        if ch.is_uppercase() && prev_lower {
            expanded.push(' ');
        }
        prev_lower = ch.is_lowercase();
        expanded.push(ch);
    }
    let norm = normalize_tr_ascii(&expanded);
    let mut tokens: Vec<String> = Vec::new();
    for part in norm.split(|c: char| !c.is_ascii_alphanumeric()) {
        if part.len() >= 2
            && !matches!(
                part,
                "fn" | "def"
                    | "let"
                    | "const"
                    | "var"
                    | "pub"
                    | "the"
                    | "and"
                    | "for"
                    | "ile"
                    | "icin"
                    | "bir"
            )
            && !tokens.iter().any(|t| t == part)
        {
            tokens.push(part.to_string());
        }
    }
    tokens
}

fn is_ast_block_boundary(trimmed: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "pub fn ",
        "pub(crate) fn ",
        "pub async fn ",
        "pub(crate) async fn ",
        "async fn ",
        "fn ",
        "pub struct ",
        "pub(crate) struct ",
        "struct ",
        "pub enum ",
        "enum ",
        "pub trait ",
        "trait ",
        "impl ",
        "impl<",
        "def ",
        "async def ",
        "class ",
        "export function ",
        "export async function ",
        "export default function ",
        "function ",
        "export class ",
        "export interface ",
        "interface ",
        "export type ",
        "export const ",
        "func ",
    ];
    PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

#[derive(Debug)]
struct AstCodeBlock {
    path: String,
    start_line: usize,
    end_line: usize,
    signature: String,
    norm_sig: String,
    norm_body: String,
    preview_line: Option<(usize, String)>,
}

/// Zero-latency (<5ms) Rust BM25 + AST block semantic fallback when regex/literal `search` finds 0 matches.
pub(crate) fn bm25_ast_block_search(root: &Path, pattern: &str, file_glob: &str) -> Option<String> {
    let query_tokens = tokenize_identifier_and_query(pattern);
    if query_tokens.is_empty() {
        return None;
    }
    let mut files: Vec<PathBuf> = Vec::new();
    fn collect_source_files(dir: &Path, depth: usize, file_glob: &str, out: &mut Vec<PathBuf>) {
        if depth > 6 || out.len() >= 250 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= 250 {
                break;
            }
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if name.starts_with('.') || is_ignored_dir_name(&name, false) {
                    continue;
                }
                collect_source_files(&p, depth + 1, file_glob, out);
            } else if !name.starts_with('.')
                && (file_glob.is_empty() || glob_pattern_matches(file_glob, &name))
            {
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(
                    ext.as_str(),
                    "rs" | "ts"
                        | "tsx"
                        | "js"
                        | "jsx"
                        | "py"
                        | "go"
                        | "swift"
                        | "kt"
                        | "java"
                        | "c"
                        | "cpp"
                        | "h"
                        | "rb"
                        | "php"
                        | "md"
                        | "toml"
                ) {
                    out.push(p);
                }
            }
        }
    }
    if root.is_file() {
        files.push(root.to_path_buf());
    } else {
        collect_source_files(root, 0, file_glob, &mut files);
    }
    if files.is_empty() {
        return None;
    }

    let mut blocks: Vec<AstCodeBlock> = Vec::new();
    for file_path in &files {
        let Ok(meta) = fs::metadata(file_path) else {
            continue;
        };
        if meta.len() > 512 * 1024 {
            continue;
        }
        let Ok(content) = fs::read_to_string(file_path) else {
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            continue;
        }
        let display_path = file_path.display().to_string();
        let mut boundaries: Vec<usize> = Vec::new();
        for (idx, line) in lines.iter().enumerate() {
            if is_ast_block_boundary(line.trim_start()) {
                boundaries.push(idx);
            }
        }
        if boundaries.is_empty() {
            let chunk = 35usize;
            let mut idx = 0;
            while idx < lines.len() {
                boundaries.push(idx);
                idx += chunk;
            }
        }
        for (b_idx, &start_idx) in boundaries.iter().enumerate() {
            let end_idx = boundaries
                .get(b_idx + 1)
                .map(|&next| next.saturating_sub(1))
                .unwrap_or_else(|| lines.len().saturating_sub(1))
                .min(start_idx + 80);
            let slice = &lines[start_idx..=end_idx];
            let sig = lines[start_idx].trim().to_string();
            let norm_sig = normalize_tr_ascii(&format!("{} {}", display_path, sig));
            let body_joined = slice.join("\n");
            let norm_body = normalize_tr_ascii(&body_joined);
            if !query_tokens
                .iter()
                .any(|t| norm_body.contains(t.as_str()) || norm_sig.contains(t.as_str()))
            {
                continue;
            }
            let mut preview_line = None;
            for (offset, raw_l) in slice.iter().enumerate().skip(1) {
                let norm_l = normalize_tr_ascii(raw_l);
                if query_tokens.iter().any(|t| norm_l.contains(t.as_str())) {
                    preview_line = Some((start_idx + 1 + offset, raw_l.trim().to_string()));
                    break;
                }
            }
            blocks.push(AstCodeBlock {
                path: display_path.clone(),
                start_line: start_idx + 1,
                end_line: end_idx + 1,
                signature: sig,
                norm_sig,
                norm_body,
                preview_line,
            });
        }
    }
    if blocks.is_empty() {
        return None;
    }

    let n_docs = (blocks.len() as f64).max(1.0);
    let avg_dl = (blocks.iter().map(|b| b.norm_body.len()).sum::<usize>() as f64 / n_docs).max(1.0);
    let mut df: Vec<usize> = vec![0; query_tokens.len()];
    for b in &blocks {
        for (i, t) in query_tokens.iter().enumerate() {
            if b.norm_body.contains(t.as_str()) || b.norm_sig.contains(t.as_str()) {
                df[i] += 1;
            }
        }
    }

    let k1 = 1.5_f64;
    let b_param = 0.75_f64;
    let mut scored: Vec<(f64, usize, &AstCodeBlock)> = Vec::new();
    for b in &blocks {
        let dl = b.norm_body.len() as f64;
        let mut score = 0.0_f64;
        let mut matched_terms = 0usize;
        for (i, t) in query_tokens.iter().enumerate() {
            let tf_body = b.norm_body.matches(t.as_str()).count() as f64;
            let tf_sig = b.norm_sig.matches(t.as_str()).count() as f64;
            let tf = tf_body + tf_sig * 2.5;
            if tf > 0.0 {
                matched_terms += 1;
                let doc_freq = df[i] as f64;
                let idf = ((n_docs - doc_freq + 0.5) / (doc_freq + 0.5) + 1.0).ln();
                let denom = tf + k1 * (1.0 - b_param + b_param * (dl / avg_dl));
                score += idf * ((tf * (k1 + 1.0)) / denom.max(0.001));
            }
        }
        // Require either >=2 distinct query tokens or a single token >=4 chars that hits a symbol/body
        if matched_terms >= 2
            || (matched_terms == 1 && query_tokens[0].len() >= 3 && query_tokens.len() == 1)
        {
            scored.push((score, matched_terms, b));
        }
    }
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
    });

    let mut out_lines = Vec::new();
    out_lines.push(format!(
        "[SPAVN BM25+AST Semantic Fallback — regex matched 0 lines; top {} symbol block(s) for `{}`]:",
        scored.len().min(8),
        pattern
    ));
    for (score, _, blk) in scored.into_iter().take(8) {
        out_lines.push(format!(
            "{}:{}-{}: [score={:.2}] {}",
            blk.path, blk.start_line, blk.end_line, score, blk.signature
        ));
        if let Some((ln, ref text)) = blk.preview_line {
            out_lines.push(format!("  L{}: {}", ln, text));
        }
    }
    Some(out_lines.join("\n"))
}

/// Locates `rg`, preferring PATH so any install manager works.
///
/// Homebrew puts `rg` in a prefix that is not always on the PATH of a GUI-launched process, which
/// is why the absolute locations are tried at all; they are a fallback, never the first guess.
pub(crate) fn rg_binary() -> &'static str {
    for candidate in ["/opt/homebrew/bin/rg", "/usr/local/bin/rg", "/usr/bin/rg"] {
        if Path::new(candidate).exists() {
            return candidate;
        }
    }
    "rg"
}

pub(crate) fn exec_search(ctx: &mut ToolContext, args: &Value) -> String {
    let pattern = get_str_alias(args, &["pattern", "query", "regex", "search"]);
    let raw_path = {
        let p = get_str_alias(args, &["path", "dir", "directory", "file"]);
        if p.is_empty() {
            "."
        } else {
            p
        }
    };
    let explicit_host = get_str_alias(args, &["host", "target"]);

    if let Some((ssh_host, remote_path)) = split_remote_prefix(raw_path) {
        let q_pat = shell_quote_remote_path(pattern);
        let q_path = shell_quote_remote_path(remote_path);
        let remote_cmd = format!(
            "export PATH=\"/opt/homebrew/bin:/usr/local/bin:$PATH\"; if command -v rg >/dev/null 2>&1; then rg -n --max-count 10 -- {} {}; else grep -rn --exclude-dir=node_modules --exclude-dir=.git --exclude-dir=target --exclude-dir=dist -- {} {} | head -n 80; fi",
            q_pat, q_path, q_pat, q_path
        );
        let out = Command::new("ssh")
            .args(["-o", "ConnectTimeout=6", &ssh_host, &remote_cmd])
            .output();
        return match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout).to_string();
                if s.trim().is_empty() {
                    "(no matches)".to_string()
                } else {
                    s
                }
            }
            Err(e) => format!("Remote search error: {}", e),
        };
    }

    let target_host = if !explicit_host.is_empty() {
        explicit_host
    } else {
        ctx.default_host.as_str()
    };
    if target_host != "local" && !target_host.is_empty() {
        if let Err(blocked) = validate_host_arg(target_host) {
            return blocked;
        }
        let ssh_host = resolve_ssh_target(target_host);
        let q_pat = shell_quote_remote_path(pattern);
        let q_path = shell_quote_remote_path(raw_path);
        let remote_cmd = format!(
            "export PATH=\"/opt/homebrew/bin:/usr/local/bin:$PATH\"; if command -v rg >/dev/null 2>&1; then rg -n --max-count 10 -- {} {}; else grep -rn --exclude-dir=node_modules --exclude-dir=.git --exclude-dir=target --exclude-dir=dist -- {} {} | head -n 80; fi",
            q_pat, q_path, q_pat, q_path
        );
        let out = Command::new("ssh")
            .args(["-o", "ConnectTimeout=6", &ssh_host, &remote_cmd])
            .output();
        return match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout).to_string();
                if s.trim().is_empty() {
                    "(no matches)".to_string()
                } else {
                    s
                }
            }
            Err(e) => format!("Remote search error: {}", e),
        };
    }

    let raw_paths: Vec<&str> = raw_path
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let mut resolved_paths: Vec<PathBuf> = Vec::new();
    for rp in &raw_paths {
        if let Err(blocked) = ctx.enforce_path(rp, false, "search") {
            return blocked;
        }
        resolved_paths.push(ctx.resolve_local_path(rp));
    }
    let path = resolved_paths
        .first()
        .cloned()
        .unwrap_or_else(|| ctx.resolve_local_path("."));
    let file_glob = get_str_alias(args, &["glob", "file_pattern", "include"]);
    let case_sensitive = args
        .get("case_sensitive")
        .or_else(|| args.get("case"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let context_lines = args
        .get("context")
        .or_else(|| args.get("context_lines"))
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).min(8))
        .unwrap_or(0);
    let mut cmd = Command::new(rg_binary());
    cmd.args([
        "-n",
        "--max-count",
        "10",
        "--max-depth",
        "6",
        "--max-filesize",
        "512K",
        "--max-columns",
        "220",
    ]);
    if !case_sensitive {
        cmd.arg("-i");
    }
    if context_lines > 0 {
        cmd.args(["-C", &context_lines.to_string()]);
    }
    if !file_glob.is_empty() {
        cmd.args(["-g", file_glob]);
    }
    push_ignore_globs(&mut cmd);
    cmd.arg(pattern);
    for p in &resolved_paths {
        cmd.arg(p.as_os_str());
    }
    match run_command_with_timeout(cmd, 8) {
        Ok((_, stdout, _, timed_out)) => {
            let raw = String::from_utf8_lossy(&stdout);
            let mut lines: Vec<String> = raw.lines().take(80).map(|s| s.to_owned()).collect();
            if timed_out {
                lines.push(
                    crate::config::tr_en(
                        "[SPAVN Search: 8s zaman sınırı doldu, ilk eşleşmeler gösteriliyor]",
                        "[SPAVN Search: 8s time limit reached, showing initial matches]"
                    )
                    .to_owned(),
                );
            }
            if lines.is_empty() {
                bm25_ast_block_search(&path, pattern, file_glob)
                    .unwrap_or_else(|| "(no matches)".to_string())
            } else {
                lines.join("\n")
            }
        }
        Err(e) if e.contains("not installed") => {
            native_search_fallback(&path, pattern, file_glob, context_lines)
        }
        Err(e) => format!("Search error: {}", e),
    }
}

/// Default and hard cap for `glob` results: enough for a real project, small enough that a
/// runaway pattern cannot flood the model context.
pub(crate) const GLOB_DEFAULT_LIMIT: usize = 100;
pub(crate) const GLOB_MAX_LIMIT: usize = 1000;
pub(crate) fn exec_glob(ctx: &mut ToolContext, args: &Value) -> String {
    let pattern = {
        let p = get_str_alias(args, &["pattern", "glob", "query", "name", "match"]);
        if p.is_empty() {
            return "glob: missing `pattern` (e.g. `**/*.rs`)".to_string();
        }
        p
    };
    let raw_path = {
        let p = get_str_alias(args, &["path", "dir", "directory", "root", "cwd"]);
        if p.is_empty() {
            "."
        } else {
            p
        }
    };
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).clamp(1, GLOB_MAX_LIMIT))
        .unwrap_or(GLOB_DEFAULT_LIMIT);
    let hidden = args
        .get("hidden")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let explicit_host = get_str_alias(args, &["host", "target"]);

    if let Some((ssh_host, remote_path)) = split_remote_prefix(raw_path) {
        let q_pat = shell_quote_remote_path(pattern);
        let q_path = shell_quote_remote_path(remote_path);
        // The positive glob goes first so the negations below still win (rg is
        // last-match-wins). `--hidden` is the only thing that re-admits dotfiles.
        let remote_cmd = format!(
            "export PATH=\"/opt/homebrew/bin:/usr/local/bin:$PATH\"; \
             if command -v rg >/dev/null 2>&1; then \
                 rg --files --color never {} -g {} -- {} | head -n {}; \
             else \
                 find {} -type f -name {} | head -n {}; \
             fi",
            if hidden { "--hidden" } else { "" },
            q_pat,
            q_path,
            limit,
            q_path,
            q_pat,
            limit
        );
        let out = Command::new("ssh")
            .args(["-o", "ConnectTimeout=6", &ssh_host, &remote_cmd])
            .output();
        return match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout).to_string();
                if s.trim().is_empty() {
                    format!(
                        "(no files match `{}` in {}:{})",
                        pattern, ssh_host, remote_path
                    )
                } else {
                    s
                }
            }
            Err(e) => format!("Remote glob error: {}", e),
        };
    }

    let target_host = if !explicit_host.is_empty() {
        explicit_host
    } else {
        ctx.default_host.as_str()
    };
    if target_host != "local" && !target_host.is_empty() {
        return format!(
            "glob: remote host '{}' configured but path has no '<host>:' prefix",
            target_host
        );
    }

    if let Err(blocked) = ctx.enforce_path(raw_path, false, "glob") {
        return blocked;
    }
    let path = ctx.resolve_local_path(raw_path);

    // Order matters: ripgrep applies `-g` overrides last-match-wins, and a *positive* glob also
    // bypasses the implicit hidden filter. So the pattern goes first, and the negations that
    // should win over it (dotfiles, build dirs) go last.
    let mut cmd = Command::new(rg_binary());
    cmd.args(["--files", "--color", "never"]);
    if hidden {
        cmd.arg("--hidden");
    }
    cmd.args(["-g", pattern]);
    if !hidden {
        cmd.args(["-g", "!**/.*"]);
    }
    push_ignore_globs(&mut cmd);
    cmd.arg(path.as_os_str());

    match run_command_with_timeout(cmd, 8) {
        Ok((_, stdout, _, timed_out)) => {
            let raw = String::from_utf8_lossy(&stdout);
            let lines: Vec<&str> = raw
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(limit)
                .collect();
            if lines.is_empty() {
                return format!("(no files match `{}` in {})", pattern, path.display());
            }
            let truncated = !timed_out && lines.len() == limit;
            let mut out = lines.join("\n");
            if timed_out {
                out.push_str("\n[glob: 8s time limit reached, showing the first matches]");
            } else if truncated {
                out.push_str(&format!(
                    "\n[glob: limit {} reached — narrow the pattern or pass a higher `limit`]",
                    limit
                ));
            }
            out
        }
        Err(e) if e.contains("not installed") => {
            native_glob_fallback(&path, pattern, limit, hidden)
        }
        Err(e) => format!("Glob error: {}", e),
    }
}

pub(crate) fn exec_memory(ctx: &mut ToolContext, args: &Value) -> String {
    let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("recall");
    let query = get_str_alias(args, &["query", "text", "content", "note"]);
    let cfg = load_user_config();
    let cwd = crate::memory::current_workdir();

    if op == "remember" {
        if ctx.access == AccessMode::Safe
            && !prompt_user_approval(ctx, "Remember", query)
        {
            return "User rejected memory write in SAFE mode.".to_string();
        }
        let raw_kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("not");
        let kind = match raw_kind {
            "not" | "tercih" | "bilgi" | "durum" | "karar" => raw_kind,
            _ => "not",
        };
        if let Some(ref remember_bin) = cfg.memory_remember_bin {
            let script_path = expand_tilde(remember_bin);
            if script_path.exists() {
                if let Ok(out) = Command::new("python3")
                    .args([script_path.to_string_lossy().as_ref(), kind, query, "spavn"])
                    .output()
                {
                    if out.status.success() {
                        let msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        return format!(
                            "{}🧠 Saved to memory ({}): {}{}",
                            CYAN, kind, msg, RESET
                        );
                    }
                }
            }
        }
        let pin =
            crate::spavn_engine::add_pin(&cwd, &format!("[{}] {}", kind.to_uppercase(), query));
        return format!(
            "{}📌 Saved to Spavn Project Memory (Pin #{}): {}{}",
            CYAN, pin.id, pin.content, RESET
        );
    }

    if let Some(ref recall_bin) = cfg.memory_recall_bin {
        let script_path = expand_tilde(recall_bin);
        if script_path.exists() {
            if let Ok(out) = Command::new("python3")
                .args([script_path.to_string_lossy().as_ref(), query])
                .output()
            {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if !text.trim().is_empty() {
                    return text;
                }
            }
        }
    }

    let arts = crate::spavn_engine::retrieve_relevant_artifacts(&cwd, query, 5);
    if arts.is_empty() {
        "(No matching memory record found)".to_string()
    } else {
        arts.into_iter()
            .map(|a| format!("- [{}] ({}): {}", a.ts, a.files.join(", "), a.summary))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(crate) fn parse_args_resilient(raw: &str) -> Value {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return v;
    }
    let stripped = trimmed
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<Value>(stripped) {
        return v;
    }
    if let (Some(s), Some(e)) = (stripped.find('{'), stripped.rfind('}')) {
        if s < e {
            if let Ok(v) = serde_json::from_str::<Value>(&stripped[s..=e]) {
                return v;
            }
        }
    }
    json!({})
}

pub(crate) fn exec_skill(args: &Value) -> String {
    let name = get_str_alias(args, &["name", "skill", "id", "query"]);
    if name.is_empty() {
        let all = crate::skills::discover_skills();
        let names: Vec<String> = all
            .into_iter()
            .map(|s| format!("@{} ({})", s.name, s.summary))
            .collect();
        return format!("Available skills:\n{}", names.join("\n"));
    }
    if let Some((entry, content)) = crate::skills::load_skill_content(name) {
        format!(
            "# Loaded Skill: @{} ({})\n{}",
            entry.name,
            entry.path.display(),
            content.trim()
        )
    } else {
        format!(
            "Skill '{}' not found. Use `/skills` to list available skills.",
            name
        )
    }
}
