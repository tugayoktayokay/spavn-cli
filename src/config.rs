use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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
mod config_cache_tests {
    use super::*;

    #[test]
    fn file_stamp_tracks_creation_edit_and_removal() {
        let dir = std::env::temp_dir().join(format!("spavn-cfgstamp-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let f = dir.join("config.json");

        assert_eq!(file_stamp(&f), None, "missing file has no stamp");
        fs::write(&f, "{}").unwrap();
        let first = file_stamp(&f);
        assert!(first.is_some());
        assert_eq!(first, file_stamp(&f), "untouched file keeps its stamp");
        fs::write(&f, "{\"defaultModel\": \"x\"}").unwrap();
        assert_ne!(first, file_stamp(&f), "edited file changes its stamp");
        fs::remove_file(&f).unwrap();
        assert_eq!(file_stamp(&f), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalidate_clears_the_cached_config() {
        let stamp = config_stamp();
        *CONFIG_CACHE.lock().unwrap() = Some((stamp, default_user_config()));
        invalidate_config_cache();
        assert!(CONFIG_CACHE.lock().unwrap().is_none());
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
    #[serde(default = "default_host_sensitive")]
    pub sensitive: bool,
}

fn default_host_sensitive() -> bool {
    true
}

/// A host is sensitive when commands run on it deserve an explicit approval in
/// AUTO mode. Remote hosts default to `sensitive: true` unless explicitly configured
/// with `"sensitive": false` in `~/.spavn/config.json`; `local` is never sensitive.
pub fn host_is_sensitive(host: &str) -> bool {
    let cfg = load_user_config();
    host_is_sensitive_in(host, &cfg.hosts)
}

pub fn host_is_sensitive_in(host: &str, hosts: &[RemoteHostConfig]) -> bool {
    if host.is_empty() || host == "local" {
        return false;
    }
    match hosts.iter().find(|h| h.key.eq_ignore_ascii_case(host)) {
        Some(h) => h.sensitive,
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
        let parsed: RemoteHostConfig = serde_json::from_str(
            r#"{"key":"prod","ssh_target":"user@1.2.3.4","badge":"","desc":""}"#,
        )
        .unwrap();
        assert!(host_is_sensitive_in("prod", &[parsed]));
        assert!(!host_is_sensitive_in(
            "lan-m3",
            &[h("lan-m3", "m3.local", false)]
        ));
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
    fn embedded_catalog_is_well_formed() {
        let catalog = builtin_byok_catalog();
        assert!(catalog.len() >= 30, "catalog failed to parse or shrank");
        let mut ids = std::collections::HashSet::new();
        for g in &catalog {
            assert!(ids.insert(g.id.clone()), "duplicate provider id {}", g.id);
            assert!(!g.base_url.is_empty(), "{} has no base_url", g.id);
            assert!(!g.models.is_empty(), "{} has no models", g.id);
            let mut keys = std::collections::HashSet::new();
            for m in &g.models {
                assert!(
                    !m.key.is_empty() && !m.model_id.is_empty(),
                    "{}: empty key",
                    g.id
                );
                assert!(
                    keys.insert(m.key.clone()),
                    "{}: duplicate model key {}",
                    g.id,
                    m.key
                );
            }
        }
        for known in ["openai", "anthropic", "gemini", "ollama", "litellm"] {
            assert!(ids.contains(known), "missing built-in provider {known}");
        }
    }

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

    #[test]
    fn omp_builtin_providers_and_chat_filter_cover_cloud_and_local() {
        let catalog = builtin_byok_catalog();
        for expected in [
            "openai",
            "anthropic",
            "gemini",
            "openrouter",
            "deepseek",
            "zai",
            "glm",
            "xai",
            "groq",
            "moonshot",
            "minimax",
            "cerebras",
            "fireworks",
            "together",
            "sambanova",
            "siliconflow",
            "deepinfra",
            "mistral",
            "nvidia",
            "huggingface",
            "baseten",
            "novita",
            "venice",
            "microsoft-github",
            "opencode-zen",
            "ollama",
            "lm-studio",
            "llama.cpp",
            "vllm",
            "litellm",
        ] {
            assert!(
                catalog.iter().any(|g| g.id == expected),
                "missing built-in provider: {expected}"
            );
        }
        assert!(crate::router::is_likely_chat_model_id("claude-sonnet-5-5"));
        assert!(crate::router::is_likely_chat_model_id("gpt-6.1-sol"));
        assert!(!crate::router::is_likely_chat_model_id(
            "text-embedding-3-small"
        ));
        assert!(!crate::router::is_likely_chat_model_id("whisper-1"));
        assert!(!crate::router::is_likely_chat_model_id("dall-e-3"));
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
    /// Global ceiling (seconds) across tool timeouts (`tools.maxTimeout` in OMP).
    #[serde(
        default,
        rename = "toolsMaxTimeoutSec",
        alias = "tools_max_timeout_sec",
        skip_serializing_if = "Option::is_none"
    )]
    pub tools_max_timeout_sec: Option<u64>,
    /// Enable OMP-style two-tier auto-background promotion for long-running foreground bash commands.
    #[serde(
        default = "default_true",
        rename = "bashAutoBackground",
        alias = "bash_auto_background"
    )]
    pub bash_auto_background: bool,
    /// Foreground wait threshold (seconds) before promoting a running bash command to `bg_N` (default 60s).
    #[serde(
        default = "default_bash_auto_bg_sec",
        rename = "bashAutoBackgroundSec",
        alias = "bash_auto_background_sec"
    )]
    pub bash_auto_background_sec: u64,
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

fn default_bash_auto_bg_sec() -> u64 {
    60
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
    write_atomic(&path, &body)
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

pub fn models_cache_path() -> PathBuf {
    spavn_dir().join("models_cache.json")
}

pub fn models_dev_cache_path() -> PathBuf {
    spavn_dir().join("models_dev_cache.json")
}

pub fn load_discovered_provider_models() -> HashMap<String, Vec<String>> {
    let path = models_cache_path();
    let Ok(raw) = fs::read_to_string(&path) else {
        return HashMap::new();
    };
    serde_json::from_str::<HashMap<String, Vec<String>>>(&raw).unwrap_or_default()
}

pub fn save_discovered_provider_models(provider_id: &str, models: &[String]) {
    if models.is_empty() {
        return;
    }
    let mut cache = load_discovered_provider_models();
    let mut deduped: Vec<String> = Vec::new();
    for m in models {
        let clean = m.trim();
        if !clean.is_empty() && !deduped.iter().any(|x| x.eq_ignore_ascii_case(clean)) {
            deduped.push(clean.to_string());
        }
    }
    if deduped.is_empty() {
        return;
    }
    cache.insert(provider_id.to_ascii_lowercase(), deduped);
    let path = models_cache_path();
    if let Ok(json_str) = serde_json::to_string_pretty(&cache) {
        let _ = fs::write(&path, format!("{}\n", json_str));
        set_owner_only_mode(&path, 0o600);
    }
}

pub fn load_models_dev_cache() -> HashMap<String, Vec<ModelConfigEntry>> {
    let path = models_dev_cache_path();
    let Ok(raw) = fs::read_to_string(&path) else {
        return HashMap::new();
    };
    serde_json::from_str::<HashMap<String, Vec<ModelConfigEntry>>>(&raw).unwrap_or_default()
}

pub fn save_models_dev_cache(cache: &HashMap<String, Vec<ModelConfigEntry>>) {
    if cache.is_empty() {
        return;
    }
    let path = models_dev_cache_path();
    if let Ok(json_str) = serde_json::to_string_pretty(cache) {
        let _ = fs::write(&path, format!("{}\n", json_str));
        set_owner_only_mode(&path, 0o600);
    }
}

/// Built-in 30-Provider OMP-Grade BYOK & Local Catalog (`spavn-cli`),
/// automatically enriched with cached live `/v1/models` discoveries (`~/.spavn/models_cache.json`)
/// and `models.dev` metadata (`~/.spavn/models_dev_cache.json`).
pub fn byok_catalog(cfg: &SpavnUserConfig) -> Vec<ProviderGroupConfig> {
    if !cfg.byok_catalog {
        return Vec::new();
    }
    let mut groups = builtin_byok_catalog();
    let discovered = load_discovered_provider_models();
    let models_dev = load_models_dev_cache();

    for group in &mut groups {
        let gid = group.id.to_ascii_lowercase();
        if let Some(dev_entries) = models_dev.get(&gid) {
            for dev in dev_entries {
                if !group.models.iter().any(|m| {
                    m.model_id.eq_ignore_ascii_case(&dev.model_id)
                        || m.key.eq_ignore_ascii_case(&dev.key)
                        || m.key.eq_ignore_ascii_case(&dev.model_id)
                }) {
                    group.models.push(dev.clone());
                }
            }
        }
        if let Some(live_ids) = discovered.get(&gid) {
            for rid in live_ids {
                if !group.models.iter().any(|m| {
                    m.model_id.eq_ignore_ascii_case(rid)
                        || m.key.eq_ignore_ascii_case(rid)
                        || m.key.eq_ignore_ascii_case(&format!("{}:{}", group.id, rid))
                }) {
                    group.models.push(ModelConfigEntry {
                        key: format!("{}:{}", group.id, rid),
                        model_id: rid.clone(),
                        label: format!("{} ({})", rid, group.id),
                        badge: "🌐 Live".to_string(),
                        desc: format!("{} · {}", rid, group.base_url),
                        context_window: group.context_window,
                    });
                }
            }
        }
    }
    groups
}

/// Built-in BYOK provider/model catalog. The data lives in `data/byok_catalog.json` (embedded at
/// compile time) so a model-list update is a JSON edit, not a 900-line Rust change.
/// `catalog_tests::embedded_catalog_is_well_formed` guards the file: a malformed edit fails CI
/// instead of shipping an empty picker.
pub fn builtin_byok_catalog() -> Vec<ProviderGroupConfig> {
    serde_json::from_str(include_str!("../data/byok_catalog.json")).unwrap_or_default()
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

/// Returns true only when `(provider_id, base_url)` matches a standard on-device local
/// inference engine (`ollama`, `lm-studio`, `llama.cpp`, `vllm`, `mlx`, `local`) rather
/// than a loopback proxy or cloud gateway. Custom local ports are configured via
/// `"is_local": true` in `~/.spavn/config.json` or `isLocal: true` in `models.yml`.
pub fn is_likely_local_engine(provider_id: &str, base_url: &str) -> bool {
    let pid = provider_id.trim().to_ascii_lowercase();
    let url = base_url.trim().to_ascii_lowercase();
    if pid.contains("proxy") || pid.contains("cloud") || pid.contains("gateway") {
        return false;
    }
    if pid.contains("local")
        || pid.contains("ollama")
        || pid.contains("lm-studio")
        || pid.contains("lmstudio")
        || pid.contains("llama")
        || pid.contains("vllm")
        || pid.contains("mlx")
    {
        return true;
    }
    url.contains(":11434")
        || url.contains(":1234")
        || url.contains(":8080")
        || url.contains(":8000")
        || url.contains(".local:")
        || url.ends_with(".local")
}

/// Parses OMP-compatible `models.yml` files (`~/.spavn/models.yml` and `~/.omp/agent/models.yml`)
/// so custom providers defined in OMP (`providers.<id>.baseUrl`, `apiKey`, `api`, `models`)
/// work seamlessly in SPAVN without duplicate manual setup.
pub fn load_external_models_yml_providers() -> Vec<ProviderGroupConfig> {
    load_external_models_yml_providers_opts(true)
}

pub fn load_external_models_yml_providers_opts(include_omp: bool) -> Vec<ProviderGroupConfig> {
    let mut paths = vec![spavn_dir().join("models.yml")];
    if include_omp {
        if let Ok(home) = std::env::var("HOME") {
            paths.push(
                PathBuf::from(home)
                    .join(".omp")
                    .join("agent")
                    .join("models.yml"),
            );
        }
    }
    let mut out: Vec<ProviderGroupConfig> = Vec::new();
    for path in paths {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let mut in_providers = false;
        let mut cur_prov: Option<ProviderGroupConfig> = None;
        let mut in_models = false;
        let mut cur_model_id = String::new();
        let mut cur_model_name = String::new();
        let mut cur_model_ctx: Option<usize> = None;

        let flush_model = |prov: &mut Option<ProviderGroupConfig>,
                           m_id: &mut String,
                           m_name: &mut String,
                           m_ctx: &mut Option<usize>| {
            if !m_id.is_empty() {
                if let Some(p) = prov.as_mut() {
                    let id_str = m_id.clone();
                    let label = if m_name.is_empty() {
                        id_str.clone()
                    } else {
                        m_name.clone()
                    };
                    if !p.models.iter().any(|x| x.model_id == id_str) {
                        p.models.push(ModelConfigEntry {
                            key: format!("{}:{}", p.id, id_str),
                            model_id: id_str,
                            label: label.clone(),
                            badge: "🔌 YAML".to_string(),
                            desc: format!("{} ({})", label, p.id),
                            context_window: *m_ctx,
                        });
                    }
                }
            }
            m_id.clear();
            m_name.clear();
            *m_ctx = None;
        };

        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indent = line.chars().take_while(|c| *c == ' ').count();
            if indent == 0 {
                in_providers = trimmed == "providers:";
                continue;
            }
            if !in_providers {
                continue;
            }
            if indent == 2 && trimmed.ends_with(':') {
                flush_model(
                    &mut cur_prov,
                    &mut cur_model_id,
                    &mut cur_model_name,
                    &mut cur_model_ctx,
                );
                if let Some(done) = cur_prov.take() {
                    if !done.base_url.is_empty()
                        && !done.models.is_empty()
                        && !out.iter().any(|p| p.id.eq_ignore_ascii_case(&done.id))
                    {
                        out.push(done);
                    }
                }
                let pid = trimmed.trim_end_matches(':').trim().to_string();
                cur_prov = Some(ProviderGroupConfig {
                    id: pid.clone(),
                    title: format!("🔌 {} (models.yml)", pid),
                    base_url: String::new(),
                    api_key_env: String::new(),
                    is_local: false,
                    protocol: Some("openai".to_string()),
                    context_window: None,
                    models: Vec::new(),
                });
                in_models = false;
                continue;
            }
            if indent == 4 {
                if trimmed == "models:" {
                    in_models = true;
                    continue;
                }
                if trimmed.ends_with(':') && !trimmed.starts_with('-') {
                    in_models = false;
                    continue;
                }
                if let Some((k, v)) = trimmed.split_once(':') {
                    let key = k.trim();
                    let val = v.trim().trim_matches('"').trim_matches('\'');
                    if let Some(p) = cur_prov.as_mut() {
                        match key {
                            "baseUrl" => {
                                p.base_url = val.to_string();
                                p.is_local = is_likely_local_engine(&p.id, val);
                            }
                            "apiKey" => {
                                if !val.is_empty() {
                                    let env_name = format!(
                                        "{}_API_KEY",
                                        p.id.chars()
                                            .map(|c| if c.is_ascii_alphanumeric() {
                                                c.to_ascii_uppercase()
                                            } else {
                                                '_'
                                            })
                                            .collect::<String>()
                                    );
                                    if resolve_api_key(&env_name).is_none() {
                                        let _ = upsert_spavn_secret(&env_name, val);
                                    }
                                    p.api_key_env = env_name;
                                }
                            }
                            "api" if val.contains("anthropic") => {
                                p.protocol = Some("anthropic".to_string());
                            }
                            "isLocal" | "is_local" => {
                                p.is_local = matches!(val, "true" | "1" | "yes");
                            }
                            _ => {}
                        }
                    }
                }
            }
            if in_models && indent >= 4 {
                if let Some(rest) = trimmed.strip_prefix("- ") {
                    // Only start a new model item on `- key: value` (e.g. `- id:` or `- contextWindow:`),
                    // never on scalar list elements like `- text` or `- image` under `input:`.
                    if indent == 4 || rest.contains(':') {
                        flush_model(
                            &mut cur_prov,
                            &mut cur_model_id,
                            &mut cur_model_name,
                            &mut cur_model_ctx,
                        );
                        if let Some((k, v)) = rest.split_once(':') {
                            let key = k.trim();
                            let val = v.trim().trim_matches('"').trim_matches('\'');
                            match key {
                                "id" => cur_model_id = val.to_string(),
                                "name" => cur_model_name = val.to_string(),
                                "contextWindow" => cur_model_ctx = val.parse::<usize>().ok(),
                                _ => {}
                            }
                        }
                    }
                } else if indent == 6 {
                    if let Some((k, v)) = trimmed.split_once(':') {
                        let key = k.trim();
                        let val = v.trim().trim_matches('"').trim_matches('\'');
                        match key {
                            "id" => cur_model_id = val.to_string(),
                            "name" => cur_model_name = val.to_string(),
                            "contextWindow" => cur_model_ctx = val.parse::<usize>().ok(),
                            _ => {}
                        }
                    }
                }
            }
        }
        flush_model(
            &mut cur_prov,
            &mut cur_model_id,
            &mut cur_model_name,
            &mut cur_model_ctx,
        );
        if let Some(done) = cur_prov.take() {
            if !done.base_url.is_empty()
                && !done.models.is_empty()
                && !out.iter().any(|p| p.id.eq_ignore_ascii_case(&done.id))
            {
                out.push(done);
            }
        }
    }
    out
}

/// Everything `load_user_config` derives its result from: the config dir, the two files it reads
/// (mtime + length) and the env vars that feed defaults. Same stamp, same config.
type ConfigStamp = (
    PathBuf,
    Vec<Option<(std::time::SystemTime, u64)>>,
    Vec<Option<String>>,
);

static CONFIG_CACHE: Mutex<Option<(ConfigStamp, SpavnUserConfig)>> = Mutex::new(None);

fn file_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn config_stamp() -> ConfigStamp {
    let dir = spavn_dir();
    let mut files = vec![dir.join("config.json"), dir.join("models.yml")];
    if let Ok(h) = std::env::var("HOME") {
        files.push(
            PathBuf::from(&h)
                .join(".omp")
                .join("agent")
                .join("models.yml"),
        );
    }
    let stamps = files.iter().map(|f| file_stamp(f)).collect();
    let env = ["HOME", "SPAVN_MODEL", "SPAVN_LANG", "LANG", "LC_ALL"]
        .iter()
        .map(|k| std::env::var(k).ok())
        .collect();
    (dir, stamps, env)
}

/// Cached by file stamp: ~45 call sites (every `resolve_endpoint`, tool call, picker) used to
/// re-read and re-parse `config.json` plus `models.yml` each time. Editing either file from
/// outside is still picked up on the next call; `save_user_config` drops the cache itself.
pub fn load_user_config() -> SpavnUserConfig {
    let stamp = config_stamp();
    if let Ok(guard) = CONFIG_CACHE.lock() {
        if let Some((cached_stamp, cfg)) = guard.as_ref() {
            if *cached_stamp == stamp {
                set_lang_en(cfg.language.eq_ignore_ascii_case("en"));
                return cfg.clone();
            }
        }
    }
    let cfg = load_user_config_uncached();
    if let Ok(mut guard) = CONFIG_CACHE.lock() {
        *guard = Some((stamp, cfg.clone()));
    }
    cfg
}

fn invalidate_config_cache() {
    if let Ok(mut guard) = CONFIG_CACHE.lock() {
        *guard = None;
    }
}

fn load_user_config_uncached() -> SpavnUserConfig {
    let cfg_path = spavn_dir().join("config.json");
    let mut cfg = if let Ok(raw) = fs::read_to_string(&cfg_path) {
        set_owner_only_mode(&cfg_path, 0o600);
        if let Ok(parsed) = serde_json::from_str::<SpavnUserConfig>(&raw) {
            set_lang_en(parsed.language.eq_ignore_ascii_case("en"));
            parsed
        } else {
            default_user_config()
        }
    } else {
        default_user_config()
    };
    for p in &mut cfg.providers {
        if p.is_local
            && p.title.contains("(models.yml)")
            && !is_likely_local_engine(&p.id, &p.base_url)
        {
            p.is_local = false;
        }
    }
    for ext in load_external_models_yml_providers_opts(cfg.providers.is_empty()) {
        let ext_url = ext
            .base_url
            .trim()
            .trim_end_matches('/')
            .to_ascii_lowercase();
        if let Some(existing) = cfg.providers.iter_mut().find(|p| {
            p.id.eq_ignore_ascii_case(&ext.id)
                || (!ext_url.is_empty()
                    && p.base_url.trim().trim_end_matches('/').to_ascii_lowercase() == ext_url)
        }) {
            for m in ext.models {
                if !existing
                    .models
                    .iter()
                    .any(|x| x.model_id.eq_ignore_ascii_case(&m.model_id))
                {
                    existing.models.push(m);
                }
            }
        } else {
            cfg.providers.push(ext);
        }
    }
    cfg
}

fn default_user_config() -> SpavnUserConfig {
    let os_en = detect_os_lang_en();
    set_lang_en(os_en);
    SpavnUserConfig {
        default_model: default_model_key(),
        fallback_cloud_model: default_model_key(),
        busy_input_mode: default_busy_mode(),
        language: if os_en { "en" } else { "tr" }.to_string(),
        local_discovery_url: None,
        local_base_url: None,
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
        tools_max_timeout_sec: None,
        bash_auto_background: true,
        bash_auto_background_sec: default_bash_auto_bg_sec(),
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
    let written = write_atomic(&cfg_path, &format!("{}\n", json_str));
    invalidate_config_cache();
    written
}

/// Held while `config.json` is read-modified-written so two terminals cannot lose each other's
/// edits. A plain `create_new` lock file (no extra dependency, works on the 1.80 MSRV); a lock
/// older than 5 s is treated as left behind by a crashed process and taken over.
struct ConfigLock(PathBuf);

impl ConfigLock {
    fn acquire() -> Self {
        let path = spavn_dir().join("config.json.lock");
        for _ in 0..100 {
            if fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .is_ok()
            {
                return Self(path);
            }
            let stale = fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(5));
            if stale {
                let _ = fs::remove_file(&path);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        Self(path)
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Atomic read-modify-write of `config.json`: re-reads the file under the lock, applies `f`,
/// saves. Prefer this over `load_user_config()` + `save_user_config()`.
pub fn update_user_config(f: impl FnOnce(&mut SpavnUserConfig)) -> Result<(), String> {
    let _lock = ConfigLock::acquire();
    invalidate_config_cache();
    let mut cfg = load_user_config();
    f(&mut cfg);
    save_user_config(&cfg)
}

/// Write via a sibling temp file + rename so concurrent readers (other terminals) never see a
/// truncated file and a crash mid-write cannot corrupt the target. Result is owner-only (0600)
/// from creation so secret/config material is never world-readable even briefly.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("spavn.tmp");
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{}.tmp.{}.{}", name, std::process::id(), seq));

    let write_res = (|| -> std::io::Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()
    })();
    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    set_owner_only_mode(&tmp, 0o600);
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Model most recently selected in any terminal; seeds new sessions (see `main`).
pub fn save_last_model(model_key: &str) {
    let key = model_key.trim();
    if !key.is_empty() {
        let _ = write_atomic(&spavn_dir().join("last_model"), &format!("{}\n", key));
    }
}

pub fn load_last_model() -> Option<String> {
    let raw = fs::read_to_string(spavn_dir().join("last_model")).ok()?;
    let key = raw.trim();
    (!key.is_empty()).then(|| key.to_string())
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
