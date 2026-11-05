pub mod anthropic;
pub mod openai;
pub mod vision;

pub use anthropic::stream_anthropic;
pub(crate) use openai::*;
pub use openai::{stream_chat_completion, stream_openai};
pub(crate) use vision::*;
pub use vision::{
    base64_encode_bytes, collapse_pasted_image_paths, extract_dragged_images_info,
    register_read_image, DraggedImage,
};

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

pub(crate) fn infer_local_model_spec_badge(id: &str) -> String {
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

pub(crate) static LOCAL_MODELS_CACHE: std::sync::Mutex<
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
