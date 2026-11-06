use super::*;

pub(crate) fn merge_consecutive_same_role(mut blocks: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for b in blocks.drain(..) {
        if let Some(last) = out.last_mut() {
            if last.get("role") == b.get("role") {
                let merged = b
                    .get("content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                if let Some(l) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
                    l.extend(merged);
                    continue;
                }
            }
        }
        out.push(b);
    }
    out
}

pub(crate) fn split_and_convert_messages(messages: &[ChatMessage]) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut blocks: Vec<Value> = Vec::new();
    for m in messages {
        if m.role == "system" {
            if let Some(c) = &m.content {
                if !system.is_empty() {
                    system.push('\n');
                }
                system.push_str(c);
            }
        }
    }
    let sanitized = sanitize_messages_for_openai_jinja(messages);
    let last_user_idx = sanitized.iter().rposition(|m| m.role == "user");
    for (idx, m) in sanitized.iter().enumerate() {
        match m.role.as_str() {
            "system" => {}
            "assistant" => {
                let mut content: Vec<Value> = Vec::new();
                if let Some(c) = &m.content {
                    if !c.trim().is_empty() {
                        content.push(json!({ "type": "text", "text": c }));
                    }
                }
                if let Some(calls) = &m.tool_calls {
                    for tc in calls {
                        let input: Value =
                            serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
                        content.push(json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.function.name,
                            "input": input,
                        }));
                    }
                }
                if !content.is_empty() {
                    blocks.push(json!({ "role": "assistant", "content": content }));
                }
            }
            "tool" => {
                let tool_use_id = m.tool_call_id.clone().unwrap_or_default();
                let text = m.content.clone().unwrap_or_default();
                let mut result_blocks = vec![json!({ "type": "text", "text": text })];
                if text.starts_with("[SPAVN Vision Image Loaded:") {
                    for (mime, b64) in extract_dragged_images(&text) {
                        result_blocks.push(json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": mime,
                                "data": b64,
                            }
                        }));
                    }
                }
                let content_val = if result_blocks.len() == 1 {
                    json!(text)
                } else {
                    Value::Array(result_blocks)
                };
                let tool_res_block = json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content_val,
                });
                let appended = if let Some(last) = blocks.last_mut() {
                    if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                        if let Some(arr) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
                            arr.push(tool_res_block.clone());
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !appended {
                    blocks.push(json!({
                        "role": "user",
                        "content": [tool_res_block],
                    }));
                }
            }
            _ => {
                let text = m.content.clone().unwrap_or_default();
                if !text.is_empty() {
                    let mut parts = vec![json!({ "type": "text", "text": text })];
                    if Some(idx) == last_user_idx && sanitized.len().saturating_sub(idx) <= 10 {
                        for (mime, b64) in extract_dragged_images(&text) {
                            parts.push(json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": mime,
                                    "data": b64,
                                }
                            }));
                        }
                    }
                    blocks.push(json!({
                        "role": "user",
                        "content": parts,
                    }));
                }
            }
        }
    }
    (system, merge_consecutive_same_role(blocks))
}

pub(crate) fn build_anthropic_payload(
    endpoint: &ModelEndpoint,
    messages: &[ChatMessage],
    tools: &Value,
) -> Result<Value, String> {
    let (system, mut converted) = split_and_convert_messages(messages);
    // Mark the conversation prefix anchor (second-to-last message block) as an ephemeral
    // prompt-cache breakpoint so multi-step tool loops hit >90% cache read on Anthropic/proxies.
    if converted.len() >= 2 {
        let anchor_idx = converted.len() - 2;
        if let Some(content_arr) = converted[anchor_idx]
            .get_mut("content")
            .and_then(|c| c.as_array_mut())
        {
            if let Some(last_block) = content_arr.last_mut() {
                if last_block.is_object() {
                    last_block["cache_control"] = json!({ "type": "ephemeral" });
                }
            }
        }
    }
    let mut payload = json!({
        "model": endpoint.model,
        "max_tokens": 16384,
        "messages": converted,
        "stream": true,
    });
    if !system.trim().is_empty() {
        payload["system"] = json!([
            {
                "type": "text",
                "text": system,
                "cache_control": { "type": "ephemeral" }
            }
        ]);
    }
    if let Some(arr) = tools.as_array() {
        if !arr.is_empty() {
            let mut converted_tools = Vec::new();
            for t in arr {
                if let Some(f) = t.get("function") {
                    let mut out = json!({
                        "name": f.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "input_schema": f.get("parameters").cloned().unwrap_or(json!({})),
                    });
                    if let Some(d) = f.get("description").and_then(|v| v.as_str()) {
                        if !d.is_empty() {
                            out["description"] = json!(d);
                        }
                    }
                    converted_tools.push(out);
                }
            }
            if let Some(last_tool) = converted_tools.last_mut() {
                last_tool["cache_control"] = json!({ "type": "ephemeral" });
            }
            if !converted_tools.is_empty() {
                payload["tools"] = json!(converted_tools);
            }
        }
    }
    Ok(payload)
}

pub fn stream_anthropic<FThink, FContent>(
    endpoint: &ModelEndpoint,
    messages: &[ChatMessage],
    tools: &Value,
    mut on_thinking: FThink,
    mut on_content: FContent,
) -> Result<CompletionResult, String>
where
    FThink: FnMut(&str),
    FContent: FnMut(&str),
{
    let start = Instant::now();
    let mut ttft_ms: u128 = 0;

    let payload = build_anthropic_payload(endpoint, messages, tools)?;
    let body_str = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let url = format!("{}/messages", endpoint.base_url.trim_end_matches('/'));

    enum StreamEvent {
        Line(String),
        Failed(String),
        End,
    }
    let (tx, rx) = std::sync::mpsc::channel::<StreamEvent>();
    let api_key = endpoint.api_key.clone();
    let request_url = url.clone();
    std::thread::spawn(move || {
        let agent = crate::http::agent_stream();
        let response = agent
            .post(&request_url)
            .header("Content-Type", "application/json")
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "prompt-caching-2024-07-31")
            .send(body_str.as_str());
        let mut response = match response {
            Ok(response) => response,
            Err(err) => {
                let _ = tx.send(StreamEvent::Failed(format!(
                    "Upstream request failed: {}",
                    err
                )));
                return;
            }
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let retry_after = retry_after_marker(&response);
            let body = response.body_mut().read_to_string().unwrap_or_default();
            let snippet: String = body.trim().chars().take(400).collect();
            let _ = tx.send(StreamEvent::Failed(format!(
                "Anthropic HTTP {}: {}{}",
                status, snippet, retry_after
            )));
            return;
        }
        let reader = BufReader::new(response.body_mut().as_reader());
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if tx.send(StreamEvent::Line(line)).is_err() {
                        return;
                    }
                }
                Err(err) => {
                    let _ = tx.send(StreamEvent::Failed(format!("Stream read error: {}", err)));
                    return;
                }
            }
        }
        let _ = tx.send(StreamEvent::End);
    });

    let mut content = String::new();
    let mut reasoning = String::new();
    let mut usage = TokenUsage::default();
    let mut stop_reason: Option<String> = None;
    let mut tool_blocks: BTreeMap<usize, (String, String, String)> = BTreeMap::new();
    let mut seen_data = false;

    loop {
        if crate::is_interrupted() {
            return Err("İşlem kullanıcı tarafından (Esc / Ctrl+C) iptal edildi.".to_string());
        }
        let line = match rx.recv_timeout(std::time::Duration::from_millis(150)) {
            Ok(StreamEvent::Line(line)) => line,
            Ok(StreamEvent::Failed(err)) => return Err(err),
            Ok(StreamEvent::End) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with("data:") {
            continue;
        }
        let data_str = trimmed.trim_start_matches("data:").trim();
        if data_str == "[DONE]" {
            break;
        }
        let v: Value = match serde_json::from_str(data_str) {
            Ok(v) => v,
            Err(_) => continue,
        };
        seen_data = true;
        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match typ {
            "message_start" => {
                usage.prompt_tokens = v
                    .pointer("/message/usage/input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(usage.prompt_tokens);
            }
            "content_block_start" => {
                let idx = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                if v.pointer("/content_block/type").and_then(|t| t.as_str()) == Some("tool_use") {
                    let id = v
                        .pointer("/content_block/id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = v
                        .pointer("/content_block/name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    crate::hud::notify_tool_call_streaming(&name, 0);
                    tool_blocks.insert(idx, (id, name, String::new()));
                    if ttft_ms == 0 {
                        ttft_ms = start.elapsed().as_millis();
                    }
                }
            }
            "content_block_delta" => {
                let idx = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let dtype = v
                    .pointer("/delta/type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match dtype {
                    "text_delta" => {
                        if let Some(t) = v.pointer("/delta/text").and_then(|x| x.as_str()) {
                            if ttft_ms == 0 {
                                ttft_ms = start.elapsed().as_millis();
                            }
                            content.push_str(t);
                            on_content(t);
                        }
                    }
                    "input_json_delta" => {
                        if let Some(p) = v.pointer("/delta/partial_json").and_then(|x| x.as_str()) {
                            if let Some(slot) = tool_blocks.get_mut(&idx) {
                                slot.2.push_str(p);
                                crate::hud::notify_tool_call_streaming_args(
                                    &slot.1,
                                    &slot.2,
                                    slot.2.len(),
                                );
                            }
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = v.pointer("/delta/thinking").and_then(|x| x.as_str()) {
                            if ttft_ms == 0 {
                                ttft_ms = start.elapsed().as_millis();
                            }
                            reasoning.push_str(t);
                            on_thinking(t);
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(sr) = v.pointer("/delta/stop_reason").and_then(|x| x.as_str()) {
                    stop_reason = Some(sr.to_string());
                }
                usage.completion_tokens = v
                    .pointer("/usage/output_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(usage.completion_tokens);
            }
            "error" => {
                let msg = v
                    .pointer("/error/message")
                    .and_then(|x| x.as_str())
                    .unwrap_or("unknown Anthropic error");
                return Err(format!("Anthropic API error: {}", msg));
            }
            "message_stop" => break,
            _ => {}
        }
    }

    if !seen_data {
        return Err("Anthropic stream produced no data.".to_string());
    }

    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for (_idx, (id, name, args)) in tool_blocks {
        tool_calls.push(ToolCall {
            id,
            call_type: "function".to_string(),
            function: ToolFunction {
                name,
                arguments: if args.is_empty() {
                    "{}".to_string()
                } else {
                    args
                },
            },
            thought_signature: None,
        });
    }
    if tool_calls.is_empty() && !content.is_empty() {
        let recovered = recover_inline_tool_calls(&content);
        if !recovered.is_empty() {
            tool_calls = recovered;
        }
    }

    let finish_reason = stop_reason.as_deref().map(|sr| match sr {
        "end_turn" | "stop_sequence" => "stop".to_string(),
        "tool_use" => "tool_calls".to_string(),
        "max_tokens" => "length".to_string(),
        other => other.to_string(),
    });

    usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;

    let message = ChatMessage {
        role: "assistant".to_string(),
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        name: None,
        thought_signature: None,
        reasoning_content: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
    };

    Ok(CompletionResult {
        message,
        usage,
        elapsed_ms: start.elapsed().as_millis(),
        ttft_ms,
        finish_reason,
    })
}

#[cfg(test)]
mod anthropic_tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            thought_signature: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn system_is_extracted_and_user_kept() {
        let msgs = vec![msg("system", "You are X"), msg("user", "hi")];
        let (system, blocks) = split_and_convert_messages(&msgs);
        assert_eq!(system, "You are X");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["role"], "user");
        assert_eq!(blocks[0]["content"][0]["text"], "hi");
    }

    #[test]
    fn tool_results_become_user_blocks() {
        let mut t = msg("tool", "file content");
        t.tool_call_id = Some("call_1".to_string());
        let a = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: ToolFunction {
                    name: "read".to_string(),
                    arguments: "{\"path\":\"a\"}".to_string(),
                },
                thought_signature: None,
            }]),
            tool_call_id: None,
            name: None,
            thought_signature: None,
            reasoning_content: None,
        };
        let msgs = vec![msg("user", "read a"), a, t, msg("user", "now summarize")];
        let (_system, blocks) = split_and_convert_messages(&msgs);
        // user, assistant(tool_use), user(tool_result), user(text) -> the two
        // trailing user messages must merge into one.
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[2]["role"], "user");
        assert_eq!(blocks[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(blocks[2]["content"][0]["type"], "tool_result");
        assert_eq!(blocks[2]["content"][1]["type"], "text");
    }

    #[test]
    fn assistant_tool_use_keeps_parsed_input() {
        let a = ChatMessage {
            role: "assistant".to_string(),
            content: Some("let me read".to_string()),
            tool_calls: Some(vec![ToolCall {
                id: "c9".to_string(),
                call_type: "function".to_string(),
                function: ToolFunction {
                    name: "write".to_string(),
                    arguments: "{\"path\":\"b\",\"content\":\"x\"}".to_string(),
                },
                thought_signature: None,
            }]),
            tool_call_id: None,
            name: None,
            thought_signature: None,
            reasoning_content: None,
        };
        let (_system, blocks) = split_and_convert_messages(&[a]);
        let content = blocks[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["name"], "write");
        assert_eq!(content[1]["input"]["path"], "b");
    }

    #[test]
    fn local_detection_parses_the_host_instead_of_matching_a_substring() {
        let ep = |u: &str| ModelEndpoint {
            key: String::new(),
            model: String::new(),
            base_url: u.to_string(),
            api_key: String::new(),
            label: String::new(),
            is_local_flag: false,
            has_quota_pool: false,
            configured_context_window: None,
        };
        // Genuinely local.
        for url in [
            "http://127.0.0.1:8080/v1",
            "http://localhost:11434/v1",
            "http://[::1]:1234/v1",
            "http://127.0.0.2:8080/v1",
            "http://mac-mini.local:8000/v1",
        ] {
            assert!(ep(url).is_local(), "{url} must be local");
        }
        // Remote hosts that merely embed a loopback name in their name or path.
        for url in [
            "https://127.0.0.1.evil.com/v1",
            "https://localhost.attacker.net/v1",
            "https://api.example.com/127.0.0.1/v1",
            "https://notlocal.example.com/v1",
            "https://api.local.example.com/v1",
        ] {
            assert!(!ep(url).is_local(), "{url} must NOT be local");
        }
    }

    #[test]
    fn anthropic_detection() {
        let base = |u: &str| ModelEndpoint {
            key: String::new(),
            model: String::new(),
            base_url: u.to_string(),
            api_key: String::new(),
            label: String::new(),
            is_local_flag: false,
            has_quota_pool: false,
            configured_context_window: None,
        };
        assert!(base("https://api.anthropic.com/v1").is_anthropic());
        assert!(!base("https://api.openai.com/v1").is_anthropic());
        assert!(!base("http://127.0.0.1:8080/v1").is_anthropic());
    }

    #[test]
    fn openai_jinja_sanitizer_strips_mid_system_and_merges_users() {
        let msgs = vec![
            msg("system", "sys1"),
            msg("user", "u1"),
            msg("system", "sys2"),
            msg("user", "u2"),
            msg("tool", "orphan_tool"),
        ];
        let serialized = serialize_messages_with_vision(&msgs);
        assert_eq!(serialized.len(), 2);
        assert_eq!(serialized[0]["role"], "system");
        assert_eq!(serialized[1]["role"], "user");
        assert_eq!(serialized[1]["content"], "u1\n\nu2");
    }

    #[test]
    fn xml_tagged_tool_calls_are_recovered_for_local_qwen35() {
        let raw = r#"<tool_call>
<function=edit>
<parameter=path>src/main.rs</parameter>
<parameter=old_text>
fn foo() {}
</parameter>
<parameter=new_text>
fn bar() {}
</parameter>
</function>
</tool_call>"#;
        let recovered = recover_inline_tool_calls(raw);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].function.name, "edit");
        let parsed: Value = serde_json::from_str(&recovered[0].function.arguments).unwrap();
        assert_eq!(parsed["path"], "src/main.rs");
        assert_eq!(parsed["old_text"], "fn foo() {}");
        assert_eq!(parsed["new_text"], "fn bar() {}");
    }

    #[test]
    fn multiple_tool_calls_with_vision_keep_all_tool_messages_contiguous() {
        register_read_image(
            "/tmp/test_multi_1.png",
            DraggedImage {
                raw_span: "/tmp/test_multi_1.png".to_string(),
                path: "/tmp/test_multi_1.png".to_string(),
                filename: "test_multi_1.png".to_string(),
                mime: "image/png",
                size_bytes: 16,
                base64: "AAAA".to_string(),
            },
        );
        let assistant_with_2_tools = ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "c1".to_string(),
                    call_type: "function".to_string(),
                    function: ToolFunction {
                        name: "read".to_string(),
                        arguments: "{\"path\":\"/tmp/test_multi_1.png\"}".to_string(),
                    },
                    thought_signature: None,
                },
                ToolCall {
                    id: "c2".to_string(),
                    call_type: "function".to_string(),
                    function: ToolFunction {
                        name: "read".to_string(),
                        arguments: "{\"path\":\"Cargo.toml\"}".to_string(),
                    },
                    thought_signature: None,
                },
            ]),
            tool_call_id: None,
            name: None,
            thought_signature: None,
            reasoning_content: None,
        };
        let t1 = ChatMessage::tool_result(
            "c1",
            "read",
            "[SPAVN Vision Image Loaded: /tmp/test_multi_1.png (image/png, 0.1 KB)]",
        );
        let t2 = ChatMessage::tool_result("c2", "read", "1: [package]");
        let msgs = vec![msg("user", "compare"), assistant_with_2_tools, t1, t2];
        let serialized = serialize_messages_with_vision(&msgs);
        // Order MUST be: user -> assistant -> tool(c1) -> tool(c2) -> user(vision)
        assert_eq!(serialized.len(), 5);
        assert_eq!(serialized[0]["role"], "user");
        assert_eq!(serialized[1]["role"], "assistant");
        assert_eq!(serialized[2]["role"], "tool");
        assert_eq!(serialized[3]["role"], "tool");
        assert_eq!(serialized[4]["role"], "user");
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn classifies_status_and_retry_after() {
        for ok in [
            "Upstream HTTP 429: slow down [retry-after=7]",
            "Anthropic HTTP 529: overloaded",
            "Upstream HTTP 500: boom",
            "Upstream HTTP 504: gateway",
            "Upstream request failed: connection reset",
        ] {
            assert!(is_retryable_upstream_error(ok), "{ok}");
        }
        for no in [
            "Upstream HTTP 400: bad request",
            "Upstream HTTP 401: unauthorized",
            "Upstream HTTP 404: model not found",
            "Stream read error: broken pipe",
        ] {
            assert!(!is_retryable_upstream_error(no), "{no}");
        }
        assert_eq!(parse_retry_after_secs("x [retry-after=7]"), Some(7));
        assert_eq!(parse_retry_after_secs("x"), None);
        assert_eq!(parse_http_status("Upstream HTTP 429: x"), Some(429));
    }
}
