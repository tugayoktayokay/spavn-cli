use super::*;

pub(crate) fn strip_html_to_clean_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(12_000));
    let lower = html.to_lowercase();
    let bytes = html.as_bytes();
    let lower_bytes = lower.as_bytes();
    let mut i = 0usize;
    let mut in_tag = false;
    let mut last_was_space = false;

    while i < bytes.len() {
        if !in_tag && i + 7 < lower_bytes.len() {
            if lower_bytes[i..].starts_with(b"<script") {
                if let Some(end) = lower[i..].find("</script>") {
                    i += end + 9;
                    continue;
                }
            }
            if lower_bytes[i..].starts_with(b"<style") {
                if let Some(end) = lower[i..].find("</style>") {
                    i += end + 8;
                    continue;
                }
            }
            if lower_bytes[i..].starts_with(b"<svg") {
                if let Some(end) = lower[i..].find("</svg>") {
                    i += end + 6;
                    continue;
                }
            }
        }
        let b = bytes[i];
        if b == b'<' {
            if lower_bytes[i..].starts_with(b"<br")
                || lower_bytes[i..].starts_with(b"<p")
                || lower_bytes[i..].starts_with(b"</p")
                || lower_bytes[i..].starts_with(b"<div")
                || lower_bytes[i..].starts_with(b"<li")
                || lower_bytes[i..].starts_with(b"<h1")
                || lower_bytes[i..].starts_with(b"<h2")
                || lower_bytes[i..].starts_with(b"<h3")
                || lower_bytes[i..].starts_with(b"<tr")
            {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                last_was_space = true;
            }
            in_tag = true;
            i += 1;
            continue;
        }
        if b == b'>' {
            in_tag = false;
            i += 1;
            continue;
        }
        if !in_tag {
            let ch = html[i..].chars().next().unwrap_or(' ');
            if ch == '\n' || ch == '\r' {
                if !out.ends_with("\n\n") {
                    out.push('\n');
                }
                last_was_space = true;
            } else if ch.is_whitespace() {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            } else {
                out.push(ch);
                last_was_space = false;
            }
            i += ch.len_utf8();
            if out.len() >= 10_000 {
                out.push_str("\n...[SPAVN Web: truncated to 10,000 chars]...");
                break;
            }
        } else {
            i += 1;
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .trim()
        .to_string()
}

/// Endpoint the `web` tool posts its MCP calls to.
///
/// There is no built-in host. Shipping one would send queries to a third party nobody chose, so
/// the endpoint comes only from `webSearchUrl` or `SPAVN_WEB_SEARCH_URL`; unset or empty leaves
/// the HTTP backend off and the caller reports how to switch it on.
pub(crate) fn web_search_endpoint() -> Option<String> {
    let configured = crate::config::load_user_config().web_search_url;
    let raw = configured.or_else(|| std::env::var("SPAVN_WEB_SEARCH_URL").ok())?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Resolves the streaming-MCP backend for the `web` tool.
///
/// Returns `Ok(None)` when search is deliberately switched off. An unusable configuration is an
/// error rather than a silent fallback: falling back would send the query to the hosted default,
/// which is exactly the host an operator who configured a server wanted to avoid.
pub(crate) fn web_search_server_tool() -> Result<Option<(String, String)>, String> {
    let cfg = crate::config::load_user_config();
    resolve_web_search_target(
        cfg.web_search_server.as_deref(),
        cfg.web_search_tool.as_deref(),
        |name| cfg.mcp_servers.contains_key(name),
    )
}

/// Pure core of [`web_search_server_tool`], split out so the validation rules are testable
/// without touching the global config or process environment.
pub(crate) fn resolve_web_search_target(
    server: Option<&str>,
    tool: Option<&str>,
    server_exists: impl Fn(&str) -> bool,
) -> Result<Option<(String, String)>, String> {
    let server = server.map(str::trim).unwrap_or_default();
    if server.is_empty() {
        return Ok(None);
    }
    if !server_exists(server) {
        return Err(format!(
            "Web search error: `webSearchServer` names `{}`, which is not in `mcpServers`.",
            server
        ));
    }
    let tool = tool.map(str::trim).unwrap_or_default();
    if tool.is_empty() {
        return Err(format!(
            "Web search error: `webSearchServer` is `{}` but `webSearchTool` is unset.",
            server
        ));
    }
    Ok(Some((server.to_string(), tool.to_string())))
}

/// Shown when `web` search has nowhere to go. Since no endpoint is baked in, this doubles as the
/// setup note instead of silently reaching for some vendor host.
pub(crate) const NO_SEARCH_BACKEND: &str = "Web search error: no search backend configured. Point \
`webSearchServer` + `webSearchTool` at an `mcpServers` entry, or `webSearchUrl` + \
`webSearchHttpTool` at an HTTP MCP endpoint.";

/// Tool name the HTTP backend serves for one operation.
///
/// Nothing is assumed about the endpoint's catalogue, so an unset name means "this operation is
/// not wired up" and the caller skips the HTTP path instead of calling a guessed tool.
pub(crate) fn web_http_tool(configured: Option<&str>) -> Option<String> {
    configured
        .map(str::trim)
        .filter(|tool| !tool.is_empty())
        .map(str::to_string)
}

/// Posts one MCP call to the configured HTTP search backend.
///
/// Host, tool name, API-key header and key env var all come from config, so a self-hosted bridge
/// substitutes the hosted default without a rebuild and without editing source.
pub(crate) fn call_web_http(tool_name: &str, arguments: Value) -> Option<String> {
    let cfg = crate::config::load_user_config();
    let endpoint = web_search_endpoint()?;
    let header = cfg
        .web_search_api_key_header
        .as_deref()
        .map(str::trim)
        .filter(|header| !header.is_empty())
        .unwrap_or("x-api-key");
    // No key is sent unless the operator names the env var: a guessed name would leak an
    // unrelated secret to whatever host the endpoint points at.
    let api_key = cfg
        .web_search_api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|env_name| !env_name.is_empty())
        .and_then(|env_name| std::env::var(env_name).ok())
        .unwrap_or_default();
    call_http_search_mcp(&endpoint, tool_name, arguments, &api_key, header)
}

pub(crate) fn call_http_search_mcp(
    endpoint: &str,
    tool_name: &str,
    arguments: Value,
    api_key: &str,
    api_key_header: &str,
) -> Option<String> {
    // Keys go in a header, never in the URL where logs and proxies keep them.
    let mut headers = vec![("Accept", "application/json, text/event-stream")];
    if !api_key.trim().is_empty() {
        headers.push((api_key_header, api_key.trim()));
    }
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments
        }
    });
    let (status, raw) =
        crate::http::post_json(endpoint, &payload.to_string(), &headers, 15).ok()?;
    if !(200..300).contains(&status) {
        return None;
    }
    for line in raw.lines() {
        let trimmed = line.trim();
        let json_str = if let Some(rest) = trimmed.strip_prefix("data:") {
            rest.trim()
        } else if trimmed.starts_with('{') {
            trimmed
        } else {
            continue;
        };
        if let Ok(val) = serde_json::from_str::<Value>(json_str) {
            if let Some(arr) = val
                .get("result")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.as_array())
            {
                for item in arr {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        if !text.trim().is_empty() {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

pub(crate) fn exec_web(ctx: &mut ToolContext, args: &Value) -> String {
    let op_raw = get_str_alias(args, &["op", "action", "mode"]);
    let url = get_str_alias(args, &["url", "href", "link"]);
    // `url` is deliberately absent: with it here, `web search {"query":"https://…"}` silently
    // became a fetch — an unrequested outbound GET the model never asked for.
    let query = get_str_alias(args, &["query", "q", "search"]);
    let is_fetch = op_raw.eq_ignore_ascii_case("fetch")
        || op_raw.eq_ignore_ascii_case("read")
        || (!url.is_empty() && (url.starts_with("http://") || url.starts_with("https://")));

    if is_fetch {
        let target_url = if !url.is_empty() { url } else { query };
        if target_url.is_empty() {
            return "Web fetch error: missing 'url' parameter.".to_string();
        }
        let allow_local = args
            .get("allow_local")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(blocked) = check_egress_gate(ctx, target_url, allow_local) {
            return blocked;
        }
        if !crate::guard::egress_is_blocked(target_url) {
            match web_search_server_tool() {
                Ok(Some((server, _))) => {
                    let cfg = crate::config::load_user_config();
                    if let Some(srv) = cfg.mcp_servers.get(&server) {
                        let tool = cfg
                            .web_fetch_tool
                            .as_deref()
                            .map(str::trim)
                            .filter(|t| !t.is_empty())
                            .unwrap_or_default();
                        if !tool.is_empty() {
                            let out = call_stdio_mcp(
                                &server,
                                srv,
                                tool,
                                &json!({ "urls": [target_url], "maxCharacters": 9000 }),
                            );
                            if !out.trim().is_empty() {
                                return out;
                            }
                        }
                    }
                }
                Ok(None) => {
                    let tool = web_http_tool(
                        crate::config::load_user_config()
                            .web_fetch_http_tool
                            .as_deref(),
                    );
                    if let Some(tool) = tool {
                        if let Some(markdown) = call_web_http(
                            &tool,
                            json!({
                                "urls": [target_url],
                                "maxCharacters": 9000
                            }),
                        ) {
                            return markdown;
                        }
                    }
                }
                Err(message) => return message,
            }
        }
        let fetched = crate::http::get_text_public(
            target_url,
            &[
                (
                    "User-Agent",
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/537.36",
                ),
                ("Accept", "text/markdown, text/plain;q=0.9, text/html;q=0.8"),
            ],
            12,
        );
        return match fetched {
            Ok((status, body)) if (200..300).contains(&status) => {
                if body.contains("<html") || body.contains("<body") || body.contains("<!DOCTYPE") {
                    strip_html_to_clean_text(&body)
                } else {
                    body.chars().take(10_000).collect()
                }
            }
            Ok((status, body)) => format!(
                "Web fetch failed: HTTP {} — {}",
                status,
                body.trim().chars().take(300).collect::<String>()
            ),
            Err(e) => format!("Web fetch error: {}", e),
        };
    }

    if query.is_empty() {
        return "Web search error: missing 'query' parameter.".to_string();
    }
    let search_args = json!({
        "query": query,
        "numResults": 6
    });
    let cfg = crate::config::load_user_config();
    match web_search_server_tool() {
        Ok(Some((server, tool))) => {
            let Some(srv) = cfg.mcp_servers.get(&server) else {
                return format!("Web search error: MCP server `{}` disappeared.", server);
            };
            return call_stdio_mcp(&server, srv, &tool, &search_args);
        }
        Ok(None) => {}
        Err(message) => return message,
    }
    let Some(tool) = web_http_tool(cfg.web_search_http_tool.as_deref()) else {
        return NO_SEARCH_BACKEND.to_string();
    };
    // A tool name without an endpoint has nothing to call; say so instead of reporting an empty
    // result set the caller would read as "nothing matched".
    if web_search_endpoint().is_none() {
        return NO_SEARCH_BACKEND.to_string();
    }
    if let Some(results) = call_web_http(&tool, search_args) {
        return results;
    }
    "Web search returned no results.".to_string()
}
