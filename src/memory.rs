use crate::config::load_user_profile_markdown;
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn current_workdir() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

fn build_workspace_repo_map(cwd: &str, compact: bool) -> String {
    let mut map = String::new();
    let root = Path::new(cwd);

    // 1. Git branch + short status (only in full/cloud mode so local KV prefix stays 100% static across edits!)
    if !compact && root.join(".git").exists() {
        if let Ok(branch_out) = Command::new("git")
            .args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
        {
            let branch = String::from_utf8_lossy(&branch_out.stdout)
                .trim()
                .to_string();
            if !branch.is_empty() {
                map.push_str(&format!("- Git Branch: `{}`\n", branch));
            }
        }
        if let Ok(st_out) = Command::new("git")
            .args(["-C", cwd, "status", "--short"])
            .output()
        {
            let status_lines: Vec<String> = String::from_utf8_lossy(&st_out.stdout)
                .lines()
                .take(15)
                .map(|s| s.to_string())
                .collect();
            if !status_lines.is_empty() {
                map.push_str(&format!("- Dirty Files: `{}`\n", status_lines.join(", ")));
            }
        }
    }

    // 2. Top-level + src/ file tree (max 60 entries)
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        let mut sorted: Vec<_> = entries.flatten().collect();
        sorted.sort_by_key(|e| e.file_name());
        for entry in sorted {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.')
                || name == "node_modules"
                || name == "target"
                || name == "dist"
                || name == "__pycache__"
            {
                continue;
            }
            let p = entry.path();
            if p.is_dir() {
                files.push(format!("{}/", name));
                if let Ok(sub_entries) = fs::read_dir(&p) {
                    let mut sub_sorted: Vec<_> = sub_entries.flatten().collect();
                    sub_sorted.sort_by_key(|e| e.file_name());
                    for sub in sub_sorted.into_iter().take(15) {
                        let sub_name = sub.file_name().to_string_lossy().to_string();
                        if !sub_name.starts_with('.') {
                            files.push(format!("  {}/{}", name, sub_name));
                        }
                    }
                }
            } else {
                files.push(name);
            }
            if files.len() >= 60 {
                break;
            }
        }
    }

    if !files.is_empty() {
        map.push_str("\n## Workspace Files (Turn-0 Repo Map — Do NOT run `ls` or `find` to discover these)\n");
        for f in files {
            map.push_str(&format!("- {}\n", f));
        }
    }

    // 2b. Symbol Map for src/, backend/, app/, lib/ or root (.rs, .py, .ts, .tsx, .js, .jsx, .go) with 1-based line numbers
    let mut sym_entries = Vec::new();
    for subdir in ["src", "backend", "app", "lib"] {
        let candidate = root.join(subdir);
        if candidate.is_dir() && sym_entries.len() < 20 {
            collect_symbol_map(&candidate, root, &mut sym_entries, 0);
        }
    }
    if sym_entries.is_empty() {
        collect_symbol_map(root, root, &mut sym_entries, 2);
    }
    if !sym_entries.is_empty() {
        map.push_str("\n## Symbol Map (Jump directly to line numbers with `read`)\n");
        let mut budget = if compact {
            SYMBOL_MAP_BUDGET_COMPACT
        } else {
            SYMBOL_MAP_BUDGET
        };
        for line in sym_entries.into_iter().take(20) {
            if line.len() + 3 > budget {
                break;
            }
            budget -= line.len() + 3;
            map.push_str(&format!("- {}\n", line));
        }
    }

    // 3. Project-local AGENTS.md or CLAUDE.md if present
    for rule_file in ["AGENTS.md", "CLAUDE.md"] {
        let rp = root.join(rule_file);
        if rp.exists() {
            if let Ok(content) = fs::read_to_string(&rp) {
                let snippet: String = content.lines().take(25).collect::<Vec<_>>().join("\n");
                map.push_str(&format!(
                    "\n## Project Rules ({})\n{}\n",
                    rule_file, snippet
                ));
                break;
            }
        }
    }

    // 4. Spavn Context Pins (`/pin`) and Active Project Goals (`/goal`)
    let pins = crate::spavn_engine::load_pins(cwd);
    if !pins.is_empty() {
        map.push_str("\n## Spavn Pinned Project Context (`/pin`)\n");
        for p in pins.into_iter().take(8) {
            map.push_str(&format!(
                "- [Pin #{}] {}\n",
                p.id,
                clip(&p.content, ITEM_BUDGET)
            ));
        }
    }
    let goals = crate::spavn_engine::load_goals(cwd);
    let active_goals: Vec<_> = goals.into_iter().filter(|g| g.state == "active").collect();
    if !active_goals.is_empty() {
        map.push_str("\n## Spavn Active Project Goals (`/goal`)\n");
        for g in active_goals.into_iter().take(5) {
            map.push_str(&format!(
                "- [Goal #{}] {}\n",
                g.id,
                clip(&g.objective, ITEM_BUDGET)
            ));
        }
    }

    map
}

fn collect_symbol_map(dir: &Path, root: &Path, out: &mut Vec<String>, depth: usize) {
    if depth > 2 || out.len() >= 20 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut vec_entries: Vec<_> = entries.flatten().collect();
    vec_entries.sort_by_key(|e| e.file_name());
    for entry in vec_entries {
        if out.len() >= 20 {
            break;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.')
            || name == "node_modules"
            || name == "target"
            || name == "__pycache__"
        {
            continue;
        }
        if path.is_dir() {
            collect_symbol_map(&path, root, out, depth + 1);
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(
            ext,
            "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "go" | "cs" | "java" | "kt" | "swift"
        ) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let mut symbols = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if crate::tools::is_structural_outline_line(line) {
                let sig = trimmed.trim_end_matches('{').trim_end_matches(':').trim();
                let short_sig: String = sig.chars().take(48).collect();
                symbols.push(format!("L{}: `{}`", idx + 1, short_sig));
                if symbols.len() >= 8 {
                    break;
                }
            }
        }
        if !symbols.is_empty() {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| name);
            out.push(format!("**{}**: {}", rel, symbols.join(", ")));
        }
    }
}

const FULL_CONSTITUTION: &str = r#"# Engineering Constitution (High-Signal Rules)
1. **Targeted Discovery (No Blind Grepping):** Use the Turn-0 `Workspace Files` and `Symbol Map` (with `L<line>` anchors) above to jump straight to `read(path, start_line, end_line)` or `search(pattern, path)`. `read` also takes a span spec — `read(path="src/x.rs:97-112,1066-1076")` — which is the ONLY correct way to fetch several line ranges at once; never shell out to `sed -n`/`awk`/`head`/`tail`/`cat` via `bash` for file inspection (one `read` replaces several round trips and keeps line numbers absolute). Never run `ls`/`find` for files already listed above, and never re-read a file right after editing it (`edit`/`write` already confirms the state).
2. **Reuse Existing Architecture:** Before writing code, inspect neighboring functions/types in the target file. Match the repository's existing naming, error handling, imports, and state patterns. Never invent a parallel convention or leave dead code behind.
3. **High-Precision Surgical Edits:**
   - Prefer `edit` (`path`, `old_text`, `new_text`) over `write` for existing files.
   - Include 2–4 unchanged surrounding lines in `old_text` so the match is 100% unique. Never include `123:` line-number prefixes in `old_text` or `new_text`.
   - If `edit` reports a syntax/brace warning or anchor mismatch, fix it immediately in the next step.
4. **Completeness & Zero Stubs:** Deliver complete, production-ready implementations end-to-end. Never leave `TODO`, placeholders, mock returns, or half-migrated callers. Update all affected callsites and imports before finishing.
5. **Non-Interactive Shell & Verification Gate:**
   - In `bash`, always pass non-interactive flags (`-y`, `--no-pager`, `CI=1`) and avoid blocking pagers/editors (`less`, `vim`, `git log` without `-n`).
   - After any code modification, MUST run a real verification check via `bash` (`cargo check`/`cargo test`, `pytest`, `npm test`, `node --check`, etc.) and confirm it passes before yielding to the user.
   - Never claim fixed/done without a behavioral check (test or runtime smoke) executed after the last edit; always report the verification level actually exercised (compile-only vs test vs runtime).
6. **Remote Host Symmetry:** All file tools (`read`, `edit`, `write`, `search`) accept `<host>:/path` prefixes configured in `~/.spavn/config.json` alongside `bash(host="<host>")`.
7. **Batch Independent Lookups:** Every tool call is a full model round trip. When a step needs several *independent* lookups, emit them together as multiple `tool_calls` in ONE assistant message — never one per turn. Never batch a write with a read of the file it is about to change.
8. **Copy-Ready Output Discipline:** Whenever you present draft replies, message options, prompts, social posts, commands, or config snippets meant for the user to copy and paste elsewhere, ALWAYS place each option's copyable text inside a fenced code block (e.g. ```` ```text ````), never inside `>` blockquotes or wrapped in outer quotation marks."#;

const COMPACT_CONSTITUTION: &str = r#"# Rules
1. Use the Workspace Files and Symbol Map above; go straight to `read(path, start_line, end_line)` or `search`. Several ranges at once: `read(path="src/x.rs:97-112,1066-1076")`. Never inspect files via `bash sed/awk/head/tail/cat`. No `ls`/`find` for listed files. Do not re-read a file after `edit`/`write`.
2. Match the neighboring code's naming, errors and imports. No dead code, no parallel conventions.
3. Prefer `edit` over `write`. `old_text` = exact snippet plus 2-4 unchanged lines so it is unique; never include `123:` line-number prefixes. If `edit` warns, fix it next step.
4. Finish fully: no TODO, stubs or mocks; update every caller and import.
5. `bash` is non-interactive: pass `-y`, `--no-pager`, `CI=1`; no pagers or editors. After any code change run a real check (`cargo check`/`cargo test`, `pytest`, `npm test`) and confirm it passes before replying. Never claim fixed/done without a behavioral check after the last edit; report the verification level actually exercised.
6. File tools and `bash(host=...)` accept `<host>:/path` for hosts in `~/.spavn/config.json`.
7. Batch independent `read`/`search`/`glob` lookups into ONE message: each separate turn is a full model round trip."#;

/// Max chars of the Turn-0 symbol map; keeps the prompt small for local models.
const SYMBOL_MAP_BUDGET: usize = 2000;
const SYMBOL_MAP_BUDGET_COMPACT: usize = 900;
const PROFILE_BUDGET: usize = 1200;
const ITEM_BUDGET: usize = 240;

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
const PROFILE_BUDGET_COMPACT: usize = 500;

/// `compact` = local/small-context models: same rules, fewer words, smaller map.
/// Session-stable wrapper around [`build_system_prompt`].
///
/// The prompt embeds `git status`, which changes after *every* edit. Rebuilding it each turn
/// makes the first message differ from the previous request, so providers cannot reuse their
/// cached prefix and re-prefill the whole conversation instead — the dominant latency cost on
/// long sessions. Caching once per `(cwd, compact, language)` keeps the prefix byte-identical
/// until the session is compacted or reset, which is when [`invalidate_system_prompt_cache`] runs.
///
/// The language is part of the key, not ambient state: `/lang` changes it process-wide, and
/// without it in the key a switch would keep serving the previous language's prompt.
type PromptCacheEntry = (String, bool, bool, String, String);

static SYSTEM_PROMPT_CACHE: std::sync::Mutex<Option<PromptCacheEntry>> =
    std::sync::Mutex::new(None);

pub fn cached_system_prompt(cwd: &str, compact: bool) -> String {
    cached_system_prompt_for_lang(cwd, compact, crate::config::is_en())
}

pub(crate) fn cached_system_prompt_for_lang(cwd: &str, compact: bool, en: bool) -> String {
    let mut slot = match SYSTEM_PROMPT_CACHE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let today = crate::spavn_engine::today_ymd();
    if let Some((cached_cwd, cached_compact, cached_en, cached_today, text)) = slot.as_ref() {
        if cached_cwd == cwd && *cached_compact == compact && *cached_en == en && cached_today == &today {
            return text.clone();
        }
    }
    let built = build_prompt(cwd, compact, en);
    *slot = Some((cwd.to_string(), compact, en, today, built.clone()));
    built
}

/// Drops the cached prompt so the next turn rebuilds branch/status/file map from scratch.
pub fn invalidate_system_prompt_cache() {
    let mut slot = match SYSTEM_PROMPT_CACHE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = None;
}

pub fn build_system_prompt(cwd: &str, compact: bool) -> String {
    build_prompt(cwd, compact, crate::config::is_en())
}

/// Builds the prompt for an explicit language, so the bytes are a pure function of the inputs.
fn build_prompt(cwd: &str, compact: bool, en: bool) -> String {
    let repo_map = build_workspace_repo_map(cwd, compact);
    let profile_budget = if compact {
        PROFILE_BUDGET_COMPACT
    } else {
        PROFILE_BUDGET
    };
    let user_profile_block = load_user_profile_markdown()
        .map(|p| {
            let p: String = p.chars().take(profile_budget).collect();
            format!("\n# User Profile (~/.spavn/PROFILE.md)\n{}\n", p)
        })
        .unwrap_or_default();
    let lang_rule = if en {
        "- Language: Respond in concise, evidence-first English."
    } else {
        "- Language: Turkish, direct, evidence-focused and clear answers; avoid unnecessary preamble/summary sentences."
    };
    let today = crate::spavn_engine::today_ymd();

    let constitution = if compact {
        COMPACT_CONSTITUTION
    } else {
        FULL_CONSTITUTION
    };
    format!(
        r#"# Identity & Role
You are **SPAVN** (`spavn.dev`), an elite autonomous software engineering agent.
{lang_rule}
- Presentation: Use clean Markdown (`### Heading`, `- **Key**: detail`), fenced code blocks, and tables for comparisons.
- Current Date: `{today}`
- Working Directory: `{cwd}`
{user_profile_block}{repo_map}
{constitution}"#
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn symbol_map_stays_within_budget() {
        let p = super::build_system_prompt(".", false);
        let sym = p.split("## Symbol Map").nth(1).unwrap_or("");
        let sym = sym.split("\n#").next().unwrap_or("");
        assert!(sym.len() <= super::SYMBOL_MAP_BUDGET + 120, "{}", sym.len());
    }

    #[test]
    fn cached_prompt_is_byte_identical_until_invalidated() {
        super::invalidate_system_prompt_cache();
        let a = super::cached_system_prompt_for_lang(".", true, true);
        let b = super::cached_system_prompt_for_lang(".", true, true);
        assert_eq!(a, b, "repeat call must not rebuild");
        // A different mode is a different key, not a stale hit.
        let c = super::cached_system_prompt_for_lang(".", false, true);
        assert_ne!(a, c);
        super::invalidate_system_prompt_cache();
        // Determinism belongs to the inputs. `is_en()` is process-global and other tests flip it
        // mid-run, so pin the language instead of reading it twice and racing them.
        for en in [false, true] {
            assert_eq!(
                super::build_prompt(".", true, en),
                super::build_prompt(".", true, en),
                "same inputs must build the same bytes (en={})",
                en
            );
        }
        assert_ne!(
            super::build_prompt(".", true, false),
            super::build_prompt(".", true, true),
            "the language must be part of the prompt"
        );
        super::invalidate_system_prompt_cache();
    }

    #[test]
    fn both_constitutions_teach_tool_batching() {
        assert!(super::FULL_CONSTITUTION.contains("Batch Independent Lookups"));
        assert!(super::COMPACT_CONSTITUTION.contains("Batch independent"));
    }

    #[test]
    fn both_constitutions_route_line_ranges_through_read_not_bash() {
        // The span syntax only pays off if the model knows it; without this rule it keeps
        // shelling out to `sed -n`, which costs an extra round trip and loses line anchors.
        for rules in [super::FULL_CONSTITUTION, super::COMPACT_CONSTITUTION] {
            assert!(rules.contains(":97-112,1066-1076"), "span example missing");
            assert!(rules.contains("sed"), "the bash-inspection ban is missing");
        }
    }

    #[test]
    fn both_constitutions_teach_verification_level_discipline() {
        for rules in [super::FULL_CONSTITUTION, super::COMPACT_CONSTITUTION] {
            assert!(
                rules.contains("never claim fixed/done without a behavioral check")
                    || rules.contains("Never claim fixed/done without a behavioral check"),
                "behavioral check rule missing"
            );
            assert!(
                rules.contains("report the verification level actually exercised"),
                "verification level reporting rule missing"
            );
        }
    }
}

#[cfg(test)]
mod compact_tests {
    #[test]
    fn compact_is_smaller_and_keeps_core_rules() {
        let full = super::build_system_prompt(".", false);
        let c = super::build_system_prompt(".", true);
        assert!(
            c.len() < full.len() * 7 / 10,
            "{} vs {}",
            c.len(),
            full.len()
        );
        for key in ["edit", "search", "cargo test", "TODO", "<host>:/path"] {
            assert!(c.contains(key), "missing {key}");
        }
    }
}

#[cfg(test)]
mod clip_tests {
    #[test]
    fn clip_is_char_safe() {
        assert_eq!(super::clip("çğüşö", 3), "çğü");
        assert_eq!(super::clip("ab", 10), "ab");
    }
}
