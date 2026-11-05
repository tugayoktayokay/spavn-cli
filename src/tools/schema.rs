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
        "Symbol navigation (`op=definition|references|symbols`) and generic MCP client (`op='mcp'`). {}",
        mcp_hint
    );
    json!([
        {
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file or directory. Supports several disjoint ranges in one call via `path:N-M,K-L` — prefer that over repeated reads or `bash sed`.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path (optional `<host>:` prefix); may embed ranges, e.g. `src/x.rs:1-12,640-665`" },
                        "start_line": { "type": "integer", "description": "Start line (1-based)" },
                        "end_line": { "type": "integer", "description": "End line (1-based, inclusive)" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit",
                "description": "Replace `old_text` with `new_text` in a file, or apply several non-overlapping edits atomically via `edits`.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File to edit" },
                        "old_text": { "type": "string", "description": "Exact existing snippet (single edit)" },
                        "new_text": { "type": "string", "description": "Replacement snippet (single edit)" },
                        "edits": {
                            "type": "array",
                            "description": "Batch of `{old_text, new_text}` applied atomically to the same file",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old_text": { "type": "string" },
                                    "new_text": { "type": "string" }
                                },
                                "required": ["old_text", "new_text"]
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
                "description": "Run a shell command locally or on a configured remote host.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Command" },
                        "host": { "type": "string", "description": "Host alias (default: local)" },
                        "cwd": { "type": "string", "description": "Working directory" },
                        "timeout_sec": { "type": "integer", "description": "Timeout seconds (default 120)" },
                        "background": { "type": "boolean", "description": "Run detached; returns PID + log path" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "search",
                "description": "Regex content search (ripgrep).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex pattern" },
                        "path": { "type": "string", "description": "Path to search (default `.`)" }
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
                "description": "Web search (`op='search'`) or fetch a URL as Markdown (`op='fetch'`).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["search", "fetch"], "description": "search | fetch" },
                        "query": { "type": "string", "description": "Query, or URL" },
                        "url": { "type": "string", "description": "URL for op='fetch'" }
                    },
                    "required": ["op"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "subagent",
                "description": "Delegate isolated read-only investigations to sub-agents with their own context: one `task`, or several via `tasks` in parallel.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "The sub-question" },
                        "context": { "type": "string", "description": "File paths or background" },
                        "expected_output": { "type": "string", "description": "Findings to return" },
                        "model": { "type": "string", "description": "Model override, e.g. local | flash" },
                        "tasks": {
                            "type": "array",
                            "description": "Up to 4 sub-tasks run concurrently",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "task": { "type": "string" },
                                    "context": { "type": "string" },
                                    "expected_output": { "type": "string" }
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
                        "op": { "type": "string", "enum": ["definition", "references", "symbols", "mcp"], "description": "Operation" },
                        "symbol": { "type": "string", "description": "Symbol for definition/references" },
                        "path": { "type": "string", "description": "Path for symbols" },
                        "server": { "type": "string", "description": "MCP server name" },
                        "tool": { "type": "string", "description": "MCP tool (omit to list)" },
                        "arguments": { "type": "object", "description": "JSON arguments" }
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
/// Cloud frontier models get the full tool inventory. Local models (`is_local == true`) suffer
/// prefill latency and schema confusion when handed 11 tools on every turn, so they get the 6
/// core file/shell tools by default plus domain tools (`memory`, `web`, `skill`, `subagent`, `lsp`)
/// only when the prompt or active syntax (`@skill`, URLs, MCP/LSP keywords) calls for them.
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
    let want_memory = lower.contains("hatırla")
        || lower.contains("hafıza")
        || lower.contains("memory")
        || lower.contains("remember")
        || lower.contains("recall")
        || lower.contains("unutma");
    let want_web = lower.contains("http://")
        || lower.contains("https://")
        || lower.contains("web")
        || lower.contains("fetch")
        || lower.contains("internette");
    let want_skill = lower.contains('@') || lower.contains("skill") || lower.contains("yetenek");
    let want_subagent =
        lower.contains("subagent") || lower.contains("alt ajan") || lower.contains("paralel");
    let want_lsp = lower.contains("lsp")
        || lower.contains("mcp")
        || lower.contains("definition")
        || lower.contains("references");
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
                    "read" | "edit" | "write" | "bash" | "search" | "glob" => true,
                    "memory" => want_memory,
                    "web" => want_web,
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
