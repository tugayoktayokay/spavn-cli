use crate::config::{home_dir, spavn_dir};
use std::fs;
use std::path::PathBuf;

const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub name: String,
    pub source: &'static str,
    pub path: PathBuf,
    pub summary: String,
}

pub fn spavn_skills_dir() -> PathBuf {
    let dir = spavn_dir().join("skills");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn first_summary_line(content: &str) -> String {
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t == "---" || t.starts_with("name:") {
            continue;
        }
        let clean = t
            .trim_start_matches('#')
            .trim_start_matches("description:")
            .trim()
            .trim_matches('"')
            .trim_matches('\'');
        if !clean.is_empty() {
            return clean.chars().take(62).collect();
        }
    }
    "Custom skill".to_string()
}

pub fn discover_skills() -> Vec<SkillEntry> {
    let mut list = Vec::new();
    let primary_dir = spavn_skills_dir();

    if let Ok(entries) = fs::read_dir(&primary_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("md") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    let content = fs::read_to_string(&p).unwrap_or_default();
                    list.push(SkillEntry {
                        name: stem.to_string(),
                        source: "spavn",
                        path: p,
                        summary: first_summary_line(&content),
                    });
                }
            }
        }
    }

    let shared_skills = home_dir().join(".claude").join("skills");
    if let Ok(entries) = fs::read_dir(&shared_skills) {
        for e in entries.flatten() {
            let p = e.path();
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.starts_with('.') || name.starts_with("gsd-") {
                continue;
            }
            let skill_md = p.join("SKILL.md");
            if skill_md.exists() {
                let content = fs::read_to_string(&skill_md).unwrap_or_default();
                list.push(SkillEntry {
                    name: name.to_string(),
                    source: "shared",
                    path: skill_md,
                    summary: first_summary_line(&content),
                });
            }
        }
    }

    list.sort_by(|a, b| a.source.cmp(b.source).then_with(|| a.name.cmp(&b.name)));
    list
}

pub fn save_custom_skill(name: &str, instructions: &str) -> Result<PathBuf, String> {
    let clean_name: String = name
        .trim()
        .trim_start_matches('@')
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if clean_name.is_empty() {
        return Err("Geçersiz yetenek (skill) adı.".to_string());
    }
    let path = spavn_skills_dir().join(format!("{}.md", clean_name));
    let body = format!("# Skill: {}\n{}\n", clean_name, instructions.trim());
    fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn remove_custom_skill(name: &str) -> Result<(), String> {
    let clean_name = name.trim().trim_start_matches('@');
    let path = spavn_skills_dir().join(format!("{}.md", clean_name));
    if !path.exists() {
        return Err(format!("~/.spavn/skills/{}.md bulunamadı.", clean_name));
    }
    fs::remove_file(&path).map_err(|e| e.to_string())
}

pub fn load_skill_content(name: &str) -> Option<(SkillEntry, String)> {
    let clean = name.trim().trim_start_matches('@').to_lowercase();
    let skills = discover_skills();
    for s in skills {
        if s.name.to_lowercase() == clean {
            if let Ok(content) = fs::read_to_string(&s.path) {
                return Some((s, content));
            }
        }
    }
    None
}

pub fn expand_inline_skills(user_input: &str) -> (String, Vec<String>) {
    let mut candidate_names: Vec<String> = Vec::new();

    for token in user_input.split_whitespace() {
        if let Some(tag) = token.strip_prefix('@') {
            let clean: String = tag
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !clean.is_empty() {
                candidate_names.push(clean);
            }
        }
    }

    let lower = user_input.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| w.len() >= 3)
        .collect();
    for s in discover_skills() {
        if s.source == "spavn" && words.contains(&s.name.to_lowercase().as_str()) {
            candidate_names.push(s.name);
        }
    }

    let mut activated = Vec::new();
    let mut extra_blocks = String::new();

    for name in candidate_names {
        if let Some((entry, content)) = load_skill_content(&name) {
            if !activated.contains(&entry.name) {
                activated.push(entry.name.clone());
                let capped: String = content.chars().take(2_000).collect();
                extra_blocks.push_str(&format!(
                    "\n\n<active_skill name=\"{}\" path=\"{}\">\n{}\n</active_skill>",
                    entry.name,
                    entry.path.display(),
                    capped.trim()
                ));
            }
        }
    }

    if extra_blocks.is_empty() {
        (user_input.to_string(), activated)
    } else {
        (format!("{}{}", user_input, extra_blocks), activated)
    }
}

pub fn render_skills_panel() {
    let skills = discover_skills();
    println!(
        "\n{}╭─ 🧩 SPAVN YETENEK (SKILLS) MERKEZİ (Mesajda @yetenek yazarak tetikleyin) ────{}",
        CYAN, RESET
    );
    for s in &skills {
        let badge = if s.source == "spavn" {
            format!("{}[SPAVN]{}", GREEN, RESET)
        } else {
            format!("{}[ORTAK]{}", MAGENTA, RESET)
        };
        println!(
            "{}│{}  {} {:<26} {}│{} {}{}{}",
            CYAN,
            RESET,
            badge,
            format!("@{}", s.name),
            CYAN,
            RESET,
            DIM,
            s.summary,
            RESET
        );
    }
    println!(
        "{}├─ ⚡ Komutlar ──────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
    println!(
        "{}│{}  {}/skill add <ad> <kural>{}  Yeni yetenek ekle (~/.spavn/skills/<ad>.md)",
        CYAN, RESET, YELLOW, RESET
    );
    println!(
        "{}│{}  {}/skill show <ad>{}         Yetenek içeriğini gör · {}/skill rm <ad>{} Sil",
        CYAN, RESET, YELLOW, RESET, YELLOW, RESET
    );
    println!(
        "{}│{}  {}@<ad> <mesajınız>{}        Sadece o mesajda yeteneği bağlama enjekte et",
        CYAN, RESET, GREEN, RESET
    );
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}
