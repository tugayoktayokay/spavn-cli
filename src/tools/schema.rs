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
    let mut val = json!([
        {
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file, directory, `http(s)://` URL (web page or image decoded inline for vision), SQLite DB (`.db`/`.sqlite`), PDF, or local image. Text reads include a `[path#TAG]` snapshot tag for `edit(tag=...)`. Supports disjoint line ranges (`path:1-12,640-665`), tail lines (`path:-50`), JSON pointer queries (`file.json?q=.a.b`), and SQLite queries (`app.db?q=users`).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Local path, `<host>:/path`, or `http(s)://` URL/image; local paths may embed ranges (`src/x.rs:1-12,640-665`), tail (`log.txt:-50`), or query (`config.json?q=.a.b`, `data.db?q=users`)" },
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
                        "tag": { "type": "string", "description": "Optional 4-hex snapshot tag from the latest `read` (`[path#TAG]`) or `edit`; rejects the edit if the file changed underneath" },
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
                        "timeout_sec": { "type": "integer", "description": "Hard kill timeout seconds (default 300, allowed 1..3600; 0 disables hard timeout)" },
                        "background": { "type": "boolean", "description": "Run detached; returns `bg_N` job ID, PID, and log path" },
                        "op": { "type": "string", "enum": ["run", "status", "wait", "list", "kill"], "description": "Background job control (`status`, `wait`, `list`, `kill`); omit for normal execution" },
                        "job_id": { "type": "string", "description": "Target `bg_N` or PID for `status`/`wait`/`kill`" }
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
        },
        {
            "type": "function",
            "function": {
                "name": "ast_grep",
                "description": "Structural AST code search and codemod rewrite powered by `sg` (ast-grep). Prefer over regex for syntax-aware multi-file refactors. Omit `rewrite` (or set `apply=false`) for read-only search/preview.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "AST pattern with metavariables, e.g. `foo($A, $B)`" },
                        "rewrite": { "type": "string", "description": "Optional replacement template, e.g. `bar($B, $A)`" },
                        "lang": { "type": "string", "description": "Optional language (rust, ts, tsx, js, python, go, cpp)" },
                        "path": { "type": "string", "description": "Target file or directory (default `.`)" },
                        "apply": { "type": "boolean", "description": "Apply rewrite in place when `rewrite` is set (default true; false = dry-run preview)" }
                    },
                    "required": ["pattern"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "ask",
                "description": "Ask the user a concise clarifying question with optional choices when a materially important tradeoff cannot be resolved from the repo. In non-interactive/headless mode, automatically returns `default`.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "Short, direct question for the user" },
                        "options": { "type": "array", "items": { "type": "string" }, "description": "2-5 concrete choices" },
                        "default": { "type": "string", "description": "Recommended default used if the user presses Enter or in headless mode" }
                    },
                    "required": ["question"]
                }
            }
        }
        ,
        {
            "type": "function",
            "function": {
                "name": "eval",
                "description": "Run code in a persistent stateful Python (`py`) or JavaScript (`js`) kernel. Variables, imports, and functions persist across calls until `reset: true`. Ideal for data analysis, multi-step calculations, JSON/CSV transforms, and quick prototyping.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "language": { "type": "string", "enum": ["py", "js"], "description": "Kernel language: `py` (Python 3) or `js` (Node/Bun)" },
                        "code": { "type": "string", "description": "Code cell to execute in the persistent kernel" },
                        "reset": { "type": "boolean", "description": "Restart the kernel before running (clears state)" },
                        "timeout_sec": { "type": "integer", "description": "Cell deadline in seconds (default 30, max 300)" }
                    },
                    "required": ["language", "code"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "browser",
                "description": "Open a URL in real headless Chrome/Chromium (supports JS-rendered pages and `localhost` dev servers). Extract rendered text/DOM, capture a PNG screenshot attached directly to vision context, or interact via CDP (`click`, `fill`, `selector`, `eval_js`).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["open", "text", "screenshot", "eval_js"], "description": "Action: `open`/`text` (rendered text), `screenshot` (PNG attached to vision), `eval_js` (run JS in page)" },
                        "url": { "type": "string", "description": "Target `http(s)://` URL (public or localhost dev server)" },
                        "selector": { "type": "string", "description": "Optional CSS selector to extract text from" },
                        "click": { "type": "string", "description": "Optional CSS selector to click before reading/screenshotting" },
                        "fill": { "type": "object", "properties": { "selector": { "type": "string" }, "value": { "type": "string" } }, "description": "Optional input field to fill before clicking/reading" },
                        "script": { "type": "string", "description": "JavaScript expression to evaluate when `op='eval_js'`" },
                        "width": { "type": "integer", "description": "Viewport width (default 1280)" },
                        "height": { "type": "integer", "description": "Viewport height (default 900)" },
                        "wait_ms": { "type": "integer", "description": "Wait time after navigation in ms (default 2500)" }
                    },
                    "required": ["url"]
                }
            }
        }
    ]);
    if let Some(arr) = val.as_array_mut() {
        for mcp in discover_configured_mcp_tools() {
            arr.push(json!({
                "type": "function",
                "function": {
                    "name": mcp.qualified_name,
                    "description": mcp.description,
                    "parameters": mcp.input_schema
                }
            }));
        }
    }
    val
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
    let want_ast = lower.contains("ast")
        || lower.contains("codemod")
        || lower.contains("refactor")
        || lower.contains("ast-grep")
        || lower.contains("ast_grep");
    let want_eval = lower.contains("eval")
        || lower.contains("python")
        || lower.contains("hesapla")
        || lower.contains("analiz")
        || lower.contains("csv")
        || lower.contains("repl");
    let want_browser = lower.contains("browser")
        || lower.contains("tarayıcı")
        || lower.contains("screenshot")
        || lower.contains("ekran görüntüsü")
        || lower.contains("localhost:")
        || lower.contains("127.0.0.1:");
    if let Value::Array(items) = base {
        let filtered: Vec<Value> = items
            .into_iter()
            .filter_map(|item| {
                let name = item
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let keep = match name {
                    "read" | "edit" | "write" | "bash" | "search" | "glob" | "web" | "memory" => {
                        true
                    }
                    "skill" => want_skill,
                    "subagent" => want_subagent,
                    "lsp" => want_lsp,
                    "ast_grep" => want_ast,
                    "eval" => want_eval,
                    "browser" => want_browser,
                    "ask" => false,
                    other if other.starts_with("mcp__") => want_lsp,
                    _ => true,
                };
                if !keep {
                    return None;
                }
                if name == "edit" {
                    // Small/local models miscount 1-based line spans (`start_line`..`end_line`),
                    // causing tail duplication on repeated edits. Expose only exact anchor
                    // replacement (`path` + `old_text` + `new_text`) so `llama-server` grammar
                    // enforces unique anchors backed by fuzzy self-healing.
                    return Some(json!({
                        "type": "function",
                        "function": {
                            "name": "edit",
                            "description": "Replace exact `old_text` with `new_text` in `path`. Include 2-4 unchanged surrounding lines in `old_text` so the match is unique. Never include line-number prefixes.",
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "path": { "type": "string", "description": "File to edit (`<host>:/path` for a remote host)" },
                                    "old_text": { "type": "string", "description": "Exact existing snippet to replace (include 2-4 unchanged context lines)" },
                                    "new_text": { "type": "string", "description": "Replacement snippet" }
                                },
                                "required": ["path", "old_text", "new_text"]
                            }
                        }
                    }));
                }
                Some(item)
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
                if matches!(name, "edit" | "write" | "bash") {
                    return false;
                }
                if let Some(mcp) = discover_configured_mcp_tools()
                    .iter()
                    .find(|t| t.qualified_name == name)
                {
                    return !mcp.mutating;
                }
                true
            })
            .collect();
        Value::Array(filtered)
    } else {
        all
    }
}
