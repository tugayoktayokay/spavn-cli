use super::menus::*;
use super::*;
use crate::config::is_en;

pub(crate) fn prompt_inline_line(prompt_label: &str) -> Option<String> {
    print!("{}{}{}: {}", CYAN, BOLD, prompt_label, RESET);
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).ok()? == 0 {
        return None;
    }
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub(crate) fn mask_secret_preview(val: &str) -> String {
    let trimmed = val.trim();
    if trimmed.eq_ignore_ascii_case("o") || trimmed.eq_ignore_ascii_case("open") {
        return trimmed.to_string();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let len = chars.len();
    if len == 0 {
        String::new()
    } else if len <= 8 {
        "•".repeat(len)
    } else {
        let prefix: String = chars[..4].iter().collect();
        let suffix: String = chars[len - 4..].iter().collect();
        let dots = "•".repeat((len - 8).min(16));
        format!("{}{}{}", prefix, dots, suffix)
    }
}

pub(crate) fn prompt_secret_line(prompt_label: &str) -> Option<String> {
    let mut orig_termios = empty_termios();
    if unsafe { tcgetattr(0, &mut orig_termios) } != 0 {
        return prompt_inline_line(prompt_label);
    }
    let _term_guard = TermiosGuard(orig_termios);
    enter_raw_mode(&orig_termios, 0);

    print!("{}{}{}: {}", CYAN, BOLD, prompt_label, RESET);
    let _ = io::stdout().flush();

    let mut stdin = io::stdin();
    let mut buf = String::new();
    let mut kbuf = [0u8; 128];
    let mut out_val = None;
    loop {
        let Ok(n) = stdin.read(&mut kbuf) else {
            println!();
            break;
        };
        if n == 0 {
            println!();
            break;
        }
        match kbuf[0] {
            b'\r' | b'\n' => {
                println!();
                let trimmed = buf.trim().to_string();
                if !trimmed.is_empty() {
                    out_val = Some(trimmed);
                }
                break;
            }
            3 => {
                println!();
                break;
            }
            27 => {
                if n == 1 {
                    enter_raw_mode(&orig_termios, 1);
                    let mut seq = [0u8; 8];
                    let sn = stdin.read(&mut seq).unwrap_or(0);
                    enter_raw_mode(&orig_termios, 0);
                    if sn == 0 {
                        println!();
                        break;
                    }
                }
            }
            127 | 8 => {
                buf.pop();
                let masked = mask_secret_preview(&buf);
                print!(
                    "\r\x1b[2K{}{}{}: {}{}{}{}",
                    CYAN, BOLD, prompt_label, RESET, GREEN, masked, RESET
                );
                let _ = io::stdout().flush();
            }
            21 => {
                buf.clear();
                print!("\r\x1b[2K{}{}{}: {}", CYAN, BOLD, prompt_label, RESET);
                let _ = io::stdout().flush();
            }
            b if b >= 32 => {
                if let Ok(text) = std::str::from_utf8(&kbuf[..n]) {
                    let mut newline_hit = false;
                    for ch in text.chars() {
                        if ch == '\r' || ch == '\n' {
                            newline_hit = true;
                            break;
                        }
                        if !ch.is_control() {
                            buf.push(ch);
                        }
                    }
                    let masked = mask_secret_preview(&buf);
                    print!(
                        "\r\x1b[2K{}{}{}: {}{}{}{}",
                        CYAN, BOLD, prompt_label, RESET, GREEN, masked, RESET
                    );
                    let _ = io::stdout().flush();
                    if newline_hit {
                        println!();
                        let trimmed = buf.trim().to_string();
                        if !trimmed.is_empty() {
                            out_val = Some(trimmed);
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    drop(_term_guard);
    unsafe {
        tcsetattr(0, TCSANOW, &orig_termios);
    }
    out_val
}

pub(crate) fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

pub(crate) fn provider_portal_url(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "openai" => Some("https://platform.openai.com/api-keys"),
        "anthropic" => Some("https://console.anthropic.com/settings/keys"),
        "gemini" => Some("https://aistudio.google.com/app/apikey"),
        "openrouter" => Some("https://openrouter.ai/settings/keys"),
        "deepseek" => Some("https://platform.deepseek.com/api_keys"),
        "zai" => Some("https://z.ai/manage-apikey/apikey-list"),
        "glm" => Some("https://open.bigmodel.cn/usercenter/apikeys"),
        "xai" => Some("https://console.x.ai/team/default/api-keys"),
        "groq" => Some("https://console.groq.com/keys"),
        "moonshot" => Some("https://platform.moonshot.ai/console/api-keys"),
        "minimax" => Some("https://platform.minimax.io"),
        "cerebras" => Some("https://cloud.cerebras.ai"),
        "fireworks" => Some("https://api.fireworks.ai/settings/api-keys"),
        "together" => Some("https://api.together.xyz/settings/api-keys"),
        "sambanova" => Some("https://cloud.sambanova.ai/apis"),
        "siliconflow" => Some("https://cloud.siliconflow.com/account/ak"),
        "deepinfra" => Some("https://deepinfra.com/dash/api_keys"),
        "mistral" => Some("https://console.mistral.ai/api-keys"),
        "nvidia" => Some("https://build.nvidia.com"),
        "huggingface" => Some("https://huggingface.co/settings/tokens"),
        "baseten" => Some("https://app.baseten.co/settings/api_keys"),
        "novita" => Some("https://novita.ai/settings/key-management"),
        "venice" => Some("https://venice.ai/settings/api"),
        "microsoft-github" => Some("https://github.com/settings/tokens"),
        "opencode-zen" => Some("https://opencode.ai/auth"),
        _ => None,
    }
}

pub(crate) fn detect_cli_token_for_provider(provider_id: &str) -> Option<(String, &'static str)> {
    match provider_id {
        "microsoft-github" => {
            let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
            if out.status.success() {
                let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !tok.is_empty() {
                    return Some((tok, "GitHub CLI (`gh auth token`)"));
                }
            }
            None
        }
        _ => None,
    }
}

pub(crate) fn prompt_and_save_provider_key(
    group: &ProviderGroupConfig,
    allow_replace_existing: bool,
) -> bool {
    if group.is_local || group.api_key_env.trim().is_empty() {
        return true;
    }
    let existing = resolve_api_key(&group.api_key_env);
    if existing.is_some() && !allow_replace_existing {
        return true;
    }
    if existing.is_none() {
        if let Some((auto_tok, source_label)) = detect_cli_token_for_provider(&group.id) {
            let _ = upsert_spavn_secret(&group.api_key_env, &auto_tok);
            println!(
                "\n{}╭─ 🔐 {} ──────────────────────────────────────────────────────────{}\n{}│{} {}✅ {} ({}) {}{}\n{}╰────────────────────────────────────────────────────────────────────────────{}",
                CYAN,
                group.title,
                RESET,
                CYAN,
                RESET,
                GREEN,
                source_label,
                mask_secret_preview(&auto_tok),
                tr_en(
                    "otomatik algılandı ve bağlandı!",
                    "auto-detected and connected!"
                ),
                RESET,
                CYAN,
                RESET
            );
            return true;
        }
    }

    let portal = provider_portal_url(&group.id);
    println!(
        "\n{}╭─ 🔐 {} {} (~/.spavn/secrets.env — 0600) ───────────{}",
        CYAN,
        group.title,
        tr_en("BAĞLANTI & GİRİŞ", "LOGIN & CONNECT"),
        RESET
    );
    if let Some(ref cur_key) = existing {
        println!(
            "{}│{} {}: {}{}{}{}",
            CYAN,
            RESET,
            tr_en("Mevcut Anahtar", "Current Key"),
            GREEN,
            group.api_key_env,
            format!(" ({})", mask_secret_preview(cur_key)),
            RESET
        );
    } else {
        println!(
            "{}│{} {}: {}{} ({}){}",
            CYAN,
            RESET,
            tr_en("Durum", "Status"),
            YELLOW,
            group.api_key_env,
            tr_en("henüz tanımlı değil", "not configured yet"),
            RESET
        );
    }
    if let Some(url) = portal {
        println!(
            "{}│{} 🌐 {}: {}{}{}  {}('o' + Enter = {}){}",
            CYAN,
            RESET,
            tr_en("Anahtar Portalı", "Key Portal"),
            "\x1b[4m",
            url,
            RESET,
            DIM,
            tr_en("tarayıcıda aç", "open in browser"),
            RESET
        );
    }
    let prompt_msg = format!(
        "│ {} {}",
        group.api_key_env,
        tr_en(
            "anahtarını yapıştırın ('o' = Tarayıcıda Aç, boş Enter = Geç/Koru)",
            "paste key ('o' = Open Browser, empty Enter = Skip/Keep)"
        )
    );
    if let Some(first_in) = prompt_secret_line(&prompt_msg) {
        let final_key =
            if first_in.eq_ignore_ascii_case("o") || first_in.eq_ignore_ascii_case("open") {
                if let Some(u) = portal {
                    open_url_in_browser(u);
                    println!(
                        "{}│{} {}↗ {} ({}){}",
                        CYAN,
                        RESET,
                        GREEN,
                        tr_en(
                            "Tarayıcıda açıldı — anahtarı kopyalayıp aşağıya yapıştırın",
                            "Opened in browser — copy your key and paste below"
                        ),
                        u,
                        RESET
                    );
                }
                prompt_secret_line(&format!("│ {}", group.api_key_env))
            } else {
                Some(first_in)
            };
        if let Some(new_key) = final_key {
            match upsert_spavn_secret(&group.api_key_env, &new_key) {
                Ok(()) => {
                    println!(
                        "{}│{} {}✅ {} ({}) {}{}",
                        CYAN,
                        RESET,
                        GREEN,
                        group.api_key_env,
                        mask_secret_preview(&new_key),
                        tr_en(
                            "güvenli olarak ~/.spavn/secrets.env dosyasına kaydedildi.",
                            "securely saved to ~/.spavn/secrets.env."
                        ),
                        RESET
                    );
                    let mut spin = LiveSpinner::start(tr_en(
                        "Anahtar doğrulanıyor ve canlı modeller (/v1/models) çekiliyor",
                        "Validating key and fetching live models (/v1/models)",
                    ));
                    let discovered =
                        crate::router::fetch_remote_models(&group.base_url, Some(&new_key), 4);
                    spin.stop();
                    if !discovered.is_empty() {
                        crate::config::save_discovered_provider_models(&group.id, &discovered);
                        println!(
                            "{}│{} {}✔ {} ({} {}){}",
                            CYAN,
                            RESET,
                            GREEN,
                            tr_en("Canlı doğrulama başarılı", "Live validation succeeded"),
                            discovered.len(),
                            tr_en("model önbelleğe alındı", "models cached"),
                            RESET
                        );
                    }
                }
                Err(e) => println!(
                    "{}│{} {}✗ {}: {}{}",
                    CYAN,
                    RESET,
                    RED,
                    tr_en("Hata", "Error"),
                    e,
                    RESET
                ),
            }
        }
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
    resolve_api_key(&group.api_key_env).is_some()
}

/// Interactive OMP-grade BYOK / Login / Provider Connection Manager (`/login`, `/provider`, `/connect`, `/byok`):
/// 1-to-1 with OMP's provider system:
/// - 30+ built-in cloud & local providers (Anthropic, OpenAI, Gemini, OpenRouter, DeepSeek, Z.AI, Zhipu GLM,
///   xAI, Groq, Moonshot Kimi, MiniMax, Cerebras, Fireworks, Together, SambaNova, SiliconFlow, DeepInfra,
///   Mistral, NVIDIA NIM, Hugging Face, Baseten, Novita, Venice, GitHub Models, OpenCode Zen,
///   plus first-class local Ollama, LM Studio, llama.cpp, vLLM, LiteLLM).
/// - Live key validation & persistent model caching (`~/.spavn/models_cache.json` + `models.dev` sync).
/// - User custom providers (`~/.spavn/config.json`, `~/.spavn/models.yml`, `~/.omp/agent/models.yml`) + interactive
///   manual custom provider wizard (`➕ Add Custom Provider`).
pub fn interactive_provider_login(preselected_provider: Option<&str>) -> Option<String> {
    let mut cfg = load_user_config();
    let catalog = byok_catalog(&cfg);
    let chosen = if let Some(pre) = preselected_provider
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let low = pre.to_ascii_lowercase();
        if matches!(low.as_str(), "local" | "scan") {
            "local-server".to_string()
        } else if matches!(low.as_str(), "custom" | "url" | "add") {
            "custom-provider".to_string()
        } else if matches!(low.as_str(), "sync" | "models.dev" | "refresh") {
            "sync-catalog".to_string()
        } else if matches!(low.as_str(), "github" | "copilot" | "microsoft" | "azure") {
            "microsoft-github".to_string()
        } else if matches!(low.as_str(), "claude" | "anthropic") {
            "anthropic".to_string()
        } else if matches!(low.as_str(), "google" | "gemini") {
            "gemini".to_string()
        } else if matches!(low.as_str(), "zhipu" | "zhipu-coding-plan") {
            "glm".to_string()
        } else if matches!(low.as_str(), "lmstudio") {
            "lm-studio".to_string()
        } else if matches!(low.as_str(), "llamacpp" | "llama" | "llama-swap") {
            "llama.cpp".to_string()
        } else if let Some(p) = cfg
            .providers
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(&low))
        {
            format!("user-prov:{}", p.id)
        } else if let Some(g) = catalog.iter().find(|g| g.id.eq_ignore_ascii_case(&low)) {
            g.id.clone()
        } else {
            low
        }
    } else {
        let mut items: Vec<MenuItem> = vec![MenuItem {
            key: "local-server".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🟢 Yerel Sunucu Tara", "🟢 Scan Local Servers"),
                RESET
            ),
            desc: tr_en(
                "Ollama (11434) · LM Studio (1234) · llama.cpp (8080) · vLLM/MLX (8000) · LiteLLM (4000) otomatik tara",
                "Auto-scan Ollama (11434) · LM Studio (1234) · llama.cpp (8080) · vLLM/MLX (8000) · LiteLLM (4000)",
            )
            .to_string(),
        }];

        // Show user's custom providers (from ~/.spavn/config.json, ~/.spavn/models.yml, ~/.omp/agent/models.yml)
        for p in &cfg.providers {
            let has_key = p.is_local
                || p.api_key_env.trim().is_empty()
                || resolve_api_key(&p.api_key_env).is_some();
            let badge = if has_key {
                format!(
                    "{}{}{}",
                    GREEN,
                    tr_en("🔌 Özel (Bağlı)", "🔌 Custom (Ready)"),
                    RESET
                )
            } else {
                format!(
                    "{}{}{}",
                    YELLOW,
                    tr_en("🔌 Özel (Anahtar)", "🔌 Custom (Key)"),
                    RESET
                )
            };
            items.push(MenuItem {
                key: format!("user-prov:{}", p.id),
                badge,
                desc: format!(
                    "{} · {} ({} {})",
                    p.title,
                    p.base_url,
                    p.models.len(),
                    tr_en("model", "models")
                ),
            });
        }

        // Built-in OMP catalog providers
        items.extend(catalog.iter().map(|g| {
            let has_key = g.is_local || resolve_api_key(&g.api_key_env).is_some();
            let badge = if g.is_local {
                format!(
                    "{}{}{}",
                    GREEN,
                    tr_en("🟢 Yerel (Hazır)", "🟢 Local (Ready)"),
                    RESET
                )
            } else if has_key {
                format!("{}{}{}", GREEN, tr_en("🟢 Bağlı", "🟢 Connected"), RESET)
            } else {
                format!(
                    "{}{}{}",
                    YELLOW,
                    tr_en("🔗 Bağlan / Giriş", "🔗 Connect / Login"),
                    RESET
                )
            };
            let clean_title = g.title.replace(" (BYOK)", "");
            MenuItem {
                key: g.id.clone(),
                badge,
                desc: format!(
                    "{} · {} ({} {})",
                    clean_title,
                    g.base_url,
                    g.models.len(),
                    tr_en("model", "models")
                ),
            }
        }));

        items.push(MenuItem {
            key: "sync-catalog".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("🌐 Katalog Güncelle", "🌐 Sync Catalog"),
                RESET
            ),
            desc: tr_en(
                "models.dev üzerinden tüm sağlayıcıların güncel model ve bağlam listesini senkronize et",
                "Sync latest frontier models & context windows from models.dev registry",
            )
            .to_string(),
        });

        items.push(MenuItem {
            key: "custom-provider".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("➕ Özel Sağlayıcı Ekle", "➕ Add Custom Provider"),
                RESET
            ),
            desc: tr_en(
                "Kendi OpenAI/Anthropic-uyumlu Base URL, Model ve API Anahtarınızı elle ekleyin",
                "Manually add your own OpenAI/Anthropic-compatible Base URL, Model ID, and API Key",
            )
            .to_string(),
        });

        select_interactive_menu(
            tr_en(
                "🔐 SPAVN SAĞLAYICI & GİRİŞ MERKEZİ (/login · /provider · /byok)",
                "🔐 SPAVN PROVIDER & LOGIN HUB (/login · /provider · /byok)",
            ),
            &items,
            0,
        )?
    };

    if chosen == "sync-catalog" {
        let mut spin = LiveSpinner::start(tr_en(
            "models.dev kayıt defterinden güncel modeller senkronize ediliyor",
            "Syncing latest models from models.dev registry",
        ));
        let count = crate::router::sync_models_dev_catalog();
        spin.stop();
        if count > 0 {
            println!(
                "\n{}✅ {} {} (~/.spavn/models_dev_cache.json){}",
                GREEN,
                count,
                tr_en(
                    "güncel model models.dev kaynağından senkronize edildi!",
                    "models synced from models.dev registry!"
                ),
                RESET
            );
        } else {
            println!(
                "\n{}⚠ {}{}",
                YELLOW,
                tr_en(
                    "models.dev kaynağına ulaşılamadı; yerel önbellek kullanılıyor.",
                    "Could not reach models.dev; using local catalog cache."
                ),
                RESET
            );
        }
        return interactive_provider_login(None);
    }

    if chosen == "local-server" {
        let mut spin = LiveSpinner::start(tr_en(
            "Yerel portlar taranıyor (11434 · 1234 · 8080 · 8000 · 4000)",
            "Scanning local ports (11434 · 1234 · 8080 · 8000 · 4000)",
        ));
        let probed = crate::router::probe_local_endpoints(cfg.local_discovery_url.as_deref());
        spin.stop();

        if let Some((base_url, disc_url, discovered)) = probed {
            cfg.local_base_url = Some(base_url.clone());
            cfg.local_discovery_url = Some(disc_url.clone());
            let _ = save_user_config(&cfg);
            let mut local_items: Vec<MenuItem> = discovered
                .iter()
                .map(|m| MenuItem {
                    key: format!("local:{}", m.id),
                    badge: if m.loaded {
                        format!("{}{}🟢 VRAM{}", BOLD, GREEN, RESET)
                    } else {
                        format!("{}⚪ Local{}", DIM, RESET)
                    },
                    desc: format!("{} · {}", m.spec_badge, base_url),
                })
                .collect();
            local_items.push(MenuItem {
                key: "__custom_local_url__".to_string(),
                badge: format!(
                    "{}{}{}",
                    CYAN,
                    tr_en("🔗 Farklı URL", "🔗 Custom URL"),
                    RESET
                ),
                desc: tr_en(
                    "Farklı bir yerel/LAN sunucu adresi gir (ör. http://192.168.1.50:8080/v1)",
                    "Enter a different local/LAN server URL (e.g. http://192.168.1.50:8080/v1)",
                )
                .to_string(),
            });
            if let Some(pick) = select_interactive_menu(
                &format!(
                    "🟢 {} ({})",
                    tr_en("YEREL SUNUCU MODELLERİ", "LOCAL SERVER MODELS"),
                    base_url
                ),
                &local_items,
                0,
            ) {
                if pick != "__custom_local_url__" {
                    return Some(pick);
                }
            } else {
                return None;
            }
        }

        println!(
            "\n{}╭─ 🟢 {} (~/.spavn/config.json) ─────────────────────────{}",
            CYAN,
            tr_en("YEREL SUNUCU BAĞLANTISI", "LOCAL SERVER CONNECTION"),
            RESET
        );
        let raw_url = prompt_inline_line(tr_en(
            "│ Yerel Sunucu URL (ör. http://127.0.0.1:11434/v1 veya http://192.168.1.50:8080/v1)",
            "│ Local Server URL (e.g. http://127.0.0.1:11434/v1 or http://192.168.1.50:8080/v1)",
        ))?;
        let (base_url, disc_url) = crate::router::derive_base_and_discovery_urls(&raw_url);
        cfg.local_base_url = Some(base_url.clone());
        cfg.local_discovery_url = Some(disc_url.clone());
        let _ = save_user_config(&cfg);
        let discovered = crate::router::discover_local_models(&disc_url, true);
        println!(
            "{}╰─ ✅ {}: {}{}{}",
            CYAN,
            tr_en("Yerel sunucu kaydedildi", "Local server saved"),
            GREEN,
            base_url,
            RESET
        );
        if !discovered.is_empty() {
            let local_items: Vec<MenuItem> = discovered
                .into_iter()
                .map(|m| MenuItem {
                    key: format!("local:{}", m.id),
                    badge: if m.loaded {
                        format!("{}{}🟢 VRAM{}", BOLD, GREEN, RESET)
                    } else {
                        format!("{}⚪ Local{}", DIM, RESET)
                    },
                    desc: format!("{} · {}", m.spec_badge, base_url),
                })
                .collect();
            return select_interactive_menu(
                &format!(
                    "🟢 {} ({})",
                    tr_en("YEREL SUNUCU MODELLERİ", "LOCAL SERVER MODELS"),
                    base_url
                ),
                &local_items,
                0,
            );
        }
        return interactive_select_model(&cfg.default_model, true);
    }

    if let Some(user_prov_id) = chosen.strip_prefix("user-prov:") {
        let mut group = cfg
            .providers
            .iter()
            .find(|p| p.id.eq_ignore_ascii_case(user_prov_id))?
            .clone();
        if !group.is_local
            && !group.api_key_env.trim().is_empty()
            && resolve_api_key(&group.api_key_env).is_none()
        {
            let _ = prompt_and_save_provider_key(&group, true);
        }
        let active_key = resolve_api_key(&group.api_key_env);
        let mut spin = LiveSpinner::start(tr_en(
            "Özel sağlayıcı modelleri sorgulanıyor (/v1/models)",
            "Querying custom provider models (/v1/models)",
        ));
        let remote_ids =
            crate::router::fetch_remote_models(&group.base_url, active_key.as_deref(), 3);
        spin.stop();
        if !remote_ids.is_empty() {
            crate::config::save_discovered_provider_models(&group.id, &remote_ids);
            for rid in &remote_ids {
                if !group.models.iter().any(|m| {
                    m.model_id.eq_ignore_ascii_case(rid) || m.key.eq_ignore_ascii_case(rid)
                }) {
                    group.models.push(ModelConfigEntry {
                        key: format!("{}:{}", group.id, rid),
                        model_id: rid.clone(),
                        label: format!("{} ({})", rid, group.id),
                        badge: "🌐 Live".to_string(),
                        desc: format!("{} · {}", rid, group.base_url),
                        context_window: None,
                    });
                }
            }
        }
        let mut model_items: Vec<MenuItem> = group
            .models
            .iter()
            .map(|m| MenuItem {
                key: if m.key.contains(':') {
                    m.key.clone()
                } else {
                    format!("{}:{}", group.id, m.key)
                },
                badge: m.badge.clone(),
                desc: m.desc.clone(),
            })
            .collect();
        if !group.api_key_env.trim().is_empty() {
            model_items.push(MenuItem {
                key: "__update_key__".to_string(),
                badge: format!(
                    "{}{}{}",
                    CYAN,
                    tr_en("🔑 Anahtar Güncelle", "🔑 Update API Key"),
                    RESET
                ),
                desc: format!("{} ({})", group.api_key_env, group.base_url),
            });
        }
        let pick = select_interactive_menu(
            &format!(
                "🔌 {} — {}",
                group.title,
                tr_en("MODEL SEÇ", "SELECT MODEL")
            ),
            &model_items,
            0,
        )?;
        if pick == "__update_key__" {
            let _ = prompt_and_save_provider_key(&group, true);
            return interactive_provider_login(Some(&group.id));
        }
        return Some(pick);
    }

    if chosen == "custom-provider" {
        println!(
            "\n{}╭─ {} (~/.spavn/config.json) ───────────────────────────{}",
            CYAN,
            tr_en("➕ ÖZEL SAĞLAYICI EKLE", "➕ ADD CUSTOM PROVIDER"),
            RESET
        );
        let prov_name = prompt_inline_line(tr_en(
            "│ Sağlayıcı Kısa Adı (ör. my-vllm, apinex, commandcode)",
            "│ Provider Short ID (e.g. my-vllm, apinex, commandcode)",
        ))?;
        let raw_base_url = prompt_inline_line(tr_en(
            "│ Base URL (ör. http://127.0.0.1:8000/v1 veya https://api.example.com/v1)",
            "│ Base URL (e.g. http://127.0.0.1:8000/v1 or https://api.example.com/v1)",
        ))?;
        let proto_in = prompt_inline_line(tr_en(
            "│ Protokol [openai / anthropic] (Varsayılan: openai — Enter ile geçin)",
            "│ Protocol [openai / anthropic] (Default: openai — press Enter to accept)",
        ))
        .unwrap_or_else(|| "openai".to_string());
        let is_anthropic_proto = proto_in.trim().eq_ignore_ascii_case("anthropic");
        let base_url = if is_anthropic_proto {
            let t = raw_base_url.trim().trim_end_matches('/');
            if t.starts_with("http://") || t.starts_with("https://") {
                t.to_string()
            } else {
                format!("https://{}", t)
            }
        } else {
            crate::router::derive_base_and_discovery_urls(&raw_base_url).0
        };
        let api_key_opt = prompt_secret_line(tr_en(
            "│ API Anahtarı (Yerel / şifresiz ise boş bırakıp Enter'a basın)",
            "│ API Key (Leave empty and press Enter if local / no-auth)",
        ));

        let env_var_name = format!(
            "{}_API_KEY",
            prov_name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                })
                .collect::<String>()
        );
        if let Some(ref secret_val) = api_key_opt {
            let _ = upsert_spavn_secret(&env_var_name, secret_val);
        }

        let mut spin = LiveSpinner::start(tr_en(
            "Sunucudan model listesi (/v1/models) sorgulanıyor",
            "Querying model list (/v1/models) from endpoint",
        ));
        let discovered_ids =
            crate::router::fetch_remote_models(&base_url, api_key_opt.as_deref(), 5);
        spin.stop();

        let (primary_model_id, extra_manual_ids) = if !discovered_ids.is_empty() {
            crate::config::save_discovered_provider_models(&prov_name, &discovered_ids);
            println!(
                "{}│{} {}✅ {} {}{}",
                CYAN,
                RESET,
                GREEN,
                discovered_ids.len(),
                tr_en(
                    "model sunucudan otomatik keşfedildi!",
                    "models auto-discovered from server!"
                ),
                RESET
            );
            let mut disc_items: Vec<MenuItem> = discovered_ids
                .iter()
                .map(|id| MenuItem {
                    key: id.clone(),
                    badge: "🌐 Live".to_string(),
                    desc: format!("{} ({})", id, prov_name),
                })
                .collect();
            disc_items.push(MenuItem {
                key: "__manual_model__".to_string(),
                badge: "✍️  Manual".to_string(),
                desc: tr_en(
                    "Model kimliğini elle yaz (virgülle birden fazla girebilirsiniz)",
                    "Type model ID(s) manually (comma-separated supported)",
                )
                .to_string(),
            });
            let picked = select_interactive_menu(
                &format!(
                    "🔌 {} — {}",
                    prov_name,
                    tr_en("KEŞFEDİLEN MODELLER", "DISCOVERED MODELS")
                ),
                &disc_items,
                0,
            )?;
            if picked == "__manual_model__" {
                let raw_ids = prompt_inline_line(tr_en(
                    "│ Model Kimliği (birden fazla için virgülle ayırın, ör. model-a, model-b)",
                    "│ Model ID(s) (comma-separated for multiple, e.g. model-a, model-b)",
                ))?;
                let mut parts: Vec<String> = raw_ids
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if parts.is_empty() {
                    return None;
                }
                let first = parts.remove(0);
                (first, parts)
            } else {
                (picked, Vec::new())
            }
        } else {
            let raw_ids = prompt_inline_line(tr_en(
                "│ Model Kimliği (birden fazla için virgülle ayırın, ör. claude-sonnet-5-5, gpt-6.1-sol)",
                "│ Model ID(s) (comma-separated for multiple, e.g. claude-sonnet-5-5, gpt-6.1-sol)",
            ))?;
            let mut parts: Vec<String> = raw_ids
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if parts.is_empty() {
                return None;
            }
            let first = parts.remove(0);
            (first, parts)
        };

        let entry_key = format!("{}:{}", prov_name, primary_model_id);
        let mut all_models = vec![ModelConfigEntry {
            key: entry_key.clone(),
            model_id: primary_model_id.clone(),
            label: format!("{} ({})", primary_model_id, prov_name),
            badge: "🔌 Custom".to_string(),
            desc: tr_en("Kullanıcı tanımlı sağlayıcı", "User-defined provider").to_string(),
            context_window: None,
        }];
        for manual_id in extra_manual_ids {
            if !all_models.iter().any(|m| m.model_id == manual_id) {
                all_models.push(ModelConfigEntry {
                    key: format!("{}:{}", prov_name, manual_id),
                    model_id: manual_id.clone(),
                    label: format!("{} ({})", manual_id, prov_name),
                    badge: "🔌 Custom".to_string(),
                    desc: format!("{} ({})", manual_id, prov_name),
                    context_window: None,
                });
            }
        }
        for extra_id in discovered_ids {
            if !all_models.iter().any(|m| m.model_id == extra_id) {
                all_models.push(ModelConfigEntry {
                    key: format!("{}:{}", prov_name, extra_id),
                    model_id: extra_id.clone(),
                    label: format!("{} ({})", extra_id, prov_name),
                    badge: "🌐 Live".to_string(),
                    desc: format!("{} ({})", extra_id, prov_name),
                    context_window: None,
                });
            }
        }
        cfg.providers
            .retain(|p| !p.id.eq_ignore_ascii_case(&prov_name));
        cfg.providers.push(ProviderGroupConfig {
            id: prov_name.clone(),
            title: format!("🔌 {} (Custom BYOK)", prov_name),
            base_url,
            api_key_env: if api_key_opt.is_some() {
                env_var_name
            } else {
                String::new()
            },
            is_local: api_key_opt.is_none(),
            protocol: if is_anthropic_proto {
                Some("anthropic".to_string())
            } else {
                Some("openai".to_string())
            },
            context_window: None,
            models: all_models,
        });
        let _ = save_user_config(&cfg);
        println!(
            "{}╰─ ✅ {}: {}{}{}",
            CYAN,
            tr_en(
                "Özel sağlayıcı kaydedildi ve aktif edildi",
                "Custom provider saved and activated"
            ),
            GREEN,
            entry_key,
            RESET
        );
        return Some(entry_key);
    }

    let mut group = catalog.into_iter().find(|g| g.id == chosen)?;

    // OMP-style Local Provider Flow (`ollama`, `lm-studio`, `llama.cpp`, `vllm`, `litellm`):
    // Probes the local port first without nagging for an API key; if not running locally, offers custom URL + optional key!
    if group.is_local {
        let active_key = resolve_api_key(&group.api_key_env);
        let mut spin = LiveSpinner::start(tr_en(
            "Yerel sağlayıcı uç noktası kontrol ediliyor",
            "Checking local provider endpoint",
        ));
        let remote_ids =
            crate::router::fetch_remote_models(&group.base_url, active_key.as_deref(), 2);
        spin.stop();
        if !remote_ids.is_empty() {
            crate::config::save_discovered_provider_models(&group.id, &remote_ids);
            let (base_url, disc_url) =
                crate::router::derive_base_and_discovery_urls(&group.base_url);
            cfg.local_base_url = Some(base_url);
            cfg.local_discovery_url = Some(disc_url);
            let _ = save_user_config(&cfg);
        } else {
            println!(
                "\n{}╭─ 🟢 {} ({}) ─────────────────────────────────{}",
                CYAN, group.title, group.base_url, RESET
            );
            println!(
                "{}│{} {}{}",
                CYAN,
                RESET,
                YELLOW,
                tr_en(
                    "Varsayılan adreste yanıt alınamadı. Özel adres/anahtar girebilir veya Enter ile geçebilirsiniz.",
                    "No response on default address. Enter custom URL/key or press Enter for default."
                )
            );
            if let Some(custom_url) = prompt_inline_line(tr_en(
                "│ Sunucu URL (Varsayılan için boş Enter)",
                "│ Server URL (Empty Enter for default)",
            )) {
                let (base_url, disc_url) =
                    crate::router::derive_base_and_discovery_urls(&custom_url);
                group.base_url = base_url.clone();
                cfg.local_base_url = Some(base_url);
                cfg.local_discovery_url = Some(disc_url);
                let _ = save_user_config(&cfg);
            }
            if !group.api_key_env.is_empty() {
                if let Some(opt_key) = prompt_secret_line(tr_en(
                    "│ Opsiyonel API Anahtarı (Yerel şifresiz mod için boş Enter)",
                    "│ Optional API Key (Empty Enter for local no-auth mode)",
                )) {
                    let _ = upsert_spavn_secret(&group.api_key_env, &opt_key);
                }
            }
            println!(
                "{}╰────────────────────────────────────────────────────────────────────────────{}",
                CYAN, RESET
            );
        }
    } else {
        let has_key = prompt_and_save_provider_key(&group, true);
        if !has_key {
            return None;
        }
    }

    let active_key = resolve_api_key(&group.api_key_env);
    let mut model_items: Vec<MenuItem> = group
        .models
        .iter()
        .map(|m| MenuItem {
            key: if m.key.contains(':') {
                m.key.clone()
            } else {
                format!("{}:{}", group.id, m.key)
            },
            badge: m.badge.clone(),
            desc: m.desc.clone(),
        })
        .collect();

    if group.is_local || active_key.is_some() {
        let mut spin = LiveSpinner::start(tr_en(
            "Sağlayıcı modelleri senkronize ediliyor (/v1/models)",
            "Syncing provider models (/v1/models)",
        ));
        let remote_ids =
            crate::router::fetch_remote_models(&group.base_url, active_key.as_deref(), 3);
        spin.stop();
        if !remote_ids.is_empty() {
            crate::config::save_discovered_provider_models(&group.id, &remote_ids);
        }
        for rid in remote_ids {
            let already_listed = model_items.iter().any(|item| {
                item.key.eq_ignore_ascii_case(&rid)
                    || item
                        .key
                        .eq_ignore_ascii_case(&format!("{}:{}", group.id, rid))
            }) || group
                .models
                .iter()
                .any(|m| m.model_id.eq_ignore_ascii_case(&rid) || m.key.eq_ignore_ascii_case(&rid));
            if !already_listed {
                model_items.push(MenuItem {
                    key: format!("{}:{}", group.id, rid),
                    badge: "🌐 Live".to_string(),
                    desc: format!("{} ({})", rid, group.id),
                });
            }
        }
    }

    select_interactive_menu(
        &format!(
            "🧠 {} — {}",
            group.title,
            tr_en("MODEL SEÇ", "SELECT MODEL")
        ),
        &model_items,
        0,
    )
}

pub enum QueueAction {
    None,
    EditMessage(String),
}

/// Interactive Queue & Busy-Turn Mode Manager (`/queue`):
/// Allows the user to:
/// 1) Switch the default behavior when typing during a running turn (`⚡ Anında Araya Gir (Steer)` vs `📥 Sıraya Al (Queue)`),
/// 2) Inspect, Edit (`✏️ Düzenle`), Promote (`⬆️ En Üste Al`), or Delete (`🗑️ Sil`) queued messages!
pub fn interactive_queue_manager() -> QueueAction {
    loop {
        let mut cfg = load_user_config();
        let queued = crate::list_queued_messages();
        let mode_label = if cfg.busy_input_mode == "queue" {
            format!(
                "{}{}{}",
                GREEN,
                tr_en(
                    "📥 SIRAYA AL (QUEUE — İşlem bitince sırayla çalıştır)",
                    "📥 QUEUE (Run sequentially after active turn finishes)"
                ),
                RESET
            )
        } else {
            format!(
                "{}{}{}",
                YELLOW,
                tr_en(
                    "⚡ ANINDA MÜDAHALE (STEER — Aktif adımda araya gir)",
                    "⚡ LIVE STEER (Inject immediately into active turn)"
                ),
                RESET
            )
        };

        let mut items = vec![
            MenuItem {
                key: "__toggle_mode__".to_string(),
                badge: format!(
                    "{}{}{}",
                    CYAN,
                    tr_en("⚙️  Varsayılan Mod", "⚙️  Default Mode"),
                    RESET
                ),
                desc: format!(
                    "{}: {} ({})",
                    tr_en("Aktif", "Active"),
                    mode_label,
                    tr_en("Değiştirmek için Enter", "Press Enter to toggle")
                ),
            },
            MenuItem {
                key: "__add_queue__".to_string(),
                badge: format!(
                    "{}{}{}",
                    GREEN,
                    tr_en("➕ Kuyruğa Ekle", "➕ Add to Queue"),
                    RESET
                ),
                desc: tr_en(
                    "Sıraya yeni bir görev/mesaj ekle",
                    "Enqueue a new task/message",
                )
                .to_string(),
            },
        ];

        for (i, msg) in queued.iter().enumerate() {
            let short: String = msg.chars().take(52).collect();
            items.push(MenuItem {
                key: format!("item:{}", i),
                badge: format!(
                    "{}{} #{}{}",
                    YELLOW,
                    tr_en("📥 Sıra", "📥 Queue"),
                    i + 1,
                    RESET
                ),
                desc: format!(
                    "\"{}\" ({})",
                    short,
                    tr_en("Düzenle / Üste Al / Sil", "Edit / Promote / Delete")
                ),
            });
        }

        if !queued.is_empty() {
            items.push(MenuItem {
                key: "__clear_queue__".to_string(),
                badge: format!(
                    "{}{}{}",
                    RED,
                    tr_en("🗑️  Tümünü Temizle", "🗑️  Clear All"),
                    RESET
                ),
                desc: format!(
                    "{} ({})",
                    tr_en("Kuyruktaki tüm mesajları sil", "Remove all queued messages"),
                    queued.len()
                ),
            });
        }

        let Some(chosen) = select_interactive_menu(
            tr_en(
                "📥 SPAVN MESAJ KUYRUĞU & CANLI MÜDAHALE AYARLARI (/queue)",
                "📥 SPAVN MESSAGE QUEUE & LIVE STEERING SETTINGS (/queue)",
            ),
            &items,
            0,
        ) else {
            return QueueAction::None;
        };

        if chosen == "__toggle_mode__" {
            cfg.busy_input_mode = if cfg.busy_input_mode == "queue" {
                "steer".to_string()
            } else {
                "queue".to_string()
            };
            let _ = save_user_config(&cfg);
            continue;
        }

        if chosen == "__add_queue__" {
            if let Some(new_msg) = prompt_inline_line(tr_en(
                "📥 Kuyruğa eklenecek mesajı yazın",
                "📥 Enter message to add to queue",
            )) {
                crate::push_queued_message(new_msg);
            }
            continue;
        }

        if chosen == "__clear_queue__" {
            crate::clear_queued_messages();
            continue;
        }

        if let Some(idx_str) = chosen.strip_prefix("item:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if let Some(msg_text) = queued.get(idx).cloned() {
                    let sub_items = vec![
                        MenuItem {
                            key: "edit".to_string(),
                            badge: format!(
                                "{}{}{}",
                                CYAN,
                                tr_en("✏️  Metni Düzenle", "✏️  Edit Text"),
                                RESET
                            ),
                            desc: tr_en(
                                "Mesajı ana giriş kutusuna çekip düzenle",
                                "Pull message into main input box to edit",
                            )
                            .to_string(),
                        },
                        MenuItem {
                            key: "promote".to_string(),
                            badge: format!(
                                "{}{}{}",
                                GREEN,
                                tr_en("⬆️  1. Sıraya Al", "⬆️  Promote to Top"),
                                RESET
                            ),
                            desc: tr_en("Kuyruğun en başına taşı", "Move to top of queue")
                                .to_string(),
                        },
                        MenuItem {
                            key: "delete".to_string(),
                            badge: format!(
                                "{}{}{}",
                                RED,
                                tr_en("🗑️  Kuyruktan Sil", "🗑️  Remove"),
                                RESET
                            ),
                            desc: tr_en(
                                "Bu mesajı kuyruktan kaldır",
                                "Delete this message from queue",
                            )
                            .to_string(),
                        },
                    ];
                    if let Some(act) = select_interactive_menu(
                        &format!(
                            "📥 {} #{}",
                            tr_en("KUYRUK MESAJI İŞLEMİ", "QUEUE ITEM ACTION"),
                            idx + 1
                        ),
                        &sub_items,
                        0,
                    ) {
                        match act.as_str() {
                            "edit" => {
                                crate::remove_queued_message(idx);
                                return QueueAction::EditMessage(msg_text);
                            }
                            "promote" => {
                                crate::promote_queued_message(idx);
                            }
                            "delete" => {
                                crate::remove_queued_message(idx);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

pub fn interactive_select_host(current_host: &str) -> Option<String> {
    let cfg = load_user_config();
    let items: Vec<MenuItem> = cfg
        .hosts
        .into_iter()
        .map(|h| MenuItem {
            key: h.key,
            badge: format!("{}{}{}", CYAN, h.badge, RESET),
            desc: h.desc,
        })
        .collect();
    let initial_idx = items
        .iter()
        .position(|h| h.key == current_host)
        .unwrap_or(0);
    select_interactive_menu(
        tr_en("🖥️  HEDEF SUNUCU (HOST) SEÇİMİ", "🖥️  SELECT TARGET HOST"),
        &items,
        initial_idx,
    )
}

pub fn interactive_select_access(current_access: AccessMode) -> Option<AccessMode> {
    let items = vec![
        MenuItem {
            key: "safe".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("🔒 SAFE (Salt-Okunur)", "🔒 SAFE (Read-Only)"),
                RESET
            ),
            desc: tr_en(
                "Her dosya yazma ve kabuk komutu için onay ister",
                "Prompts for approval before every file write or shell command",
            )
            .to_string(),
        },
        MenuItem {
            key: "auto".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("⚡ AUTO (Dengeli Otonom)", "⚡ AUTO (Smart Autonomous)"),
                RESET
            ),
            desc: tr_en(
                "Geliştirme serbest; kritik komutlar onay ister",
                "Development edits allowed; critical commands require approval",
            )
            .to_string(),
        },
        MenuItem {
            key: "sovereign".to_string(),
            badge: format!(
                "{}{}{}",
                RED,
                tr_en("🔥 SOVEREIGN (Tam Yetki)", "🔥 SOVEREIGN (Full Authority)"),
                RESET
            ),
            desc: tr_en(
                "Tüm hedeflerde otonom icra",
                "Fully autonomous execution on all hosts",
            )
            .to_string(),
        },
    ];
    let cur_key = match current_access {
        AccessMode::Safe => "safe",
        AccessMode::Auto => "auto",
        AccessMode::Sovereign => "sovereign",
    };
    let initial_idx = items.iter().position(|a| a.key == cur_key).unwrap_or(1);
    select_interactive_menu(
        tr_en("🛡️  YETKİ MODU SEÇİMİ", "🛡️  SELECT ACCESS MODE"),
        &items,
        initial_idx,
    )
    .and_then(|k| AccessMode::parse_alias(&k))
}

pub fn interactive_select_language() -> Option<String> {
    let items = vec![
        MenuItem {
            key: "tr".to_string(),
            badge: format!("{}🇹🇷 Turkish (TR){}", CYAN, RESET),
            desc: "Use Turkish for the CLI interface, menus and status bars.".to_string(),
        },
        MenuItem {
            key: "en".to_string(),
            badge: format!("{}🇬🇧 English (EN){}", GREEN, RESET),
            desc: "Use English for CLI interface, menus, and status bars".to_string(),
        },
    ];
    let initial_idx = if is_en() { 1 } else { 0 };
    if let Some(chosen) = select_interactive_menu(
        tr_en(
            "🌐 DİL SEÇİMİ / LANGUAGE SELECTION (/lang)",
            "🌐 LANGUAGE SELECTION (/lang)",
        ),
        &items,
        initial_idx,
    ) {
        let mut cfg = load_user_config();
        cfg.language = chosen.clone();
        let _ = save_user_config(&cfg);
        return Some(chosen);
    }
    None
}

pub fn interactive_select_role(
    current_role: crate::spavn_engine::AgentRoleKey,
) -> Option<crate::spavn_engine::AgentRoleKey> {
    use crate::spavn_engine::AgentRoleKey;
    let roles = [
        (
            AgentRoleKey::Auto,
            tr_en("🎯 Otomatik Dispatcher", "🎯 Auto Dispatcher"),
            tr_en(
                "Prompt içeriğine göre en uygun uzmanlık rolünü otomatik seçer",
                "Automatically selects the best specialist role based on prompt",
            ),
        ),
        (
            AgentRoleKey::Coder,
            "💻 Coder",
            tr_en(
                "Minimum kapsamda, mevcut kod stiline uygun temiz uygulama",
                "Clean implementation with minimal blast radius adhering to code style",
            ),
        ),
        (
            AgentRoleKey::Reviewer,
            "🔍 Reviewer",
            tr_en(
                "Kenar durumları, yarış koşulları ve hataları dosya:satır ile bulur",
                "Finds edge cases, race conditions, and bugs with exact file:line citations",
            ),
        ),
        (
            AgentRoleKey::Security,
            "🛡️  Security",
            tr_en(
                "Secret sızıntısı, yetki atlama ve injection zafiyetlerini denetler",
                "Audits secret exposure, auth bypass, and injection vulnerabilities",
            ),
        ),
        (
            AgentRoleKey::Test,
            "🧪 Test",
            tr_en(
                "Deterministik, hızlı doğrulama ve sınır durumu testleri yazar",
                "Writes deterministic, fast verification and boundary condition tests",
            ),
        ),
        (
            AgentRoleKey::Docs,
            "📖 Docs",
            tr_en(
                "Yalnızca koda dayalı, sıfır şişirmeli teknik dokümantasyon",
                "Zero-fluff technical documentation grounded strictly in code",
            ),
        ),
        (
            AgentRoleKey::Refactor,
            "🧹 Refactor",
            tr_en(
                "Davranışı bozmadan mimari sadeleştirme ve ölü kod temizliği",
                "Behavior-preserving structural simplification and dead-code removal",
            ),
        ),
    ];
    let items: Vec<MenuItem> = roles
        .iter()
        .map(|(r, badge, desc)| MenuItem {
            key: r.as_str().to_string(),
            badge: format!("{}{}{}", CYAN, badge, RESET),
            desc: (*desc).to_string(),
        })
        .collect();
    let initial_idx = roles
        .iter()
        .position(|(r, _, _)| *r == current_role)
        .unwrap_or(0);
    select_interactive_menu(
        tr_en(
            "🎭 SPAVN UZMANLIK ROLÜ SEÇİMİ (/role)",
            "🎭 SELECT SPAVN SPECIALIST ROLE (/role)",
        ),
        &items,
        initial_idx,
    )
    .and_then(|k| AgentRoleKey::parse_alias(&k))
}

pub fn interactive_select_skill() -> Option<String> {
    let skills = crate::skills::discover_skills();
    if skills.is_empty() {
        return None;
    }
    let items: Vec<MenuItem> = skills
        .into_iter()
        .map(|s| MenuItem {
            key: s.name,
            badge: format!("{}{}{}", MAGENTA, s.source, RESET),
            desc: s.summary.chars().take(42).collect(),
        })
        .collect();
    select_interactive_menu(
        tr_en(
            "🧩 YETENEK (SKILL) SEÇİM MENÜSÜ",
            "🧩 SELECT SPECIALIST SKILL (@skills)",
        ),
        &items,
        0,
    )
}

pub fn interactive_select_session(current_path: &Path) -> Option<PathBuf> {
    let sessions = crate::session::list_recent_sessions(20);
    if sessions.is_empty() {
        return None;
    }
    let mut initial_idx = 0usize;
    let items: Vec<MenuItem> = sessions
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            if s.path == current_path {
                initial_idx = idx;
            }
            let short_id = s
                .filename
                .trim_start_matches("session-")
                .trim_end_matches(".jsonl");
            let active_tag = if s.path == current_path {
                format!(" {}[ACTIVE]{}", GREEN, RESET)
            } else {
                String::new()
            };
            MenuItem {
                key: format!("#{:02} · {}", idx + 1, short_id),
                badge: format!(
                    "{}🕒 {:<11}{} · 💬 {} msj{}",
                    CYAN, s.age_label, RESET, s.message_count, active_tag
                ),
                desc: s.preview.clone(),
            }
        })
        .collect();

    let chosen_key = select_interactive_menu("🗂️  PAST SESSIONS (/resume)", &items, initial_idx)?;
    let chosen_idx = items.iter().position(|it| it.key == chosen_key)?;
    sessions.get(chosen_idx).map(|s| s.path.clone())
}

/// Arrow-key picker over the live task list: the way to close a task without memorising its index.
///
/// Every Enter toggles the highlighted task and the menu is rebuilt from the shared list, so one
/// run can close several tasks in a row. Esc/q leaves whatever was already toggled in place and
/// returns it, letting the caller print a single confirmation line instead of one per toggle.
/// Returns the 1-based index of the last toggled task, or `None` when there is nothing to pick
/// (empty list, no TTY, or an immediate cancel).
pub fn interactive_select_live_task() -> Option<usize> {
    if !stdin_is_tty() || crate::list_live_tasks().is_empty() {
        return None;
    }
    let mut anchor = 0usize;
    let mut last_toggled: Option<usize> = None;
    loop {
        let items: Vec<MenuItem> = crate::list_live_tasks()
            .iter()
            .enumerate()
            .map(|(idx, t)| {
                let (icon, color, label) = match t.status {
                    crate::LiveTaskStatus::Done => ("✓", GREEN, tr_en("tamam", "done")),
                    crate::LiveTaskStatus::InProgress => ("▶", YELLOW, tr_en("sürüyor", "running")),
                    crate::LiveTaskStatus::Pending => ("○", DIM, tr_en("bekliyor", "pending")),
                };
                MenuItem {
                    key: format!("#{:02}", idx + 1),
                    badge: format!("{}{} {}{}", color, icon, label, RESET),
                    desc: t.title.clone(),
                }
            })
            .collect();

        let Some(chosen) = select_interactive_menu(
            tr_en("📋 GÖREV DURUMU (/todo)", "📋 TASK STATUS (/todo)"),
            &items,
            anchor,
        ) else {
            break;
        };
        let Ok(idx) = chosen.trim_start_matches('#').parse::<usize>() else {
            break;
        };
        if !crate::toggle_live_task(idx) {
            break;
        }
        last_toggled = Some(idx);
        anchor = idx.saturating_sub(1);
    }
    last_toggled
}

#[cfg(test)]
mod menu_tests {
    use super::*;
    use crate::hud::keys::NavKey;

    fn menu_item(key: &str, badge: &str, desc: &str) -> MenuItem {
        MenuItem {
            key: key.to_string(),
            badge: badge.to_string(),
            desc: desc.to_string(),
        }
    }

    #[test]
    fn menu_search_filters_by_key_badge_and_desc_case_insensitively() {
        let items = vec![
            menu_item("/diff", "Git Değişiklikleri", "Çalışma ağacı farkı"),
            menu_item("/model", "Sağlayıcı & Model", "Claude Sonnet"),
            menu_item("/commit", "Commit", "Değişiklikleri işle"),
        ];

        assert_eq!(filter_menu_rows(&items, ""), vec![0, 1, 2]);
        assert_eq!(filter_menu_rows(&items, "CLAUDE"), vec![1]);
        assert_eq!(filter_menu_rows(&items, "commit"), vec![2]);
        assert_eq!(filter_menu_rows(&items, "agac"), Vec::<usize>::new());
        assert_eq!(filter_menu_rows(&items, "ağacı"), vec![0]);
    }

    #[test]
    fn provider_search_matches_provider_titles_only() {
        let providers = vec![
            (
                "Anthropic".to_string(),
                vec![menu_item("claude-sonnet", "Sonnet", "fast")],
            ),
            (
                "Ollama (Yerel)".to_string(),
                vec![menu_item("qwen", "Qwen", "yerel")],
            ),
        ];

        assert_eq!(filter_provider_rows(&providers, ""), vec![0, 1]);
        assert_eq!(filter_provider_rows(&providers, "anth"), vec![0]);
        assert_eq!(filter_provider_rows(&providers, "yerel"), vec![1]);
        // A model name must not drag its provider into the provider column.
        assert_eq!(
            filter_provider_rows(&providers, "qwen"),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn column_step_follows_the_arrow_direction_and_never_wraps() {
        // Right always means "into the model column", Left always means "back to the
        // provider column" — the column must never flip to the opposite side of the
        // direction the user pressed.
        assert!(!step_column(NavKey::Left, false));
        assert!(!step_column(NavKey::Left, true));
        assert!(step_column(NavKey::Right, false));
        assert!(step_column(NavKey::Right, true));
        // Tab / Shift-Tab are a toggle, so they do flip and do not wrap into a jump.
        assert!(step_column(NavKey::BackTab, false));
        assert!(!step_column(NavKey::BackTab, true));
        // Vertical movement keeps the column that already has focus.
        assert!(step_column(NavKey::Up, true));
        assert!(!step_column(NavKey::Down, false));
    }

    #[test]
    fn live_task_picker_stays_silent_without_a_tty() {
        crate::add_live_task("tatlı test görevi".to_string());
        assert_eq!(interactive_select_live_task(), None);
        crate::clear_live_tasks();
    }

    #[test]
    fn mask_secret_preview_masks_keys_while_preserving_browser_shortcut() {
        assert_eq!(mask_secret_preview(""), "");
        assert_eq!(mask_secret_preview("o"), "o");
        assert_eq!(mask_secret_preview("OPEN"), "OPEN");
        assert_eq!(mask_secret_preview("short123"), "••••••••");
        assert_eq!(
            mask_secret_preview("sk-proj-1234567890abcdef"),
            "sk-p••••••••••••••••cdef"
        );
    }

    #[test]
    fn derive_base_and_discovery_urls_normalizes_all_local_url_forms() {
        assert_eq!(
            crate::router::derive_base_and_discovery_urls("127.0.0.1:11434"),
            (
                "http://127.0.0.1:11434/v1".to_string(),
                "http://127.0.0.1:11434/v1/models".to_string()
            )
        );
        assert_eq!(
            crate::router::derive_base_and_discovery_urls("http://127.0.0.1:8080/v1/"),
            (
                "http://127.0.0.1:8080/v1".to_string(),
                "http://127.0.0.1:8080/v1/models".to_string()
            )
        );
        assert_eq!(
            crate::router::derive_base_and_discovery_urls("http://127.0.0.1:1234/v1/models"),
            (
                "http://127.0.0.1:1234/v1".to_string(),
                "http://127.0.0.1:1234/v1/models".to_string()
            )
        );
    }

    #[test]
    fn resolve_endpoint_routes_prefixed_provider_model_to_requested_provider() {
        let ep = crate::router::resolve_endpoint("openrouter:deepseek-chat");
        assert_eq!(ep.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(ep.model, "deepseek-chat");

        let claude = crate::router::resolve_endpoint("anthropic:claude-opus-5-5");
        assert_eq!(claude.model, "claude-opus-5-5");
        assert_eq!(claude.base_url, "https://api.anthropic.com/v1");
        assert_eq!(claude.key, "anthropic:claude-opus-5-5");
    }
}
