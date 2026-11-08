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

pub fn derive_base_and_discovery_urls(raw_url: &str) -> (String, String) {
    let trimmed = raw_url.trim().trim_end_matches('/');
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{}", trimmed)
    };
    if let Some(base) = with_scheme.strip_suffix("/v1/models") {
        (format!("{}/v1", base), with_scheme)
    } else if let Some(base) = with_scheme.strip_suffix("/models") {
        (base.to_string(), with_scheme)
    } else if with_scheme.ends_with("/v1") {
        (with_scheme.clone(), format!("{}/models", with_scheme))
    } else {
        (
            format!("{}/v1", with_scheme),
            format!("{}/v1/models", with_scheme),
        )
    }
}

fn local_tcp_port_reachable(url: &str, timeout_ms: u64) -> bool {
    let no_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = no_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    let Some((host, port_str)) = host_port.rsplit_once(':') else {
        return true;
    };
    let Ok(port) = port_str.parse::<u16>() else {
        return true;
    };
    let clean_host = host.trim_matches(['[', ']']);
    let ip: std::net::IpAddr = if clean_host.eq_ignore_ascii_case("localhost") {
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    } else if let Ok(parsed) = clean_host.parse() {
        parsed
    } else {
        return true;
    };
    let addr = std::net::SocketAddr::new(ip, port);
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(timeout_ms))
        .is_ok()
}

pub fn probe_local_endpoints(
    configured_discovery_url: Option<&str>,
) -> Option<(String, String, Vec<DiscoveredLocalModel>)> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(u) = configured_discovery_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        candidates.push(u.to_string());
    }
    for std_url in [
        "http://127.0.0.1:11434/v1/models",
        "http://127.0.0.1:1234/v1/models",
        "http://127.0.0.1:8080/v1/models",
        "http://127.0.0.1:8000/v1/models",
        "http://127.0.0.1:4000/v1/models",
        "http://127.0.0.1:9931/v1/models",
    ] {
        if !candidates.iter().any(|c| c.eq_ignore_ascii_case(std_url)) {
            candidates.push(std_url.to_string());
        }
    }
    for cand in candidates {
        if !local_tcp_port_reachable(&cand, 120) {
            continue;
        }
        let (base_url, disc_url) = derive_base_and_discovery_urls(&cand);
        let models = discover_local_models(&disc_url, true);
        if !models.is_empty() {
            return Some((base_url, disc_url, models));
        }
    }
    None
}

pub fn is_likely_chat_model_id(id: &str) -> bool {
    let low = id.trim().to_ascii_lowercase();
    if low.is_empty() {
        return false;
    }
    let non_chat_markers = [
        "embedding",
        "tts-",
        "-tts",
        "whisper",
        "dall-e",
        "moderation",
        "babbage-002",
        "davinci-002",
        "rerank",
        "transcribe",
        "realtime-preview",
        "audio-preview",
        "canary-",
        "prompt-guard",
        "llama-guard",
    ];
    !non_chat_markers.iter().any(|m| low.contains(m))
}

pub fn fetch_remote_models(
    base_url: &str,
    api_key: Option<&str>,
    timeout_secs: u64,
) -> Vec<String> {
    let raw_trimmed = base_url.trim().trim_end_matches('/');
    if raw_trimmed.is_empty() {
        return Vec::new();
    }
    let trimmed = raw_trimmed
        .strip_suffix("/chat/completions")
        .or_else(|| raw_trimmed.strip_suffix("/messages"))
        .unwrap_or(raw_trimmed)
        .trim_end_matches('/');
    if trimmed.starts_with("http://127.0.0.1")
        || trimmed.starts_with("http://localhost")
        || trimmed.starts_with("http://[::1]")
    {
        if !local_tcp_port_reachable(trimmed, 150) {
            return Vec::new();
        }
    }
    let primary_url = if trimmed.ends_with("/models") {
        trimmed.to_string()
    } else {
        format!("{}/models", trimmed)
    };
    let clean_key = api_key
        .map(str::trim)
        .filter(|k| !k.is_empty() && *k != "local");
    let is_anthropic = trimmed.to_ascii_lowercase().contains("anthropic.com");
    let auth_header = clean_key.map(|k| format!("Bearer {}", k));
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(k) = clean_key {
        if is_anthropic {
            headers.push(("x-api-key", k));
            headers.push(("anthropic-version", "2023-06-01"));
        } else if let Some(ref bearer) = auth_header {
            headers.push(("Authorization", bearer.as_str()));
        }
    }

    let parse_models_body = |body: &str| -> Vec<String> {
        let Ok(val) = serde_json::from_str::<Value>(body) else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        if let Some(arr) = val.get("data").and_then(|d| d.as_array()) {
            for item in arr {
                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                    let clean_id = id.trim().strip_prefix("models/").unwrap_or(id.trim());
                    if is_likely_chat_model_id(clean_id) && !out.iter().any(|x| x == clean_id) {
                        out.push(clean_id.to_string());
                    }
                }
            }
        } else if let Some(arr) = val.get("models").and_then(|d| d.as_array()) {
            for item in arr {
                let id_opt = item
                    .get("id")
                    .or_else(|| item.get("name"))
                    .or_else(|| item.get("model"))
                    .and_then(|v| v.as_str());
                if let Some(id) = id_opt {
                    let clean_id = id.trim().strip_prefix("models/").unwrap_or(id.trim());
                    if is_likely_chat_model_id(clean_id) && !out.iter().any(|x| x == clean_id) {
                        out.push(clean_id.to_string());
                    }
                }
            }
        }
        out
    };

    if let Ok((status, body)) = crate::http::get_text(&primary_url, &headers, timeout_secs) {
        if (200..300).contains(&status) {
            let parsed = parse_models_body(&body);
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }
    if !trimmed.ends_with("/v1") && !trimmed.contains("/v1/") && !trimmed.ends_with("/models") {
        let fallback_url = format!("{}/v1/models", trimmed);
        if let Ok((status, body)) = crate::http::get_text(&fallback_url, &headers, timeout_secs) {
            if (200..300).contains(&status) {
                let parsed = parse_models_body(&body);
                if !parsed.is_empty() {
                    return parsed;
                }
            }
        }
    }
    // Ollama native `/api/tags` fallback when `/v1/models` is unavailable
    if trimmed.contains(":11434") || trimmed.contains("ollama") {
        let root = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
        let tags_url = format!("{}/api/tags", root);
        if let Ok((status, body)) = crate::http::get_text(&tags_url, &headers, timeout_secs) {
            if (200..300).contains(&status) {
                let parsed = parse_models_body(&body);
                if !parsed.is_empty() {
                    return parsed;
                }
            }
        }
    }
    Vec::new()
}

/// Fetches `https://models.dev/api.json` (like OMP's `mapModelsDevToModels`) and caches
/// tool-capable chat models (`id`, `label`, `context_window`) per provider in `~/.spavn/models_dev_cache.json`.
pub fn sync_models_dev_catalog() -> usize {
    let Ok((status, body)) = crate::http::get_text("https://models.dev/api.json", &[], 6) else {
        return 0;
    };
    if !(200..300).contains(&status) {
        return 0;
    }
    let Ok(root) = serde_json::from_str::<Value>(&body) else {
        return 0;
    };
    let Some(obj) = root.as_object() else {
        return 0;
    };
    let provider_map = [
        ("anthropic", "anthropic"),
        ("openai", "openai"),
        ("google", "gemini"),
        ("openrouter", "openrouter"),
        ("deepseek", "deepseek"),
        ("zai", "zai"),
        ("zhipuai", "glm"),
        ("xai", "xai"),
        ("groq", "groq"),
        ("moonshotai", "moonshot"),
        ("minimax", "minimax"),
        ("cerebras", "cerebras"),
        ("fireworks-ai", "fireworks"),
        ("togetherai", "together"),
        ("sambanova", "sambanova"),
        ("siliconflow", "siliconflow"),
        ("deepinfra", "deepinfra"),
        ("mistral", "mistral"),
        ("nvidia", "nvidia"),
        ("huggingface", "huggingface"),
        ("baseten", "baseten"),
        ("novita-ai", "novita"),
        ("venice", "venice"),
        ("github-models", "microsoft-github"),
        ("opencode", "opencode-zen"),
    ];
    let mut cache: std::collections::HashMap<String, Vec<crate::config::ModelConfigEntry>> =
        std::collections::HashMap::new();
    let mut total_models = 0usize;
    for (src_id, dst_id) in provider_map {
        let Some(models_obj) = obj
            .get(src_id)
            .and_then(|p| p.get("models"))
            .and_then(|m| m.as_object())
        else {
            continue;
        };
        let mut entries: Vec<crate::config::ModelConfigEntry> = Vec::new();
        for (model_id, meta) in models_obj {
            if !is_likely_chat_model_id(model_id) {
                continue;
            }
            if meta.get("tool_call").and_then(|v| v.as_bool()) == Some(false) {
                continue;
            }
            let name = meta
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(model_id);
            let ctx = meta
                .get("limit")
                .and_then(|l| l.get("context"))
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let reasoning = meta
                .get("reasoning")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let badge = if reasoning {
                "🧠 Reasoning".to_string()
            } else {
                "🌐 Catalog".to_string()
            };
            entries.push(crate::config::ModelConfigEntry {
                key: format!("{}:{}", dst_id, model_id),
                model_id: model_id.clone(),
                label: name.to_string(),
                badge,
                desc: format!(
                    "{}{}",
                    name,
                    ctx.map(|c| format!(" · {}k ctx", c / 1000))
                        .unwrap_or_default()
                ),
                context_window: ctx,
            });
            total_models += 1;
        }
        if !entries.is_empty() {
            cache.insert(dst_id.to_string(), entries);
        }
    }
    if total_models > 0 {
        crate::config::save_models_dev_cache(&cache);
    }
    total_models
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
        if self.has_quota_pool {
            return false;
        }
        if !self.model.is_empty() {
            let k = self.model.to_lowercase();
            if k.contains("gemini")
                || k.contains("argon")
                || k.contains("claude")
                || k.contains("opus")
                || k.contains("sonnet")
                || k.contains("haiku")
                || k.contains("fable")
                || k.contains("gpt-4")
                || k.contains("gpt-5")
                || k.contains("gpt-6")
                || k.contains("grok-")
                || k.contains("glm-5")
            {
                return false;
            }
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

    /// True for the cheap/free tiers where long round-trips (LLM context compaction, rich
    /// tool schemas) cost more than the tokens they save.
    pub fn is_low_cost_tier(&self) -> bool {
        let key = self.model.to_lowercase();
        key.contains("free") || key.contains("flash") || self.key.to_lowercase().contains("free")
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
        if k.contains("gpt-6") || k.contains("gpt-5.6") || k.contains("gpt-5.5") {
            1_050_000
        } else if k.contains("gemini")
            || k.contains("argon")
            || k.contains("gpt-4.1")
            || k.contains("deepseek-v4")
            || k.contains("deepseek-flash")
            || k.contains("glm-5.3")
            || k.contains("haiku-5")
            || ((k.contains("claude")
                || k.contains("opus")
                || k.contains("sonnet")
                || k.contains("fable"))
                && !k.contains("haiku"))
        {
            1_000_000
        } else if k.contains("gpt-5.4") {
            272_000
        } else if k.contains("grok-4") {
            256_000
        } else if k.contains("haiku")
            || k.contains("minimax")
            || k.contains("gpt-5")
            || k.contains("glm-5.1")
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
    // If `provider_id:model_key` is explicitly passed, resolve inside that exact provider first.
    if let Some((prov_prefix, custom_model)) = clean.split_once(':') {
        for group in &cfg.providers {
            if group.id.eq_ignore_ascii_case(prov_prefix) {
                let group_has_pool =
                    !group.is_local && group.api_key_env.is_empty() && has_user_quota_script;
                let api_key =
                    resolve_api_key(&group.api_key_env).unwrap_or_else(|| "local".to_string());
                for m in &group.models {
                    if m.key.eq_ignore_ascii_case(custom_model)
                        || m.model_id.eq_ignore_ascii_case(custom_model)
                    {
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
    for match_by_key_only in [true, false] {
        for group in &cfg.providers {
            let group_has_pool =
                !group.is_local && group.api_key_env.is_empty() && has_user_quota_script;
            for m in &group.models {
                let matched = if match_by_key_only {
                    m.key.eq_ignore_ascii_case(clean)
                } else {
                    m.model_id.eq_ignore_ascii_case(clean)
                };
                if matched {
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
        }
    }

    // 3. Check built-in BYOK catalog providers (OpenAI, Anthropic, Gemini, DeepSeek, MiniMax, GLM, Groq, xAI, OpenRouter, Ollama)
    let catalog = byok_catalog(&cfg);
    if let Some((prov_prefix, custom_model)) = clean.split_once(':') {
        let prov_low = prov_prefix.to_ascii_lowercase();
        let canonical_prov = match prov_low.as_str() {
            "zen" | "opencode" => "opencode-zen",
            "github" | "copilot" => "microsoft-github",
            "google" => "gemini",
            "zhipu" | "zhipu-coding-plan" => "glm",
            "lmstudio" => "lm-studio",
            "llamacpp" => "llama.cpp",
            other => other,
        };
        for group in &catalog {
            if group.id.eq_ignore_ascii_case(prov_prefix)
                || group.id.eq_ignore_ascii_case(canonical_prov)
            {
                let api_key =
                    resolve_api_key(&group.api_key_env).unwrap_or_else(|| "local".to_string());
                for m in &group.models {
                    if m.key.eq_ignore_ascii_case(custom_model)
                        || m.key.eq_ignore_ascii_case(clean)
                        || m.model_id.eq_ignore_ascii_case(custom_model)
                    {
                        return ModelEndpoint {
                            key: format!("{}:{}", group.id, m.key),
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
    for group in catalog {
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
