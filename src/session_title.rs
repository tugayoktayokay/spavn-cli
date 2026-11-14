//! Auto session title.
//!
//! The first real user prompt of a session spawns a detached, non-blocking LLM call with the
//! original request wrapped in `<message>` tags — the wrapper keeps the model from *executing*
//! the text instead of titling it. The generated line is cleaned, truncated to 80 chars
//! (`hud::derive_topic` clamps the terminal title further to 48) and becomes the sticky window
//! topic: before the title lands the newest user line is the topic, after it only the title is,
//! so a follow-up prompt never rewrites it.
//!
//! Errors are swallowed: a failed title request leaves the current topic untouched.

use crate::hud;
use crate::router::{ChatMessage, ModelEndpoint};
use crate::session::safe_utf8_prefix;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// One attempt per process: the first prompt titles the session, everything after is sticky.
static TITLING: AtomicBool = AtomicBool::new(false);
static SESSION_TITLE: Mutex<Option<String>> = Mutex::new(None);

/// Longest title accepted from the model ([`hud`] clamps harder).
const MAX_TITLE_CHARS: usize = 80;
/// Requests at least 4 tokens short of this budget do not need truncating for the title call.
const REQUEST_BUDGET_BYTES: usize = 2000;

/// The sticky session title, once the background call has delivered it.
pub fn session_title() -> Option<String> {
    SESSION_TITLE.lock().ok()?.clone()
}

/// Clears the sticky title when switching to a different session (`/new`, `/resume`).
pub fn reset_session_title() {
    TITLING.store(false, Ordering::Relaxed);
    if let Ok(mut slot) = SESSION_TITLE.lock() {
        *slot = None;
    }
}

/// True when a session should be auto-titled from this prompt candidate.
fn wants_title_request(request: &str) -> bool {
    let trimmed = request.trim();
    // Internal turn banners (`[SPAVN Auto-Compact …]`), slash commands and skill preambles
    // never describe intent, so they never title the session.
    !(trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.lines().next().unwrap_or("").starts_with('['))
}

/// Clean the model's first line into a title, or `None` when nothing usable survived.
fn clean_model_title(reply: &str) -> Option<String> {
    let mut collapsed = String::new();
    let mut fenced = false;
    for line in reply.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || trimmed.is_empty() || trimmed.starts_with('<') || trimmed.starts_with('#') {
            continue;
        }
        collapsed = trimmed
            .trim_start_matches(['#', '-', '*', '>', '•', '`', '"', '\''])
            .trim_end_matches(['"', '`', '\'', '*', '_'])
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !collapsed.is_empty() {
            break;
        }
    }
    let mut title: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
    if title.is_empty() {
        return None;
    }
    match title.chars().last() {
        Some(c) if !c.is_ascii_punctuation() => {}
        _ => {
            // Trim trailing punctuation (".", "…" etc.).
            while title.ends_with(|c: char| c.is_ascii_punctuation() || c == '…') {
                title.pop();
            }
        }
    }
    if title.is_empty() {
        return None;
    }
    Some(title)
}

/// Spawn the one-shot background titling call for the first real prompt of the session.
///
/// Cheap guards are applied here so internal banners (`[SPAVN …]`), slash commands, empty and
/// skill-preamble prompts never open a second request. Runs fully detached; the result is
/// published through [`session_title`] and pushed into the window topic immediately.
pub fn spawn_auto_title(endpoint: &ModelEndpoint, request: &str) {
    if !wants_title_request(request)
        || TITLING
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    let endpoint = endpoint.clone();
    let request = safe_utf8_prefix(request, REQUEST_BUDGET_BYTES).to_string();
    std::thread::spawn(move || {
        let prompt = ChatMessage::user(format!(
            "<message>\n{}\n</message>\n\nReply with only the title.",
            request.trim()
        ));
        let mut title = String::new();
        let tools = serde_json::json!([]);
        let result = crate::router::stream_chat_completion(
            &endpoint,
            &[
                ChatMessage::system(
                    "You generate a short, natural title (max 6 words) for an engineering CLI \
                     session based on the user's message. Reply with only the title: no quotes, \
                     no markdown, no trailing punctuation.",
                ),
                prompt,
            ],
            &tools,
            |_| {},
            |delta| title.push_str(delta),
        );
        let title = match result {
            Ok(_) => title,
            // Model never answered; the current topic keeps serving.
            Err(_) => return,
        };
        let Some(clean) = clean_model_title(&title) else {
            return;
        };
        if let Ok(mut slot) = SESSION_TITLE.lock() {
            *slot = Some(clean.clone());
        }
        hud::set_window_topic(&clean);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_title_collapses_whitespace_and_takes_first_line() {
        assert_eq!(
            clean_model_title("\n\n  fix   the parser \nsecond line"),
            Some("fix the parser".to_string())
        );
    }

    #[test]
    fn clean_title_strips_markdown_and_trailing_punctuation() {
        assert_eq!(
            clean_model_title("**`RTL bootstrap on macOS`**. "),
            Some("RTL bootstrap on macOS".to_string())
        );
    }

    #[test]
    fn clean_title_skips_code_fences_and_wrappers() {
        assert_eq!(clean_model_title("```rust\nlet x = 1;\n```"), None);
        assert_eq!(clean_model_title("<message>echo</message>"), None);
    }

    #[test]
    fn clean_title_truncates_at_char_boundary() {
        let long = "ağç".repeat(60);
        let title = clean_model_title(&long).unwrap();
        assert!(title.chars().count() <= MAX_TITLE_CHARS);
        assert!(title.is_char_boundary(title.len()));
    }

    #[test]
    fn clean_title_empty_reply_is_none() {
        assert_eq!(clean_model_title("   \n"), None);
    }

    #[test]
    fn reset_clears_sticky_title_and_rearms_guard() {
        *SESSION_TITLE.lock().unwrap() = Some("old session".to_string());
        TITLING.store(true, Ordering::Relaxed);
        reset_session_title();
        assert_eq!(session_title(), None);
        assert!(!TITLING.load(Ordering::Relaxed));
    }
}
