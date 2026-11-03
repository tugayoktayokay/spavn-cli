use crate::config::{builtin_byok_catalog, load_user_config, resolve_api_key};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolFunction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: Some(text.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            thought_signature: None,
            reasoning_content: None,
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(text.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            thought_signature: None,
            reasoning_content: None,
        }
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        name: impl Into<String>,
        output: impl Into<String>,
    ) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(output.into()),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            name: Some(name.into()),
            thought_signature: None,
            reasoning_content: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredLocalModel {
    pub id: String,
    pub loaded: bool,
    pub context_window: Option<usize>,
}

static LOCAL_MODELS_CACHE: std::sync::Mutex<
    Option<(std::time::Instant, String, Vec<DiscoveredLocalModel>)>,
> = std::sync::Mutex::new(None);

pub fn discover_local_models(
    discovery_url: &str,
    force_refresh: bool,
) -> Vec<DiscoveredLocalModel> {
    if !force_refresh {
        if let Ok(guard) = LOCAL_MODELS_CACHE.lock() {
            if let Some((ts, ref url, ref list)) = *guard {
                if url == discovery_url && ts.elapsed().as_secs() < 60 {
                    return list.clone();
                }
            }
        }
    }
    let Ok((_, body)) = crate::http::get_text(discovery_url, &[], 2) else {
        return Vec::new();
    };
    let Ok(val) = serde_json::from_str::<Value>(&body) else {
        return Vec::new();
    };
    let Some(arr) = val.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for item in arr {
        let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let status = item
            .get("status")
            .and_then(|s| s.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let ctx_win = item
            .get("max_model_len")
            .or_else(|| item.get("context_length"))
            .or_else(|| item.get("n_ctx"))
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        models.push(DiscoveredLocalModel {
            id: id.to_string(),
            loaded: status == "loaded",
            context_window: ctx_win,
        });
    }
    if let Ok(mut guard) = LOCAL_MODELS_CACHE.lock() {
        *guard = Some((
            std::time::Instant::now(),
            discovery_url.to_string(),
            models.clone(),
        ));
    }
    models
}

#[derive(Debug, Clone)]
pub struct ModelEndpoint {
    pub key: String,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub label: String,
    pub is_local_flag: bool,
    pub has_quota_pool: bool,
    pub configured_context_window: Option<usize>,
}

impl ModelEndpoint {
    pub fn is_local(&self) -> bool {
        self.is_local_flag
            || self.base_url.contains(":8000")
            || self.base_url.contains(":11434")
            || self.base_url.contains(":1234")
            || self.base_url.contains(".local")
    }

    pub fn is_smart_pool_proxy(&self) -> bool {
        !self.is_local() && self.has_quota_pool
    }

    pub fn context_window_tokens(&self) -> usize {
        if let Some(cw) = self.configured_context_window {
            if cw > 0 {
                return cw;
            }
        }
        if self.is_local() {
            let k = self.model.to_lowercase();
            if k.contains("128k") {
                return 131_072;
            }
            if k.contains("64k") {
                return 65_536;
            }
            return 32_768;
        }
        let k = self.model.to_lowercase();
        if k.contains("gemini") || k.contains("gpt-4.1") {
            1_000_000
        } else if k.contains("grok-4") {
            256_000
        } else if k.contains("claude")
            || k.contains("opus")
            || k.contains("sonnet")
            || k.contains("haiku")
            || k.contains("minimax")
            || k.contains("gpt-5")
            || k.starts_with("o3")
            || k.starts_with("o4")
        {
            200_000
        } else {
            128_000
        }
    }
}

#[allow(dead_code)]
pub fn detect_active_local_model(discovery_url: &str) -> Option<String> {
    let list = discover_local_models(discovery_url, false);
    if let Some(loaded) = list.iter().find(|m| m.loaded) {
        return Some(loaded.id.clone());
    }
    list.first().map(|m| m.id.clone())
}

pub fn resolve_endpoint(key: &str) -> ModelEndpoint {
    let cfg = load_user_config();
    let clean = key.trim();
    let lower = clean.to_lowercase();
    let has_user_quota_script = cfg.quota_quick_script.is_some();

    // 1. Explicit `local:<model_id>` prefix or `local` keyword
    if let Some(ref local_base) = cfg.local_base_url {
        if lower == "local" {
            let discovered = cfg
                .local_discovery_url
                .as_deref()
                .map(|u| discover_local_models(u, false))
                .unwrap_or_default();
            let chosen = discovered
                .iter()
                .find(|m| m.loaded)
                .or_else(|| discovered.first());
            let model_id = chosen
                .map(|m| m.id.clone())
                .unwrap_or_else(|| "local-model".to_string());
            let ctx_win = chosen.and_then(|m| m.context_window);
            return ModelEndpoint {
                key: model_id.clone(),
                label: format!("Local ({})", model_id),
                model: model_id,
                base_url: local_base.clone(),
                api_key: "local".to_string(),
                is_local_flag: true,
                has_quota_pool: false,
                configured_context_window: ctx_win,
            };
        }
        if let Some(stripped) = clean.strip_prefix("local:") {
            let discovered = cfg
                .local_discovery_url
                .as_deref()
                .map(|u| discover_local_models(u, false))
                .unwrap_or_default();
            let ctx_win = discovered
                .iter()
                .find(|m| m.id.eq_ignore_ascii_case(stripped))
                .and_then(|m| m.context_window);
            return ModelEndpoint {
                key: stripped.to_string(),
                label: format!("Local ({})", stripped),
                model: stripped.to_string(),
                base_url: local_base.clone(),
                api_key: "local".to_string(),
                is_local_flag: true,
                has_quota_pool: false,
                configured_context_window: ctx_win,
            };
        }
    }

    // 2. Check user-configured providers in ~/.spavn/config.json
    for group in &cfg.providers {
        let group_has_pool =
            !group.is_local && group.api_key_env.is_empty() && has_user_quota_script;
        for m in &group.models {
            if m.key.eq_ignore_ascii_case(clean) || m.model_id.eq_ignore_ascii_case(clean) {
                let api_key =
                    resolve_api_key(&group.api_key_env).unwrap_or_else(|| "local".to_string());
                return ModelEndpoint {
                    key: m.key.clone(),
                    model: m.model_id.clone(),
                    base_url: group.base_url.clone(),
                    api_key,
                    label: m.label.clone(),
                    is_local_flag: group.is_local,
                    has_quota_pool: group_has_pool,
                    configured_context_window: m.context_window.or(group.context_window),
                };
            }
        }
        if let Some((prov_prefix, custom_model)) = clean.split_once(':') {
            if group.id.eq_ignore_ascii_case(prov_prefix) {
                let api_key =
                    resolve_api_key(&group.api_key_env).unwrap_or_else(|| "local".to_string());
                return ModelEndpoint {
                    key: clean.to_string(),
                    model: custom_model.to_string(),
                    base_url: group.base_url.clone(),
                    api_key,
                    label: format!("{} ({})", custom_model, group.id),
                    is_local_flag: group.is_local,
                    has_quota_pool: group_has_pool,
                    configured_context_window: group.context_window,
                };
            }
        }
    }

    // 3. Check built-in BYOK catalog providers (OpenAI, Anthropic, Gemini, DeepSeek, MiniMax, GLM, Groq, xAI, OpenRouter, Ollama)
    for group in builtin_byok_catalog() {
        for m in &group.models {
            if m.key.eq_ignore_ascii_case(clean)
                || m.model_id.eq_ignore_ascii_case(clean)
                || group.id.eq_ignore_ascii_case(clean)
            {
                let api_key =
                    resolve_api_key(&group.api_key_env).unwrap_or_else(|| "local".to_string());
                return ModelEndpoint {
                    key: m.key.clone(),
                    model: m.model_id.clone(),
                    base_url: group.base_url.clone(),
                    api_key,
                    label: m.label.clone(),
                    is_local_flag: group.is_local,
                    has_quota_pool: false,
                    configured_context_window: m.context_window.or(group.context_window),
                };
            }
        }
        if let Some((prov_prefix, custom_model)) = clean.split_once(':') {
            if group.id.eq_ignore_ascii_case(prov_prefix) {
                let api_key =
                    resolve_api_key(&group.api_key_env).unwrap_or_else(|| "local".to_string());
                return ModelEndpoint {
                    key: clean.to_string(),
                    model: custom_model.to_string(),
                    base_url: group.base_url.clone(),
                    api_key,
                    label: format!("{} ({})", custom_model, group.id),
                    is_local_flag: group.is_local,
                    has_quota_pool: false,
                    configured_context_window: group.context_window,
                };
            }
        }
    }

    // 4. Dynamic Local Discovery match (any model ID present on the configured local server)
    if let Some(ref local_base) = cfg.local_base_url {
        let discovered = cfg
            .local_discovery_url
            .as_deref()
            .map(|u| discover_local_models(u, false))
            .unwrap_or_default();
        if let Some(found) = discovered.iter().find(|m| m.id.eq_ignore_ascii_case(clean)) {
            return ModelEndpoint {
                key: found.id.clone(),
                label: format!("Local ({})", found.id),
                model: found.id.clone(),
                base_url: local_base.clone(),
                api_key: "local".to_string(),
                is_local_flag: true,
                has_quota_pool: false,
                configured_context_window: found.context_window,
            };
        }
    }

    // 5. Fallback to first configured provider or environment base URL
    let fallback_group = cfg.providers.first();
    let fallback_base = fallback_group
        .map(|p| p.base_url.clone())
        .or_else(|| std::env::var("SPAVN_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
    let fallback_key = std::env::var("SPAVN_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .unwrap_or_else(|_| "local".to_string());
    let fallback_is_local = fallback_group.map(|p| p.is_local).unwrap_or(false);
    let fallback_has_pool = fallback_group
        .map(|p| !p.is_local && p.api_key_env.is_empty() && has_user_quota_script)
        .unwrap_or(false);

    ModelEndpoint {
        key: clean.to_string(),
        model: clean.to_string(),
        base_url: fallback_base,
        api_key: fallback_key,
        label: clean.to_string(),
        is_local_flag: fallback_is_local,
        has_quota_pool: fallback_has_pool,
        configured_context_window: fallback_group.and_then(|p| p.context_window),
    }
}

#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

pub struct CompletionResult {
    pub message: ChatMessage,
    pub usage: TokenUsage,
    pub elapsed_ms: u128,
    pub ttft_ms: u128,
    pub finish_reason: Option<String>,
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Detects local image paths dragged into the terminal (including macOS `\ `-escaped paths)
/// and returns `(mime_type, base64_data)` pairs for multimodal vision.
fn extract_dragged_images(text: &str) -> Vec<(&'static str, String)> {
    let mut results = Vec::new();
    let exts = [
        (".png", "image/png"),
        (".jpg", "image/jpeg"),
        (".jpeg", "image/jpeg"),
        (".webp", "image/webp"),
        (".gif", "image/gif"),
    ];
    let lower = text.to_lowercase();
    for (ext, mime) in exts {
        let mut search_from = 0usize;
        while let Some(pos) = lower[search_from..].find(ext) {
            let end_idx = search_from + pos + ext.len();
            search_from = end_idx;
            // Walk backwards to find the start of the path (`/`, `~`)
            let prefix = &text[..end_idx];
            let mut start_candidates = Vec::new();
            for (i, ch) in prefix.char_indices() {
                if ch == '/' || ch == '~' {
                    if i == 0 {
                        start_candidates.push(i);
                    } else {
                        let prev = prefix[..i].chars().last().unwrap_or(' ');
                        if prev == ' ' || prev == '\'' || prev == '"' || prev == '\n' {
                            start_candidates.push(i);
                        }
                    }
                }
            }
            for &start_idx in start_candidates.iter().rev() {
                let raw_path = text[start_idx..end_idx]
                    .trim_matches(|c| c == '\'' || c == '"')
                    .replace("\\ ", " ");
                let expanded = crate::config::expand_user_path(&raw_path);
                if expanded.is_file() {
                    if let Ok(bytes) = std::fs::read(&expanded) {
                        if bytes.len() <= 15 * 1024 * 1024 {
                            results.push((mime, base64_encode(&bytes)));
                            break;
                        }
                    }
                }
            }
        }
    }
    results
}

fn serialize_messages_with_vision(messages: &[ChatMessage]) -> Vec<Value> {
    let last_user_idx = messages.iter().rposition(|m| m.role == "user");
    messages
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let mut v = serde_json::to_value(m).unwrap_or_else(|_| json!({}));
            if Some(idx) == last_user_idx {
                if let Some(ref txt) = m.content {
                    let imgs = extract_dragged_images(txt);
                    if !imgs.is_empty() {
                        let mut parts = vec![json!({ "type": "text", "text": txt })];
                        for (mime, b64) in imgs {
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", mime, b64)
                                }
                            }));
                        }
                        v["content"] = Value::Array(parts);
                    }
                }
            }
            v
        })
        .collect()
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
        payload["temperature"] = json!(0.2);
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
    let url = format!(
        "{}/chat/completions",
        endpoint.base_url.trim_end_matches('/')
    );

    enum StreamEvent {
        Line(String),
        Failed(String),
        End,
    }

    let (tx, rx) = std::sync::mpsc::channel::<StreamEvent>();
    let auth_header = format!("Bearer {}", endpoint.api_key);
    let request_url = url.clone();
    std::thread::spawn(move || {
        let agent = crate::http::agent_stream();
        let response = agent
            .post(&request_url)
            .header("Content-Type", "application/json")
            .header("Authorization", &auth_header)
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
            let body = response.body_mut().read_to_string().unwrap_or_default();
            let snippet: String = body.trim().chars().take(400).collect();
            let _ = tx.send(StreamEvent::Failed(format!(
                "Upstream HTTP {}: {}",
                status, snippet
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

        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
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
                crate::hud::notify_tool_call_streaming(
                    &entry.function.name,
                    entry.function.arguments.len(),
                );
            }
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

fn recover_inline_tool_calls(text: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let valid_names = [
        "read", "edit", "write", "bash", "search", "memory", "skill", "web",
    ];
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find("<tool_call>") {
        let abs_start = search_from + start + "<tool_call>".len();
        if let Some(end) = text[abs_start..].find("</tool_call>") {
            let inner = text[abs_start..abs_start + end].trim();
            if let Some(tc) = parse_candidate_tool_json(inner, calls.len(), &valid_names) {
                calls.push(tc);
            }
            search_from = abs_start + end + "</tool_call>".len();
        } else {
            break;
        }
    }
    if !calls.is_empty() {
        return calls;
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

fn repair_unescaped_json_newlines(raw: &str) -> String {
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

fn parse_candidate_tool_json(raw: &str, idx: usize, valid_names: &[&str]) -> Option<ToolCall> {
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
