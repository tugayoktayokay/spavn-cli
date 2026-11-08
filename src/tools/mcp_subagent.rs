use super::*;

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

pub(crate) fn format_mcp_jsonrpc_response(
    server_name: &str,
    tool_name: &str,
    raw_json: &str,
) -> String {
    let trimmed = raw_json.trim();
    let v: Value = if trimmed.starts_with('{') {
        match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return raw_json.chars().take(12_000).collect(),
        }
    } else if let Some(sse_v) = trimmed
        .lines()
        .filter_map(|l| l.trim().strip_prefix("data:"))
        .find_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
    {
        sse_v
    } else {
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
        max_chars,
        text.chars().count()
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
                    return format!("User rejected MCP call: {}:{}", server_name, tool_name);
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
            let mut headers: Vec<(&str, &str)> =
                vec![("Accept", "application/json, text/event-stream")];
            for (k, v) in &srv.headers {
                headers.push((k.as_str(), v.as_str()));
            }
            return match crate::http::post_json(url, &req_body.to_string(), &headers, 30) {
                Ok((status, body)) if (200..300).contains(&status) => {
                    format_mcp_jsonrpc_response(server_name, &tool_name, &body)
                }
                Ok((status, body)) => {
                    if status == 401
                        && body.contains("https://www.lazyweb.com/mcp/public")
                        && srv.headers.is_empty()
                    {
                        if let Ok((200..=299, pub_body)) = crate::http::post_json(
                            "https://www.lazyweb.com/mcp/public",
                            &req_body.to_string(),
                            &headers,
                            30,
                        ) {
                            return format_mcp_jsonrpc_response(server_name, &tool_name, &pub_body);
                        }
                    }
                    format!(
                        "MCP HTTP {} ({}): {}",
                        status,
                        server_name,
                        cap_mcp_text(body.trim(), 600)
                    )
                }
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

    crate::tools::lsp::exec_lsp(ctx, args)
}
