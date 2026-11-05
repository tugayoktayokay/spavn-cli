use super::*;

/// Appends the server's `Retry-After` (seconds form) to the error text as `[retry-after=N]`.
pub(crate) fn retry_after_marker(response: &ureq::http::Response<ureq::Body>) -> String {
    response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|n| format!(" [retry-after={}]", n))
        .unwrap_or_default()
}

pub(crate) fn parse_http_status(err: &str) -> Option<u16> {
    let rest = &err[err.find("HTTP ")? + 5..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

pub(crate) fn parse_retry_after_secs(err: &str) -> Option<u64> {
    let rest = &err[err.find("[retry-after=")? + 13..];
    rest.split(']').next()?.parse().ok()
}

pub(crate) fn is_retryable_upstream_error(err: &str) -> bool {
    if let Some(status) = parse_http_status(err) {
        return matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504 | 529);
    }
    // No HTTP status: only connection-level failures before any output streamed are safe to redo.
    err.contains("Upstream request failed")
        || err.contains("rate_limit")
        || err.contains("overloaded_error")
}

pub fn stream_chat_completion<FThink, FContent>(
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
    let delays_ms = [1200u64, 2500u64, 6000u64];
    let mut attempt = 0usize;
    loop {
        let res = if endpoint.is_anthropic() {
            stream_anthropic(endpoint, messages, tools, &mut on_thinking, &mut on_content)
        } else {
            stream_openai(endpoint, messages, tools, &mut on_thinking, &mut on_content)
        };
        match res {
            Ok(ok) => return Ok(ok),
            Err(err) => {
                if attempt < delays_ms.len()
                    && !crate::is_interrupted()
                    && is_retryable_upstream_error(&err)
                {
                    // Honor Retry-After, capped so a hostile value cannot stall the agent.
                    let wait_ms = delays_ms[attempt]
                        .max(parse_retry_after_secs(&err).unwrap_or(0).min(30) * 1000);
                    attempt += 1;
                    crate::hud::set_spinner_label(&format!(
                        "⏳ Rate-limit/Yoğunluk — yeniden deneniyor ({}/{})",
                        attempt,
                        delays_ms.len()
                    ));
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                    continue;
                }
                return Err(err);
            }
        }
    }
}

pub fn stream_openai<FThink, FContent>(
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

    let serialized_messages = serialize_messages_with_vision(messages);
    let mut payload = json!({
        "model": endpoint.model,
        "messages": serialized_messages,
        "stream": true,
        "stream_options": { "include_usage": true }
    });

    if endpoint.is_local() {
        payload["cache_prompt"] = json!(true);
    }
    if endpoint.base_url.contains("minimax") {
        payload["reasoning_split"] = json!(true);
    }

    if let Some(arr) = tools.as_array() {
        if !arr.is_empty() {
            payload["tools"] = tools.clone();
        }
    }
    let body_str = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let base_trimmed = endpoint.base_url.trim_end_matches('/');
    let url = if base_trimmed.contains("openai.azure.com") && !base_trimmed.contains("api-version=")
    {
        format!(
            "{}/chat/completions?api-version=2025-01-01-preview",
            base_trimmed
        )
    } else {
        format!("{}/chat/completions", base_trimmed)
    };

    enum StreamEvent {
        Line(String),
        Failed(String),
        End,
    }

    let (tx, rx) = std::sync::mpsc::channel::<StreamEvent>();
    let auth_header = format!("Bearer {}", endpoint.api_key);
    let raw_api_key = endpoint.api_key.clone();
    let request_url = url.clone();
    std::thread::spawn(move || {
        let agent = crate::http::agent_stream();
        let mut req = agent
            .post(&request_url)
            .header("Content-Type", "application/json")
            .header("Authorization", &auth_header)
            .header("api-key", &raw_api_key);
        if request_url.contains("opencode.ai") {
            let sid = format!(
                "ses_{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            );
            req = req
                .header("user-agent", "opencode/1.19.0 ai-sdk/provider-utils/4.0.23")
                .header("x-opencode-client", "cli")
                .header("x-opencode-session", &sid)
                .header("x-opencode-session-id", &sid);
        }
        let response = req.send(body_str.as_str());
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
                "Upstream HTTP {}: {}{}",
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
    let mut message_signature: Option<String> = None;
    let mut tool_calls_map: BTreeMap<usize, ToolCall> = BTreeMap::new();
    let mut usage = TokenUsage::default();
    let mut raw_error_lines = Vec::new();
    let mut finish_reason: Option<String> = None;
    let mut last_chunk_at = Instant::now();
    let ttft_stall_limit =
        std::time::Duration::from_secs(if endpoint.is_local() { 90 } else { 50 });
    let inter_token_stall_limit = std::time::Duration::from_secs(35);
    loop {
        if crate::is_interrupted() {
            return Err("İşlem kullanıcı tarafından (Esc / Ctrl+C) iptal edildi.".to_string());
        }
        let line = match rx.recv_timeout(std::time::Duration::from_millis(150)) {
            Ok(StreamEvent::Line(line)) => {
                last_chunk_at = Instant::now();
                line
            }
            Ok(StreamEvent::Failed(err)) => return Err(err),
            Ok(StreamEvent::End) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let limit = if ttft_ms == 0 {
                    ttft_stall_limit
                } else {
                    inter_token_stall_limit
                };
                if last_chunk_at.elapsed() >= limit {
                    return Err(format!(
                        "Upstream request failed: stream stalled for {}s without data",
                        limit.as_secs()
                    ));
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with("data:") {
            if raw_error_lines.len() < 12 {
                raw_error_lines.push(trimmed.to_string());
            }
            continue;
        }

        let data_str = trimmed.trim_start_matches("data:").trim();
        if data_str == "[DONE]" {
            break;
        }

        let parsed: Value = match serde_json::from_str(data_str) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(err) = parsed.get("error") {
            return Err(format!("Upstream API error: {}", err));
        }

        if let Some(u) = parsed.get("usage") {
            usage.prompt_tokens = u
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(usage.prompt_tokens);
            usage.completion_tokens = u
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(usage.completion_tokens);
            usage.total_tokens = u
                .get("total_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(usage.total_tokens);
        }

        let Some(choice) = parsed.get("choices").and_then(|c| c.get(0)) else {
            continue;
        };
        if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            if !fr.is_empty() {
                finish_reason = Some(fr.to_string());
            }
        }
        let delta = choice
            .get("delta")
            .or_else(|| choice.get("message"))
            .unwrap_or(&Value::Null);
        if ttft_ms == 0
            && (delta.get("content").is_some()
                || delta.get("reasoning_content").is_some()
                || delta.get("thinking").is_some()
                || delta.get("tool_calls").is_some())
        {
            ttft_ms = start.elapsed().as_millis();
        }

        if let Some(sig) = delta.get("thought_signature").and_then(|v| v.as_str()) {
            if !sig.is_empty() {
                message_signature = Some(sig.to_string());
            }
        }

        if let Some(think) = delta
            .get("reasoning_content")
            .or_else(|| delta.get("thinking"))
            .and_then(|v| v.as_str())
        {
            if !think.is_empty() {
                reasoning.push_str(think);
                on_thinking(think);
            }
        }

        if let Some(txt) = delta.get("content").and_then(|v| v.as_str()) {
            if !txt.is_empty() {
                content.push_str(txt);
                on_content(txt);
            }
        }

        let mut abort_runaway_tool_stream = false;
        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                if endpoint.is_local() && idx >= 4 {
                    abort_runaway_tool_stream = true;
                    break;
                }
                let entry = tool_calls_map.entry(idx).or_insert_with(|| ToolCall {
                    id: format!("call_{}", idx),
                    call_type: "function".to_string(),
                    function: ToolFunction {
                        name: String::new(),
                        arguments: String::new(),
                    },
                    thought_signature: None,
                });

                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    if !id.is_empty() {
                        entry.id = id.to_string();
                    }
                }
                if let Some(sig) = tc.get("thought_signature").and_then(|v| v.as_str()) {
                    if !sig.is_empty() {
                        entry.thought_signature = Some(sig.to_string());
                    }
                }
                if let Some(func) = tc.get("function") {
                    if let Some(n) = func.get("name").and_then(|v| v.as_str()) {
                        entry.function.name.push_str(n);
                    }
                    if let Some(a) = func.get("arguments").and_then(|v| v.as_str()) {
                        entry.function.arguments.push_str(a);
                    }
                }
                let cur_name = entry.function.name.clone();
                let cur_args = entry.function.arguments.clone();
                crate::hud::notify_tool_call_streaming(&cur_name, cur_args.len());

                if idx > 0 && cur_args.len() >= 12 {
                    let prefix_len = cur_args.len().min(18);
                    let cur_prefix = cur_args.get(..prefix_len).unwrap_or("");
                    let is_dup = tool_calls_map.iter().any(|(&other_idx, other_tc)| {
                        other_idx < idx
                            && other_tc.function.name == cur_name
                            && (other_tc.function.arguments == cur_args
                                || (cur_name == "read"
                                    && prefix_len >= 14
                                    && !cur_prefix.is_empty()
                                    && other_tc.function.arguments.starts_with(cur_prefix)))
                    });
                    if is_dup {
                        tool_calls_map.remove(&idx);
                        abort_runaway_tool_stream = true;
                        break;
                    }
                }
            }
        }
        if abort_runaway_tool_stream {
            break;
        }
    }

    if content.is_empty()
        && reasoning.is_empty()
        && tool_calls_map.is_empty()
        && !raw_error_lines.is_empty()
    {
        return Err(format!(
            "Upstream request failed: {}",
            raw_error_lines.join(" ")
        ));
    }

    let mut tool_calls: Vec<ToolCall> = tool_calls_map.into_values().collect();
    if tool_calls.is_empty() && !content.is_empty() {
        let recovered = recover_inline_tool_calls(&content);
        if !recovered.is_empty() {
            tool_calls = recovered;
            content = strip_recovered_tool_markup(&content);
        }
    }
    if tool_calls.is_empty() && !reasoning.is_empty() {
        let recovered_from_think = recover_inline_tool_calls(&reasoning);
        if !recovered_from_think.is_empty() {
            tool_calls = recovered_from_think;
            reasoning = strip_recovered_tool_markup(&reasoning);
        }
    }

    if message_signature.is_none() {
        if let Some(first_sig) = tool_calls.first().and_then(|t| t.thought_signature.clone()) {
            message_signature = Some(first_sig);
        }
    }

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
        thought_signature: message_signature,
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

pub(crate) fn strip_recovered_tool_markup(text: &str) -> String {
    let mut out = text.to_string();
    for (open, close) in [
        ("<tool_call>", "</tool_call>"),
        ("<function=", "</function>"),
    ] {
        while let Some(s) = out.find(open) {
            if let Some(e) = out[s..].find(close) {
                let end_idx = s + e + close.len();
                out.replace_range(s..end_idx, "");
            } else {
                break;
            }
        }
    }
    let trimmed = out.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return String::new();
    }
    trimmed.to_string()
}

pub(crate) fn recover_inline_tool_calls(text: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let valid_names = [
        "read",
        "edit",
        "write",
        "bash",
        "search",
        "glob",
        "find_files",
        "list_files",
        "file_glob",
        "memory",
        "skill",
        "web",
        "subagent",
        "spawn_submodels",
        "task",
        "lsp",
        "mcp",
    ];
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find("<tool_call>") {
        let abs_start = search_from + start + "<tool_call>".len();
        if let Some(end) = text[abs_start..].find("</tool_call>") {
            let inner = text[abs_start..abs_start + end].trim();
            if let Some(tc) = parse_candidate_tool_json(inner, calls.len(), &valid_names) {
                calls.push(tc);
            } else {
                calls.extend(parse_xml_tagged_tool_calls(inner, &valid_names));
            }
            search_from = abs_start + end + "</tool_call>".len();
        } else {
            break;
        }
    }
    if !calls.is_empty() {
        return calls;
    }
    if text.contains("<function=") && text.contains("</function>") {
        let xml_calls = parse_xml_tagged_tool_calls(text, &valid_names);
        if !xml_calls.is_empty() {
            return xml_calls;
        }
    }

    let mut fence_from = 0;
    while let Some(fstart) = text[fence_from..].find("```") {
        let after_ticks = fence_from + fstart + 3;
        let Some(nl) = text[after_ticks..].find('\n') else {
            break;
        };
        let body_start = after_ticks + nl + 1;
        let Some(fend) = text[body_start..].find("```") else {
            break;
        };
        let block = text[body_start..body_start + fend].trim();
        if let Some(tc) = parse_candidate_tool_json(block, calls.len(), &valid_names) {
            calls.push(tc);
        }
        fence_from = body_start + fend + 3;
    }
    if !calls.is_empty() {
        return calls;
    }

    let trimmed = text.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        if let Some(tc) = parse_candidate_tool_json(trimmed, 0, &valid_names) {
            calls.push(tc);
        }
    }
    if !calls.is_empty() {
        return calls;
    }

    // Aider-style `<<<<<<< SEARCH ... ======= ... >>>>>>> REPLACE` fallback recovery
    // Rescues local models (Qwen, Gemma, Llama, DeepSeek) & BYOK endpoints when they emit raw diff blocks
    if text.contains("<<<<<<< SEARCH") && text.contains(">>>>>>> REPLACE") {
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0usize;
        let mut current_file = String::new();
        while i < lines.len() {
            let line_trim = lines[i].trim();
            if line_trim.starts_with("<<<<<<< SEARCH") {
                // Look backwards 1-3 lines for file path header if needed
                let mut look = i;
                while look > 0 && i - look <= 3 {
                    look -= 1;
                    let cand = lines[look]
                        .trim()
                        .trim_matches('`')
                        .trim_matches('*')
                        .trim_start_matches('#')
                        .trim();
                    if !cand.is_empty()
                        && !cand.starts_with("```")
                        && (cand.contains('/') || cand.contains('.'))
                        && !cand.contains(' ')
                    {
                        current_file = cand.to_string();
                        break;
                    }
                }
                i += 1;
                let mut old_lines = Vec::new();
                while i < lines.len() && !lines[i].trim().starts_with("=======") {
                    old_lines.push(lines[i]);
                    i += 1;
                }
                if i < lines.len() && lines[i].trim().starts_with("=======") {
                    i += 1;
                    let mut new_lines = Vec::new();
                    while i < lines.len() && !lines[i].trim().starts_with(">>>>>>> REPLACE") {
                        new_lines.push(lines[i]);
                        i += 1;
                    }
                    if i < lines.len() && !current_file.is_empty() {
                        let args_obj = json!({
                            "path": current_file,
                            "old_text": old_lines.join("\n"),
                            "new_text": new_lines.join("\n")
                        });
                        calls.push(ToolCall {
                            id: format!("aider_sr_{}", calls.len()),
                            call_type: "function".to_string(),
                            function: ToolFunction {
                                name: "edit".to_string(),
                                arguments: args_obj.to_string(),
                            },
                            thought_signature: None,
                        });
                    }
                }
            }
            i += 1;
        }
    }

    calls
}

pub(crate) fn repair_unescaped_json_newlines(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 16);
    let mut in_str = false;
    let mut esc = false;
    for ch in raw.chars() {
        if esc {
            out.push(ch);
            esc = false;
            continue;
        }
        if ch == '\\' && in_str {
            out.push(ch);
            esc = true;
            continue;
        }
        if ch == '"' {
            in_str = !in_str;
            out.push(ch);
            continue;
        }
        if in_str && ch == '\n' {
            out.push_str("\\n");
        } else if in_str && ch == '\r' {
            out.push_str("\\r");
        } else if in_str && ch == '\t' {
            out.push_str("\\t");
        } else {
            out.push(ch);
        }
    }
    out
}

pub(crate) fn parse_candidate_tool_json(raw: &str, idx: usize, valid_names: &[&str]) -> Option<ToolCall> {
    let val: Value = serde_json::from_str(raw)
        .or_else(|_| serde_json::from_str(&repair_unescaped_json_newlines(raw)))
        .ok()?;
    let name = val
        .get("name")
        .or_else(|| val.get("tool"))
        .or_else(|| val.get("function"))
        .and_then(|v| v.as_str())?;
    if !valid_names.contains(&name) {
        return None;
    }
    let args_val = val
        .get("arguments")
        .or_else(|| val.get("parameters"))
        .or_else(|| val.get("args"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let args_str = if let Some(s) = args_val.as_str() {
        s.to_string()
    } else {
        serde_json::to_string(&args_val).ok()?
    };

    Some(ToolCall {
        id: format!("local_call_{}", idx),
        call_type: "function".to_string(),
        function: ToolFunction {
            name: name.to_string(),
            arguments: args_str,
        },
        thought_signature: None,
    })
}

pub(crate) fn parse_xml_tagged_tool_calls(text: &str, valid_names: &[&str]) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut search_from = 0usize;
    while let Some(fn_start) = text[search_from..].find("<function=") {
        let name_start = search_from + fn_start + "<function=".len();
        let Some(gt_rel) = text[name_start..].find('>') else {
            break;
        };
        let raw_name = text[name_start..name_start + gt_rel]
            .trim()
            .trim_matches('"')
            .trim_matches('\'');
        let body_start = name_start + gt_rel + 1;
        let Some(fn_end_rel) = text[body_start..].find("</function>") else {
            break;
        };
        let fn_body = &text[body_start..body_start + fn_end_rel];
        search_from = body_start + fn_end_rel + "</function>".len();

        if !valid_names.contains(&raw_name) {
            continue;
        }
        let mut params = serde_json::Map::new();
        let mut p_from = 0usize;
        while let Some(p_start) = fn_body[p_from..].find("<parameter=") {
            let k_start = p_from + p_start + "<parameter=".len();
            let Some(k_gt) = fn_body[k_start..].find('>') else {
                break;
            };
            let param_key = fn_body[k_start..k_start + k_gt]
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            let v_start = k_start + k_gt + 1;
            let Some(p_end) = fn_body[v_start..].find("</parameter>") else {
                break;
            };
            let raw_val = &fn_body[v_start..v_start + p_end];
            let clean_val = raw_val
                .strip_prefix('\n')
                .unwrap_or(raw_val)
                .strip_suffix('\n')
                .unwrap_or(raw_val.strip_prefix('\n').unwrap_or(raw_val));
            if matches!(param_key, "start_line" | "end_line" | "timeout_sec") {
                if let Ok(n) = clean_val.trim().parse::<u64>() {
                    params.insert(param_key.to_string(), json!(n));
                } else {
                    params.insert(param_key.to_string(), json!(clean_val));
                }
            } else {
                params.insert(param_key.to_string(), json!(clean_val));
            }
            p_from = v_start + p_end + "</parameter>".len();
        }
        if !params.is_empty() {
            calls.push(ToolCall {
                id: format!("xml_call_{}", calls.len()),
                call_type: "function".to_string(),
                function: ToolFunction {
                    name: raw_name.to_string(),
                    arguments: Value::Object(params).to_string(),
                },
                thought_signature: None,
            });
        }
    }
    calls
}

// ─────────────────────────────────────────────────────────────────────────────
// Native Anthropic Messages API (/v1/messages)
// ─────────────────────────────────────────────────────────────────────────────

