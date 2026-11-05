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
fn menu_row_matches(item: &MenuItem, query: &str) -> bool {
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
fn step_in_visible(visible: &[usize], current: usize, forward: bool) -> Option<usize> {
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
fn step_column(nav: NavKey, focus_right: bool) -> bool {
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
                "Switch CLI interface language (English ⇄ Türkçe)",
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
        }
    }

    // 3. Built-in BYOK Catalog Providers (OpenAI, Anthropic, Gemini, OpenRouter, DeepSeek, MiniMax, GLM, Groq, xAI, Ollama)
    for byok_group in byok_catalog(&cfg) {
        if byok_group.id == "ollama" && local_idx_in_providers.is_some() {
            continue;
        }
        let has_key = byok_group.is_local || resolve_api_key(&byok_group.api_key_env).is_some();
        let key_status = if has_key {
            format!("{}🔑 Hazır{}", GREEN, RESET)
        } else {
            format!("{}🔒 /byok ile anahtar gir{}", YELLOW, RESET)
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
        if let Some(mi) = list
            .iter()
            .position(|m| m.key == current_key || m.key.strip_prefix("local:") == Some(current_key))
        {
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
                "{}╭─ {} {}(←/→: Sütun · ↑/↓: Seç · Yaz: ara · Enter · Esc){} ",
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
                        result = Some(m.key.clone());
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

fn prompt_inline_line(prompt_label: &str) -> Option<String> {
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

fn open_url_in_browser(url: &str) {
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

fn provider_portal_url(provider_id: &str) -> Option<&'static str> {
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

fn detect_cli_token_for_provider(provider_id: &str) -> Option<(String, &'static str)> {
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
        if matches!(low.as_str(), "local" | "m3" | "llama" | "vllm" | "lmstudio") {
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
                "\n{}╭─ 🔐 {} ───────────────────────────────────────────{}\n{}│{} {}⚡ {} üzerinden oturum otomatik algılandı ve bağlandı!{}\n{}╰────────────────────────────────────────────────────────────────────────────{}",
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
            badge: format!("{}🇹🇷 Türkçe (TR){}", CYAN, RESET),
            desc: "CLI arayüzünü, menüleri ve durum çubuklarını Türkçe kullan".to_string(),
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
            "🌐 LANGUAGE SELECTION / DİL SEÇİMİ (/lang)",
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
                format!(" {}[AKTİF]{}", GREEN, RESET)
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

    let chosen_key =
        select_interactive_menu("🗂️  GEÇMİŞ OTURUMLAR (/resume)", &items, initial_idx)?;
    let chosen_idx = items.iter().position(|it| it.key == chosen_key)?;
    sessions.get(chosen_idx).map(|s| s.path.clone())
}
#[cfg(test)]
mod menu_tests {
    use super::*;

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
                vec![menu_item("claude-sonnet", "Sonnet", "hızlı")],
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
}
