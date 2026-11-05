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
pub(crate) fn native_glob_fallback(root: &Path, pattern: &str, limit: usize, hidden: bool) -> String {
    let mut matches: Vec<String> = Vec::new();
    let mut hit_limit = false;
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

/// Native zero-dependency content search used when `rg` is not installed on the host.
/// Supports literal substrings and `|` / `\|` alternations, caps per-file matches at 10,
/// and skips binary/minified/ignored files.
pub(crate) fn native_search_fallback(root: &Path, pattern: &str) -> String {
    let normalized_pat = pattern.replace("\\|", "|");
    let terms: Vec<&str> = normalized_pat
        .split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if terms.is_empty() {
        return "(no matches)".to_string();
    }
    let mut results: Vec<String> = Vec::new();
    fn search_file(path: &Path, terms: &[&str], out: &mut Vec<String>) {
        let Ok(meta) = fs::metadata(path) else {
            return;
        };
        if meta.len() > 512 * 1024 {
            return;
        }
        let Ok(content) = fs::read_to_string(path) else {
            return;
        };
        let mut file_hits = 0usize;
        let disp = path.to_string_lossy();
        for (idx, line) in content.lines().enumerate() {
            if line.len() > 500 {
                continue;
            }
            if terms.iter().any(|t| line.contains(t)) {
                let clipped: String = line.chars().take(220).collect();
                out.push(format!("{}:{}:{}", disp, idx + 1, clipped));
                file_hits += 1;
                if file_hits >= 10 || out.len() >= 80 {
                    break;
                }
            }
        }
    }
    fn walk_search(cur: &Path, depth: usize, terms: &[&str], out: &mut Vec<String>) {
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
                walk_search(&p, depth + 1, terms, out);
            } else if !name.starts_with('.') {
                search_file(&p, terms, out);
            }
        }
    }
    if root.is_file() {
        search_file(root, &terms, &mut results);
    } else {
        walk_search(root, 0, &terms, &mut results);
    }
    if results.is_empty() {
        "(no matches)".to_string()
    } else {
        results.join("\n")
    }
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

    if let Err(blocked) = ctx.enforce_path(raw_path, false, "search") {
        return blocked;
    }
    let path = expand_tilde(raw_path);
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
    push_ignore_globs(&mut cmd);
    cmd.arg(pattern).arg(path.as_os_str());

    match run_command_with_timeout(cmd, 8) {
        Ok((_, stdout, _, timed_out)) => {
            let raw = String::from_utf8_lossy(&stdout);
            let mut lines: Vec<&str> = raw.lines().take(80).collect();
            if timed_out {
                lines.push("[SPAVN Search: 8s zaman sınırı doldu, ilk eşleşmeler gösteriliyor]");
            }
            if lines.is_empty() {
                "(no matches)".to_string()
            } else {
                lines.join("\n")
            }
        }
        Err(e) if e.contains("not installed") => native_search_fallback(&path, pattern),
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
    let path = expand_tilde(raw_path);

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
            && !prompt_user_approval(ctx, "Hafızaya Kayıt (remember)", query)
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
                            "{}🧠 Hafızaya kaydedildi ({}): {}{}",
                            CYAN, kind, msg, RESET
                        );
                    }
                }
            }
        }
        let pin =
            crate::spavn_engine::add_pin(&cwd, &format!("[{}] {}", kind.to_uppercase(), query));
        return format!(
            "{}📌 Spavn Proje Hafızasına kaydedildi (Pin #{}): {}{}",
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
        "(Eşleşen hafıza kaydı bulunamadı)".to_string()
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
