use super::keys::{decode_nav_key, NavKey};
use super::*;

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub key: String,
    pub badge: String,
    pub desc: String,
}

/// Drops ANSI escape sequences so menu search matches only the visible text.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Case-insensitive token match over a menu row (key + badge + description).
pub(crate) fn menu_row_matches(item: &MenuItem, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {}",
        item.key,
        strip_ansi(&item.badge),
        strip_ansi(&item.desc)
    )
    .to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|tok| haystack.contains(tok))
}

/// Row indices of `items` matching the free-text query, in original order.
pub fn filter_menu_rows(items: &[MenuItem], query: &str) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, it)| menu_row_matches(it, query))
        .map(|(i, _)| i)
        .collect()
}

/// Row indices of providers whose title matches the query (used by the model picker).
pub fn filter_provider_rows(providers: &[(String, Vec<MenuItem>)], query: &str) -> Vec<usize> {
    providers
        .iter()
        .enumerate()
        .filter(|(_, (title, _))| {
            query.is_empty()
                || strip_ansi(title)
                    .to_lowercase()
                    .contains(&query.to_lowercase())
        })
        .map(|(i, _)| i)
        .collect()
}

/// Moves a cursor that lives on `current` to the neighbour of it inside `visible`.
pub(crate) fn step_in_visible(visible: &[usize], current: usize, forward: bool) -> Option<usize> {
    if visible.is_empty() {
        return None;
    }
    let pos = visible.iter().position(|&i| i == current).unwrap_or(0);
    let next = if forward {
        (pos + 1) % visible.len()
    } else if pos == 0 {
        visible.len() - 1
    } else {
        pos - 1
    };
    visible.get(next).copied()
}

/// Column-focus step for the two-column model picker.
///
/// `Left` and `Right` move directly to the provider or model column instead of toggling,
/// avoiding inverted header hints (`PROVIDER (← Back)` / `MODELS (→)`).
pub(crate) fn step_column(nav: NavKey, focus_right: bool) -> bool {
    match nav {
        NavKey::Left => false,
        NavKey::Right => true,
        // Up/Down never move the column; Tab is an explicit toggle.
        NavKey::Up | NavKey::Down => focus_right,
        NavKey::BackTab => !focus_right,
        NavKey::Home | NavKey::End => focus_right,
    }
}

pub fn select_interactive_menu(
    title: &str,
    items: &[MenuItem],
    initial_idx: usize,
) -> Option<String> {
    if items.is_empty() {
        return None;
    }

    let mut orig_termios = empty_termios();

    let has_tty = unsafe { tcgetattr(0, &mut orig_termios) == 0 };
    if !has_tty {
        return Some(items[initial_idx.min(items.len() - 1)].key.clone());
    }

    let _term_guard = TermiosGuard(orig_termios);
    enter_raw_mode(&orig_termios, 1);

    let mut sel_pos = initial_idx.min(items.len() - 1);
    // Free-text filter: printable keys narrow the list, Esc clears it.
    let mut query = String::new();
    let mut matched: Vec<usize> = filter_menu_rows(items, &query);
    let max_vis_items = items.len().min(12);
    let total_lines = max_vis_items + 2;

    print!("\n\x1b[?25l");
    for _ in 0..total_lines {
        println!();
    }

    let mut stdin = io::stdin();
    let mut kbuf = [0u8; 8];
    let mut result: Option<String> = None;
    let _ = crate::take_resized();
    let (mut last_rows, mut last_cols) = term_size();
    let mut needs_redraw = true;

    loop {
        let (rows, cols) = term_size();
        if (rows, cols) != (last_rows, last_cols) || crate::take_resized() {
            thread::sleep(Duration::from_millis(16));
            let (r2, c2) = term_size();
            last_rows = r2;
            last_cols = c2;
            needs_redraw = true;
        }

        if needs_redraw {
            matched = filter_menu_rows(items, &query);
            sel_pos = sel_pos.min(matched.len().saturating_sub(1));
            let max_w = last_cols.saturating_sub(2).max(20);
            let inner_w = max_w.saturating_sub(2);
            let mut out = format!("\x1b[?2026h\x1b[?7l\x1b[{}A\r\x1b[J", total_lines);
            let raw_hdr = if query.is_empty() {
                format!(
                    "{}╭─ {} {}(↑/↓ · Enter · Yaz: ara · Esc){} ",
                    CYAN, title, YELLOW, CYAN
                )
            } else {
                format!(
                    "{}╭─ {} {}[{}] {}/{} · Esc: temizle{} ",
                    CYAN,
                    title,
                    YELLOW,
                    query,
                    matched.len(),
                    items.len(),
                    CYAN
                )
            };
            let (trunc_hdr, used_hdr) = truncate_ansi_to_width(&raw_hdr, max_w.saturating_sub(1));
            let hdr_pad = "─".repeat(max_w.saturating_sub(used_hdr + 1));
            out.push_str(&format!("{}{}{}╮{}\n", trunc_hdr, CYAN, hdr_pad, RESET));

            let key_col_w = items
                .iter()
                .map(|it| truncate_ansi_to_width(&it.key, 26).1)
                .max()
                .unwrap_or(10)
                .clamp(10, 22);
            let badge_col_w = items
                .iter()
                .map(|it| truncate_ansi_to_width(&it.badge, 30).1)
                .max()
                .unwrap_or(12)
                .clamp(10, 24);

            let win_start = if sel_pos >= max_vis_items {
                sel_pos + 1 - max_vis_items
            } else {
                0
            };

            for r in 0..max_vis_items {
                let Some(&gi) = matched.get(win_start + r) else {
                    let empty_row = if r == 0 && matched.is_empty() {
                        let msg = tr_en("  eşleşme yok", "  no match");
                        format!("{}{}{}", DIM, msg, RESET)
                    } else {
                        String::new()
                    };
                    let (trunc_empty, used_empty) = truncate_ansi_to_width(&empty_row, inner_w);
                    let empty_pad = " ".repeat(inner_w.saturating_sub(used_empty));
                    out.push_str(&format!(
                        "{}│{}{}{}│{}\n",
                        CYAN, RESET, trunc_empty, empty_pad, RESET
                    ));
                    continue;
                };
                let item = &items[gi];
                let is_selected = gi == matched[sel_pos];
                let (k_str, k_used) = truncate_ansi_to_width(&item.key, key_col_w);
                let k_pad = " ".repeat(key_col_w.saturating_sub(k_used));
                let (b_str, b_used) = truncate_ansi_to_width(&item.badge, badge_col_w);
                let b_pad = " ".repeat(badge_col_w.saturating_sub(b_used));
                let fixed_used = 4 + key_col_w + 3 + badge_col_w + 3;
                let desc_budget = inner_w.saturating_sub(fixed_used);

                let row_inner = if desc_budget >= 8 && !item.desc.is_empty() {
                    let (d_str, _) = truncate_ansi_to_width(&item.desc, desc_budget);
                    if is_selected {
                        format!(
                            " \x1b[1;36m❯\x1b[0m \x1b[1;7;36m {}{} \x1b[0m {}│{} {}{} {}│{} {}{}{}",
                            k_str, k_pad, DIM, RESET, b_str, b_pad, DIM, RESET, BOLD, d_str, RESET
                        )
                    } else {
                        format!(
                            "    {}{} {}│{} {}{} {}│{} {}{}{}",
                            k_str, k_pad, DIM, RESET, b_str, b_pad, DIM, RESET, DIM, d_str, RESET
                        )
                    }
                } else if is_selected {
                    format!(
                        " \x1b[1;36m❯\x1b[0m \x1b[1;7;36m {}{} \x1b[0m {}│{} {}{}",
                        k_str, k_pad, DIM, RESET, b_str, b_pad
                    )
                } else {
                    format!(
                        "    {}{} {}│{} {}{}",
                        k_str, k_pad, DIM, RESET, b_str, b_pad
                    )
                };

                let (trunc_inner, used_inner) = truncate_ansi_to_width(&row_inner, inner_w);
                let right_pad = " ".repeat(inner_w.saturating_sub(used_inner));
                out.push_str(&format!(
                    "{}│{}{}{}{}│{}\n",
                    CYAN, RESET, trunc_inner, right_pad, CYAN, RESET
                ));
            }
            let bot_pad = "─".repeat(inner_w);
            out.push_str(&format!(
                "{}╰{}╯{}\n\x1b[?7h\x1b[?2026l",
                CYAN, bot_pad, RESET
            ));
            print!("{}", out);
            let _ = io::stdout().flush();
            needs_redraw = false;
        }

        let n = match stdin.read(&mut kbuf) {
            Ok(0) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                needs_redraw = true;
                continue;
            }
            Err(_) => break,
            Ok(mut n) => {
                if n == 1 && kbuf[0] == 27 {
                    thread::sleep(Duration::from_millis(25));
                    if let Ok(n2) = stdin.read(&mut kbuf[1..]) {
                        n += n2;
                    }
                }
                needs_redraw = true;
                n
            }
        };

        if n == 1 && kbuf[0] == 27 {
            if query.is_empty() {
                break;
            }
            // First Esc clears the active filter, a second one closes the menu.
            query.clear();
            sel_pos = 0;
            needs_redraw = true;
            continue;
        }
        if let Some(nav) = decode_nav_key(&kbuf[..n]) {
            match nav {
                NavKey::Up | NavKey::BackTab => {
                    if let Some(cur) = matched.get(sel_pos).copied() {
                        if let Some(prev) = step_in_visible(&matched, cur, false) {
                            sel_pos = matched.iter().position(|&i| i == prev).unwrap_or(0);
                        }
                    }
                }
                NavKey::Down => {
                    if let Some(cur) = matched.get(sel_pos).copied() {
                        if let Some(next) = step_in_visible(&matched, cur, true) {
                            sel_pos = matched.iter().position(|&i| i == next).unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        match kbuf[0] {
            b'\r' | b'\n' => {
                if let Some(&gi) = matched.get(sel_pos) {
                    result = Some(items[gi].key.clone());
                }
                break;
            }
            127 | 8 => {
                if !query.is_empty() {
                    query.pop();
                    sel_pos = 0;
                }
            }
            3 => break,
            b'q' | b'Q' if query.is_empty() => break,
            b'k' | b'K' if query.is_empty() => {
                if let Some(cur) = matched.get(sel_pos).copied() {
                    if let Some(prev) = step_in_visible(&matched, cur, false) {
                        sel_pos = matched.iter().position(|&i| i == prev).unwrap_or(0);
                    }
                }
            }
            b'j' | b'J' | b'\t' if query.is_empty() => {
                if let Some(cur) = matched.get(sel_pos).copied() {
                    if let Some(next) = step_in_visible(&matched, cur, true) {
                        sel_pos = matched.iter().position(|&i| i == next).unwrap_or(0);
                    }
                }
            }
            c if (32..=126).contains(&c) => {
                query.push(c as char);
                sel_pos = 0;
            }
            _ => {}
        }
    }

    print!("\x1b[{}A\r\x1b[J\x1b[?7h\x1b[?25h", total_lines);
    let _ = io::stdout().flush();
    unsafe {
        tcsetattr(0, TCSANOW, &orig_termios);
    }
    result
}

pub fn interactive_select_slash_command() -> Option<String> {
    let items = vec![
        MenuItem {
            key: "/model".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("🧠 Sağlayıcı & Model", "🧠 Provider & Model"),
                RESET
            ),
            desc: tr_en(
                "Sağlayıcı (←/→) ve AI modelini (↑/↓) iki sütunda seç",
                "Select provider (←/→) and AI model (↑/↓) in dual columns",
            )
            .to_string(),
        },
        MenuItem {
            key: "/byok".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🔑 BYOK Anahtar/URL", "🔑 BYOK Key/URL"),
                RESET
            ),
            desc: tr_en(
                "OpenAI/Anthropic/Gemini/DeepSeek/Özel API anahtarı ve URL ekle",
                "Add OpenAI/Anthropic/Gemini/DeepSeek/Custom API key & URL",
            )
            .to_string(),
        },
        MenuItem {
            key: "/local".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🟢 Yerel Modeller", "🟢 Local Models"),
                RESET
            ),
            desc: tr_en(
                "Yerel GPU/llama-swap/Ollama modellerini doğrudan aç",
                "Open local GPU/llama-swap/Ollama models directly",
            )
            .to_string(),
        },
        MenuItem {
            key: "/queue".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("📥 Mesaj Kuyruğu", "📥 Message Queue"),
                RESET
            ),
            desc: tr_en(
                "Bekleyen mesajları düzenle/sırala ve Anında(Steer)/Kuyruk modunu seç",
                "Manage queued messages and toggle Live Steer / Queue mode",
            )
            .to_string(),
        },
        MenuItem {
            key: "/lang".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("🌐 Dil / Language", "🌐 Language / Dil"),
                RESET
            ),
            desc: tr_en(
                "CLI arayüz dilini değiştir (Türkçe ⇄ English)",
                "Switch CLI interface language (English ⇄ Turkish)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/resume".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("🗂️  Oturumlar", "🗂️  Sessions"),
                RESET
            ),
            desc: tr_en(
                "Geçmiş konuşmaları önizlemeli olarak seç ve yükle",
                "Browse and resume recent sessions with preview",
            )
            .to_string(),
        },
        MenuItem {
            key: "/recap".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("※ Oturum Özeti", "※ Session Recap"),
                RESET
            ),
            desc: tr_en(
                "Oturumun son adım özetini göster veya otomatik boşta kalma (idle) özetini aç/kapat",
                "Show current session recap or toggle automatic idle recap line",
            )
            .to_string(),
        },
        MenuItem {
            key: "/new".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("✨ Yeni Oturum", "✨ New Session"),
                RESET
            ),
            desc: tr_en("Temiz yeni bir oturum başlat", "Start a fresh session").to_string(),
        },
        MenuItem {
            key: "/fork".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("🌿 Dallandır", "🌿 Fork Session"),
                RESET
            ),
            desc: tr_en(
                "Mevcut konuşmayı yeni oturum dosyasına klonla",
                "Clone current conversation into a new session branch",
            )
            .to_string(),
        },
        MenuItem {
            key: "/role".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("🎭 Uzmanlık Rolü", "🎭 Agent Role"),
                RESET
            ),
            desc: "Auto / Coder / Reviewer / Security / Test / Docs / Refactor".to_string(),
        },
        MenuItem {
            key: "/plan".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("📐 PLAN Fazı", "📐 PLAN Phase"),
                RESET
            ),
            desc: tr_en(
                "Salt-okunur mimari tasarım fazına geç (write/edit/bash kilitli, kısayol: \\)",
                "Switch to read-only planning phase (write/edit/bash locked, shortcut: \\)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/code".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("💻 CODE Fazı", "💻 CODE Phase"),
                RESET
            ),
            desc: tr_en(
                "Tam uygulama fazına geç (tüm araçlar aktif, kısayol: \\)",
                "Switch to coding/implementation phase (all tools active, shortcut: \\)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/janitor".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🧹 Kalite Kapıları", "🧹 Quality Gates"),
                RESET
            ),
            desc: tr_en(
                "Spavn Janitor (cargo check / tsc / lint / test) çalıştır",
                "Run Spavn Janitor (cargo check / tsc / lint / test)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/audit".to_string(),
            badge: format!(
                "{}{}{}",
                CYAN,
                tr_en("⛓️  HMAC Audit İzi", "⛓️  HMAC Audit Ledger"),
                RESET
            ),
            desc: tr_en(
                "Hangi prompt ve model hangi dosyayı değiştirdi (zincir doğrulamalı)",
                "Inspect cryptographic HMAC chain of file edits by prompt & model",
            )
            .to_string(),
        },
        MenuItem {
            key: "/wiki".to_string(),
            badge: format!("{}📖 Repo Wiki{}", CYAN, RESET),
            desc: tr_en(
                "Proje modül ve dosya mimarisi haritasını çıkar",
                "Generate repository module & architecture map",
            )
            .to_string(),
        },
        MenuItem {
            key: "/pin".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("📌 Bağlam Sabitle", "📌 Pin Context"),
                RESET
            ),
            desc: tr_en(
                "Proje için kalıcı mimari kural/not sabitle (/pins)",
                "Pin persistent architectural rules/notes for this repo (/pins)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/goal".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🎯 Proje Hedefleri", "🎯 Project Goals"),
                RESET
            ),
            desc: tr_en(
                "Aktif proje hedeflerini görüntüle ve yönet (/goals)",
                "View and manage active project goals (/goals)",
            )
            .to_string(),
        },
        MenuItem {
            key: "/host".to_string(),
            badge: format!(
                "{}{}{}",
                MAGENTA,
                tr_en("🖥️  Hedef Cihaz", "🖥️  Target Host"),
                RESET
            ),
            desc: tr_en(
                "Varsayılan çalışma hedefini seç",
                "Select default execution host",
            )
            .to_string(),
        },
        MenuItem {
            key: "/skills".to_string(),
            badge: format!("{}{}{}", YELLOW, tr_en("🧩 Yetenekler", "🧩 Skills"), RESET),
            desc: tr_en(
                "Uzman yeteneklerden (@skills) seç ve oturuma yükle",
                "Select and activate specialized @skills into session",
            )
            .to_string(),
        },
        MenuItem {
            key: "/access".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("🛡️  Yetki Modu", "🛡️  Access Mode"),
                RESET
            ),
            desc: tr_en(
                "SAFE / AUTO / SOVEREIGN erişim modunu seç",
                "Select SAFE / AUTO / SOVEREIGN execution mode",
            )
            .to_string(),
        },
        MenuItem {
            key: "/compact".to_string(),
            badge: format!(
                "{}{}{}",
                YELLOW,
                tr_en("🗜️  Sıkıştır", "🗜️  Compact"),
                RESET
            ),
            desc: tr_en(
                "Bağlamı özetleyerek token kullanımını düşür",
                "Compact conversation context to reduce token usage",
            )
            .to_string(),
        },

        MenuItem {
            key: "/reload".to_string(),
            badge: format!(
                "{}{}{}",
                GREEN,
                tr_en("🔄 Canlı Reload", "🔄 Live Reload"),
                RESET
            ),
            desc: tr_en(
                "Yeni derlenen binary ve yetenekleri oturumu koruyarak yükle",
                "Hot-reload binary and skills while preserving current session",
            )
            .to_string(),
        },
        MenuItem {
            key: "/exit".to_string(),
            badge: format!("{}{}{}", RED, tr_en("🚪 Çıkış", "🚪 Exit"), RESET),
            desc: tr_en("Oturumu kaydederek çık", "Save session and exit").to_string(),
        },
    ];
    select_interactive_menu(
        tr_en(
            "⚡ SPAVN v0.1 KOMUT MENÜSÜ (spavn.dev)",
            "⚡ SPAVN v0.1 COMMAND MENU (spavn.dev)",
        ),
        &items,
        0,
    )
}

/// Dual-Column Master-Detail Model Picker (`←`/`→` Provider, `↑`/`↓` Model, `Enter` Select, `Esc` Cancel).
/// Combines user-configured providers (`~/.spavn/config.json`), dynamically discovered local models,
/// and the built-in 10-Provider BYOK Catalog!
pub fn interactive_select_model(current_key: &str, local_first: bool) -> Option<String> {
    let cfg = load_user_config();
    let mut providers: Vec<(String, Vec<MenuItem>)> = Vec::new();
    let mut prov_ids: Vec<Option<String>> = Vec::new();

    // 1. User-configured provider groups from ~/.spavn/config.json
    for group in &cfg.providers {
        let items: Vec<MenuItem> = group
            .models
            .iter()
            .map(|m| MenuItem {
                key: m.key.clone(),
                badge: m.badge.clone(),
                desc: m.desc.clone(),
            })
            .collect();
        if !items.is_empty() {
            providers.push((format!("{} ({})", group.title, items.len()), items));
            prov_ids.push(Some(group.id.clone()));
        }
    }

    // 2. Dynamic Local Model Discovery (if `local_discovery_url` is configured)
    let mut local_idx_in_providers = None;
    if let Some(ref disc_url) = cfg.local_discovery_url {
        let mut spin = LiveSpinner::start(tr_en(
            "Yerel model sunucusu taranıyor",
            "Scanning local model server",
        ));
        let discovered = crate::router::discover_local_models(disc_url, true);
        spin.stop();

        let mut local_models: Vec<MenuItem> = Vec::new();
        for m in discovered {
            let badge = if m.loaded {
                format!(
                    "{}{}🟢 {}{}",
                    BOLD,
                    GREEN,
                    tr_en("VRAM'DE AKTİF", "ACTIVE IN VRAM"),
                    RESET
                )
            } else {
                format!(
                    "{}⚪ {}{}",
                    DIM,
                    tr_en("Hazır (Yüklenir)", "Ready (On-Demand)"),
                    RESET
                )
            };
            let ctx_label = format_token_short(m.context_window.unwrap_or(131_072));
            local_models.push(MenuItem {
                key: format!("local:{}", m.id),
                badge,
                desc: format!(
                    "{} · {} · {} ctx",
                    m.spec_badge,
                    tr_en("Yerel GPU", "Local GPU"),
                    ctx_label
                ),
            });
        }
        local_models.sort_by_key(|m| if m.badge.contains("🟢") { 0 } else { 1 });
        if !local_models.is_empty() {
            local_idx_in_providers = Some(providers.len());
            providers.push((
                format!(
                    "🟢 {} ({})",
                    tr_en("Yerel GPU Sunucusu", "Local GPU Server"),
                    local_models.len()
                ),
                local_models,
            ));
            prov_ids.push(None);
        }
    }

    // 3. Built-in BYOK Catalog Providers (OpenAI, Anthropic, Gemini, OpenRouter, DeepSeek, MiniMax, GLM, Groq, xAI, Ollama)
    for byok_group in byok_catalog(&cfg) {
        if byok_group.id == "ollama" && local_idx_in_providers.is_some() {
            continue;
        }
        let has_key = byok_group.is_local || resolve_api_key(&byok_group.api_key_env).is_some();
        let key_status = if has_key {
            format!(
                "{}🔑 {}{}",
                GREEN,
                crate::config::tr_en("Hazır", "Ready"),
                RESET
            )
        } else {
            format!(
                "{}🔒 {}{}",
                YELLOW,
                crate::config::tr_en("/byok ile anahtar gir", "/byok to enter key"),
                RESET
            )
        };
        let items: Vec<MenuItem> = byok_group
            .models
            .iter()
            .map(|m| MenuItem {
                key: m.key.clone(),
                badge: format!("{} · {}", m.badge, key_status),
                desc: m.desc.clone(),
            })
            .collect();
        providers.push((format!("{} ({})", byok_group.title, items.len()), items));
        prov_ids.push(Some(byok_group.id.clone()));
    }

    if providers.is_empty() {
        return None;
    }

    let mut orig_termios = empty_termios();
    let has_tty = unsafe { tcgetattr(0, &mut orig_termios) == 0 };
    if !has_tty {
        return Some(current_key.to_string());
    }

    let _term_guard = TermiosGuard(orig_termios);
    enter_raw_mode(&orig_termios, 1);

    let mut prov_idx = if local_first {
        local_idx_in_providers.unwrap_or(0)
    } else {
        0
    };
    let mut model_indices = vec![0usize; providers.len()];
    for (pi, (_, list)) in providers.iter().enumerate() {
        let pid_opt = prov_ids.get(pi).and_then(|p| p.as_deref());
        if let Some(mi) = list.iter().position(|m| {
            m.key == current_key
                || m.key.strip_prefix("local:") == Some(current_key)
                || pid_opt.is_some_and(|pid| {
                    current_key
                        .strip_prefix(&format!("{}:", pid))
                        .is_some_and(|rest| rest == m.key)
                })
        }) {
            if !local_first {
                prov_idx = pi;
            }
            model_indices[pi] = mi;
        }
    }
    let mut focus_right = local_first;
    // Per-column free-text filters: typing narrows the focused column only.
    let mut prov_query = String::new();
    let mut model_query = String::new();

    let body_rows = 10usize;
    let total_lines = body_rows + 4;

    print!("\n\x1b[?25l");
    for _ in 0..total_lines {
        println!();
    }

    let mut stdin = io::stdin();
    let mut kbuf = [0u8; 8];
    let mut result: Option<String> = None;
    let _ = crate::take_resized();
    let (mut last_rows, mut last_cols) = term_size();
    let mut needs_redraw = true;

    loop {
        let (rows, cols) = term_size();
        if (rows, cols) != (last_rows, last_cols) || crate::take_resized() {
            thread::sleep(Duration::from_millis(16));
            let (r2, c2) = term_size();
            last_rows = r2;
            last_cols = c2;
            needs_redraw = true;
        }

        if needs_redraw {
            let max_w = last_cols.saturating_sub(2).max(20);
            let right_inner_w = max_w.saturating_sub(33);
            let vis_provs = filter_provider_rows(&providers, &prov_query);
            // Keep the two columns in sync with the active filters: the highlight always
            // lands on a visible row so `Enter` never picks an off-screen provider/model.
            if !vis_provs.is_empty() && !vis_provs.contains(&prov_idx) {
                prov_idx = vis_provs[0];
            }
            let cur_models = &providers[prov_idx].1;
            let vis_models = filter_menu_rows(cur_models, &model_query);
            if !vis_models.is_empty() && !vis_models.contains(&model_indices[prov_idx]) {
                model_indices[prov_idx] = vis_models[0];
            }
            let mut out = format!("\x1b[?2026h\x1b[?7l\x1b[{}A\r\x1b[J", total_lines);
            let raw_hdr = format!(
                "{}╭─ {} {}(←/→: Column · ↑/↓: Select · Type to search · Enter · Esc){} ",
                CYAN,
                tr_en("🧠 MODEL SEÇİMİ", "🧠 SELECT MODEL"),
                YELLOW,
                CYAN
            );
            let (trunc_hdr, used_hdr) = truncate_ansi_to_width(&raw_hdr, max_w.saturating_sub(1));
            let hdr_pad = "─".repeat(max_w.saturating_sub(used_hdr + 1));
            out.push_str(&format!("{}{}{}╮{}\n", trunc_hdr, CYAN, hdr_pad, RESET));

            let left_hdr_style = if !prov_query.is_empty() {
                format!(
                    "\x1b[1;36m❯ {}[{}] {}/{}\x1b[0m",
                    tr_en("SAĞLAYICI ", "PROVIDER "),
                    prov_query,
                    vis_provs.len(),
                    providers.len()
                )
            } else if !focus_right {
                tr_en(
                    "\x1b[1;36m❯ SAĞLAYICI (PROVIDER)\x1b[0m      ",
                    "\x1b[1;36m❯ PROVIDER            \x1b[0m      ",
                )
                .to_string()
            } else {
                tr_en(
                    "\x1b[2m  SAĞLAYICI (← Geri)\x1b[0m        ",
                    "\x1b[2m  PROVIDER (← Back)   \x1b[0m      ",
                )
                .to_string()
            };
            let right_hdr_style = if !model_query.is_empty() {
                format!(
                    "\x1b[1;36m❯ {}[{}] {}/{}\x1b[0m",
                    tr_en("MODELLER ", "MODELS "),
                    model_query,
                    vis_models.len(),
                    providers[prov_idx].1.len()
                )
            } else if focus_right {
                tr_en(
                    "\x1b[1;36m❯ MODELLER (↑/↓ Seç · Enter: Uygula)\x1b[0m",
                    "\x1b[1;36m❯ MODELS (↑/↓ Select · Enter: Apply)\x1b[0m",
                )
                .to_string()
            } else {
                tr_en(
                    "\x1b[2m  MODELLER (→ Sağ ok ile geç)\x1b[0m",
                    "\x1b[2m  MODELS (→ Right arrow to focus)\x1b[0m",
                )
                .to_string()
            };
            let (r_hdr_trunc, r_hdr_used) =
                truncate_ansi_to_width(&right_hdr_style, right_inner_w.saturating_sub(1));
            let r_hdr_pad = " ".repeat(right_inner_w.saturating_sub(r_hdr_used + 1));
            let col_hdr = format!(
                "{}│{} {} {}│{} {}{}{}│{}",
                CYAN, RESET, left_hdr_style, CYAN, RESET, r_hdr_trunc, r_hdr_pad, CYAN, RESET
            );
            out.push_str(&format!("{}\n", col_hdr));

            let sep_right = "─".repeat(right_inner_w);
            let raw_sep = format!(
                "{}├──────────────────────────────┼{}┤{}",
                CYAN, sep_right, RESET
            );
            out.push_str(&format!("{}\n", raw_sep));

            let p_pos = vis_provs.iter().position(|&i| i == prov_idx).unwrap_or(0);
            let p_win_start = if p_pos >= body_rows {
                p_pos + 1 - body_rows
            } else {
                0
            };

            let cur_m_idx = model_indices[prov_idx].min(cur_models.len().saturating_sub(1));
            let m_pos = vis_models.iter().position(|&i| i == cur_m_idx).unwrap_or(0);
            let m_win_start = if m_pos >= body_rows {
                m_pos + 1 - body_rows
            } else {
                0
            };

            for r in 0..body_rows {
                let p_idx = vis_provs.get(p_win_start + r).copied();
                let left_cell = if let Some(p_idx) = p_idx {
                    let pname = providers[p_idx].0.as_str();
                    let (trunc_p, used_p) = truncate_ansi_to_width(pname, 25);
                    let pad_p = " ".repeat(25usize.saturating_sub(used_p));
                    if p_idx == prov_idx {
                        if !focus_right {
                            format!(
                                " \x1b[1;36m❯\x1b[0m \x1b[1;7;36m {}{} \x1b[0m",
                                trunc_p, pad_p
                            )
                        } else {
                            format!(
                                " \x1b[1;32m▸\x1b[0m \x1b[1;36m {}{} \x1b[0m",
                                trunc_p, pad_p
                            )
                        }
                    } else {
                        format!("    {}{}{}{} ", DIM, trunc_p, pad_p, RESET)
                    }
                } else {
                    " ".repeat(30)
                };

                let m_idx = vis_models.get(m_win_start + r).copied();
                let right_cell = if let Some(m_idx) = m_idx {
                    let m = &cur_models[m_idx];
                    let (trunc_k, used_k) = truncate_ansi_to_width(&m.key, 24);
                    let pad_k = " ".repeat(24usize.saturating_sub(used_k));
                    let (trunc_b, used_b) = truncate_ansi_to_width(&m.badge, 22);
                    let pad_b = " ".repeat(22usize.saturating_sub(used_b));
                    if m_idx == cur_m_idx {
                        if focus_right {
                            format!(
                                "\x1b[1;36m❯\x1b[0m \x1b[1;7;36m {}{} \x1b[0m {}│{} {}{} {}│{} {}{}{}",
                                trunc_k, pad_k, DIM, RESET, trunc_b, pad_b, DIM, RESET, BOLD, m.desc, RESET
                            )
                        } else {
                            format!(
                                "\x1b[1;36m▸\x1b[0m  {}{}{}{}  {}│{} {}{} {}│{} {}{}{}",
                                BOLD,
                                trunc_k,
                                pad_k,
                                RESET,
                                DIM,
                                RESET,
                                trunc_b,
                                pad_b,
                                DIM,
                                RESET,
                                DIM,
                                m.desc,
                                RESET
                            )
                        }
                    } else {
                        format!(
                            "   {}{}{}  {}│{} {}{} {}│{} {}{}{}",
                            trunc_k,
                            pad_k,
                            RESET,
                            DIM,
                            RESET,
                            trunc_b,
                            pad_b,
                            DIM,
                            RESET,
                            DIM,
                            m.desc,
                            RESET
                        )
                    }
                } else {
                    String::new()
                };

                let (r_trunc, r_used) =
                    truncate_ansi_to_width(&right_cell, right_inner_w.saturating_sub(1));
                let r_pad = " ".repeat(right_inner_w.saturating_sub(r_used + 1));
                let full_row = format!(
                    "{}│{}{}{}│{} {}{}{}│{}",
                    CYAN, RESET, left_cell, CYAN, RESET, r_trunc, r_pad, CYAN, RESET
                );
                out.push_str(&format!("{}\n", full_row));
            }

            let bot_right = "─".repeat(right_inner_w);
            let raw_bot = format!(
                "{}╰──────────────────────────────┴{}╯{}",
                CYAN, bot_right, RESET
            );
            out.push_str(&format!("{}\n\x1b[?7h\x1b[?2026l", raw_bot));
            print!("{}", out);
            let _ = io::stdout().flush();
            needs_redraw = false;
        }

        let n = match stdin.read(&mut kbuf) {
            Ok(0) => continue,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                needs_redraw = true;
                continue;
            }
            Err(_) => break,
            Ok(mut n) => {
                if n == 1 && kbuf[0] == 27 {
                    thread::sleep(Duration::from_millis(25));
                    if let Ok(n2) = stdin.read(&mut kbuf[1..]) {
                        n += n2;
                    }
                }
                needs_redraw = true;
                n
            }
        };

        if n == 1 && kbuf[0] == 27 {
            if !prov_query.is_empty() || !model_query.is_empty() {
                // First Esc clears the active filter, a second one closes the picker.
                prov_query.clear();
                model_query.clear();
                needs_redraw = true;
                continue;
            }
            break;
        }

        if let Some(nav) = decode_nav_key(&kbuf[..n]) {
            match nav {
                // Left/Right are absolute, not a toggle: Right enters the model column, Left
                // returns to the provider column, matching the header hints
                // (`PROVIDER (← Back)` / `MODELS (→ Right arrow to focus)`). Tab is the only
                // explicit toggle. None of them wrap at the edge.
                NavKey::Left | NavKey::Right | NavKey::BackTab => {
                    focus_right = step_column(nav, focus_right);
                }
                NavKey::Up | NavKey::Down => {
                    let forward = nav == NavKey::Down;
                    if !focus_right {
                        let vis = filter_provider_rows(&providers, &prov_query);
                        if !vis.is_empty() {
                            let pos = vis.iter().position(|&i| i == prov_idx).unwrap_or(0);
                            prov_idx = if forward {
                                vis[(pos + 1) % vis.len()]
                            } else {
                                vis[(pos + vis.len() - 1) % vis.len()]
                            };
                        }
                    } else {
                        let vis = filter_menu_rows(&providers[prov_idx].1, &model_query);
                        if !vis.is_empty() {
                            let cur = model_indices[prov_idx];
                            let pos = vis.iter().position(|&i| i == cur).unwrap_or(0);
                            model_indices[prov_idx] = if forward {
                                vis[(pos + 1) % vis.len()]
                            } else {
                                vis[(pos + vis.len() - 1) % vis.len()]
                            };
                        }
                    }
                }
                NavKey::Home | NavKey::End => {}
            }
            continue;
        }

        match kbuf[0] {
            b'\r' | b'\n' => {
                if !focus_right {
                    // Apply the provider filter selection, then move into the model column.
                    let vis = filter_provider_rows(&providers, &prov_query);
                    if !vis.is_empty() {
                        let pos = vis.iter().position(|&i| i == prov_idx).unwrap_or(0);
                        prov_idx = vis[pos];
                    }
                    focus_right = true;
                } else {
                    let cur_models = &providers[prov_idx].1;
                    let vis = filter_menu_rows(cur_models, &model_query);
                    let chosen = if vis.is_empty() {
                        None
                    } else {
                        let cur = model_indices[prov_idx].min(cur_models.len().saturating_sub(1));
                        let pos = vis.iter().position(|&i| i == cur).unwrap_or(0);
                        cur_models.get(vis[pos])
                    };
                    if let Some(m) = chosen {
                        let collides_in_earlier_group =
                            providers[..prov_idx].iter().any(|(_, items)| {
                                items
                                    .iter()
                                    .any(|other| other.key.eq_ignore_ascii_case(&m.key))
                            });
                        if collides_in_earlier_group {
                            if let Some(Some(pid)) = prov_ids.get(prov_idx) {
                                result = Some(format!("{}:{}", pid, m.key));
                            } else {
                                result = Some(m.key.clone());
                            }
                        } else {
                            result = Some(m.key.clone());
                        }
                    }
                    break;
                }
            }
            b'\t' => {
                focus_right = !focus_right;
            }
            127 | 8 => {
                // Backspace edits the search box of the focused column.
                if focus_right {
                    model_query.pop();
                } else {
                    prov_query.pop();
                }
            }
            3 => break,
            // Any other printable bytes become a live filter for the focused column.
            b if b >= 32 => {
                if let Ok(text) = std::str::from_utf8(&kbuf[..n]) {
                    let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                    if !typed.is_empty() {
                        if focus_right {
                            model_query.push_str(&typed);
                        } else {
                            prov_query.push_str(&typed);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    print!("\x1b[{}A\r\x1b[J\x1b[?7h\x1b[?25h", total_lines);
    let _ = io::stdout().flush();
    unsafe {
        tcsetattr(0, TCSANOW, &orig_termios);
    }
    result
}
