use super::*;

pub(crate) fn run_single_subagent_worker(
    model_key: &str,
    default_host: &str,
    policy: &PathPolicy,
    task: &str,
    extra_ctx: &str,
    expected: &str,
) -> String {
    let ep = crate::router::resolve_endpoint(model_key);
    let mut sub_ctx = ToolContext::new(
        AccessMode::Safe,
        default_host.to_string(),
        policy.cwd.clone(),
        crate::spavn_engine::AgentPhase::Plan,
    );
    sub_ctx.policy = policy.clone();
    sub_ctx.can_prompt = false;

    let mut sub_tools = tool_definitions_for_phase(crate::spavn_engine::AgentPhase::Plan);
    if let Value::Array(ref mut items) = sub_tools {
        items.retain(|it| {
            it.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                != Some("subagent")
        });
    }

    let mut user_prompt = format!("## Sub-Agent Task\n{}", task.trim());
    if !extra_ctx.trim().is_empty() {
        user_prompt.push_str(&format!("\n\n## Context\n{}", extra_ctx.trim()));
    }
    if !expected.trim().is_empty() {
        user_prompt.push_str(&format!("\n\n## Expected Output\n{}", expected.trim()));
    }

    let mut msgs = vec![
        crate::router::ChatMessage::system(
            "You are a focused, read-only SPAVN Sub-Agent. Use `read` and `search` to investigate the codebase and return a concise, high-signal summary with exact file:line references. Do not speculate.",
        ),
        crate::router::ChatMessage::user(user_prompt),
    ];

    for step in 0..6 {
        if crate::is_interrupted() {
            return "[Subagent cancelled via Esc/Ctrl+C]".to_string();
        }
        crate::hud::set_spinner_label(&format!("🐝 Subagent searching (step {}/6)", step + 1));
        let Ok(res) = crate::router::stream_chat_completion(&ep, &msgs, &sub_tools, |_| {}, |_| {})
        else {
            break;
        };
        let msg = res.message;
        msgs.push(msg.clone());
        if let Some(tcs) = msg.tool_calls {
            for tc in tcs.into_iter().take(4) {
                let tname = tc.function.name.trim().to_string();
                let out = if matches!(
                    tname.as_str(),
                    "read" | "search" | "memory" | "web" | "lsp" | "skill"
                ) {
                    execute_tool(&mut sub_ctx, &tname, &tc.function.arguments)
                } else {
                    "Subagent error: only read-only tools (read, search, memory, web, lsp, skill) are allowed."
                        .to_string()
                };
                msgs.push(crate::router::ChatMessage::tool_result(tc.id, tname, out));
            }
            continue;
        }
        let ans = msg
            .content
            .or(msg.reasoning_content)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !ans.is_empty() {
            return ans;
        }
        break;
    }

    msgs.into_iter()
        .rev()
        .find(|m| m.role == "assistant")
        .and_then(|m| m.content.or(m.reasoning_content))
        .unwrap_or_else(|| "(Sub-agent completed without textual summary)".to_string())
}

/// Runs one JSON-RPC exchange against a stdio MCP server and formats the reply.
///
/// Split out of the `lsp` dispatcher so the transport can be exercised against a throwaway server
/// without going through `load_user_config()`, which would make the test mutate the user's real
/// `~/.spavn/config.json`.
pub(crate) fn call_stdio_mcp(
    server_name: &str,
    srv: &crate::config::McpServerConfig,
    tool_name: &str,
    mcp_args: &Value,
) -> String {
    let Some(cmd_bin) = &srv.command else {
        return format!("MCP server '{}' has no `command`.", server_name);
    };
    let expanded_bin = crate::config::expand_user_path(cmd_bin);
    let expanded_args: Vec<String> = srv
        .args
        .iter()
        .map(|a| {
            if a.starts_with("~/") {
                crate::config::expand_user_path(a).display().to_string()
            } else {
                a.clone()
            }
        })
        .collect();
    let mut cmd = Command::new(&expanded_bin);
    // The child used to inherit the whole environment, so a compromised or mispackaged
    // MCP server could read every API key the user has exported. Only PATH/HOME plus the
    // server's own configured values cross the boundary.
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let expanded_env: Vec<(String, String)> = srv
        .env
        .iter()
        .map(|(k, v)| {
            let value = if v.starts_with("~/") {
                crate::config::expand_user_path(v).display().to_string()
            } else {
                v.clone()
            };
            (k.clone(), value)
        })
        .collect();
    cmd.args(&expanded_args)
        .env_clear()
        .env("PATH", inherited_path)
        .env("HOME", crate::config::home_dir())
        .envs(expanded_env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let Ok(mut child) = cmd.spawn() else {
        return format!(
            "MCP server '{}' failed to spawn `{}`; check the `command` in `mcpServers`",
            server_name,
            expanded_bin.display()
        );
    };
    let init_msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "spavn", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let call_msg = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": if tool_name.is_empty() { "tools/list" } else { "tools/call" },
        "params": if tool_name.is_empty() {
            json!({})
        } else {
            json!({ "name": tool_name, "arguments": mcp_args })
        }
    });
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{}", init_msg);
        let _ = writeln!(
            stdin,
            "{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}"
        );
        let _ = writeln!(stdin, "{}", call_msg);
    }
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stdout);
            let mut last_json = String::new();
            // A single JSON-RPC line has no size bound on the wire; capping it here keeps a
            // chatty or hostile server from forcing a multi-megabyte parse per call.
            const MCP_LINE_CAP: usize = 32 * 1024 * 1024; // 32MB for high-res Computer Use screenshots
            for line in reader.lines().map_while(Result::ok) {
                let trimmed = line.trim();
                if trimmed.len() > MCP_LINE_CAP {
                    let _ = tx.send(format!(
                        "{{\"error\":\"MCP response line exceeded {} bytes\"}}",
                        MCP_LINE_CAP
                    ));
                    return;
                }
                if trimmed.starts_with('{') {
                    last_json = trimmed.to_string();
                    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                        if v.get("id").and_then(|id| id.as_i64()) == Some(2) {
                            let _ = tx.send(trimmed.to_string());
                            return;
                        }
                    }
                }
            }
            if !last_json.is_empty() {
                let _ = tx.send(last_json);
            }
        });
    }
    let received = rx.recv_timeout(std::time::Duration::from_secs(45)).ok();
    let _ = child.kill();
    let _ = child.wait();
    match received {
        Some(json_line) => format_mcp_jsonrpc_response(server_name, tool_name, &json_line),
        None => "(MCP server returned no response within 45s)".to_string(),
    }
}

/// Collapses a sub-agent task into one short, single-line header label.
pub(crate) fn one_line_task(task: &str) -> String {
    let flat = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 72 {
        return flat;
    }
    let mut cut = flat.chars().take(72).collect::<String>();
    cut.push('…');
    cut
}

pub(crate) fn exec_subagent(ctx: &mut ToolContext, args: &Value) -> String {
    let model_override = get_str_alias(args, &["model", "endpoint"]);
    let target_model = if model_override.is_empty() {
        ctx.current_model.clone()
    } else if model_override.eq_ignore_ascii_case("local") {
        let cfg = crate::config::load_user_config();
        cfg.local_discovery_url
            .as_deref()
            .and_then(crate::router::detect_active_local_model)
            .unwrap_or(cfg.default_model)
    } else {
        model_override.to_string()
    };

    if let Some(tasks_arr) = args.get("tasks").and_then(|v| v.as_array()) {
        if !tasks_arr.is_empty() {
            let batch: Vec<(String, String, String)> = tasks_arr
                .iter()
                .take(4)
                .filter_map(|item| {
                    let t = get_str_alias(item, &["task", "prompt", "query", "objective"]);
                    if t.trim().is_empty() {
                        return None;
                    }
                    let c = get_str_alias(item, &["context", "files", "background"]);
                    let e = get_str_alias(item, &["expected_output", "format", "output"]);
                    Some((t.to_string(), c.to_string(), e.to_string()))
                })
                .collect();
            if batch.is_empty() {
                return "Subagent error: missing 'task' parameter in tasks array.".to_string();
            }
            let host = ctx.default_host.clone();
            let policy = ctx.policy.clone();
            let results: Vec<String> = std::thread::scope(|s| {
                let handles: Vec<_> = batch
                    .iter()
                    .enumerate()
                    .map(|(idx, (t, c, e))| {
                        let m = target_model.as_str();
                        let h = host.as_str();
                        let p = &policy;
                        s.spawn(move || {
                            let res = run_single_subagent_worker(m, h, p, t, c, e);
                            // The task text can be paragraphs long; the header only needs
                            // enough of it to tell the sub-agents apart.
                            format!("### Sub-Agent #{} ({})\n{}", idx + 1, one_line_task(t), res)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .enumerate()
                    .map(|(idx, h)| match h.join() {
                        Ok(out) => out,
                        // A panicking worker used to vanish silently, leaving the parent
                        // model one result short with no hint that anything failed.
                        Err(_) => format!(
                            "### Sub-Agent #{} (failed)\n[Subagent panicked; its result is missing. Re-run it alone with `subagent`.]",
                            idx + 1
                        ),
                    })
                    .collect()
            });
            return results.join("\n\n");
        }
    }

    let task = get_str_alias(args, &["task", "prompt", "query", "objective"]);
    if task.trim().is_empty() {
        return "Subagent error: missing 'task' parameter.".to_string();
    }
    let extra_ctx = get_str_alias(args, &["context", "files", "background"]);
    let expected = get_str_alias(args, &["expected_output", "format", "output"]);

    run_single_subagent_worker(
        &target_model,
        &ctx.default_host,
        &ctx.policy,
        task,
        extra_ctx,
        expected,
    )
}

pub(crate) fn format_mcp_jsonrpc_response(
    server_name: &str,
    tool_name: &str,
    raw_json: &str,
) -> String {
    let Ok(v) = serde_json::from_str::<Value>(raw_json) else {
        return raw_json.chars().take(12_000).collect();
    };
    if let Some(err) = v.get("error") {
        return format!("MCP error ({}/{}): {}", server_name, tool_name, err);
    }
    let Some(res) = v.get("result") else {
        return raw_json.chars().take(12_000).collect();
    };
    if let Some(tools_arr) = res.get("tools").and_then(|t| t.as_array()) {
        let mut lines = vec![format!(
            "MCP server `{}` tools ({}):",
            server_name,
            tools_arr.len()
        )];
        for t in tools_arr {
            let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let desc: String = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .chars()
                .take(180)
                .collect();
            lines.push(format!("- `{}`: {}", name, desc));
        }
        return lines.join("\n");
    }
    if let Some(content_arr) = res.get("content").and_then(|c| c.as_array()) {
        let mut out_parts = Vec::new();
        for (idx, item) in content_arr.iter().enumerate() {
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "text" => {
                    if let Some(txt) = item.get("text").and_then(|t| t.as_str()) {
                        out_parts.push(txt.to_string());
                    }
                }
                "image" => {
                    if let Some(b64) = item.get("data").and_then(|d| d.as_str()) {
                        let raw_mime = item
                            .get("mimeType")
                            .and_then(|m| m.as_str())
                            .unwrap_or("image/png");
                        let mime: &'static str = match raw_mime {
                            "image/jpeg" | "image/jpg" => "image/jpeg",
                            "image/webp" => "image/webp",
                            "image/gif" => "image/gif",
                            _ => "image/png",
                        };
                        let approx_bytes = (b64.len() * 3) / 4;
                        let kb = approx_bytes as f64 / 1024.0;
                        let fname = format!("mcp-{}-{}-{}.png", server_name, tool_name, idx);
                        let vpath = format!("/tmp/{}", fname);
                        let drag_img = crate::router::DraggedImage {
                            raw_span: vpath.clone(),
                            path: vpath.clone(),
                            filename: fname,
                            mime,
                            size_bytes: approx_bytes,
                            base64: b64.to_string(),
                        };
                        crate::hud::render_inline_image_preview(&drag_img);
                        crate::router::register_read_image(&vpath, drag_img);
                        out_parts.push(format!(
                            "[SPAVN Vision Image Loaded: {} ({}, {:.1} KB) — multimodal image attached to your visual context]",
                            vpath, mime, kb
                        ));
                    }
                }
                _ => {
                    out_parts.push(item.to_string());
                }
            }
        }
        if !out_parts.is_empty() {
            return cap_mcp_text(&out_parts.join("\n"), 16_000);
        }
    }
    cap_mcp_text(&res.to_string(), 12_000)
}

/// Bounds an MCP payload and says so, instead of silently losing the tail.
///
/// The stdio path has no transport-level body limit (the HTTP path inherits ureq's 10 MB), so a
/// chatty server could pour megabytes into the context window in one call.
pub(crate) fn cap_mcp_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!(
        "{}\n… (MCP response truncated: first {} of {} chars)",
        kept,
        text.chars().count(),
        max_chars
    )
}

/// Verbs an MCP tool may be assumed not to mutate on.
///
/// The gate used to be a deny-list (`send`, `write`, `delete`, …), which missed `commit`,
/// `move_file`, `push`, `checkout`, `evaluate` and `merge_pull_request`. Treating everything that
/// is not a known read verb as mutating fails closed instead. A server the user knows is
/// side-effect free can opt out with `"readOnly": true` in `mcpServers`.
pub(crate) const MCP_READ_VERBS: &[&str] = &[
    "capture_display",
    "describe",
    "fetch",
    "find",
    "get",
    "inspect",
    "list",
    "read",
    "resolve",
    "search",
    "terms",
    "think",
];

/// True when `server:tool` may have side effects and so needs the phase/approval gate.
pub(crate) fn is_mutating_mcp_tool(
    server_name: &str,
    tool_name: &str,
    server_is_read_only: bool,
) -> bool {
    if tool_name.trim().is_empty() || server_is_read_only {
        return false;
    }
    // A desktop-driving server mutates everything it touches, whatever its verbs are spelled
    // like; `readOnly: true` in the config is the only way to opt one out.
    if server_name.eq_ignore_ascii_case("sovereign") {
        return true;
    }
    let name = tool_name.to_ascii_lowercase();
    let name = name.strip_prefix("sovereign_").unwrap_or(&name);
    !MCP_READ_VERBS.iter().any(|verb| {
        name == *verb
            || name.starts_with(&format!("{}_", verb))
            || name.starts_with(&format!("{}-", verb))
    })
}

pub(crate) fn exec_lsp_and_mcp(ctx: &mut ToolContext, args: &Value) -> String {
    let op = get_str_alias(args, &["op", "action", "mode"]).to_ascii_lowercase();
    if op == "mcp" || !get_str_alias(args, &["server", "mcp_server"]).is_empty() {
        let server_name = get_str_alias(args, &["server", "mcp_server"]);
        let raw_tool_name = get_str_alias(args, &["tool", "name", "method"]);
        let tool_name = if server_name == "sovereign"
            && !raw_tool_name.is_empty()
            && !raw_tool_name.starts_with("sovereign_")
        {
            format!("sovereign_{}", raw_tool_name)
        } else {
            raw_tool_name.to_string()
        };
        let mcp_args = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let server_is_read_only = load_user_config()
            .mcp_servers
            .get(server_name)
            .map(|s| s.read_only)
            .unwrap_or(false);
        if is_mutating_mcp_tool(server_name, &tool_name, server_is_read_only) {
            if ctx.phase == crate::spavn_engine::AgentPhase::Plan {
                return format!(
                    "BLOCKED in PLAN phase: mutating MCP tool `{}:{}` is not allowed in read-only planning mode.",
                    server_name, tool_name
                );
            }
            if ctx.access == AccessMode::Safe {
                let preview = format!("{} {}", tool_name, mcp_args);
                if !prompt_user_approval(ctx, &format!("mcp:{}", server_name), &preview) {
                    return format!(
                        "User rejected MCP call: {}:{}",
                        server_name, tool_name
                    );
                }
            }
        }
        let cfg = crate::config::load_user_config();
        let Some(srv) = cfg.mcp_servers.get(server_name) else {
            let mut available: Vec<String> = cfg.mcp_servers.keys().cloned().collect();
            available.sort();
            return format!(
                "MCP server '{}' not found in ~/.spavn/config.json `mcpServers`. Configured servers: {:?}",
                server_name, available
            );
        };
        if let Some(url) = &srv.url {
            let req_body = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": if tool_name.is_empty() { "tools/list" } else { "tools/call" },
                "params": if tool_name.is_empty() {
                    json!({})
                } else {
                    json!({ "name": tool_name, "arguments": mcp_args })
                }
            });
            return match crate::http::post_json(
                url,
                &req_body.to_string(),
                &[("Accept", "application/json, text/event-stream")],
                30,
            ) {
                Ok((_, body)) => format_mcp_jsonrpc_response(server_name, &tool_name, &body),
                Err(e) => format!("MCP HTTP error ({}): {}", server_name, e),
            };
        }
        if srv.command.is_some() {
            return call_stdio_mcp(server_name, srv, &tool_name, &mcp_args);
        }
        return format!(
            "MCP server '{}' has neither `command` nor `url`.",
            server_name
        );
    }

    let symbol = get_str_alias(args, &["symbol", "name", "query", "identifier"]);
    let raw_path = {
        let p = get_str_alias(args, &["path", "file", "dir"]);
        if p.is_empty() {
            "."
        } else {
            p
        }
    };
    if op == "symbols" || (symbol.is_empty() && !raw_path.is_empty()) {
        let target = ctx.resolve_local_path(raw_path);
        if target.is_file() {
            if let Ok(content) = fs::read_to_string(&target) {
                let mut syms = Vec::new();
                for (idx, line) in content.lines().enumerate() {
                    if is_structural_outline_line(line) {
                        syms.push(format!("{:>4}: {}", idx + 1, line.trim()));
                    }
                }
                if syms.is_empty() {
                    return "(No structural symbols found in file)".to_string();
                }
                return syms.join("\n");
            }
        }
        return build_live_project_signatures(&target.to_string_lossy());
    }

    if symbol.is_empty() {
        return "LSP error: missing 'symbol' parameter.".to_string();
    }
    let is_def = op == "definition" || op == "def";
    let pattern = if is_def {
        format!(
            r"(fn|struct|enum|trait|type|const|let|static|def|class|function|interface|func)\s+{}\b",
            symbol
        )
    } else {
        format!(r"\b{}\b", symbol)
    };
    exec_search(
        ctx,
        &json!({
            "pattern": pattern,
            "path": raw_path,
            "context": if is_def { 6 } else { 0 }
        }),
    )
}
