//! The terminal window title (`OSC 0`).
//!
//! A title is three parts: a static brand glyph, a state glyph
//! that doubles as the progress indicator while a request runs, and the topic of the session
//! (`⚡ ⠂ düzelt: parser testi`). The topic is the newest user line, so a screenshot drop lands
//! in the title as its chip (`🖼️ Screenshot.png (11 KB)`) without any special case.
//!
//! Writes are deduped (the animation frame differs every tick, so an unchanged title is a no-op)
//! and skipped entirely when stdout is not a TTY: a piped or scripted run must stay byte-identical.
//!
//! The first write pushes the previous title onto the terminal's title stack (`CSI 22;2t`) and
//! [`restore_window_title`] pops it (`CSI 23;2t`), so quitting leaves the shell's title intact.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use super::toolcard::sanitize;

/// Brand glyph: the same bolt the assistant header draws.
const BRAND: &str = "⚡";
/// Idle: the agent is waiting for input.
const IDLE_GLYPH: char = '>';
/// Attention: an access gate is waiting for an answer, so the title has to survive a lost window.
const ATTENTION_GLYPH: char = '!';
/// Working: a braille frame per tick, the same family the spinner uses.
const FRAMES: [char; 8] = ['⠂', '⠢', '⠖', '⠶', '⡶', '⣷', '⣾', '⣂'];
const FRAME_MS: u64 = 80;
/// Longest topic we let into a title; terminals truncate the tail themselves, and a full first
/// line of a pasted stack trace would push the state glyph out of view.
const MAX_TOPIC_CHARS: usize = 48;

const STATE_IDLE: u8 = 0;
const STATE_WORKING: u8 = 1;
const STATE_ATTENTION: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(STATE_IDLE);
static FRAME: AtomicUsize = AtomicUsize::new(0);
static TICKING: AtomicBool = AtomicBool::new(false);
static PUSHED: AtomicBool = AtomicBool::new(false);
static TOPIC: Mutex<Option<String>> = Mutex::new(None);
static LAST_TITLE: Mutex<Option<String>> = Mutex::new(None);

/// What the agent is doing, as far as the title is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleState {
    Idle,
    Working,
    Attention,
}

/// Sets the topic shown next to the state glyph. Empty input keeps the current topic so a
/// follow-up prompt does not blank the title.
pub fn set_window_topic(topic: &str) {
    let cleaned = derive_topic(topic);
    if cleaned.is_empty() {
        return;
    }
    if let Ok(mut slot) = TOPIC.lock() {
        *slot = Some(cleaned);
    }
    refresh();
}

pub fn set_title_state(state: TitleState) {
    STATE.store(
        match state {
            TitleState::Idle => STATE_IDLE,
            TitleState::Working => STATE_WORKING,
            TitleState::Attention => STATE_ATTENTION,
        },
        Ordering::Relaxed,
    );
    if state == TitleState::Working {
        ensure_ticker();
    }
    refresh();
}

/// Keeps the title in [`TitleState::Working`] for as long as it lives and drops it back to
/// `Idle`, so every early return inside a turn restores the static glyph without repeating
/// `set_title_state` on each path.
pub struct WorkingGuard(());

impl WorkingGuard {
    pub fn new() -> Self {
        set_title_state(TitleState::Working);
        Self(())
    }
}

impl Default for WorkingGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WorkingGuard {
    fn drop(&mut self) {
        set_title_state(TitleState::Idle);
    }
}

/// Pops the pushed title, so a quit hands the terminal back with the shell's own title.
pub fn restore_window_title() {
    STATE.store(STATE_IDLE, Ordering::Relaxed);
    if let Ok(mut slot) = LAST_TITLE.lock() {
        slot.take();
    }
    if !PUSHED.swap(false, Ordering::Relaxed) {
        return;
    }
    let mut out = io::stdout();
    let _ = write!(out, "\x1b]0;\x07\x1b[23;2t");
    let _ = out.flush();
}

fn refresh() {
    write_title(&compose_title());
}

fn compose_title() -> String {
    let glyph = match STATE.load(Ordering::Relaxed) {
        STATE_WORKING => FRAMES[FRAME.load(Ordering::Relaxed) % FRAMES.len()],
        STATE_ATTENTION => ATTENTION_GLYPH,
        _ => IDLE_GLYPH,
    };
    let topic = TOPIC.lock().ok().and_then(|slot| slot.clone());
    render_title(glyph, topic.as_deref())
}

/// `⚡ ⠂ topic` — no topic yet is just the brand and the state glyph.
fn render_title(glyph: char, topic: Option<&str>) -> String {
    match topic {
        Some(topic) if !topic.is_empty() => format!("{} {} {}", BRAND, glyph, topic),
        _ => format!("{} {}", BRAND, glyph),
    }
}

fn write_title(title: &str) {
    if !io::stdout().is_terminal() {
        return;
    }
    if let Ok(mut last) = LAST_TITLE.lock() {
        if last.as_deref() == Some(title) {
            return;
        }
        *last = Some(title.to_string());
    }
    let mut out = io::stdout();
    if !PUSHED.swap(true, Ordering::Relaxed) {
        // Save the shell's title once; `restore_window_title` pops it back.
        let _ = write!(out, "\x1b[22;2t");
    }
    let _ = write!(out, "\x1b]0;{}\x07", title);
    let _ = out.flush();
}

/// Advances the animation while a request runs. One ticker at a time; it exits on its own as soon
/// as the state leaves `Working`, so a finished turn never leaves a repainting thread behind.
fn ensure_ticker() {
    if TICKING.swap(true, Ordering::Relaxed) {
        return;
    }
    thread::spawn(|| {
        while STATE.load(Ordering::Relaxed) == STATE_WORKING {
            thread::sleep(Duration::from_millis(FRAME_MS));
            if STATE.load(Ordering::Relaxed) != STATE_WORKING {
                break;
            }
            FRAME.fetch_add(1, Ordering::Relaxed);
            refresh();
        }
        TICKING.store(false, Ordering::Relaxed);
    });
}

/// One display line, banner-free, at most [`MAX_TOPIC_CHARS`] characters.
fn derive_topic(raw: &str) -> String {
    let cleaned = sanitize(raw);
    let line = cleaned.lines().map(str::trim).find(|line| {
        !line.is_empty()
            && !line.starts_with("[SPAVN")
            && !line.starts_with('<')
            && !line.starts_with("```")
    });
    let Some(line) = line else {
        return String::new();
    };
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TOPIC_CHARS {
        return collapsed;
    }
    let mut short: String = collapsed.chars().take(MAX_TOPIC_CHARS - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_topic_takes_first_real_line() {
        assert_eq!(
            derive_topic("\n\n  fix   the parser \nsecond line"),
            "fix the parser"
        );
    }

    #[test]
    fn derive_topic_skips_internal_banners() {
        assert_eq!(
            derive_topic("[SPAVN Context Compactor: 1763 bytes pruned]\n<active_skill>x\nreal ask"),
            "real ask"
        );
    }

    #[test]
    fn derive_topic_strips_escapes_and_truncates() {
        assert_eq!(derive_topic("\x1b[31mred\x1b[0m title"), "red title");
        let long = "a".repeat(200);
        let topic = derive_topic(&long);
        assert_eq!(topic.chars().count(), MAX_TOPIC_CHARS);
        assert!(topic.ends_with('…'));
    }

    #[test]
    fn derive_topic_of_empty_prompt_is_empty() {
        assert_eq!(derive_topic("   \n\t\n"), "");
    }

    #[test]
    fn render_title_shows_glyph_state_and_topic() {
        assert_eq!(
            render_title(IDLE_GLYPH, Some("düzelt: parser")),
            "⚡ > düzelt: parser"
        );
        assert_eq!(
            render_title(ATTENTION_GLYPH, Some("düzelt: parser")),
            "⚡ ! düzelt: parser"
        );
        assert_eq!(render_title(FRAMES[0], None), "⚡ ⠂");
    }
}
