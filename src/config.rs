use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub fn set_owner_only_mode(path: &Path, mode: u32) {
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

static LANG_EN: AtomicBool = AtomicBool::new(true);

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

/// Headless CLI (`spavn update`) yanıtlarının dili: doğrudan OS dilini yansıtır.
/// macOS'ta arayüz dili `AppleLanguages`'ta tutulur (`LANG` yalnızca bölgeyi taşır,
/// ör. İngilizce arayüz + `tr_TR.UTF-8`), Linux/terminalde LC_ALL/LANG yeterlidir.
/// `SPAVN_LANG` her ikisini ezer; config.json dili yalnızca etkileşimli oturumda geçerlidir.
pub fn detect_os_lang_en() -> bool {
    if let Ok(s) = std::env::var("SPAVN_LANG") {
        return s.to_lowercase().starts_with("en");
    }
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
    {
        if let Ok(text) = String::from_utf8(out.stdout) {
            for line in text.lines() {
                let entry = line.trim().trim_matches(|c| c == '"' || c == ',');
                if entry.is_empty() || entry.starts_with('(') || entry.starts_with(')') {
                    continue;
                }
                if let Some(code) = entry.split('-').next() {
                    return code.eq_ignore_ascii_case("en");
                }
            }
        }
    }
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .map(|l| l.to_lowercase().starts_with("en"))
        .unwrap_or(false)
}

#[cfg(test)]
mod lang_detect_tests {
    use super::*;
    #[test]
    fn detect_honours_spavn_lang() {
        std::env::set_var("SPAVN_LANG", "en");
        assert!(detect_os_lang_en(), "SPAVN_LANG=en must yield EN");
        std::env::set_var("SPAVN_LANG", "tr");
        assert!(!detect_os_lang_en());
        std::env::remove_var("SPAVN_LANG");
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
    /// Wire protocol for this provider. `"anthropic"` selects the native
    /// Anthropic Messages API for custom proxies that are not on anthropic.com.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
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

    #[test]
    fn documented_config_keys_parse_in_both_spellings() {
        // The README documents camelCase for the newer keys; snake_case stays accepted so
        // hand-written configs from earlier releases keep loading.
        for raw in [
            r#"{"webSearchUrl":"https://example.test/mcp","byokCatalog":false}"#,
            r#"{"web_search_url":"https://example.test/mcp","byok_catalog":false}"#,
        ] {
            let parsed: SpavnUserConfig = serde_json::from_str(raw).expect(raw);
            assert_eq!(
                parsed.web_search_url.as_deref(),
                Some("https://example.test/mcp")
            );
            assert!(!parsed.byok_catalog);
        }
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
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            by_key("opus").map(|m| m.model_id.as_str()),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            by_key("haiku").map(|m| m.model_id.as_str()),
            Some("claude-haiku-5-5")
        );
        assert_eq!(
            by_key("fable").map(|m| m.model_id.as_str()),
            Some("claude-fable-5-1")
        );
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct McpServerConfig {
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    pub url: Option<String>,
    /// Claims the server has no side effects, so its tools skip the mutating-tool gate.
    /// Opt-in only: the default treats every unlisted tool as a mutator.
    #[serde(default, rename = "readOnly", alias = "read_only")]
    pub read_only: bool,
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
    /// MCP endpoint the `web` tool posts search calls to. Unset or an empty string leaves the HTTP
    /// backend off; nothing is baked in, so no external host is reached unless this is set.
    #[serde(default, rename = "webSearchUrl", alias = "web_search_url")]
    pub web_search_url: Option<String>,
    /// Name of an entry in `mcpServers` that serves web search. When set, `web` goes through
    /// that server instead of the built-in HTTP endpoint, so no external host is baked in.
    #[serde(default, rename = "webSearchServer")]
    pub web_search_server: Option<String>,
    /// Tool to call on `web_search_server` for search. Ignored unless `web_search_server` is set.
    #[serde(default, rename = "webSearchTool")]
    pub web_search_tool: Option<String>,
    /// Tool to call on `web_search_server` for page fetch. Unset keeps the built-in fetch path.
    #[serde(default, rename = "webFetchTool")]
    pub web_fetch_tool: Option<String>,
    /// Header the hosted web-search backend expects the API key in. Unset uses `x-api-key`.
    #[serde(default, rename = "webSearchApiKeyHeader")]
    pub web_search_api_key_header: Option<String>,
    /// Env var holding the web-search API key. Unset sends no key at all; the name is never
    /// guessed, so an unrelated secret cannot leak to the configured endpoint.
    #[serde(default, rename = "webSearchApiKeyEnv")]
    pub web_search_api_key_env: Option<String>,
    /// Tool name on `webSearchUrl` serving search. Unset means search over HTTP is not wired up.
    #[serde(default, rename = "webSearchHttpTool")]
    pub web_search_http_tool: Option<String>,
    /// Tool name on `webSearchUrl` serving fetch. Unset falls back to the plain GET path.
    #[serde(default, rename = "webFetchHttpTool")]
    pub web_fetch_http_tool: Option<String>,
    /// Built-in search providers in the order to try them, e.g. `["brave", "duckduckgo"]`.
    /// Unset uses the default chain (configured API keys first, then DuckDuckGo and Brave's
    /// web page); an empty list turns built-in search off.
    #[serde(default, rename = "webSearchProviders")]
    pub web_search_providers: Option<Vec<String>>,
    /// SearXNG instance for the built-in `searxng` provider (`SPAVN_SEARXNG_URL` also works).
    #[serde(default, rename = "searxngUrl")]
    pub searxng_url: Option<String>,
    /// Serve the built-in BYOK provider catalog alongside `providers`.
    /// Set `false` to run the provider list entirely from config.
    #[serde(
        default = "default_true",
        rename = "byokCatalog",
        alias = "byok_catalog"
    )]
    pub byok_catalog: bool,
    /// Check for a newer release once per `updateCheckIntervalHours` at startup. The check is a
    /// single GET to `updateCheckUrl` (default: this build's GitHub releases) and never downloads
    /// anything; set `false` to keep the binary fully offline.
    #[serde(
        default = "default_true",
        rename = "updateCheck",
        alias = "update_check"
    )]
    pub update_check: bool,
    /// Hours between background release checks. Minimum 1.
    #[serde(
        default = "default_update_interval",
        rename = "updateCheckIntervalHours"
    )]
    pub update_check_interval_hours: u64,
    /// Release endpoint for the update check. Unset derives it from `CARGO_PKG_REPOSITORY`;
    /// empty disables the check. Must answer with JSON carrying `tag_name`.
    #[serde(default, rename = "updateCheckUrl")]
    pub update_check_url: Option<String>,
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
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(default, rename = "lspServers")]
    pub lsp_servers: HashMap<String, Vec<String>>,
}

fn default_model_key() -> String {
    std::env::var("SPAVN_MODEL").unwrap_or_else(|_| "sonnet".to_string())
}

fn default_update_interval() -> u64 {
    24
}

fn default_true() -> bool {
    true
}

fn default_busy_mode() -> String {
    "queue".to_string()
}

fn default_language() -> String {
    if let Ok(s) = std::env::var("SPAVN_LANG") {
        return if s.to_lowercase().starts_with("en") {
            "en".to_string()
        } else {
            "tr".to_string()
        };
    }
    if let Ok(locale) = std::env::var("LC_ALL").or_else(|_| std::env::var("LANG")) {
        let low = locale.to_lowercase();
        if low.starts_with("tr") {
            return "tr".to_string();
        }
        if low.starts_with("en") && !spavn_dir().join("PROFILE.md").exists() {
            return "en".to_string();
        }
    }
    // Küresel hedef kitle ön tanımı: bilinmeyen yerel → İngilizce.
    "en".to_string()
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
        return Err(tr_en("API anahtarı boş olamaz.", "API key cannot be empty.").to_string());
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
///
/// This is only the *default* list. `"byokCatalog": false` in `~/.spavn/config.json` hides it
/// entirely, so a deployment can ship nothing but its own `providers`.
pub fn byok_catalog(cfg: &SpavnUserConfig) -> Vec<ProviderGroupConfig> {
    if !cfg.byok_catalog {
        return Vec::new();
    }
    builtin_byok_catalog()
}

pub fn builtin_byok_catalog() -> Vec<ProviderGroupConfig> {
    vec![
        ProviderGroupConfig {
            id: "openai".to_string(),
            title: "🟡 OpenAI (BYOK)".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            is_local: false,
            protocol: None,
            context_window: Some(1_050_000),
            models: vec![
                ModelConfigEntry {
                    key: "gpt-6.1-sol".to_string(),
                    model_id: "gpt-6.1-sol".to_string(),
                    label: "OpenAI GPT-6.1 Sol".to_string(),
                    badge: "🟡 Coding".to_string(),
                    desc: "Near-Astra complex coding & agentic workhorse (1.05M ctx)".to_string(),
                    context_window: Some(1_050_000),
                },
                ModelConfigEntry {
                    key: "gpt-6-astra".to_string(),
                    model_id: "gpt-6-astra".to_string(),
                    label: "OpenAI GPT-6 Astra".to_string(),
                    badge: "🧠 Flagship".to_string(),
                    desc: "Most capable frontier reasoning & coding model (1.05M ctx)".to_string(),
                    context_window: Some(1_050_000),
                },
                ModelConfigEntry {
                    key: "gpt-6.1-terra".to_string(),
                    model_id: "gpt-6.1-terra".to_string(),
                    label: "OpenAI GPT-6.1 Terra".to_string(),
                    badge: "⚡ Fast".to_string(),
                    desc: "Fast visual reasoning, computer use & writing (1.05M ctx)".to_string(),
                    context_window: Some(1_050_000),
                },
                ModelConfigEntry {
                    key: "gpt-6-luna".to_string(),
                    model_id: "gpt-6-luna".to_string(),
                    label: "OpenAI GPT-6 Luna".to_string(),
                    badge: "🚀 Efficient".to_string(),
                    desc: "Ultra-fast, cost-efficient high-volume model (1.05M ctx)".to_string(),
                    context_window: Some(1_050_000),
                },
                ModelConfigEntry {
                    key: "gpt-5.6-sol".to_string(),
                    model_id: "gpt-5.6-sol".to_string(),
                    label: "OpenAI GPT-5.6 Sol".to_string(),
                    badge: "🟡 GPT-5.6".to_string(),
                    desc: "GPT-5.6 flagship for complex professional work (1.05M ctx)".to_string(),
                    context_window: Some(1_050_000),
                },
                ModelConfigEntry {
                    key: "gpt-5.5".to_string(),
                    model_id: "gpt-5.5".to_string(),
                    label: "OpenAI GPT-5.5".to_string(),
                    badge: "🟡 GPT-5.5".to_string(),
                    desc: "GPT-5.5 flagship for coding & professional work (1.05M ctx)".to_string(),
                    context_window: Some(1_050_000),
                },
                ModelConfigEntry {
                    key: "gpt-5.4".to_string(),
                    model_id: "gpt-5.4".to_string(),
                    label: "OpenAI GPT-5.4".to_string(),
                    badge: "🟡 GPT-5.4".to_string(),
                    desc: "OpenAI GPT-5.4 coding & reasoning".to_string(),
                    context_window: Some(272_000),
                },
                ModelConfigEntry {
                    key: "gpt-5.4-mini".to_string(),
                    model_id: "gpt-5.4-mini".to_string(),
                    label: "OpenAI GPT-5.4 Mini".to_string(),
                    badge: "⚡ Mini".to_string(),
                    desc: "Fast mini model for coding, computer use & sub-agents".to_string(),
                    context_window: Some(272_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "anthropic".to_string(),
            title: "🟣 Anthropic Claude (BYOK)".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            is_local: false,
            protocol: None,
            context_window: Some(1_000_000),
            models: vec![
                ModelConfigEntry {
                    key: "claude-sonnet-5-5".to_string(),
                    model_id: "claude-sonnet-5-5".to_string(),
                    label: "Claude Sonnet 5.5".to_string(),
                    badge: "⚡ Balanced".to_string(),
                    desc: "Anthropic Claude Sonnet 5.5 — fast coding & agents (1M ctx)".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "claude-opus-5-5".to_string(),
                    model_id: "claude-opus-5-5".to_string(),
                    label: "Claude Opus 5.5".to_string(),
                    badge: "🧠 Deep".to_string(),
                    desc: "Anthropic Claude Opus 5.5 — flagship reasoning & SWE (1M ctx)"
                        .to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "claude-fable-5-1".to_string(),
                    model_id: "claude-fable-5-1".to_string(),
                    label: "Claude Fable 5.1".to_string(),
                    badge: "👑 Frontier".to_string(),
                    desc: "Anthropic Claude Fable 5.1 — top capability tier (1M ctx)".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "claude-haiku-5-5".to_string(),
                    model_id: "claude-haiku-5-5".to_string(),
                    label: "Claude Haiku 5.5".to_string(),
                    badge: "🚀 Fast".to_string(),
                    desc: "Anthropic Claude Haiku 5.5 — fastest & lowest cost (1M ctx)".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "sonnet".to_string(),
                    model_id: "claude-sonnet-5-5".to_string(),
                    label: "Sonnet (short name)".to_string(),
                    badge: "⚡ Balanced".to_string(),
                    desc: "Alias → claude-sonnet-5-5 (1M context)".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "opus".to_string(),
                    model_id: "claude-opus-5-5".to_string(),
                    label: "Opus (short name)".to_string(),
                    badge: "🧠 Deep".to_string(),
                    desc: "Alias → claude-opus-5-5 (1M context)".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "haiku".to_string(),
                    model_id: "claude-haiku-5-5".to_string(),
                    label: "Haiku (short name)".to_string(),
                    badge: "🚀 Fast".to_string(),
                    desc: "Alias → claude-haiku-5-5 (1M context)".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "fable".to_string(),
                    model_id: "claude-fable-5-1".to_string(),
                    label: "Fable (short name)".to_string(),
                    badge: "👑 Frontier".to_string(),
                    desc: "Alias → claude-fable-5-1 (1M context)".to_string(),
                    context_window: Some(1_000_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "gemini".to_string(),
            title: "🔵 Google Gemini (BYOK)".to_string(),
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            api_key_env: "GEMINI_API_KEY".to_string(),
            is_local: false,
            protocol: None,
            context_window: Some(1_000_000),
            models: vec![
                ModelConfigEntry {
                    key: "gemini-4-argon".to_string(),
                    model_id: "gemini-4-argon".to_string(),
                    label: "Gemini 4 Argon".to_string(),
                    badge: "🧠 Flagship".to_string(),
                    desc: "Google Gemini 4 Argon — flagship reasoning (1M ctx)".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "gemini-3.8-flash".to_string(),
                    model_id: "gemini-3.8-flash".to_string(),
                    label: "Gemini 3.8 Flash".to_string(),
                    badge: "🔵 SWE/Agents".to_string(),
                    desc: "Google Gemini 3.8 Flash — long-horizon SWE & agents (1M ctx)"
                        .to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "gemini-3.6-flash".to_string(),
                    model_id: "gemini-3.6-flash".to_string(),
                    label: "Gemini 3.6 Flash".to_string(),
                    badge: "⚡ Balanced".to_string(),
                    desc: "Google Gemini 3.6 Flash — general agentic & multimodal (1M ctx)"
                        .to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "gemini-3.5-flash-lite".to_string(),
                    model_id: "gemini-3.5-flash-lite".to_string(),
                    label: "Gemini 3.5 Flash-Lite".to_string(),
                    badge: "🚀 Lite".to_string(),
                    desc: "Google Gemini 3.5 Flash-Lite — fastest & lowest cost (1M ctx)"
                        .to_string(),
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
            protocol: None,
            context_window: Some(1_000_000),
            models: vec![
                ModelConfigEntry {
                    key: "openrouter/auto".to_string(),
                    model_id: "openrouter/auto".to_string(),
                    label: "OpenRouter Auto".to_string(),
                    badge: "🌐 Auto".to_string(),
                    desc: "OpenRouter Smart Router".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "anthropic/claude-sonnet-5.5".to_string(),
                    model_id: "anthropic/claude-sonnet-5.5".to_string(),
                    label: "Claude Sonnet 5.5 (OpenRouter)".to_string(),
                    badge: "🟣 Claude 5.5".to_string(),
                    desc: "Anthropic Claude Sonnet 5.5 via OpenRouter".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "anthropic/claude-opus-5.5".to_string(),
                    model_id: "anthropic/claude-opus-5.5".to_string(),
                    label: "Claude Opus 5.5 (OpenRouter)".to_string(),
                    badge: "🟣 Opus 5.5".to_string(),
                    desc: "Anthropic Claude Opus 5.5 via OpenRouter".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "openai/gpt-6.1-sol".to_string(),
                    model_id: "openai/gpt-6.1-sol".to_string(),
                    label: "GPT-6.1 Sol (OpenRouter)".to_string(),
                    badge: "🟡 GPT-6.1".to_string(),
                    desc: "OpenAI GPT-6.1 Sol via OpenRouter".to_string(),
                    context_window: Some(1_050_000),
                },
                ModelConfigEntry {
                    key: "google/gemini-4-argon".to_string(),
                    model_id: "google/gemini-4-argon".to_string(),
                    label: "Gemini 4 Argon (OpenRouter)".to_string(),
                    badge: "🔵 Gemini 4".to_string(),
                    desc: "Google Gemini 4 Argon via OpenRouter".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "deepseek/deepseek-v4-pro".to_string(),
                    model_id: "deepseek/deepseek-v4-pro".to_string(),
                    label: "DeepSeek V4 Pro (OpenRouter)".to_string(),
                    badge: "🐋 DeepSeek V4".to_string(),
                    desc: "DeepSeek V4 Pro via OpenRouter".to_string(),
                    context_window: Some(1_000_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "deepseek".to_string(),
            title: "🐋 DeepSeek (BYOK)".to_string(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
            is_local: false,
            protocol: None,
            context_window: Some(1_000_000),
            models: vec![
                ModelConfigEntry {
                    key: "deepseek-v4-pro".to_string(),
                    model_id: "deepseek-v4-pro".to_string(),
                    label: "DeepSeek V4 Pro".to_string(),
                    badge: "🧠 V4 Pro".to_string(),
                    desc: "DeepSeek V4 Pro — 1.6T/49B MoE flagship reasoning & coding (1M ctx)"
                        .to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "deepseek-flash".to_string(),
                    model_id: "deepseek-flash".to_string(),
                    label: "DeepSeek V4.1 Flash".to_string(),
                    badge: "⚡ V4.1 Flash".to_string(),
                    desc: "DeepSeek V4.1 Flash — fast multimodal & thinking agent (1M ctx)"
                        .to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "deepseek-chat".to_string(),
                    model_id: "deepseek-chat".to_string(),
                    label: "DeepSeek Chat (Pointer)".to_string(),
                    badge: "🐋 Chat".to_string(),
                    desc: "DeepSeek default chat pointer".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "deepseek-reasoner".to_string(),
                    model_id: "deepseek-reasoner".to_string(),
                    label: "DeepSeek Reasoner (Pointer)".to_string(),
                    badge: "🧠 Reasoner".to_string(),
                    desc: "DeepSeek default reasoner pointer".to_string(),
                    context_window: Some(1_000_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "minimax".to_string(),
            title: "🟠 MiniMax (BYOK)".to_string(),
            base_url: "https://api.minimax.io/v1".to_string(),
            api_key_env: "MINIMAX_API_KEY".to_string(),
            is_local: false,
            protocol: None,
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
            title: "🟢 Z.ai / GLM (BYOK)".to_string(),
            base_url: "https://open.bigmodel.cn/api/paas/v4".to_string(),
            api_key_env: "GLM_API_KEY".to_string(),
            is_local: false,
            protocol: None,
            context_window: Some(1_000_000),
            models: vec![
                ModelConfigEntry {
                    key: "glm-5.3".to_string(),
                    model_id: "glm-5.3".to_string(),
                    label: "GLM-5.3".to_string(),
                    badge: "🧠 GLM-5.3".to_string(),
                    desc: "Z.ai GLM-5.3 flagship coding & agent model (1M ctx)".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "glm-5.3-flash".to_string(),
                    model_id: "glm-5.3-flash".to_string(),
                    label: "GLM-5.3 Flash".to_string(),
                    badge: "⚡ Flash".to_string(),
                    desc: "Z.ai GLM-5.3 Flash native multimodal high-speed model".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "glm-5.1".to_string(),
                    model_id: "glm-5.1".to_string(),
                    label: "GLM-5.1".to_string(),
                    badge: "🟢 GLM-5.1".to_string(),
                    desc: "Z.ai GLM-5.1 long-horizon coding agent".to_string(),
                    context_window: Some(200_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "groq".to_string(),
            title: "⚡ Groq LPU (BYOK)".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            api_key_env: "GROQ_API_KEY".to_string(),
            is_local: false,
            protocol: None,
            context_window: Some(131_072),
            models: vec![
                ModelConfigEntry {
                    key: "openai/gpt-oss-120b".to_string(),
                    model_id: "openai/gpt-oss-120b".to_string(),
                    label: "OpenAI GPT-OSS 120B (Groq)".to_string(),
                    badge: "⚡ ~500 tps".to_string(),
                    desc: "Flagship open-weight 120B reasoning & tool-use on Groq LPU".to_string(),
                    context_window: Some(131_072),
                },
                ModelConfigEntry {
                    key: "qwen/qwen3.8-27b".to_string(),
                    model_id: "qwen/qwen3.8-27b".to_string(),
                    label: "Qwen 3.8 27B (Groq)".to_string(),
                    badge: "⚡ ~450 tps".to_string(),
                    desc: "Qwen 3.8 27B coding & reasoning on Groq LPU".to_string(),
                    context_window: Some(131_072),
                },
                ModelConfigEntry {
                    key: "minimaxai/minimax-m2.7".to_string(),
                    model_id: "minimaxai/minimax-m2.7".to_string(),
                    label: "MiniMax M2.7 (Groq)".to_string(),
                    badge: "⚡ ~260 tps".to_string(),
                    desc: "MiniMax M2.7 196K context coding model on Groq LPU".to_string(),
                    context_window: Some(196_608),
                },
                ModelConfigEntry {
                    key: "llama-3.3-70b-versatile".to_string(),
                    model_id: "llama-3.3-70b-versatile".to_string(),
                    label: "Llama 3.3 70B (Groq)".to_string(),
                    badge: "⚡ ~280 tps".to_string(),
                    desc: "Meta Llama 3.3 70B Versatile on Groq LPU".to_string(),
                    context_window: Some(131_072),
                },
                ModelConfigEntry {
                    key: "openai/gpt-oss-20b".to_string(),
                    model_id: "openai/gpt-oss-20b".to_string(),
                    label: "OpenAI GPT-OSS 20B (Groq)".to_string(),
                    badge: "🚀 ~1000 tps".to_string(),
                    desc: "Ultra-low latency 20B open-weight model on Groq LPU".to_string(),
                    context_window: Some(131_072),
                },
            ],
        },
        ProviderGroupConfig {
            id: "xai".to_string(),
            title: "✖️  xAI Grok (BYOK)".to_string(),
            base_url: "https://api.x.ai/v1".to_string(),
            api_key_env: "XAI_API_KEY".to_string(),
            is_local: false,
            protocol: None,
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
            protocol: None,
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
            id: "opencode-zen".to_string(),
            title: "⚫ OpenCode Zen / Go (BYOK)".to_string(),
            base_url: "https://opencode.ai/zen/go/v1".to_string(),
            api_key_env: "OPENCODE_API_KEY".to_string(),
            is_local: false,
            protocol: None,
            context_window: Some(1_050_000),
            models: vec![
                ModelConfigEntry {
                    key: "zen:gpt-6-luna".to_string(),
                    model_id: "gpt-6-luna".to_string(),
                    label: "OpenCode GPT-6 Luna".to_string(),
                    badge: "⚫ Zen".to_string(),
                    desc: "OpenCode Go GPT-6 Luna".to_string(),
                    context_window: Some(1_050_000),
                },
                ModelConfigEntry {
                    key: "zen:gpt-5.6-luna".to_string(),
                    model_id: "gpt-5.6-luna".to_string(),
                    label: "OpenCode GPT-5.6 Luna".to_string(),
                    badge: "⚫ Zen".to_string(),
                    desc: "OpenCode Go GPT-5.6 Luna".to_string(),
                    context_window: Some(1_050_000),
                },
                ModelConfigEntry {
                    key: "zen:deepseek-v4.1-flash".to_string(),
                    model_id: "deepseek-v4.1-flash".to_string(),
                    label: "OpenCode DeepSeek V4.1 Flash".to_string(),
                    badge: "⚫ Zen".to_string(),
                    desc: "OpenCode Go DeepSeek V4.1 Flash".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "zen:grok-4.7".to_string(),
                    model_id: "grok-4.7".to_string(),
                    label: "OpenCode Grok 4.7".to_string(),
                    badge: "⚫ Zen".to_string(),
                    desc: "OpenCode Go Grok 4.7".to_string(),
                    context_window: Some(1_000_000),
                },
                ModelConfigEntry {
                    key: "zen:glm-5.3".to_string(),
                    model_id: "glm-5.3".to_string(),
                    label: "OpenCode GLM-5.3".to_string(),
                    badge: "⚫ Zen".to_string(),
                    desc: "OpenCode Go GLM-5.3".to_string(),
                    context_window: Some(1_000_000),
                },
            ],
        },
        ProviderGroupConfig {
            id: "ollama".to_string(),
            title: "🦙 Ollama / LM Studio (Local)".to_string(),
            base_url: "http://127.0.0.1:11434/v1".to_string(),
            api_key_env: String::new(),
            is_local: true,
            protocol: None,
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
        web_search_url: None,
        web_search_server: None,
        web_search_tool: None,
        web_fetch_tool: None,
        web_search_api_key_header: None,
        web_search_api_key_env: None,
        web_search_http_tool: None,
        web_fetch_http_tool: None,
        web_search_providers: None,
        searxng_url: None,
        byok_catalog: true,
        update_check: true,
        update_check_interval_hours: default_update_interval(),
        update_check_url: None,
        quota_quick_script: None,
        quota_full_script: None,
        memory_recall_bin: None,
        memory_remember_bin: None,
        protected_paths: Vec::new(),
        allowed_paths: Vec::new(),
        providers: Vec::new(),
        hosts: default_hosts(),
        mcp_servers: HashMap::new(),
        lsp_servers: HashMap::new(),
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
