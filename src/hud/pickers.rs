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
        "openrouter" => Some("https://openrouter.ai/keys"),
        "deepseek" => Some("https://platform.deepseek.com/api_keys"),
        "minimax" => Some("https://platform.minimax.io"),
        "glm" => Some("https://open.bigmodel.cn/usercenter/apikeys"),
        "groq" => Some("https://console.groq.com/keys"),
        "xai" => Some("https://console.x.ai"),
        "microsoft-github" => Some("https://github.com/settings/tokens"),
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
        "gemini" => {
            let out = Command::new("gcloud")
                .args(["auth", "print-access-token"])
                .output()
                .ok()?;
            if out.status.success() {
                let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !tok.is_empty() {
                    return Some((tok, "Google Cloud CLI (`gcloud auth`)"));
                }
            }
            None
        }
        _ => None,
    }
}

/// Interactive BYOK / Login / Provider Connection Manager (`/login`, `/provider`, `/connect`, `/byok`):
/// Allows any user to connect providers via auto-detected CLI auth (`gh`, `gcloud`), one-click browser
/// portal launch, API key entry (saved in `~/.spavn/secrets.env` with 0600), local server discovery,
/// OR custom OpenAI-compatible Base URL + Model + API Key!
pub fn interactive_provider_login(preselected_provider: Option<&str>) -> Option<String> {
    let catalog = byok_catalog(&load_user_config());
    let chosen = if let Some(pre) = preselected_provider
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let low = pre.to_ascii_lowercase();
        if matches!(
            low.as_str(),
            "local" | "llama" | "vllm" | "lmstudio" | "mlx"
        ) {
            "local-server".to_string()
        } else if matches!(low.as_str(), "custom" | "url") {
            "custom-provider".to_string()
        } else if matches!(low.as_str(), "github" | "copilot" | "microsoft" | "azure") {
            "microsoft-github".to_string()
        } else if matches!(low.as_str(), "claude" | "anthropic") {
            "anthropic".to_string()
        } else if matches!(low.as_str(), "google" | "gemini") {
            "gemini".to_string()
        } else if let Some(g) = catalog.iter().find(|g| g.id.eq_ignore_ascii_case(&low)) {
            g.id.clone()
        } else {
            low
        }
    } else {
        let cfg = load_user_config();
        let mut items: Vec<MenuItem> = vec![MenuItem {
            key: "local-server".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🟢 Yerel Sunucu", "🟢 Local Server"),
                RESET
            ),
            desc: tr_en(
                "llama-swap / Ollama / LM Studio / vLLM adresini tara ve bağlan",
                "Scan & connect llama-swap / Ollama / LM Studio / vLLM endpoint",
            )
            .to_string(),
        }];
        items.extend(
            catalog
                .iter()
                .filter(|g| g.id != "ollama" || cfg.local_discovery_url.is_none())
                .map(|g| {
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
                        desc: format!("{} · {}", clean_title, g.base_url),
                    }
                }),
        );

        items.push(MenuItem {
            key: "custom-provider".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("➕ Özel Sağlayıcı Ekle", "➕ Add Custom Provider"),
                RESET
            ),
            desc: tr_en(
                "Kendi OpenAI/Azure-uyumlu Base URL, Model ve API Anahtarınızı bağlayın",
                "Connect your own OpenAI/Azure-compatible Base URL, Model ID, and API Key",
            )
            .to_string(),
        });

        select_interactive_menu(
            tr_en(
                "🔐 SPAVN HIZLI BAĞLANTI & SAĞLAYICI MERKEZİ (/login · /provider · /byok)",
                "🔐 SPAVN QUICK CONNECT & PROVIDER HUB (/login · /provider · /byok)",
            ),
            &items,
            0,
        )?
    };

    if chosen == "local-server" {
        let cfg = load_user_config();
        return interactive_select_model(&cfg.default_model, true);
    }

    if chosen == "custom-provider" {
        println!(
            "\n{}╭─ {} (~/.spavn/config.json) ───────────────────────────{}",
            CYAN,
            tr_en("➕ ÖZEL SAĞLAYICI EKLE", "➕ ADD CUSTOM PROVIDER"),
            RESET
        );
        let prov_name = prompt_inline_line(tr_en(
            "│ Sağlayıcı Kısa Adı (ör. my-vllm, fireworks, together)",
            "│ Provider Short ID (e.g. my-vllm, fireworks, together)",
        ))?;
        let base_url = prompt_inline_line(tr_en(
            "│ Base URL (ör. https://api.together.xyz/v1 veya http://127.0.0.1:8000/v1)",
            "│ Base URL (e.g. https://api.together.xyz/v1 or http://127.0.0.1:8000/v1)",
        ))?;
        let model_id = prompt_inline_line(tr_en(
            "│ Model Kimliği (ör. meta-llama/Llama-3.3-70B-Instruct-Turbo)",
            "│ Model ID (e.g. meta-llama/Llama-3.3-70B-Instruct-Turbo)",
        ))?;
        let api_key_opt = prompt_inline_line(tr_en(
            "│ API Anahtarı (Yerel ise boş bırakıp Enter'a basın)",
            "│ API Key (Leave empty and press Enter if local)",
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

        let mut cfg = load_user_config();
        let entry_key = format!("{}:{}", prov_name, model_id);
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
            protocol: None,
            context_window: None,
            models: vec![ModelConfigEntry {
                key: entry_key.clone(),
                model_id: model_id.clone(),
                label: format!("{} ({})", model_id, prov_name),
                badge: "🔌 Custom".to_string(),
                desc: tr_en("Kullanıcı tanımlı sağlayıcı", "User-defined provider").to_string(),
                context_window: None,
            }],
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

    let group = catalog.into_iter().find(|g| g.id == chosen)?;
    if !group.is_local {
        let mut key_configured = false;
        if let Some((auto_tok, source_label)) = detect_cli_token_for_provider(&group.id) {
            let _ = upsert_spavn_secret(&group.api_key_env, &auto_tok);
            println!(
                "\n{}╭─ 🔐 {} ───────────────────────────────────────────{}\n{}│{} {}⚡ {} automatic session auto-detected and connected!{}\n{}╰────────────────────────────────────────────────────────────────────────────{}",
                CYAN,
                group.title,
                RESET,
                CYAN,
                RESET,
                GREEN,
                source_label,
                RESET,
                CYAN,
                RESET
            );
            key_configured = true;
        }
        if !key_configured {
            let portal = provider_portal_url(&group.id);
            println!(
                "\n{}╭─ 🔐 {} {} (~/.spavn/secrets.env — 0600) ───────────{}",
                CYAN,
                group.title,
                tr_en("BAĞLANTI & GİRİŞ", "LOGIN & CONNECT"),
                RESET
            );
            if let Some(u) = portal {
                println!(
                    "{}│{} {}🌐 {}: {}{}{} {}",
                    CYAN,
                    RESET,
                    DIM,
                    tr_en("Anahtar Sayfası", "Key Portal"),
                    CYAN,
                    u,
                    RESET,
                    tr_en(
                        "(Tarayıcıda açmak için 'o' yazıp Enter'a basın)",
                        "(Type 'o' + Enter to open in browser)"
                    )
                );
            }
            let prompt_msg = format!(
                "│ {} {}",
                group.api_key_env,
                tr_en(
                    "anahtarını yapıştırın ('o' = Tarayıcıda Aç, boş Enter = Mevcutu Koru)",
                    "paste key ('o' = Open Browser, empty Enter = Keep Existing)"
                )
            );
            if let Some(first_in) = prompt_inline_line(&prompt_msg) {
                let final_key = if first_in.eq_ignore_ascii_case("o")
                    || first_in.eq_ignore_ascii_case("open")
                {
                    if let Some(u) = portal {
                        open_url_in_browser(u);
                        println!(
                            "{}│{} {}🌐 {}{}",
                            CYAN,
                            RESET,
                            GREEN,
                            tr_en(
                                "Tarayıcı açıldı — anahtarı kopyalayıp aşağıya yapıştırın:",
                                "Browser opened — copy your key and paste below:"
                            ),
                            RESET
                        );
                    }
                    prompt_inline_line(&format!("│ {} ", group.api_key_env))
                } else {
                    Some(first_in)
                };
                if let Some(new_key) = final_key {
                    match upsert_spavn_secret(&group.api_key_env, &new_key) {
                        Ok(()) => println!(
                            "{}│{} {}✅ {} {}{}",
                            CYAN,
                            RESET,
                            GREEN,
                            group.api_key_env,
                            tr_en(
                                "güvenli olarak ~/.spavn/secrets.env dosyasına kaydedildi ve bağlandı.",
                                "securely saved to ~/.spavn/secrets.env and connected."
                            ),
                            RESET
                        ),
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
        }
    }

    let model_items: Vec<MenuItem> = group
        .models
        .iter()
        .map(|m| MenuItem {
            key: m.key.clone(),
            badge: m.badge.clone(),
            desc: m.desc.clone(),
        })
        .collect();
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
}
