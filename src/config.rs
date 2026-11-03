use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

fn set_owner_only_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

static LANG_EN: AtomicBool = AtomicBool::new(false);

pub fn is_en() -> bool {
    LANG_EN.load(Ordering::Relaxed)
}

pub fn set_lang_en(en: bool) {
    LANG_EN.store(en, Ordering::Relaxed);
}

#[inline]
pub fn tr_en<'a>(tr: &'a str, en: &'a str) -> &'a str {
    if is_en() {
        en
    } else {
        tr
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelConfigEntry {
    pub key: String,
    pub model_id: String,
    pub label: String,
    pub badge: String,
    pub desc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProviderGroupConfig {
    pub id: String,
    pub title: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub is_local: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    pub models: Vec<ModelConfigEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RemoteHostConfig {
    pub key: String,
    pub ssh_target: String,
    pub badge: String,
    pub desc: String,
    #[serde(default)]
    pub sensitive: bool,
}

/// A host is sensitive when commands run on it deserve an explicit approval in
/// AUTO mode: either the config marks it `sensitive`, or it executes remotely
/// (`ssh_target != "local"`). Unknown non-local aliases are treated as
/// sensitive by default; `local` never is.
pub fn host_is_sensitive(host: &str) -> bool {
    let cfg = load_user_config();
    host_is_sensitive_in(host, &cfg.hosts)
}

pub fn host_is_sensitive_in(host: &str, hosts: &[RemoteHostConfig]) -> bool {
    if host.is_empty() || host == "local" {
        return false;
    }
    match hosts.iter().find(|h| h.key.eq_ignore_ascii_case(host)) {
        Some(h) => h.sensitive || h.ssh_target != "local",
        None => true,
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;

    fn h(key: &str, ssh: &str, sensitive: bool) -> RemoteHostConfig {
        RemoteHostConfig {
            key: key.to_string(),
            ssh_target: ssh.to_string(),
            badge: String::new(),
            desc: String::new(),
            sensitive,
        }
    }

    #[test]
    fn local_is_never_sensitive() {
        let hosts = vec![h("local", "local", false), h("weird", "local", true)];
        assert!(!host_is_sensitive_in("local", &hosts));
        assert!(!host_is_sensitive_in("", &hosts));
    }

    #[test]
    fn remote_host_is_sensitive_by_default() {
        let hosts = vec![h("prod", "user@1.2.3.4", false)];
        assert!(host_is_sensitive_in("prod", &hosts));
    }

    #[test]
    fn explicit_sensitive_flag_wins_on_local_ssh() {
        let hosts = vec![h("weird", "local", true)];
        assert!(host_is_sensitive_in("weird", &hosts));
    }

    #[test]
    fn unknown_nonlocal_alias_is_sensitive() {
        let hosts = vec![h("local", "local", false)];
        assert!(host_is_sensitive_in("mystery", &hosts));
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn anthropic_short_aliases_resolve() {
        let catalog = builtin_byok_catalog();
        let anthropic = catalog
            .iter()
            .find(|g| g.id == "anthropic")
            .expect("anthropic group present");
        let by_key = |k: &str| anthropic.models.iter().find(|m| m.key == k);
        assert_eq!(
            by_key("sonnet").map(|m| m.model_id.as_str()),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(
            by_key("opus").map(|m| m.model_id.as_str()),
            Some("claude-opus-4-6")
        );
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SpavnUserConfig {
    #[serde(default = "default_model_key")]
    pub default_model: String,
    #[serde(default = "default_model_key")]
    pub fallback_cloud_model: String,
    #[serde(default = "default_busy_mode")]
    pub busy_input_mode: String, // "steer" | "queue"
    #[serde(default = "default_language")]
    pub language: String, // "tr" | "en"
    pub local_discovery_url: Option<String>,
    pub local_base_url: Option<String>,
    pub quota_quick_script: Option<String>,
    pub quota_full_script: Option<String>,
    pub memory_recall_bin: Option<String>,
    pub memory_remember_bin: Option<String>,
    #[serde(default)]
    pub protected_paths: Vec<String>,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    #[serde(default)]
    pub providers: Vec<ProviderGroupConfig>,
    #[serde(default = "default_hosts")]
    pub hosts: Vec<RemoteHostConfig>,
}

fn default_model_key() -> String {
    std::env::var("SPAVN_MODEL").unwrap_or_else(|_| "sonnet".to_string())
}

fn default_busy_mode() -> String {
    "queue".to_string()
}

fn default_language() -> String {
    std::env::var("SPAVN_LANG")
        .ok()
        .map(|s| {
            if s.to_lowercase().starts_with("en") {
                "en".to_string()
            } else {
                "tr".to_string()
            }
        })
        .unwrap_or_else(|| "tr".to_string())
}

pub fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn spavn_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("SPAVN_HOME") {
        let p = PathBuf::from(custom);
        let _ = fs::create_dir_all(&p);
        set_owner_only_mode(&p, 0o700);
        return p;
    }
    let dir = home_dir().join(".spavn");
    let _ = fs::create_dir_all(&dir);
    set_owner_only_mode(&dir, 0o700);
    dir
}

pub fn secrets_env_path() -> PathBuf {
    spavn_dir().join("secrets.env")
}

/// Returns an available user shell. `SPAVN_SHELL` wins, then `$SHELL`, then the
/// first existing fallback (POSIX shells on Unix, PowerShell/cmd on Windows).
pub fn shell_bin() -> String {
    for candidate in [
        std::env::var("SPAVN_SHELL").ok(),
        std::env::var("SHELL").ok(),
    ]
    .into_iter()
    .flatten()
    {
        if !candidate.trim().is_empty() && PathBuf::from(&candidate).exists() {
            return candidate;
        }
    }
    #[cfg(windows)]
    {
        for candidate in [
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Windows\System32\cmd.exe",
        ] {
            if PathBuf::from(candidate).exists() {
                return candidate.to_string();
            }
        }
        return "powershell.exe".to_string();
    }
    #[cfg(not(windows))]
    {
        for candidate in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
            if PathBuf::from(candidate).exists() {
                return candidate.to_string();
            }
        }
        "sh".to_string()
    }
}

pub fn expand_user_path(raw: &str) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix(r"~\")) {
        return home_dir().join(stripped);
    }
    PathBuf::from(raw)
}

/// Reads `~/.spavn/secrets.env` (ported from Spavn IDE `readSpavnSecrets()`).
pub fn read_spavn_secrets() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(raw) = fs::read_to_string(secrets_env_path()) else {
        return map;
    };
    for line in raw.lines() {
        let trimmed = line.trim().trim_start_matches("export ").trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim().to_string();
            let val = v
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim_start_matches("Bearer ")
                .trim()
                .to_string();
            if !key.is_empty() && !val.is_empty() {
                map.insert(key, val);
            }
        }
    }
    map
}

/// Upserts an API key into `~/.spavn/secrets.env` with `0600` permissions (ported from Spavn IDE `upsertSecret()`).
pub fn upsert_spavn_secret(env_name: &str, value: &str) -> Result<(), String> {
    let clean_key = env_name.trim();
    let clean_val = value
        .trim()
        .trim_start_matches("Bearer ")
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if clean_key.is_empty() || clean_val.is_empty() {
        return Err("API anahtarı boş olamaz.".to_string());
    }
    let path = secrets_env_path();
    let existing = fs::read_to_string(&path).unwrap_or_else(|_| {
        "# Spavn BYOK Provider Keys (0600 - Never committed to git)\n".to_string()
    });
    let assignment = format!(
        "export {}=\"{}\"",
        clean_key,
        clean_val.replace('"', "\\\"")
    );
    let mut replaced = false;
    let mut lines: Vec<String> = existing
        .lines()
        .map(|l| {
            let t = l.trim().trim_start_matches("export ").trim();
            if t.starts_with(&format!("{}=", clean_key)) {
                replaced = true;
                assignment.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !replaced {
        lines.push(assignment);
    }
    let body = format!("{}\n", lines.join("\n"));
    fs::write(&path, body).map_err(|e| e.to_string())?;
    set_owner_only_mode(&path, 0o600);
    Ok(())
}

/// Resolves an API key from process env or `~/.spavn/secrets.env`.
pub fn resolve_api_key(env_name: &str) -> Option<String> {
    if env_name.is_empty() {
        return Some("local".to_string());
    }
    let clean = env_name
        .trim()
        .trim_start_matches("env:")
        .trim_start_matches("${")
        .trim_end_matches('}');
    if let Ok(v) = std::env::var(clean) {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    read_spavn_secrets().remove(clean)
}

/// Built-in 12-Provider BYOK Catalog ported from `spavn/src/catalog.ts` & `spavn-editor`.
pub fn builtin_byok_catalog() -> Vec<ProviderGroupConfig> {
    vec![
        ProviderGroupConfig {
            id: "openai".to_string(),
            title: "🟡 OpenAI (BYOK)".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            is_local: false,
            context_window: Some(200_000),
            models: vec![
                ModelConfigEntry {
                    key: "gpt-5.4".to_string(),
                    model_id: "gpt-5.4".to_string(),
                    label: "OpenAI GPT-5.4".to_string(),
                    badge: "🟡 Flagship".to_string(),
                    desc: "OpenAI GPT-5.4".to_string(),
                    context_window: Some(200_000),
                },
                ModelConfigEntry {
                    key: "gpt-5.2".to_string(),
                    model_id: "gpt-5.2".to_string(),
                    label: "OpenAI GPT-5.2".to_string(),
                    badge: "🟡 Coding".to_string(),
                    desc: "OpenAI GPT-5.2".to_string(),
                    context_window: Some(200_000),
                },
                ModelConfigEntry {
                    key: "gpt-4.1".to_string(),
                    model_id: "gpt-4.1".to_string(),
                    label: "OpenAI GPT-4.1".to_string(),
                    badge: "⚡ Fast".to_string(),
                    desc: "OpenAI GPT-4.1".to_string(),
                    context_window: Some(1_000_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "anthropic".to_string(),
            title: "🟣 Anthropic Claude (BYOK)".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            is_local: false,
            context_window: Some(200_000),
            models: vec![
                ModelConfigEntry {
                    key: "claude-opus-4-6".to_string(),
                    model_id: "claude-opus-4-6".to_string(),
                    label: "Claude Opus 4.6".to_string(),
                    badge: "🧠 Deep".to_string(),
                    desc: "Anthropic Claude Opus 4.6".to_string(),
                    context_window: Some(200_000),
                },
                ModelConfigEntry {
                    key: "claude-sonnet-4-6".to_string(),
                    model_id: "claude-sonnet-4-6".to_string(),
                    label: "Claude Sonnet 4.6".to_string(),
                    badge: "⚡ Balanced".to_string(),
                    desc: "Anthropic Claude Sonnet 4.6".to_string(),
                    context_window: Some(200_000),
                },
                ModelConfigEntry {
                    key: "sonnet".to_string(),
                    model_id: "claude-sonnet-4-6".to_string(),
                    label: "Sonnet (kısa ad)".to_string(),
                    badge: "⚡ Balanced".to_string(),
                    desc: "Alias → claude-sonnet-4-6".to_string(),
                    context_window: Some(200_000),
                },
                ModelConfigEntry {
                    key: "opus".to_string(),
                    model_id: "claude-opus-4-6".to_string(),
                    label: "Opus (kısa ad)".to_string(),
                    badge: "🧠 Deep".to_string(),
                    desc: "Alias → claude-opus-4-6".to_string(),
                    context_window: Some(200_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "gemini".to_string(),
            title: "🔵 Google Gemini (BYOK)".to_string(),
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            api_key_env: "GEMINI_API_KEY".to_string(),
            is_local: false,
            context_window: Some(1_000_000),
            models: vec![
                ModelConfigEntry {
                    key: "gemini-2.5-pro".to_string(),
                    model_id: "gemini-2.5-pro".to_string(),
                    label: "Gemini 2.5 Pro".to_string(),
                    badge: "🔵 Pro".to_string(),
                    desc: "Google Gemini 2.5 Pro".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "gemini-2.5-flash".to_string(),
                    model_id: "gemini-2.5-flash".to_string(),
                    label: "Gemini 2.5 Flash".to_string(),
                    badge: "🚀 Flash".to_string(),
                    desc: "Google Gemini 2.5 Flash".to_string(),
                    context_window: Some(1_000_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "openrouter".to_string(),
            title: "🌐 OpenRouter (BYOK)".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            api_key_env: "OPENROUTER_API_KEY".to_string(),
            is_local: false,
            context_window: Some(200_000),
            models: vec![ModelConfigEntry {
                key: "openrouter/auto".to_string(),
                model_id: "openrouter/auto".to_string(),
                label: "OpenRouter Auto".to_string(),
                badge: "🌐 Auto".to_string(),
                desc: "OpenRouter Smart Router".to_string(),
                context_window: Some(200_000),
            }],
        },
        ProviderGroupConfig {
            id: "deepseek".to_string(),
            title: "🐋 DeepSeek (BYOK)".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
            is_local: false,
            context_window: Some(128_000),
            models: vec![
                ModelConfigEntry {
                    key: "deepseek-chat".to_string(),
                    model_id: "deepseek-chat".to_string(),
                    label: "DeepSeek V3".to_string(),
                    badge: "🐋 Chat".to_string(),
                    desc: "DeepSeek Chat V3".to_string(),
                    context_window: Some(128_000),
                },
                ModelConfigEntry {
                    key: "deepseek-reasoner".to_string(),
                    model_id: "deepseek-reasoner".to_string(),
                    label: "DeepSeek R1".to_string(),
                    badge: "🧠 R1".to_string(),
                    desc: "DeepSeek Reasoner R1".to_string(),
                    context_window: Some(128_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "minimax".to_string(),
            title: "🟠 MiniMax (BYOK)".to_string(),
            base_url: "https://api.minimax.io/v1".to_string(),
            api_key_env: "MINIMAX_API_KEY".to_string(),
            is_local: false,
            context_window: Some(200_000),
            models: vec![
                ModelConfigEntry {
                    key: "MiniMax-M3".to_string(),
                    model_id: "MiniMax-M3".to_string(),
                    label: "MiniMax M3".to_string(),
                    badge: "🟠 M3".to_string(),
                    desc: "MiniMax M3 Reasoning".to_string(),
                    context_window: Some(200_000),
                },
                ModelConfigEntry {
                    key: "MiniMax-M2.7".to_string(),
                    model_id: "MiniMax-M2.7".to_string(),
                    label: "MiniMax M2.7".to_string(),
                    badge: "🟠 M2.7".to_string(),
                    desc: "MiniMax M2.7 Coding".to_string(),
                    context_window: Some(200_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "glm".to_string(),
            title: "🟢 Zhipu GLM (BYOK)".to_string(),
            base_url: "https://open.bigmodel.cn/api/paas/v4".to_string(),
            api_key_env: "GLM_API_KEY".to_string(),
            is_local: false,
            context_window: Some(128_000),
            models: vec![
                ModelConfigEntry {
                    key: "glm-5.1".to_string(),
                    model_id: "glm-5.1".to_string(),
                    label: "GLM-5.1".to_string(),
                    badge: "🟢 GLM-5.1".to_string(),
                    desc: "Zhipu GLM-5.1".to_string(),
                    context_window: Some(128_000),
                },
                ModelConfigEntry {
                    key: "glm-4.7".to_string(),
                    model_id: "glm-4.7".to_string(),
                    label: "GLM-4.7".to_string(),
                    badge: "🟢 GLM-4.7".to_string(),
                    desc: "Zhipu GLM-4.7".to_string(),
                    context_window: Some(128_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "groq".to_string(),
            title: "⚡ Groq LPU (BYOK)".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key_env: "GROQ_API_KEY".to_string(),
            is_local: false,
            context_window: Some(128_000),
            models: vec![ModelConfigEntry {
                key: "llama-3.3-70b-versatile".to_string(),
                model_id: "llama-3.3-70b-versatile".to_string(),
                label: "Llama 3.3 70B (Groq)".to_string(),
                badge: "⚡ Ultra-Fast".to_string(),
                desc: "Groq LPU Inference".to_string(),
                context_window: Some(128_000),
            }],
        },
        ProviderGroupConfig {
            id: "xai".to_string(),
            title: "✖️  xAI Grok (BYOK)".to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            api_key_env: "XAI_API_KEY".to_string(),
            is_local: false,
            context_window: Some(256_000),
            models: vec![ModelConfigEntry {
                key: "grok-4".to_string(),
                model_id: "grok-4".to_string(),
                label: "xAI Grok 4".to_string(),
                badge: "✖️  Grok 4".to_string(),
                desc: "xAI Grok 4".to_string(),
                context_window: Some(256_000),
            }],
        },
        ProviderGroupConfig {
            id: "microsoft-github".to_string(),
            title: "🪟 Microsoft / GitHub Models (BYOK)".to_string(),
            base_url: "https://models.inference.ai.azure.com".to_string(),
            api_key_env: "GITHUB_TOKEN".to_string(),
            is_local: false,
            context_window: Some(128_000),
            models: vec![
                ModelConfigEntry {
                    key: "github:gpt-4.1".to_string(),
                    model_id: "gpt-4.1".to_string(),
                    label: "GitHub Models GPT-4.1".to_string(),
                    badge: "🪟 Azure/GitHub".to_string(),
                    desc: "Microsoft Azure AI Inference".to_string(),
                    context_window: Some(128_000),
                },
                ModelConfigEntry {
                    key: "github:phi-4".to_string(),
                    model_id: "Phi-4".to_string(),
                    label: "Microsoft Phi-4".to_string(),
                    badge: "🪟 Microsoft Research".to_string(),
                    desc: "Microsoft Phi-4 Reasoning".to_string(),
                    context_window: Some(128_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "ollama".to_string(),
            title: "🦙 Ollama / LM Studio (Local)".to_string(),
            base_url: "http://127.0.0.1:11434/v1".to_string(),
            api_key_env: String::new(),
            is_local: true,
            context_window: Some(32_768),
            models: vec![ModelConfigEntry {
                key: "ollama:qwen2.5-coder".to_string(),
                model_id: "qwen2.5-coder:32b".to_string(),
                label: "Ollama Qwen2.5-Coder".to_string(),
                badge: "🦙 Local".to_string(),
                desc: "127.0.0.1:11434".to_string(),
                context_window: Some(32_768),
            }],
        },
    ]
}

fn default_hosts() -> Vec<RemoteHostConfig> {
    vec![RemoteHostConfig {
        key: "local".to_string(),
        ssh_target: "local".to_string(),
        badge: "💻 Local Workspace".to_string(),
        desc: "Local development machine".to_string(),
        sensitive: false,
    }]
}

pub fn load_user_config() -> SpavnUserConfig {
    let cfg_path = spavn_dir().join("config.json");
    if let Ok(raw) = fs::read_to_string(&cfg_path) {
        set_owner_only_mode(&cfg_path, 0o600);
        if let Ok(parsed) = serde_json::from_str::<SpavnUserConfig>(&raw) {
            set_lang_en(parsed.language.eq_ignore_ascii_case("en"));
            return parsed;
        }
    }
    let lang = default_language();
    set_lang_en(lang.eq_ignore_ascii_case("en"));
    SpavnUserConfig {
        default_model: default_model_key(),
        fallback_cloud_model: default_model_key(),
        busy_input_mode: default_busy_mode(),
        language: lang,
        local_discovery_url: std::env::var("SPAVN_LOCAL_MODELS_URL").ok(),
        local_base_url: std::env::var("SPAVN_LOCAL_BASE_URL").ok(),
        quota_quick_script: None,
        quota_full_script: None,
        memory_recall_bin: None,
        memory_remember_bin: None,
        protected_paths: Vec::new(),
        allowed_paths: Vec::new(),
        providers: Vec::new(),
        hosts: default_hosts(),
    }
}

pub fn save_user_config(cfg: &SpavnUserConfig) -> Result<(), String> {
    set_lang_en(cfg.language.eq_ignore_ascii_case("en"));
    let cfg_path = spavn_dir().join("config.json");
    let json_str = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(&cfg_path, format!("{}\n", json_str)).map_err(|e| e.to_string())?;
    set_owner_only_mode(&cfg_path, 0o600);
    Ok(())
}

pub fn load_user_profile_markdown() -> Option<String> {
    let profile_path = spavn_dir().join("PROFILE.md");
    if let Ok(raw) = fs::read_to_string(profile_path) {
        let trimmed = raw.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}
