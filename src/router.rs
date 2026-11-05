use crate::config::{byok_catalog, load_user_config, resolve_api_key};
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
    pub spec_badge: String,
}

fn infer_local_model_spec_badge(id: &str) -> String {
    let lower = id.to_ascii_lowercase();
    let quant = if lower.contains("q8") {
        "Q8"
    } else if lower.contains("q6") {
        "Q6"
    } else if lower.contains("q5") {
        "Q5"
    } else {
        "Q4"
    };
    let mut tags = vec![quant];
    if lower.contains("mtp") || lower.contains("cyber-tiel") || lower.contains("sage") {
        tags.push("MTP");
    }
    if lower.contains("sage") || lower.contains("vl") || lower.contains("vision") {
        tags.push("Vision");
    }
    tags.join("·")
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
    let Ok((_, body)) = crate::http::get_text(discovery_url, &[], 5) else {
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
            .get("loaded_context_length")
            .or_else(|| item.get("max_model_len"))
            .or_else(|| item.get("context_length"))
            .or_else(|| item.get("max_context_length"))
            .or_else(|| item.get("n_ctx"))
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        models.push(DiscoveredLocalModel {
            id: id.to_string(),
            loaded: status == "loaded",
            context_window: ctx_win,
            spec_badge: infer_local_model_spec_badge(id),
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

/// Bare host of a base URL, lowercased: scheme, userinfo, path and port removed.
///
/// `https://user@127.0.0.1:8080/v1` → `127.0.0.1`.
pub(crate) fn url_host(base_url: &str) -> String {
    let no_scheme = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let authority = no_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        // IPv6 literal: keep the brackets so `[::1]` stays unambiguous.
        match rest.split_once(']') {
            Some((inner, _)) => return format!("[{}]", inner.to_lowercase()),
            None => host_port,
        }
    } else {
        host_port
            .split_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port)
    };
    host.to_lowercase()
}

impl ModelEndpoint {
    /// True only when the endpoint really is on this machine (or an mDNS `.local` peer).
    ///
    /// This used to be a substring test over the whole URL, so `https://127.0.0.1.evil.com`
    /// or `https://localhost.attacker.net` reported *local*: the escaping logic, prompt
    /// trimming and cache hints would then treat a remote host as a local engine. The host
    /// is parsed out and compared exactly instead.
    pub fn is_local(&self) -> bool {
        if self.is_local_flag {
            return true;
        }
        let host = url_host(&self.base_url);
        host == "localhost"
            || host == "::1"
            || host == "[::1]"
            || host == "0.0.0.0"
            || host
                .parse::<std::net::Ipv4Addr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
            || host.ends_with(".local")
    }
    /// True when the prompt should be trimmed: a local engine, or a fast/flash-tier
    /// model where every saved prefill token is latency the user feels.
    pub fn wants_compact_prompt(&self) -> bool {
        if self.is_local() {
            return true;
        }
        let key = self.model.to_lowercase();
        key.contains("flash")
            || key.contains("mini")
            || key.contains("haiku")
            || key.contains("small")
    }

    /// True when the endpoint speaks the native Anthropic Messages API
    /// (`/v1/messages`, `x-api-key`) rather than the OpenAI chat-completions
    /// protocol. Auto-detected from an `api.anthropic.com` base URL; the
    /// `SPAVN_FORCE_ANTHROPIC` env var overrides for custom Anthropic-protocol
    /// proxies whose host is not anthropic.com.
    pub fn is_anthropic(&self) -> bool {
        if std::env::var("SPAVN_FORCE_ANTHROPIC")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
        {
            return true;
        }
        let base = self.base_url.trim_end_matches('/');
        if base.to_lowercase().contains("anthropic.com") {
            return true;
        }
        load_user_config().providers.iter().any(|p| {
            p.protocol.as_deref() == Some("anthropic") && p.base_url.trim_end_matches('/') == base
        })
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
            if k.contains("256k") || k.contains("262k") {
                return 262_144;
            }
            if k.contains("32k") {
                return 32_768;
            }
            if k.contains("64k") {
                return 65_536;
            }
            return 131_072;
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
    for group in byok_catalog(&cfg) {
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

pub fn base64_encode_bytes(data: &[u8]) -> String {
    base64_encode(data)
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

#[derive(Clone, Debug)]
pub struct DraggedImage {
    pub raw_span: String,
    pub path: String,
    pub filename: String,
    pub mime: &'static str,
    pub size_bytes: usize,
    pub base64: String,
}

static PASTED_IMAGE_CACHE: std::sync::Mutex<Vec<(String, DraggedImage)>> =
    std::sync::Mutex::new(Vec::new());

pub fn register_read_image(path_key: &str, img: DraggedImage) {
    let marker = format!("[SPAVN Vision Image Loaded: {} (", path_key);
    if let Ok(mut cache) = PASTED_IMAGE_CACHE.lock() {
        cache.retain(|(k, _)| k != &marker);
        cache.push((marker, img));
    }
}

/// Collapses any dragged local image file paths inside `input` into compact OMP-style
/// `[🖼️ Screenshot.png (136KB)]` tokens and caches their `DraggedImage` payload for submission!
pub fn collapse_pasted_image_paths(input: &str) -> (String, bool) {
    let found = extract_dragged_images_raw(input);
    if found.is_empty() {
        return (input.to_string(), false);
    }
    let mut updated = input.to_string();
    let mut changed = false;
    if let Ok(mut cache) = PASTED_IMAGE_CACHE.lock() {
        for img in found {
            let kb = (img.size_bytes as f64 / 1024.0).max(1.0);
            let badge = format!("[🖼️ {} ({:.0}KB)]", img.filename, kb);
            if updated.contains(&img.raw_span) {
                updated = updated.replace(&img.raw_span, &badge);
                changed = true;
            }
            if !cache.iter().any(|(b, _)| b == &badge) {
                cache.push((badge, img));
            }
        }
    }
    (updated, changed)
}

/// Detects local image paths dragged into the terminal (including macOS `\ `-escaped paths)
/// OR previously collapsed `[🖼️ ...]` tokens, returning full metadata + base64.
pub fn extract_dragged_images_info(text: &str) -> Vec<DraggedImage> {
    let mut results = extract_dragged_images_raw(text);
    if let Ok(cache) = PASTED_IMAGE_CACHE.lock() {
        for (badge, img) in cache.iter() {
            if text.contains(badge) && !results.iter().any(|r| r.path == img.path) {
                results.push(img.clone());
            }
        }
    }
    results
}

fn extract_dragged_images_raw(text: &str) -> Vec<DraggedImage> {
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
                let raw_slice = &text[start_idx..end_idx];
                let raw_path = raw_slice
                    .trim_matches(|c| c == '\'' || c == '"')
                    .replace("\\ ", " ");
                let expanded = crate::config::expand_user_path(&raw_path);
                if expanded.is_file() {
                    if let Ok(bytes) = std::fs::read(&expanded) {
                        if bytes.len() <= 15 * 1024 * 1024 {
                            let fname = expanded
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| raw_path.clone());
                            results.push(DraggedImage {
                                raw_span: raw_slice.to_string(),
                                path: expanded.to_string_lossy().to_string(),
                                filename: fname,
                                mime,
                                size_bytes: bytes.len(),
                                base64: base64_encode(&bytes),
                            });
                            break;
                        }
                    }
                }
            }
        }
    }
    results
}

fn extract_dragged_images(text: &str) -> Vec<(&'static str, String)> {
    extract_dragged_images_info(text)
        .into_iter()
        .map(|img| (img.mime, img.base64))
        .collect()
}

fn sanitize_messages_for_openai_jinja(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for (idx, m) in messages.iter().enumerate() {
        if m.role == "system" && idx > 0 {
            continue;
        }
        if m.role == "user" {
            if let Some(last) = out.last_mut() {
                if last.role == "user" {
                    let prev = last.content.clone().unwrap_or_default();
                    let cur = m.content.clone().unwrap_or_default();
                    if !cur.is_empty() {
                        last.content = Some(if prev.is_empty() {
                            cur
                        } else {
                            format!("{}\n\n{}", prev, cur)
                        });
                    }
                    continue;
                }
            }
        }
        if m.role == "tool" {
            let valid_prev = out
                .last()
                .map(|p| {
                    p.role == "tool"
                        || (p.role == "assistant"
                            && p.tool_calls
                                .as_ref()
                                .map(|t| !t.is_empty())
                                .unwrap_or(false))
                })
                .unwrap_or(false);
            if !valid_prev {
                continue;
            }
        }
        let mut clean = m.clone();
        if let Some(ref mut tcs) = clean.tool_calls {
            for tc in tcs.iter_mut() {
                if serde_json::from_str::<Value>(&tc.function.arguments)
                    .map(|v| !v.is_object())
                    .unwrap_or(true)
                {
                    tc.function.arguments = "{}".to_string();
                }
            }
        }
        out.push(clean);
    }
    out
}

fn serialize_messages_with_vision(messages: &[ChatMessage]) -> Vec<Value> {
    let sanitized = sanitize_messages_for_openai_jinja(messages);
    let last_user_idx = sanitized.iter().rposition(|m| m.role == "user");
    let mut out_vals = Vec::with_capacity(sanitized.len() + 2);
    let total_len = sanitized.len();
    let mut pending_tool_vision_parts: Vec<Value> = Vec::new();

    for (idx, m) in sanitized.iter().enumerate() {
        let mut v = serde_json::to_value(m).unwrap_or_else(|_| json!({}));
        // Strip non-standard fields that foreign upstreams (OpenCode, OpenAI, Groq) reject across protocols
        if let Some(obj) = v.as_object_mut() {
            obj.remove("thought_signature");
            if let Some(tcs) = obj.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
                for tc in tcs {
                    if let Some(tc_obj) = tc.as_object_mut() {
                        tc_obj.remove("thought_signature");
                    }
                }
            }
        }
        if m.role == "assistant" && v.get("content").is_none() {
            v["content"] = json!("");
        }
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
        out_vals.push(v);

        let in_active_turn = last_user_idx.map(|u| idx >= u).unwrap_or(true);
        if m.role == "tool" && in_active_turn && idx + 6 >= total_len {
            if let Some(txt) = &m.content {
                if txt.starts_with("[SPAVN Vision Image Loaded:") {
                    let imgs = extract_dragged_images(txt);
                    if !imgs.is_empty() {
                        pending_tool_vision_parts.push(json!({
                            "type": "text",
                            "text": format!("[Visual content of tool result `{}`]:", txt)
                        }));
                        for (mime, b64) in imgs {
                            pending_tool_vision_parts.push(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", mime, b64)
                                }
                            }));
                        }
                    }
                }
            }
        }

        let next_is_tool = sanitized
            .get(idx + 1)
            .map(|n| n.role == "tool")
            .unwrap_or(false);
        if !next_is_tool && !pending_tool_vision_parts.is_empty() {
            let parts = std::mem::take(&mut pending_tool_vision_parts);
            out_vals.push(json!({
                "role": "user",
                "content": parts
            }));
        }
    }
    out_vals
}

/// Appends the server's `Retry-After` (seconds form) to the error text as `[retry-after=N]`.
fn retry_after_marker(response: &ureq::http::Response<ureq::Body>) -> String {
    response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|n| format!(" [retry-after={}]", n))
        .unwrap_or_default()
}

fn parse_http_status(err: &str) -> Option<u16> {
    let rest = &err[err.find("HTTP ")? + 5..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn parse_retry_after_secs(err: &str) -> Option<u64> {
    let rest = &err[err.find("[retry-after=")? + 13..];
    rest.split(']').next()?.parse().ok()
}

fn is_retryable_upstream_error(err: &str) -> bool {
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

fn strip_recovered_tool_markup(text: &str) -> String {
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

fn recover_inline_tool_calls(text: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let valid_names = [
        "read",
        "edit",
        "write",
        "bash",
        "search",
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

fn parse_xml_tagged_tool_calls(text: &str, valid_names: &[&str]) -> Vec<ToolCall> {
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

fn merge_consecutive_same_role(mut blocks: Vec<Value>) -> Vec<Value> {
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

fn split_and_convert_messages(messages: &[ChatMessage]) -> (String, Vec<Value>) {
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
                    if Some(idx) == last_user_idx {
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

fn build_anthropic_payload(
    endpoint: &ModelEndpoint,
    messages: &[ChatMessage],
    tools: &Value,
) -> Result<Value, String> {
    let (system, converted) = split_and_convert_messages(messages);
    let mut payload = json!({
        "model": endpoint.model,
        "max_tokens": 16384,
        "messages": converted,
        "stream": true,
    });
    if !system.trim().is_empty() {
        payload["system"] = json!(system);
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
                                crate::hud::notify_tool_call_streaming(&slot.1, slot.2.len());
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
