use super::*;

pub fn tool_definitions() -> Value {
    let cfg = crate::config::load_user_config();
    let mut mcp_names: Vec<String> = cfg.mcp_servers.keys().cloned().collect();
    mcp_names.sort();
    let mcp_hint = if mcp_names.is_empty() {
        "No MCP servers configured (`mcpServers` in `~/.spavn/config.json`).".to_string()
    } else {
        format!(
            "MCP servers: `{}`. Call with `op='mcp'`, `server=<name>` and no `tool` to list what a \
             server exposes, then again with `tool=<name>` and `arguments={{...}}` to run it.",
            mcp_names.join("`, `")
        )
    };
    let lsp_desc = format!(
        "Real Language Server Protocol client (`op=definition|references|hover|symbols|diagnostics|implementation|type_definition|workspace_symbol`) for Rust (rust-analyzer), TS/JS (typescript-language-server), Python (pyright), Go (gopls), C/C++ (clangd), with ripgrep heuristic fallback; also generic MCP client (`op='mcp'`). {}",
        mcp_hint
    );
    json!([
        {
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file, directory, SQLite DB (`.db`/`.sqlite`), PDF, or image. Supports disjoint line ranges (`path:1-12,640-665`), tail lines (`path:-50`), JSON pointer queries (`file.json?q=.a.b`), and SQLite table/SQL queries (`app.db?q=SELECT ...` or `app.db?q=users`).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path (optional `<host>:` prefix); may embed ranges (`src/x.rs:1-12,640-665`), tail (`log.txt:-50`), or query (`config.json?q=.a.b`, `data.db?q=users`)" },
                        "start_line": { "type": "integer", "description": "Start line (1-based)" },
                        "end_line": { "type": "integer", "description": "End line (1-based, inclusive)" },
                        "q": { "type": "string", "description": "Optional JSON pointer (`.a.b`) for `.json` or table/SQL query for `.db`/`.sqlite`" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Replace `old_text` with `new_text`, or replace/insert by line range (`start_line`..`end_line`, `path:10-25`, or `insert_after_line` without `old_text`), apply in-memory multi-hunk `edits` with automatic line-delta tracking, or apply a cross-file atomic transaction via `files`.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File to edit (single-file mode; supports inline `src/x.rs:10-25`)" },
                        "old_text": { "type": "string", "description": "Exact existing snippet (optional when `start_line`/`end_line` or `insert_after_line` is used)" },
                        "new_text": { "type": "string", "description": "Replacement snippet (single edit)" },
                        "start_line": { "type": "integer", "description": "1-based start line for token-free range replacement" },
                        "end_line": { "type": "integer", "description": "1-based inclusive end line for token-free range replacement" },
                        "insert_after_line": { "type": "integer", "description": "Insert `new_text` after this 1-based line (0 = top of file)" },
                        "edits": {
                            "type": "array",
                            "description": "Batch of hunks (`{old_text, new_text}` or `{start_line, end_line, new_text}` or `{insert_after_line, new_text}`) applied in-memory with auto line-delta tracking and single atomic disk write",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old_text": { "type": "string" },
                                    "new_text": { "type": "string" },
                                    "start_line": { "type": "integer" },
                                    "end_line": { "type": "integer" },
                                    "insert_after_line": { "type": "integer" }
                                },
                                "required": ["new_text"]
                            }
                        },
                        "files": {
                            "type": "array",
                            "description": "Cross-file atomic transaction: array of `{path, old_text, new_text}` applied across multiple files with full rollback on error",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": { "type": "string" },
                                    "old_text": { "type": "string" },
                                    "new_text": { "type": "string" }
                                },
                                "required": ["path", "old_text", "new_text"]
                            }
                        }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write",
                "description": "Create or overwrite a file (`<host>:/path` for a remote host).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Target path" },
                        "content": { "type": "string", "description": "Full file content" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run a shell command or zero-quoting `py`/`js` script via stdin locally or on a configured remote host.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command, or raw Python/Node script when `lang` is `py` or `js`" },
                        "lang": { "type": "string", "enum": ["sh", "py", "js"], "description": "Optional runtime (`py` pipes `command` to `python3 -` via stdin with zero quote escaping; `js` pipes to `node -`)" },
                        "host": { "type": "string", "description": "Host alias (default: local)" },
                        "cwd": { "type": "string", "description": "Working directory" },
                        "timeout_sec": { "type": "integer", "description": "Timeout seconds (default 300)" },
                        "background": { "type": "boolean", "description": "Run detached; returns `bg_N` job ID, PID, and log path" },
                        "op": { "type": "string", "enum": ["run", "status", "list", "kill"], "description": "Background job control (`status`, `list`, `kill`); omit for normal execution" },
                        "job_id": { "type": "string", "description": "Target `bg_N` or PID for `status`/`kill`" }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search",
                "description": "Case-insensitive regex/symbol content search across files (`;` joins multiple paths) with automatic Rust BM25+AST block semantic fallback when regex finds 0 matches. Supports `glob` filter, `case` sensitivity, and `context` lines.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex, symbol pattern, or natural-language/Turkish symbol query" },
                        "path": { "type": "string", "description": "File or `;`-separated directories to search (default `.`)" },
                        "glob": { "type": "string", "description": "Optional filename glob filter, e.g. `*.tsx` or `*.rs`" },
                        "case": { "type": "boolean", "description": "Case-sensitive match (default false)" },
                        "context": { "type": "integer", "description": "Surrounding context lines per match (0..8, default 0)" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "glob",
                "description": "List files matching a glob, no content search. Skips node_modules/target/.git and dotfiles unless `hidden`.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob, e.g. `**/*.rs`" },
                        "path": { "type": "string", "description": "Root dir (default `.`; supports `<host>:/path`)" },
                        "limit": { "type": "integer", "description": "Max files (default 100, cap 1000)" },
                        "hidden": { "type": "boolean", "description": "Include dotfiles" },
                        "host": { "type": "string", "description": "Host alias" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "memory",
                "description": "Search or record persistent project/user memory and decisions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["recall", "remember"], "description": "recall | remember" },
                        "query": { "type": "string", "description": "Query, or note text" },
                        "kind": { "type": "string", "description": "durum | karar | bilgi | not" }
                    },
                    "required": ["op", "query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "skill",
                "description": "Load domain instructions from ~/.spavn/skills or ~/.claude/skills.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Skill name" }
                    },
                    "required": ["name"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web",
                "description": "Web search (`op='search'`, built-in provider chain with fallback) or fetch a URL as Markdown (`op='fetch'`).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["search", "fetch"], "description": "search | fetch" },
                        "query": { "type": "string", "description": "Query, or URL" },
                        "url": { "type": "string", "description": "URL for op='fetch'" },
                        "limit": { "type": "integer", "description": "Max search results (default 8, max 20)" },
                        "recency": { "type": "string", "enum": ["day", "week", "month", "year"], "description": "Only results from this period" }
                    },
                    "required": ["op"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "subagent",
                "description": "Delegate investigations or code modifications to isolated sub-agents: one `task` or up to 8 `tasks` concurrently. Supports read-only `scout` (default) and full-capability `worker` agents in isolated worktrees.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "The sub-question or modification objective" },
                        "context": { "type": "string", "description": "Shared file paths or background context" },
                        "expected_output": { "type": "string", "description": "Findings or changes to report" },
                        "kind": { "type": "string", "enum": ["scout", "worker"], "description": "Agent kind: 'scout' (read-only, default) or 'worker' (full tools in isolated git worktree)" },
                        "max_steps": { "type": "integer", "description": "Step budget (default 24 scout / 40 worker, max 80)" },
                        "model": { "type": "string", "description": "Model override, e.g. local | flash" },
                        "tasks": {
                            "type": "array",
                            "description": "Up to 8 sub-tasks run concurrently",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "task": { "type": "string" },
                                    "context": { "type": "string" },
                                    "expected_output": { "type": "string" },
                                    "kind": { "type": "string", "enum": ["scout", "worker"] },
                                    "max_steps": { "type": "integer" },
                                    "model": { "type": "string" }
                                },
                                "required": ["task"]
                            }
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "lsp",
                "description": lsp_desc,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": {
                            "type": "string",
                            "enum": [
                                "definition",
                                "references",
                                "hover",
                                "symbols",
                                "diagnostics",
                                "implementation",
                                "type_definition",
                                "workspace_symbol",
                                "mcp"
                            ],
                            "description": "LSP or MCP operation to perform"
                        },
                        "path": { "type": "string", "description": "Target file or directory path relative to workspace root" },
                        "line": { "type": "integer", "description": "1-based line number for position-based ops (definition, references, hover)" },
                        "column": { "type": "integer", "description": "1-based column number (optional; computed automatically from `symbol` if omitted)" },
                        "symbol": { "type": "string", "description": "Target symbol or query string. For position ops, finds the symbol on `line` to calculate column. For definition/references without path/line, searches workspace symbols first." },
                        "timeout_sec": { "type": "integer", "description": "Timeout in seconds (e.g. wait time for diagnostics)" },
                        "server": { "type": "string", "description": "MCP server name (for op='mcp')" },
                        "tool": { "type": "string", "description": "MCP tool name (omit to list tools)" },
                        "arguments": { "type": "object", "description": "MCP JSON arguments" }
                    },
                    "required": ["op"]
                }
            }
        }
    ])
}

pub fn tool_definitions_for_phase(phase: crate::spavn_engine::AgentPhase) -> Value {
    // The schema shape only changes with the phase, so build it once per phase instead of
    // re-serializing 6.7 KB of JSON on every model request.
    static CACHE: std::sync::Mutex<[Option<Value>; 2]> = std::sync::Mutex::new([None, None]);
    let idx = usize::from(phase == crate::spavn_engine::AgentPhase::Plan);
    if let Ok(mut slot) = CACHE.lock() {
        if let Some(hit) = slot[idx].as_ref() {
            return hit.clone();
        }
        let built = build_tool_definitions_for_phase(phase);
        slot[idx] = Some(built.clone());
        return built;
    }
    build_tool_definitions_for_phase(phase)
}

/// Local-LLM-aware tool schema selector.
///
/// Cloud frontier models get the full tool inventory. Local models (`is_local == true`) get the
/// 8 core tools (`read`, `edit`, `write`, `bash`, `search`, `glob`, `web`, `memory`) by default
/// so they can always search the web and query Second Brain memory without breaking KV-cache
/// prefix across turns, plus domain tools (`skill`, `subagent`, `lsp`) when requested.
pub fn tool_definitions_for_turn(
    phase: crate::spavn_engine::AgentPhase,
    is_local: bool,
    user_prompt: &str,
) -> Value {
    let base = tool_definitions_for_phase(phase);
    if !is_local {
        return base;
    }
    let lower = user_prompt.to_lowercase();
    let want_skill = lower.contains('@') || lower.contains("skill") || lower.contains("yetenek");
    let want_subagent = lower.contains("subagent")
        || lower.contains("alt ajan")
        || lower.contains("paralel")
        || lower.contains("worker")
        || lower.contains("scout")
        || lower.contains("delegate");
    let want_lsp = lower.contains("lsp")
        || lower.contains("mcp")
        || lower.contains("definition")
        || lower.contains("references")
        || lower.contains("diagnostics")
        || lower.contains("hover")
        || lower.contains("symbols")
        || lower.contains("symbol");
    if let Value::Array(items) = base {
        let filtered: Vec<Value> = items
            .into_iter()
            .filter(|item| {
                let name = item
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                match name {
                    "read" | "edit" | "write" | "bash" | "search" | "glob" | "web" | "memory" => {
                        true
                    }
                    "skill" => want_skill,
                    "subagent" => want_subagent,
                    "lsp" => want_lsp,
                    _ => true,
                }
            })
            .collect();
        Value::Array(filtered)
    } else {
        base
    }
}

pub(crate) fn build_tool_definitions_for_phase(phase: crate::spavn_engine::AgentPhase) -> Value {
    let all = tool_definitions();
    if phase == crate::spavn_engine::AgentPhase::Code {
        return all;
    }
    if let Value::Array(items) = all {
        let filtered: Vec<Value> = items
            .into_iter()
            .filter(|item| {
                let name = item
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                !matches!(name, "edit" | "write" | "bash")
            })
            .collect();
        Value::Array(filtered)
    } else {
        all
    }
}
