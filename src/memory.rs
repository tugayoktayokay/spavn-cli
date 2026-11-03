use crate::config::load_user_profile_markdown;
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn current_workdir() -> String {
    std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

fn build_workspace_repo_map(cwd: &str) -> String {
    let mut map = String::new();
    let root = Path::new(cwd);

    // 1. Git branch + short status
    if root.join(".git").exists() {
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

    // 2b. Symbol Map for src/ (.rs, .py, .ts, .tsx, .js, .jsx) with exact 1-based line numbers
    let src_dir = root.join("src");
    if src_dir.is_dir() {
        let mut sym_entries = Vec::new();
        collect_symbol_map(&src_dir, root, &mut sym_entries, 0);
        if !sym_entries.is_empty() {
            map.push_str("\n## Symbol Map (src/ — Jump directly to line numbers with `read`)\n");
            for line in sym_entries.into_iter().take(20) {
                map.push_str(&format!("- {}\n", line));
            }
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
        for p in pins {
            map.push_str(&format!("- [Pin #{}] {}\n", p.id, p.content));
        }
    }
    let goals = crate::spavn_engine::load_goals(cwd);
    let active_goals: Vec<_> = goals.into_iter().filter(|g| g.state == "active").collect();
    if !active_goals.is_empty() {
        map.push_str("\n## Spavn Active Project Goals (`/goal`)\n");
        for g in active_goals {
            map.push_str(&format!("- [Goal #{}] {}\n", g.id, g.objective));
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
        if !matches!(ext, "rs" | "py" | "ts" | "tsx" | "js" | "jsx") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let mut symbols = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            let is_sym = trimmed.starts_with("pub fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("pub enum ")
                || trimmed.starts_with("def ")
                || trimmed.starts_with("async def ")
                || trimmed.starts_with("export function ")
                || trimmed.starts_with("export async function ")
                || trimmed.starts_with("export class ")
                || trimmed.starts_with("export interface ");
            if is_sym {
                let sig = trimmed.trim_end_matches('{').trim_end_matches(':').trim();
                let short_sig: String = sig.chars().take(64).collect();
                symbols.push(format!("L{}: `{}`", idx + 1, short_sig));
                if symbols.len() >= 18 {
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

pub fn build_system_prompt(cwd: &str) -> String {
    let repo_map = build_workspace_repo_map(cwd);
    let user_profile_block = load_user_profile_markdown()
        .map(|p| format!("\n# User Profile (~/.spavn/PROFILE.md)\n{}\n", p))
        .unwrap_or_default();
    let lang_rule = if crate::config::is_en() {
        "- Language: Respond in concise, evidence-first English."
    } else {
        "- Dil: Türkçe, doğrudan, kanıt odaklı ve net yanıt ver; gereksiz giriş/özet cümleleri kurma."
    };

    format!(
        r#"# Identity & Role
You are **SPAVN** (`spavn.dev`), an elite autonomous software engineering agent.
{lang_rule}
- Presentation: Use clean Markdown (`### Heading`, `- **Key**: detail`), fenced code blocks, and tables for comparisons.
- Working Directory: `{cwd}`
{user_profile_block}{repo_map}
# Engineering Constitution (High-Signal Rules)
1. **Targeted Discovery (No Blind Grepping):** Use the Turn-0 `Workspace Files` and `Symbol Map` (with `L<line>` anchors) above to jump straight to `read(path, start_line, end_line)` or `search(pattern, path)`. Never run `ls`/`find` for files already listed above, and never re-read a file right after editing it (`edit`/`write` already confirms the state).
2. **Reuse Existing Architecture:** Before writing code, inspect neighboring functions/types in the target file. Match the repository's existing naming, error handling, imports, and state patterns. Never invent a parallel convention or leave dead code behind.
3. **High-Precision Surgical Edits:**
   - Prefer `edit` (`path`, `old_text`, `new_text`) over `write` for existing files.
   - Include 2–4 unchanged surrounding lines in `old_text` so the match is 100% unique. Never include `123:` line-number prefixes in `old_text` or `new_text`.
   - If `edit` reports a syntax/brace warning or anchor mismatch, fix it immediately in the next step.
4. **Completeness & Zero Stubs:** Deliver complete, production-ready implementations end-to-end. Never leave `TODO`, placeholders, mock returns, or half-migrated callers. Update all affected callsites and imports before finishing.
5. **Non-Interactive Shell & Verification Gate:**
   - In `bash`, always pass non-interactive flags (`-y`, `--no-pager`, `CI=1`) and avoid blocking pagers/editors (`less`, `vim`, `git log` without `-n`).
   - After any code modification, MUST run a real verification check via `bash` (`cargo check`/`cargo test`, `pytest`, `npm test`, `node --check`, etc.) and confirm it passes before yielding to the user.
6. **Remote Host Symmetry:** All file tools (`read`, `edit`, `write`, `search`) accept `<host>:/path` prefixes configured in `~/.spavn/config.json` alongside `bash(host="<host>")`."#
    )
}
