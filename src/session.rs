use crate::config::spavn_dir;
use crate::router::ChatMessage;
use std::fs;
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;

fn secure_file(path: &Path) {
    #[cfg(unix)]
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE));
    #[cfg(not(unix))]
    let _ = path;
}

fn harden_sessions_dir(dir: &Path) {
    #[cfg(unix)]
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(PRIVATE_DIR_MODE));
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                secure_file(&path);
            }
        }
    }
}

static SESSIONS_HARDENED: std::sync::Once = std::sync::Once::new();

pub fn sessions_dir() -> PathBuf {
    let dir = spavn_dir().join("sessions");
    let _ = fs::create_dir_all(&dir);
    SESSIONS_HARDENED.call_once(|| harden_sessions_dir(&dir));
    dir
}

pub fn new_session_path() -> PathBuf {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ts = dur.as_secs();
    let dir = sessions_dir();
    let mut path = dir.join(format!("session-{}.jsonl", ts));
    if path.exists() && fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
        path = dir.join(format!("session-{}-{}.jsonl", ts, dur.subsec_millis()));
    }
    if let Ok(file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        drop(file);
    }
    secure_file(&path);
    path
}

pub fn latest_session_path() -> Option<PathBuf> {
    let dir = sessions_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    entries.sort();
    entries.pop()
}

pub fn load_session(path: &PathBuf) -> Vec<ChatMessage> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        if let Ok(msg) = serde_json::from_str::<ChatMessage>(&line) {
            if !messages.is_empty() && msg.role == "system" {
                continue;
            }
            messages.push(msg);
        }
    }
    messages
}

pub fn append_message(path: &PathBuf, msg: &ChatMessage) {
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        secure_file(path);
        if let Ok(line) = serde_json::to_string(msg) {
            let _ = writeln!(file, "{}", line);
        }
    }
}

pub fn rewrite_session(path: &PathBuf, messages: &[ChatMessage]) {
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        secure_file(path);
        for msg in messages {
            if let Ok(line) = serde_json::to_string(msg) {
                let _ = writeln!(file, "{}", line);
            }
        }
    }
}

pub fn fork_session(messages: &[ChatMessage]) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let forked_path = sessions_dir().join(format!("session-{}-fork.jsonl", ts));
    rewrite_session(&forked_path, messages);
    forked_path
}

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub path: PathBuf,
    pub filename: String,
    pub age_label: String,
    pub message_count: usize,
    pub preview: String,
}

fn clean_user_preview(raw: &str) -> String {
    let mut s = raw;
    while let Some(start) = s.find("<active_skill") {
        if let Some(end) = s[start..].find("</active_skill>") {
            s = &s[start + end + "</active_skill>".len()..];
        } else {
            break;
        }
    }
    let collapsed: String = s
        .lines()
        .map(|l| l.trim())
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("[SPAVN")
                && !l.starts_with("<spavn_knowledge")
                && !l.starts_with("</spavn_knowledge")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut chars = trimmed.chars();
    let head: String = chars.by_ref().take(42).collect();
    if chars.next().is_some() {
        format!("{}…", head)
    } else {
        head
    }
}

fn format_relative_age(now_secs: u64, mtime_secs: u64) -> String {
    let diff = now_secs.saturating_sub(mtime_secs);
    if diff < 60 {
        format!("{}sn önce", diff.max(1))
    } else if diff < 3600 {
        format!("{}dk önce", diff / 60)
    } else if diff < 86400 {
        format!("{}sa {}dk önce", diff / 3600, (diff % 3600) / 60)
    } else {
        format!("{} gün önce", diff / 86400)
    }
}

pub fn list_recent_sessions(limit: usize) -> Vec<SessionSummary> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    if let Ok(rd) = fs::read_dir(sessions_dir()) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                files.push((p, mtime));
            }
        }
    }

    files.sort_by_key(|a| std::cmp::Reverse(a.1));

    let mut summaries = Vec::new();
    for (path, mtime_secs) in files.into_iter().take(limit) {
        let msgs = load_session(&path);
        let mut msg_count = 0usize;
        let mut first_user = String::new();
        let mut last_user = String::new();
        for m in &msgs {
            if m.role == "user" || m.role == "assistant" {
                msg_count += 1;
            }
            if m.role == "user" {
                if let Some(ref c) = m.content {
                    let cleaned = clean_user_preview(c);
                    if !cleaned.is_empty() {
                        if first_user.is_empty() {
                            first_user = cleaned.clone();
                        }
                        last_user = cleaned;
                    }
                }
            }
        }
        let preview = if !last_user.is_empty() {
            last_user
        } else if !first_user.is_empty() {
            first_user
        } else {
            "(Boş oturum)".to_string()
        };
        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let age_label = format_relative_age(now_secs, mtime_secs);
        summaries.push(SessionSummary {
            path,
            filename,
            age_label,
            message_count: msg_count,
            preview,
        });
    }
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn session_files_and_dir_are_private() {
        let dir = std::env::temp_dir().join(format!("spavn-session-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("SPAVN_HOME", &dir);

        let path = new_session_path();
        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(sessions_dir()).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "session file must be 0600");
        assert_eq!(dir_mode, 0o700, "sessions dir must be 0700");

        let _ = fs::remove_file(&path);
        std::env::remove_var("SPAVN_HOME");
        let _ = fs::remove_dir_all(&dir);
    }
}
