use crate::config::spavn_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

pub fn project_slug(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("default")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn today_ymd() -> String {
    if let Ok(out) = Command::new("/bin/date").arg("+%Y-%m-%d").output() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    "2026-11-03".to_string()
}

fn now_iso_local() -> String {
    if let Ok(out) = Command::new("/bin/date").arg("+%Y-%m-%d %H:%M:%S").output() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    format!("{}", now_unix())
}

fn keyed_chain_hash(prev_hash: &str, payload: &str) -> String {
    let secret = "spavn-audit-chain-v0.1";
    let mut h1: u64 = 0xcbf29ce484222325;
    let mut h2: u64 = 0x84222325cbf29ce4;
    for b in secret
        .bytes()
        .chain(prev_hash.bytes())
        .chain(payload.bytes())
    {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(0x100000001b3);
        h2 = h2.wrapping_add((b as u64).wrapping_mul(0x9e3779b97f4a7c15));
        h2 = (h2 ^ (h2 >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    }
    format!("{:016x}{:016x}", h1, h2)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRoleKey {
    Auto,
    Coder,
    Reviewer,
    Security,
    Test,
    Docs,
    Refactor,
}

impl AgentRoleKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentRoleKey::Auto => "auto",
            AgentRoleKey::Coder => "coder",
            AgentRoleKey::Reviewer => "reviewer",
            AgentRoleKey::Security => "security",
            AgentRoleKey::Test => "test",
            AgentRoleKey::Docs => "docs",
            AgentRoleKey::Refactor => "refactor",
        }
    }

    pub fn badge(&self) -> &'static str {
        match self {
            AgentRoleKey::Auto => "🎯 AUTO (Otomatik Rol)",
            AgentRoleKey::Coder => "💻 CODER (Uygulama)",
            AgentRoleKey::Reviewer => "🔍 REVIEWER (Denetim)",
            AgentRoleKey::Security => "🛡️  SECURITY (Güvenlik)",
            AgentRoleKey::Test => "🧪 TEST (Doğrulama)",
            AgentRoleKey::Docs => "📖 DOCS (Dokümantasyon)",
            AgentRoleKey::Refactor => "🧹 REFACTOR (Mimari Temizlik)",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "auto" | "otomatik" => Some(Self::Auto),
            "coder" | "code" | "kod" => Some(Self::Coder),
            "reviewer" | "review" | "incele" => Some(Self::Reviewer),
            "security" | "sec" | "güvenlik" => Some(Self::Security),
            "test" | "qa" => Some(Self::Test),
            "docs" | "doc" | "doküman" => Some(Self::Docs),
            "refactor" | "clean" => Some(Self::Refactor),
            _ => None,
        }
    }

    pub fn preamble(&self) -> &'static str {
        match self {
            AgentRoleKey::Auto | AgentRoleKey::Coder => {
                "[SPAVN ROLE: CODER] Implement the requested change cleanly with minimal blast radius, adhering to existing code style. Do not break unrelated behavior."
            }
            AgentRoleKey::Reviewer => {
                "[SPAVN ROLE: REVIEWER] Find concrete bugs, edge cases, race conditions, and regressions. Cite exact file:line locations and provide surgical fixes. Zero fluff."
            }
            AgentRoleKey::Security => {
                "[SPAVN ROLE: SECURITY] Focus on injection, auth bypass, secret exposure, unsafe input handling, and privilege escalation. Explain and fix every finding."
            }
            AgentRoleKey::Test => {
                "[SPAVN ROLE: TEST] Write deterministic, fast verification tests covering happy paths, boundary conditions, and error paths."
            }
            AgentRoleKey::Docs => {
                "[SPAVN ROLE: DOCS] Write concise, verifiable technical documentation grounded strictly in actual source files."
            }
            AgentRoleKey::Refactor => {
                "[SPAVN ROLE: REFACTOR] Improve structure, eliminate duplication, and delete dead code WITHOUT changing external behavior. Verify compilation after every step."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPhase {
    Plan,
    Code,
}

impl AgentPhase {
    pub fn badge(&self) -> &'static str {
        match self {
            AgentPhase::Plan => "📐 PLAN (Salt-Okunur Tasarım)",
            AgentPhase::Code => "💻 CODE (Uygulama)",
        }
    }

    pub fn short_badge(&self) -> &'static str {
        match self {
            AgentPhase::Plan => "📐 PLAN",
            AgentPhase::Code => "💻 CODE",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "plan" | "tasarim" | "tasarım" | "design" => Some(Self::Plan),
            "code" | "kod" | "act" | "apply" | "uygula" => Some(Self::Code),
            _ => None,
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            AgentPhase::Plan => AgentPhase::Code,
            AgentPhase::Code => AgentPhase::Plan,
        }
    }

    pub fn preamble(&self) -> Option<&'static str> {
        match self {
            AgentPhase::Plan => Some(
                "[SPAVN PHASE: PLAN] READ-ONLY phase: the write, edit and bash tools are hard-locked; you may use read, search, web, memory and skill only. Your deliverable is a concrete plan: target files, ordered steps, risks, and a verification/test strategy. When the plan is complete, stop and wait — the user unlocks changes with /code or \\. Do not attempt writes.",
            ),
            AgentPhase::Code => None,
        }
    }
}

pub fn dispatch_role_from_prompt(prompt: &str) -> AgentRoleKey {
    let lower = prompt.to_lowercase();
    let rules: &[(AgentRoleKey, &[&str])] = &[
        (
            AgentRoleKey::Reviewer,
            &[
                "review",
                "incele",
                "denetle",
                "audit",
                "gözden geçir",
                "kontrol et",
                "değerlendir",
            ],
        ),
        (
            AgentRoleKey::Security,
            &[
                "güvenlik",
                "security",
                "vulnerab",
                "zafiyet",
                "secret",
                "sızıntı",
                "cve",
                "xss",
                "injection",
                "csrf",
                "yetki",
                "auth",
            ],
        ),
        (
            AgentRoleKey::Test,
            &[
                "test", "spec", "coverage", "kapsam", "vitest", "jest", "pytest", "ctest", "unit",
                "e2e",
            ],
        ),
        (
            AgentRoleKey::Docs,
            &[
                "readme",
                "doküman",
                "dokümantasyon",
                "document",
                "jsdoc",
                "comment",
                "rehber",
                "wiki",
            ],
        ),
        (
            AgentRoleKey::Refactor,
            &[
                "refactor",
                "yeniden düzenle",
                "sadeleştir",
                "temizle",
                "cleanup",
                "rename",
                "basitleştir",
                "dedup",
            ],
        ),
    ];

    let mut best = AgentRoleKey::Coder;
    let mut best_score = 0usize;
    for (role, kws) in rules {
        let score = kws.iter().filter(|kw| lower.contains(**kw)).count();
        if score > best_score {
            best_score = score;
            best = *role;
        }
    }
    best
}

pub fn detect_secrets_in_text(text: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    if text.contains("-----BEGIN ") && text.contains("PRIVATE KEY-----") {
        found.push("private-key");
    }
    for token in text
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | ')' | '(' | '='))
    {
        if token.len() == 20
            && token.starts_with("AKIA")
            && token
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && !found.contains(&"aws-access-key")
        {
            found.push("aws-access-key");
        }
        if token.len() >= 38
            && (token.starts_with("ghp_")
                || token.starts_with("gho_")
                || token.starts_with("ghu_")
                || token.starts_with("ghs_")
                || token.starts_with("ghr_"))
            && !found.contains(&"github-token")
        {
            found.push("github-token");
        }
        if token.len() >= 28 && token.starts_with("sk_live_") && !found.contains(&"stripe-live-key")
        {
            found.push("stripe-live-key");
        }
        if token.len() >= 15
            && (token.starts_with("xoxb-")
                || token.starts_with("xoxa-")
                || token.starts_with("xoxp-")
                || token.starts_with("xoxr-"))
            && !found.contains(&"slack-token")
        {
            found.push("slack-token");
        }
        if token.len() == 39 && token.starts_with("AIza") && !found.contains(&"google-api-key") {
            found.push("google-api-key");
        }
    }
    found
}

pub fn scrub_secrets(text: &str) -> String {
    let mut out = text.to_string();
    while let Some(start) = out.find("-----BEGIN ") {
        if let Some(priv_pos) = out[start..].find("PRIVATE KEY-----") {
            if let Some(end_rel) = out[start + priv_pos..].find("-----END ") {
                if let Some(end_tail) = out[start + priv_pos + end_rel + 9..].find("-----") {
                    let full_end = start + priv_pos + end_rel + 9 + end_tail + 5;
                    out.replace_range(start..full_end, "[REDACTED:private-key]");
                    continue;
                }
            }
        }
        break;
    }
    let prefixes: &[(&str, &str)] = &[
        ("AKIA", "[REDACTED:aws-key]"),
        ("ghp_", "[REDACTED:github-token]"),
        ("gho_", "[REDACTED:github-token]"),
        ("ghs_", "[REDACTED:github-token]"),
        ("sk_live_", "[REDACTED:stripe-key]"),
        ("xoxb-", "[REDACTED:slack-token]"),
        ("xoxp-", "[REDACTED:slack-token]"),
        ("AIza", "[REDACTED:google-key]"),
    ];
    for (pref, replacement) in prefixes {
        let mut search_from = 0usize;
        while let Some(rel) = out[search_from..].find(pref) {
            let idx = search_from + rel;
            let end = out[idx..]
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
                .map(|e| idx + e)
                .unwrap_or(out.len());
            let tok_len = end - idx;
            if tok_len >= 16 {
                out.replace_range(idx..end, replacement);
                search_from = idx + replacement.len();
            } else {
                search_from = end;
            }
        }
    }
    out
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AuditActionRecord {
    pub ts: String,
    pub kind: String,
    pub model: String,
    pub host: String,
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
    pub prompt_preview: String,
    pub prev_hash: String,
    pub hash: String,
}

pub fn record_audit_action(
    kind: &str,
    model: &str,
    host: &str,
    path: &str,
    additions: usize,
    deletions: usize,
    prompt_preview: &str,
) {
    let dir = spavn_dir().join("audit");
    let _ = fs::create_dir_all(&dir);
    let file_path = dir.join(format!("{}.jsonl", today_ymd()));

    let mut prev_hash = String::new();
    if let Ok(f) = fs::File::open(&file_path) {
        let reader = BufReader::new(f);
        for line in reader.lines().map_while(Result::ok) {
            if let Ok(rec) = serde_json::from_str::<AuditActionRecord>(&line) {
                prev_hash = rec.hash;
            }
        }
    }

    let ts = now_iso_local();
    let clean_prompt: String = scrub_secrets(prompt_preview)
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(90)
        .collect();

    let payload = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        ts, kind, model, host, path, additions, deletions, clean_prompt
    );
    let hash = keyed_chain_hash(&prev_hash, &payload);

    let rec = AuditActionRecord {
        ts,
        kind: kind.to_string(),
        model: model.to_string(),
        host: host.to_string(),
        path: path.to_string(),
        additions,
        deletions,
        prompt_preview: clean_prompt,
        prev_hash,
        hash,
    };

    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
    {
        if let Ok(json_line) = serde_json::to_string(&rec) {
            let _ = writeln!(file, "{}", json_line);
        }
    }
}

pub fn load_audit_records(
    limit: usize,
    file_filter: Option<&str>,
) -> (Vec<AuditActionRecord>, bool) {
    let dir = spavn_dir().join("audit");
    let Ok(rd) = fs::read_dir(&dir) else {
        return (Vec::new(), true);
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    files.sort();

    let mut all_records = Vec::new();
    let mut chain_valid = true;

    for fpath in files {
        let Ok(file) = fs::File::open(&fpath) else {
            continue;
        };
        let reader = BufReader::new(file);
        let mut prev_hash = String::new();
        for line in reader.lines().map_while(Result::ok) {
            if let Ok(rec) = serde_json::from_str::<AuditActionRecord>(&line) {
                let payload = format!(
                    "{}|{}|{}|{}|{}|{}|{}|{}",
                    rec.ts,
                    rec.kind,
                    rec.model,
                    rec.host,
                    rec.path,
                    rec.additions,
                    rec.deletions,
                    rec.prompt_preview
                );
                let expected = keyed_chain_hash(&prev_hash, &payload);
                if rec.prev_hash != prev_hash || rec.hash != expected {
                    chain_valid = false;
                }
                prev_hash = rec.hash.clone();
                if let Some(flt) = file_filter {
                    if !rec.path.contains(flt) {
                        continue;
                    }
                }
                all_records.push(rec);
            }
        }
    }

    let len = all_records.len();
    let tail = if len > limit {
        all_records.into_iter().skip(len - limit).collect()
    } else {
        all_records
    };
    (tail, chain_valid)
}

pub fn render_audit_panel(file_filter: Option<&str>) {
    let (records, valid) = load_audit_records(15, file_filter);
    let chain_badge = if valid {
        format!("{}🟢 ZİNCİR DOĞRULANDI (HMAC-FNV128){}", GREEN, RESET)
    } else {
        format!("{}🔴 ZİNCİR KIRILMIŞ{}", RED, RESET)
    };
    let title = match file_filter {
        Some(f) => format!("🔍 SPAVN BLAME / DOSYA İZİ ({})", f),
        None => "⛓️  SPAVN AI ACTION AUDIT TRAIL".to_string(),
    };

    println!(
        "\n{}╭─ {} · {} ─────────────────────{}",
        CYAN, title, chain_badge, RESET
    );
    if records.is_empty() {
        println!(
            "{}│{} {}Henüz kayıtlı aksiyon izi bulunamadı.{}",
            CYAN, RESET, DIM, RESET
        );
    } else {
        for r in records {
            let short_hash = &r.hash[..8.min(r.hash.len())];
            println!(
                "{}│{} {}[{}]{} {} · 🧠 {}{}{} · 📄 {}{}{} ({}+{}{}/{}-{}{})",
                CYAN,
                RESET,
                DIM,
                short_hash,
                RESET,
                r.ts,
                GREEN,
                r.model,
                RESET,
                BOLD,
                r.path,
                RESET,
                GREEN,
                r.additions,
                RESET,
                RED,
                r.deletions,
                RESET
            );
            if !r.prompt_preview.is_empty() {
                println!(
                    "{}│{}    └─ 💬 Prompt: {}\"{}\"{}",
                    CYAN, RESET, DIM, r.prompt_preview, RESET
                );
            }
        }
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ContextPin {
    pub id: usize,
    pub project: String,
    pub content: String,
    pub created_at: String,
}

fn pins_file(cwd: &str) -> PathBuf {
    let dir = spavn_dir().join("pins");
    let _ = fs::create_dir_all(&dir);
    dir.join(format!("{}.json", project_slug(cwd)))
}

pub fn load_pins(cwd: &str) -> Vec<ContextPin> {
    let Ok(raw) = fs::read_to_string(pins_file(cwd)) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<ContextPin>>(&raw).unwrap_or_default()
}

pub fn add_pin(cwd: &str, text: &str) -> ContextPin {
    let mut pins = load_pins(cwd);
    let next_id = pins.iter().map(|p| p.id).max().unwrap_or(0) + 1;
    let pin = ContextPin {
        id: next_id,
        project: project_slug(cwd),
        content: text.trim().to_string(),
        created_at: now_iso_local(),
    };
    pins.push(pin.clone());
    if let Ok(s) = serde_json::to_string_pretty(&pins) {
        let _ = fs::write(pins_file(cwd), s);
    }
    pin
}

pub fn remove_pin(cwd: &str, id: usize) -> bool {
    let mut pins = load_pins(cwd);
    let before = pins.len();
    pins.retain(|p| p.id != id);
    if pins.len() != before {
        if let Ok(s) = serde_json::to_string_pretty(&pins) {
            let _ = fs::write(pins_file(cwd), s);
        }
        return true;
    }
    false
}

pub fn render_pins_panel(cwd: &str) {
    let pins = load_pins(cwd);
    println!(
        "\n{}╭─ 📌 SPAVN SABİTLENMİŞ PROJE BAĞLAMI (PINS — {}) ─────────────────────{}",
        CYAN,
        project_slug(cwd),
        RESET
    );
    if pins.is_empty() {
        println!(
            "{}│{} {}Sabitlenmiş kural/not yok. Eklemek için: /pin <kural veya mimari karar>{}",
            CYAN, RESET, DIM, RESET
        );
    } else {
        for p in pins {
            println!(
                "{}│{} {}#{:<2}{} {} {}({}){}",
                CYAN, RESET, YELLOW, p.id, RESET, p.content, DIM, p.created_at, RESET
            );
        }
        println!(
            "{}│{} {}Silmek için: /unpin <id>{}",
            CYAN, RESET, DIM, RESET
        );
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct KnowledgeArtifact {
    pub ts: String,
    pub project: String,
    pub files: Vec<String>,
    pub summary: String,
}

pub fn record_knowledge_artifact(cwd: &str, files: &[String], summary: &str) {
    if files.is_empty() || summary.trim().is_empty() {
        return;
    }
    let dir = spavn_dir().join("artifacts");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{}.jsonl", project_slug(cwd)));
    let item = KnowledgeArtifact {
        ts: now_iso_local(),
        project: project_slug(cwd),
        files: files.to_vec(),
        summary: summary.trim().chars().take(220).collect(),
    };
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        if let Ok(line) = serde_json::to_string(&item) {
            let _ = writeln!(f, "{}", line);
        }
    }
}

pub fn retrieve_relevant_artifacts(cwd: &str, query: &str, k: usize) -> Vec<KnowledgeArtifact> {
    let path = spavn_dir()
        .join("artifacts")
        .join(format!("{}.jsonl", project_slug(cwd)));
    let Ok(f) = fs::File::open(path) else {
        return Vec::new();
    };
    let q_terms: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 3)
        .map(|s| s.to_string())
        .collect();
    let reader = BufReader::new(f);
    let mut items: Vec<KnowledgeArtifact> = reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<KnowledgeArtifact>(&l).ok())
        .collect();

    if q_terms.is_empty() {
        items.reverse();
        items.truncate(k);
        return items;
    }

    let mut scored: Vec<(usize, KnowledgeArtifact)> = items
        .into_iter()
        .map(|art| {
            let hay = format!("{} {}", art.files.join(" "), art.summary).to_lowercase();
            let hits = q_terms.iter().filter(|t| hay.contains(t.as_str())).count();
            (hits, art)
        })
        .filter(|(hits, _)| *hits > 0)
        .collect();
    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    scored.into_iter().take(k).map(|(_, a)| a).collect()
}

pub struct GateOutcome {
    pub name: String,
    pub command: String,
    pub passed: bool,
    pub duration_ms: u128,
    pub output_preview: String,
}

pub fn detect_project_gates(cwd: &str) -> Vec<(String, String)> {
    let root = Path::new(cwd);
    let mut gates = Vec::new();
    if root.join("Cargo.toml").exists() {
        gates.push(("rust-check".to_string(), "cargo check --quiet".to_string()));
    }
    if root.join("tsconfig.json").exists() {
        gates.push(("ts-types".to_string(), "npx tsc --noEmit".to_string()));
    }
    if root.join("package.json").exists() {
        if let Ok(pkg) = fs::read_to_string(root.join("package.json")) {
            if pkg.contains("\"lint\"") {
                gates.push(("npm-lint".to_string(), "npm run lint --silent".to_string()));
            }
            if pkg.contains("\"test\"") {
                gates.push(("npm-test".to_string(), "npm test --silent".to_string()));
            }
        }
    }
    if root.join("pyproject.toml").exists() || root.join("pytest.ini").exists() {
        gates.push(("pytest".to_string(), "pytest -q".to_string()));
    }
    gates
}

pub fn run_janitor_gates(cwd: &str) -> Vec<GateOutcome> {
    let gates = detect_project_gates(cwd);
    let mut outcomes = Vec::new();
    for (name, cmd) in gates {
        let start = std::time::Instant::now();
        let out = Command::new(crate::config::shell_bin())
            .arg("-c")
            .arg(&cmd)
            .current_dir(cwd)
            .output();
        let ms = start.elapsed().as_millis();
        match out {
            Ok(o) => {
                let passed = o.status.success();
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                let preview: String = combined
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" | ")
                    .chars()
                    .take(120)
                    .collect();
                outcomes.push(GateOutcome {
                    name,
                    command: cmd,
                    passed,
                    duration_ms: ms,
                    output_preview: preview,
                });
                if !passed {
                    break;
                }
            }
            Err(e) => {
                outcomes.push(GateOutcome {
                    name,
                    command: cmd,
                    passed: false,
                    duration_ms: ms,
                    output_preview: e.to_string(),
                });
                break;
            }
        }
    }
    outcomes
}

pub fn render_janitor_panel(cwd: &str) {
    let outcomes = run_janitor_gates(cwd);
    println!(
        "\n{}╭─ 🧹 SPAVN JANITOR KALİTE KAPILARI ({}) ──────────────────────────────{}",
        CYAN,
        project_slug(cwd),
        RESET
    );
    if outcomes.is_empty() {
        println!(
            "{}│{} {}Bu dizinde otomatik tespit edilen kalite kapısı (Cargo.toml / tsconfig / package.json) yok.{}",
            CYAN, RESET, DIM, RESET
        );
    } else {
        for g in outcomes {
            let badge = if g.passed {
                format!("{}🟢 PASS ({:>4}ms){}", GREEN, g.duration_ms, RESET)
            } else {
                format!("{}🔴 FAIL ({:>4}ms){}", RED, g.duration_ms, RESET)
            };
            println!(
                "{}│{} {:<14} │ {} │ {}{}{}",
                CYAN, RESET, g.name, badge, DIM, g.command, RESET
            );
            if !g.output_preview.is_empty() {
                println!(
                    "{}│{}    └─ {}{}{}",
                    CYAN, RESET, YELLOW, g.output_preview, RESET
                );
            }
        }
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}

pub fn render_repo_wiki(cwd: &str) {
    let root = Path::new(cwd);
    let ignored = [
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        "out",
        "out-build",
        ".next",
        "vendor",
    ];
    let mut ext_counts: HashMap<String, usize> = HashMap::new();
    let mut modules: HashMap<String, Vec<String>> = HashMap::new();
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;

    #[allow(clippy::too_many_arguments)]
    fn walk_wiki(
        dir: &Path,
        root: &Path,
        ignored: &[&str],
        depth: usize,
        ext_counts: &mut HashMap<String, usize>,
        modules: &mut HashMap<String, Vec<String>>,
        total_files: &mut usize,
        total_bytes: &mut u64,
    ) {
        if depth > 3 || *total_files >= 2000 {
            return;
        }
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || ignored.contains(&name.as_str()) {
                continue;
            }
            let p = entry.path();
            if p.is_dir() {
                walk_wiki(
                    &p,
                    root,
                    ignored,
                    depth + 1,
                    ext_counts,
                    modules,
                    total_files,
                    total_bytes,
                );
            } else if p.is_file() {
                let rel = p
                    .strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .to_string();
                let ext = p
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("other")
                    .to_lowercase();
                *ext_counts.entry(ext).or_insert(0) += 1;
                *total_files += 1;
                if let Ok(m) = entry.metadata() {
                    *total_bytes += m.len();
                }
                let top_mod = rel.split('/').next().unwrap_or("root").to_string();
                let entry_list = modules.entry(top_mod).or_default();
                if entry_list.len() < 6 {
                    entry_list.push(rel);
                }
            }
        }
    }

    walk_wiki(
        root,
        root,
        &ignored,
        0,
        &mut ext_counts,
        &mut modules,
        &mut total_files,
        &mut total_bytes,
    );

    let mut top_exts: Vec<(String, usize)> = ext_counts.into_iter().collect();
    top_exts.sort_by_key(|a| std::cmp::Reverse(a.1));
    let langs: String = top_exts
        .into_iter()
        .take(6)
        .map(|(e, c)| format!("{} ({})", e, c))
        .collect::<Vec<_>>()
        .join(", ");

    println!(
        "\n{}╭─ 📖 SPAVN REPO WIKI — {} ({} dosya, {} KB) ───────────────────{}",
        CYAN,
        project_slug(cwd),
        total_files,
        total_bytes / 1024,
        RESET
    );
    println!(
        "{}│{} {}Dil / Dosya Dağılımı:{} {}",
        CYAN, RESET, BOLD, RESET, langs
    );
    let mut mod_keys: Vec<_> = modules.keys().cloned().collect();
    mod_keys.sort();
    for k in mod_keys.into_iter().take(10) {
        if let Some(files) = modules.get(&k) {
            println!(
                "{}│{}  ├─ {}📁 {:<16}{} → {}{}{}",
                CYAN,
                RESET,
                GREEN,
                k,
                RESET,
                DIM,
                files.join(", "),
                RESET
            );
        }
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SpavnGoal {
    pub id: usize,
    pub project: String,
    pub objective: String,
    pub state: String,
    pub created_at: String,
}

fn goals_file() -> PathBuf {
    spavn_dir().join("goals.json")
}

pub fn load_goals(cwd: &str) -> Vec<SpavnGoal> {
    let Ok(raw) = fs::read_to_string(goals_file()) else {
        return Vec::new();
    };
    let all: Vec<SpavnGoal> = serde_json::from_str(&raw).unwrap_or_default();
    let slug = project_slug(cwd);
    all.into_iter().filter(|g| g.project == slug).collect()
}

pub fn add_goal(cwd: &str, objective: &str) -> SpavnGoal {
    let mut all: Vec<SpavnGoal> = fs::read_to_string(goals_file())
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default();
    let next_id = all.iter().map(|g| g.id).max().unwrap_or(0) + 1;
    let g = SpavnGoal {
        id: next_id,
        project: project_slug(cwd),
        objective: objective.trim().to_string(),
        state: "active".to_string(),
        created_at: now_iso_local(),
    };
    all.push(g.clone());
    if let Ok(s) = serde_json::to_string_pretty(&all) {
        let _ = fs::write(goals_file(), s);
    }
    g
}

pub fn complete_goal(id: usize) -> bool {
    let mut all: Vec<SpavnGoal> = fs::read_to_string(goals_file())
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default();
    let mut found = false;
    for g in &mut all {
        if g.id == id {
            g.state = "done".to_string();
            found = true;
        }
    }
    if found {
        if let Ok(s) = serde_json::to_string_pretty(&all) {
            let _ = fs::write(goals_file(), s);
        }
    }
    found
}

pub fn render_goals_panel(cwd: &str) {
    let goals = load_goals(cwd);
    println!(
        "\n{}╭─ 🎯 SPAVN PROJE HEDEFLERİ (GOALS — {}) ─────────────────────────────{}",
        CYAN,
        project_slug(cwd),
        RESET
    );
    if goals.is_empty() {
        println!(
            "{}│{} {}Aktif hedef yok. Eklemek için: /goal <hedef açıklaması> · Tamamlamak için: /goal done <id>{}",
            CYAN, RESET, DIM, RESET
        );
    } else {
        for g in goals {
            let badge = if g.state == "done" {
                format!("{}✅ DONE{}", GREEN, RESET)
            } else {
                format!("{}🎯 ACTIVE{}", YELLOW, RESET)
            };
            println!(
                "{}│{} #{:<2} │ {} │ {} {}({}){}",
                CYAN, RESET, g.id, badge, g.objective, DIM, g.created_at, RESET
            );
        }
    }
    println!(
        "{}╰────────────────────────────────────────────────────────────────────────────{}",
        CYAN, RESET
    );
}
