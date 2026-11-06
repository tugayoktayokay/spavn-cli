use super::*;

pub fn strip_internal_tags_for_display(raw: &str) -> String {
    let mut s = raw.trim();
    loop {
        if let Some(start) = s.find("<spavn_knowledge_artifacts>") {
            if let Some(end) = s[start..].find("</spavn_knowledge_artifacts>") {
                s = s[start + end + "</spavn_knowledge_artifacts>".len()..].trim();
                continue;
            }
        }
        if let Some(start) = s.find("<active_skill") {
            if let Some(end) = s[start..].find("</active_skill>") {
                s = s[start + end + "</active_skill>".len()..].trim();
                continue;
            }
        }
        if s.starts_with("[SPAVN ROLE:") || s.starts_with("[SPAVN PHASE:") {
            if let Some(nl) = s.find("\n\n") {
                s = s[nl + 2..].trim();
                continue;
            } else if let Some(nl) = s.find('\n') {
                s = s[nl + 1..].trim();
                continue;
            }
        }
        break;
    }
    s.to_string()
}

pub enum PromptEvent {
    Submit(String),
    CycleAccess,
    CyclePhase,
    Reload,
    ShowSkills,
    Exit,
}

pub fn fetch_quick_quota_line() -> String {
    let cfg = load_user_config();
    if let Some(ref script_raw) = cfg.quota_quick_script {
        let script_path = expand_user_path(script_raw);
        if script_path.exists() {
            let node_bin = if Path::new("/opt/homebrew/bin/node").exists() {
                "/opt/homebrew/bin/node"
            } else {
                "node"
            };
            if let Ok(out) = Command::new(node_bin)
                .arg(script_path.to_string_lossy().to_string())
                .output()
            {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    tr_en("BYOK & Yerel Hazır (/byok)", "BYOK & Local Ready (/byok)").to_string()
}

pub fn render_welcome_header(
    _endpoint: &ModelEndpoint,
    _access: AccessMode,
    _default_host: &str,
    cwd: &str,
    session_name: &str,
) {
    let (_, cols) = term_size();
    let full_w = cols.saturating_sub(2).max(6);
    let home = std::env::var("HOME").unwrap_or_default();
    let short_cwd = if !home.is_empty() && cwd.starts_with(&home) {
        let rest = &cwd[home.len()..];
        if rest.is_empty() {
            "~".to_string()
        } else {
            format!("~{}", rest)
        }
    } else {
        cwd.to_string()
    };
    let short_session = session_name.trim_end_matches(".jsonl");
    let git_branch = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty());

    let p_top = format!("{}╭─{} ", CYAN, RESET);
    let p_bot = format!("{}╰─{} ", CYAN, RESET);
    let sep = format!(" {}·{} ", DIM, RESET);
    let mut row1_items = vec![
        format!("{}⚡ SPAVN v{}{}", BOLD, env!("CARGO_PKG_VERSION"), RESET),
        format!("📂 {}{}{}", DIM, short_cwd, RESET),
    ];
    if let Some(br) = git_branch {
        row1_items.push(format!("🌿 {}{}{}", GREEN, br, RESET));
    }
    row1_items.push(format!(
        "💬 {}{}{}",
        DIM,
        short_session.trim_start_matches("session-"),
        RESET
    ));

    println!();
    for line in flex_wrap_items(&p_top, &p_bot, &sep, &row1_items, full_w) {
        println!("{}", line);
    }
}

/// Boxed upgrade notice for the welcome banner and the input loop.
///
/// Prints nothing unless a newer release was actually found, so the caller can render it
/// unconditionally. The wording carries the exact command for the channel this binary was
/// installed from, so the user never has to look it up.
pub fn render_update_notice(info: &crate::update::UpdateInfo) {
    let width = info.url.chars().count().max(info.command.chars().count()) + 12;
    let rule = "─".repeat(width.min(72));
    let title = tr_en("YENİ SÜRÜM VAR", "NEW VERSION AVAILABLE");
    println!("\n{}╭─ ⬆️  {} {}{}", YELLOW, title, rule, RESET);
    println!(
        "{}│{}  {}v{} → {}{}v{}{}",
        YELLOW, RESET, DIM, info.current, BOLD, RESET, info.latest, RESET
    );
    println!(
        "{}│{}  {}: {}{}{}",
        YELLOW,
        RESET,
        tr_en("Güncelle", "Update"),
        BOLD,
        info.command,
        RESET
    );
    if !info.url.is_empty() {
        println!("{}│{}  {}{}{}", YELLOW, RESET, DIM, info.url, RESET);
    }
    println!("{}╰{}{}", YELLOW, rule, RESET);
}

pub(crate) fn bar_for_pct(pct: f64) -> String {
    let filled = ((pct / 10.0).round() as usize).min(10);
    let empty = 10 - filled;
    let color = if pct >= 50.0 {
        GREEN
    } else if pct >= 20.0 {
        YELLOW
    } else {
        RED
    };
    format!(
        "{}{}{}{}",
        color,
        "█".repeat(filled),
        "░".repeat(empty),
        RESET
    )
}

pub fn render_full_quota_card() {
    let cfg = load_user_config();
    let Some(ref script_raw) = cfg.quota_full_script else {
        println!(
            "\n{}ℹ️  Harici kota betiği tanımlı değil (~/.spavn/config.json -> quota_full_script). BYOK sağlayıcı durumu için `/byok` komutunu kullanın.{}",
            YELLOW, RESET
        );
        return;
    };
    let script_path = expand_user_path(script_raw);
    if !script_path.exists() {
        println!(
            "\n{}Kota betiği bulunamadı: {}{}",
            YELLOW,
            script_path.display(),
            RESET
        );
        return;
    }

    let mut spin = LiveSpinner::start("Çoklu hesap kotaları sorgulanıyor");
    let node_bin = if Path::new("/opt/homebrew/bin/node").exists() {
        "/opt/homebrew/bin/node"
    } else {
        "node"
    };
    let out = Command::new(node_bin)
        .arg(script_path.to_string_lossy().to_string())
        .output();
    spin.stop();

    let Ok(o) = out else {
        println!("{}Kota bilgisi alınamadı.{}", RED, RESET);
        return;
    };
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&o.stdout) else {
        println!("{}Kota yanıtı ayrıştırılamadı.{}", RED, RESET);
        return;
    };

    println!(
        "\n{}╭─ 🔋 SPAVN KOTA PANELİ ─────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
    if let Some(arr) = parsed.as_array() {
        for acc in arr {
            let email = acc.get("email").and_then(|v| v.as_str()).unwrap_or("?");
            let active = acc.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
            let badge = if active { "🟢 [AKTİF]" } else { "⚪" };
            println!("{}│{} {} {}{}{}", CYAN, RESET, badge, BOLD, email, RESET);
            if let Some(groups) = acc.get("groups").and_then(|v| v.as_array()) {
                for g in groups {
                    let gname = g.get("displayName").and_then(|v| v.as_str()).unwrap_or("");
                    if let Some(buckets) = g.get("buckets").and_then(|v| v.as_array()) {
                        for b in buckets {
                            let bname = b.get("displayName").and_then(|v| v.as_str()).unwrap_or("");
                            let pct = b
                                .get("remainingPercent")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            println!(
                                "{}│{}    ├─ {:<18} {:<26} {} {:>5.1}%",
                                CYAN,
                                RESET,
                                gname,
                                bname,
                                bar_for_pct(pct),
                                pct
                            );
                        }
                    }
                }
            }
        }
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}

pub fn render_fleet_radar() {
    let cfg = load_user_config();
    let mut spin = LiveSpinner::start("Tanımlı çalışma hedefleri (hosts) taranıyor");

    let handles: Vec<_> = cfg
        .hosts
        .into_iter()
        .map(|h| {
            thread::spawn(move || {
                let start = Instant::now();
                let res = if h.ssh_target == "local" || h.key == "local" {
                    Command::new("uptime").output()
                } else {
                    Command::new("ssh")
                        .args(["-o", "ConnectTimeout=3", &h.ssh_target, "uptime"])
                        .output()
                };
                let ms = start.elapsed().as_millis();
                match res {
                    Ok(o) if o.status.success() => {
                        let up = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        (h.key, true, ms, up)
                    }
                    _ => (h.key, false, ms, "Ulaşılamadı".to_string()),
                }
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    spin.stop();

    println!(
        "\n{}╭─ 🛰️  SPAVN HEDEF SUNUCU RADARI (~/.spavn/config.json) ─────────────────────{}",
        CYAN, RESET
    );
    for (label, ok, ms, up) in results {
        let status = if ok {
            format!("{}🟢 ONLINE ({:>3}ms){}", GREEN, ms, RESET)
        } else {
            format!("{}🔴 OFFLINE{}", RED, RESET)
        };
        println!(
            "{}│{} {:<17} │ {} │ {}{}{}",
            CYAN, RESET, label, status, DIM, up, RESET
        );
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}

pub fn render_assistant_header(model_key: &str) {
    close_thinking_box_if_open();
    spinner_print(&format!(
        "\n{}╭─ {}⚡ SPAVN{} {}· {}{}{}\n",
        CYAN, BOLD, RESET, DIM, CYAN, model_key, RESET
    ));
}

/// Renders an inline image preview in iTerm2 / WezTerm via OSC 1337 (`\x1b]1337;File=...`).
pub fn render_inline_image_preview(img: &crate::router::DraggedImage) {
    pause_active_spinner();
    let kb = (img.size_bytes as f64 / 1024.0).max(0.1);
    println!(
        "{}│{} {}🖼️  [Görsel Eklendi: {}{}{} ({:.1} KB)]{}",
        CYAN, RESET, GREEN, BOLD, img.filename, RESET, kb, RESET
    );
    let term_prog = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let lc_term = std::env::var("LC_TERMINAL").unwrap_or_default();
    if term_prog == "iTerm.app" || term_prog == "WezTerm" || lc_term == "iTerm2" {
        println!(
            "{}│{} \x1b]1337;File=inline=1;width=46;height=12;preserveAspectRatio=1:{}\x07",
            CYAN, RESET, img.base64
        );
        let _ = io::stdout().flush();
    }
    resume_active_spinner();
}

pub fn render_assistant_footer(
    model_key: &str,
    access_badge: &str,
    ttft_ms: u128,
    total_sec: f64,
    tok_per_sec: f64,
) {
    let speed_color = if tok_per_sec >= 40.0 {
        GREEN
    } else if tok_per_sec >= 18.0 {
        CYAN
    } else {
        DIM
    };
    spinner_print(&format!(
        "\r{}╰─{} {}✓ {}{} {}· {} · TTFT {}ms · {:.1}s · {}{:.1} tok/s{}\n",
        CYAN,
        RESET,
        GREEN,
        RESET,
        model_key,
        DIM,
        access_badge,
        ttft_ms,
        total_sec,
        speed_color,
        tok_per_sec,
        RESET
    ));
}

pub use crate::markdown::MarkdownStreamRenderer;

pub fn render_session_resume_preview(session_path: &Path, messages: &[ChatMessage]) {
    let fname = session_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let short_name = fname
        .trim_start_matches("session-")
        .trim_end_matches(".jsonl");
    let text_msgs: Vec<&ChatMessage> = messages
        .iter()
        .filter(|m| {
            (m.role == "user" || m.role == "assistant")
                && m.content
                    .as_deref()
                    .map(|s| !strip_internal_tags_for_display(s).is_empty())
                    .unwrap_or(false)
        })
        .collect();
    let last_user_preview = text_msgs
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_deref())
        .map(strip_internal_tags_for_display)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains('╭') && !l.contains('╰') && !l.contains('│'))
        .collect::<Vec<_>>()
        .join(" ");
    let snippet: String = last_user_preview.chars().take(56).collect();
    let suffix = if snippet.is_empty() {
        String::new()
    } else if last_user_preview.chars().count() > 56 {
        format!(" · \"{}…\"", snippet)
    } else {
        format!(" · \"{}\"", snippet)
    };

    let (_, cols) = term_size();
    let full_w = cols.saturating_sub(2).max(6);
    let raw = format!(
        "{}╭─ 🗂️  {}{}{} {}({} {}{}){}",
        CYAN,
        BOLD,
        short_name,
        RESET,
        DIM,
        text_msgs.len(),
        tr_en("mesaj", "msgs"),
        suffix,
        RESET
    );
    let (trunc, _) = truncate_ansi_to_width(&raw, full_w);
    println!("\n{}", trunc);
}

#[cfg(test)]
mod recap_tests {
    use super::*;
    use crate::markdown::render_markdown_table;
    use crate::router::ChatMessage;

    #[test]
    fn recap_line_summarizes_latest_turn_and_ignores_system_only() {
        let sys_only = vec![ChatMessage::system("sys prompt")];
        assert!(build_session_recap_line(&sys_only, 100).is_none());

        let msgs = vec![
            ChatMessage::system("sys prompt"),
            ChatMessage::user("[SPAVN ROLE: CODER] Implement\n\nrecap özelliğini kodlayalım"),
            ChatMessage {
                role: "assistant".to_string(),
                content: Some(
                    "### Durum\n- **Idle Recap**: 6 saniye sonra alt dock'ta gösterilir."
                        .to_string(),
                ),
                reasoning_content: None,
                thought_signature: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];
        let line = build_session_recap_line(&msgs, 120).expect("recap should be generated");
        assert!(line.contains("recap:"));
        assert!(line.contains("recap özelliğini kodlayalım"));
        assert!(line.contains("Durum"));
    }

    #[test]
    fn markdown_table_renders_box_borders_and_wraps_wide_cells() {
        let rows = vec![
            "| # | Dosya | Tespit Edilen Kusur | Uygulanan Çözüm |".to_string(),
            "|---|---|---|---|".to_string(),
            "| **1** | `src/hud.rs` | **Hızlı Düşünce Akışı:** `stream_thinking_delta` ve `spinner_print_locked` güncellendi. | Ortak `rebuild_spinner_dock_lines()` fonksiyonu çıkarıldı. |".to_string(),
            "| **2** | `src/router.rs` | Anthropic native akışında araç bildirimi eklendi. | `notify_tool_call_streaming` devreye alındı. |".to_string(),
        ];
        let rendered = render_markdown_table(&rows, 100);
        assert!(rendered.contains('╭'));
        assert!(rendered.contains('┬'));
        assert!(rendered.contains('┼'));
        assert!(rendered.contains('╰'));
        assert!(!rendered.contains("|---|"));
        for line in rendered.lines() {
            let (_, vw) = truncate_ansi_to_width(line, usize::MAX);
            assert!(
                vw <= 100,
                "table line visual width {} exceeded 100 cols: {}",
                vw,
                line
            );
        }
    }

    #[test]
    fn atomic_image_token_deletes_and_navigates_as_single_unit() {
        let s = "merhaba [🖼️ Screenshot 2026-11-04.png (136KB)] test";
        let mut chars: Vec<char> = s.chars().collect();
        let close_pos = chars.iter().position(|&c| c == ']').unwrap() + 1;
        let span = super::image_token_span_before_cursor(&chars, close_pos)
            .expect("should detect atomic image token before cursor");
        assert_eq!(
            super::image_token_span_at_cursor(&chars, span.0),
            Some(span)
        );
        chars.drain(span.0..span.1);
        let after: String = chars.into_iter().collect();
        assert_eq!(after, "merhaba  test");
    }

    #[test]
    fn zero_cost_live_tasks_sync_and_advance() {
        let _g = crate::live_task_test_lock();
        crate::clear_live_tasks();
        let plan =
            "Here is the plan:\n- [ ] Update config.rs\n- [ ] Refactor hud.rs\n- [ ] Run cargo test";
        crate::sync_live_tasks_from_text(plan);
        let tasks = crate::list_live_tasks();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].status, crate::LiveTaskStatus::InProgress);

        crate::advance_live_tasks_on_tool("edit", "src/config.rs");
        let tasks2 = crate::list_live_tasks();
        assert_eq!(tasks2[0].status, crate::LiveTaskStatus::Done);
        assert_eq!(tasks2[1].status, crate::LiveTaskStatus::InProgress);

        let bar = crate::format_live_tasks_compact_bar(100).expect("should render task bar");
        assert!(bar.contains("[1/3]"));
        crate::clear_live_tasks();
    }
}
