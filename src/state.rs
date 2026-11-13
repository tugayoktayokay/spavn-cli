//! Process-wide runtime state (state): steer/prompt queues, live task list, and signal flags.
//!
//! These are the only mutable globals in the crate. Everything that needs to reach them
//! (tool executor, spinner, slash dispatcher) does so through the free functions here,
//! which keeps the locking discipline in one place.

use crate::ansi::{BOLD, CYAN, DIM, GREEN, RESET, YELLOW};
use crate::hud;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static SOFT_STOP: AtomicBool = AtomicBool::new(false);
static RESIZED: AtomicBool = AtomicBool::new(false);
static STEER_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static PROMPT_QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

pub fn push_steer_message(msg: String) {
    if let Ok(mut q) = STEER_QUEUE.lock() {
        q.push_back(msg);
    }
}

pub fn pop_steer_messages() -> Vec<String> {
    if let Ok(mut q) = STEER_QUEUE.lock() {
        q.drain(..).collect()
    } else {
        Vec::new()
    }
}

pub fn steer_message_count() -> usize {
    STEER_QUEUE.lock().map(|q| q.len()).unwrap_or(0)
}

pub fn pop_last_steer_message() -> Option<String> {
    STEER_QUEUE.lock().ok()?.pop_back()
}

pub fn clear_steer_messages() {
    if let Ok(mut q) = STEER_QUEUE.lock() {
        q.clear();
    }
}

pub fn push_queued_message(msg: String) {
    if let Ok(mut q) = PROMPT_QUEUE.lock() {
        q.push_back(msg);
    }
}

pub fn pop_next_queued_message() -> Option<String> {
    PROMPT_QUEUE.lock().ok()?.pop_front()
}

pub fn pop_last_queued_message() -> Option<String> {
    PROMPT_QUEUE.lock().ok()?.pop_back()
}

pub fn peek_last_pending_message() -> Option<(bool, String)> {
    if let Ok(sq) = STEER_QUEUE.lock() {
        if let Some(last) = sq.back() {
            return Some((true, last.clone()));
        }
    }
    if let Ok(pq) = PROMPT_QUEUE.lock() {
        if let Some(last) = pq.back() {
            return Some((false, last.clone()));
        }
    }
    None
}

pub fn pop_last_pending_message() -> Option<(bool, String)> {
    if let Some(s) = pop_last_steer_message() {
        return Some((true, s));
    }
    if let Some(q) = pop_last_queued_message() {
        return Some((false, q));
    }
    None
}

pub fn list_pending_messages() -> Vec<(bool, String)> {
    let mut out = Vec::new();
    if let Ok(sq) = STEER_QUEUE.lock() {
        for m in sq.iter() {
            out.push((true, m.clone()));
        }
    }
    if let Ok(pq) = PROMPT_QUEUE.lock() {
        for m in pq.iter() {
            out.push((false, m.clone()));
        }
    }
    out
}

pub fn list_queued_messages() -> Vec<String> {
    PROMPT_QUEUE
        .lock()
        .map(|q| q.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn remove_queued_message(idx: usize) {
    if let Ok(mut q) = PROMPT_QUEUE.lock() {
        if idx < q.len() {
            q.remove(idx);
        }
    }
}

pub fn promote_queued_message(idx: usize) {
    if let Ok(mut q) = PROMPT_QUEUE.lock() {
        if idx > 0 && idx < q.len() {
            if let Some(item) = q.remove(idx) {
                q.push_front(item);
            }
        }
    }
}

pub fn clear_queued_messages() {
    if let Ok(mut q) = PROMPT_QUEUE.lock() {
        q.clear();
    }
}

pub fn queued_message_count() -> usize {
    PROMPT_QUEUE.lock().map(|q| q.len()).unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveTaskStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Debug, Clone)]
pub struct LiveTaskItem {
    pub title: String,
    pub status: LiveTaskStatus,
}

static LIVE_TASKS: std::sync::LazyLock<Mutex<Vec<LiveTaskItem>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

pub fn list_live_tasks() -> Vec<LiveTaskItem> {
    LIVE_TASKS.lock().map(|t| t.clone()).unwrap_or_default()
}

pub fn clear_live_tasks() {
    if let Ok(mut t) = LIVE_TASKS.lock() {
        t.clear();
    }
}

/// Serialises tests that touch the process-wide task list.
#[cfg(test)]
pub fn live_task_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GUARD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drops a finished list at the start of a new turn.
///
/// A completed bar describes work that is already over; leaving it up let it bleed into the
/// next, unrelated request while still reading `[3/3]`.
pub fn clear_finished_live_tasks() {
    if let Ok(mut t) = LIVE_TASKS.lock() {
        if !t.is_empty() && t.iter().all(|i| i.status == LiveTaskStatus::Done) {
            t.clear();
        }
    }
}

pub fn add_live_task(title: String) {
    let clean = title.trim().to_string();
    if clean.is_empty() {
        return;
    }
    if let Ok(mut t) = LIVE_TASKS.lock() {
        t.push(LiveTaskItem {
            title: clean,
            status: LiveTaskStatus::Pending,
        });
    }
}

pub fn toggle_live_task(idx_1based: usize) -> bool {
    if idx_1based == 0 {
        return false;
    }
    if let Ok(mut t) = LIVE_TASKS.lock() {
        if let Some(item) = t.get_mut(idx_1based - 1) {
            item.status = match item.status {
                LiveTaskStatus::Done => LiveTaskStatus::Pending,
                _ => LiveTaskStatus::Done,
            };
            return true;
        }
    }
    false
}

/// Zero-roundtrip task extractor: parses markdown checkboxes (`- [ ]`, `- [x]`) or
/// updates active task progress based on assistant output and tool calls without extra LLM turns.
pub fn sync_live_tasks_from_text(text: &str) {
    let mut parsed = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let rest = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| {
                let bytes = trimmed.as_bytes();
                if bytes.len() >= 3
                    && bytes[0].is_ascii_digit()
                    && bytes[1] == b'.'
                    && bytes[2] == b' '
                {
                    Some(&trimmed[3..])
                } else {
                    None
                }
            });
        if let Some(r) = rest {
            let r = r.trim_start();
            if let Some(title) = r.strip_prefix("[ ] ").or_else(|| r.strip_prefix("[] ")) {
                let clean = title.trim().trim_matches('*').trim_matches('`').trim();
                if !clean.is_empty() && clean.len() <= 120 {
                    parsed.push(LiveTaskItem {
                        title: clean.to_string(),
                        status: LiveTaskStatus::Pending,
                    });
                }
            } else if let Some(title) = r
                .strip_prefix("[x] ")
                .or_else(|| r.strip_prefix("[X] "))
                .or_else(|| r.strip_prefix("[✓] "))
            {
                let clean = title.trim().trim_matches('*').trim_matches('`').trim();
                if !clean.is_empty() && clean.len() <= 120 {
                    parsed.push(LiveTaskItem {
                        title: clean.to_string(),
                        status: LiveTaskStatus::Done,
                    });
                }
            } else if let Some(title) = r.strip_prefix("[~] ").or_else(|| r.strip_prefix("[>] ")) {
                let clean = title.trim().trim_matches('*').trim_matches('`').trim();
                if !clean.is_empty() && clean.len() <= 120 {
                    parsed.push(LiveTaskItem {
                        title: clean.to_string(),
                        status: LiveTaskStatus::InProgress,
                    });
                }
            }
        }
    }
    // Adopt a parsed checklist only when it is the plan actually in play: there is none yet,
    // the last one finished, or this restates a title we already track. Without that gate a
    // checklist quoted from a README replaced the live plan mid-run.
    let carried: Vec<(String, LiveTaskStatus)> = LIVE_TASKS
        .lock()
        .map(|t| {
            t.iter()
                .map(|i| (i.title.to_lowercase(), i.status.clone()))
                .collect()
        })
        .unwrap_or_default();
    let restates = parsed.iter().any(|i| {
        carried
            .iter()
            .any(|(title, _)| *title == i.title.to_lowercase())
    });
    let adoptable = (parsed.len() >= 2
        && (carried.is_empty()
            || carried.iter().all(|(_, s)| *s == LiveTaskStatus::Done)
            || restates))
        || (parsed.len() == 1 && restates);
    if adoptable {
        // A fresh checklist usually restates a plan the assistant already emitted, so a plain
        // replace would rewind the bar to task 1 mid-run. Titles are the identity: carry the
        // known status over, and let an explicit `[x]`/`[~]` marker win.
        for item in parsed.iter_mut() {
            if item.status != LiveTaskStatus::Pending {
                continue;
            }
            let key = item.title.to_lowercase();
            if let Some((_, prev)) = carried.iter().find(|(title, _)| *title == key) {
                item.status = prev.clone();
            }
        }
        if !parsed
            .iter()
            .any(|i| i.status == LiveTaskStatus::InProgress)
        {
            if let Some(first_pending) = parsed
                .iter_mut()
                .find(|i| i.status == LiveTaskStatus::Pending)
            {
                first_pending.status = LiveTaskStatus::InProgress;
            }
        }
        if let Ok(mut t) = LIVE_TASKS.lock() {
            *t = parsed;
        }
    }
}

/// Automatically advances task progress when tools touch files mentioned in a task item
/// or when all edits are verified. Zero LLM roundtrip cost.
///
/// `target` is the structural argument (`src/config.rs`, `cargo test`), never the rendered
/// header summary: a stem derived from `[local] cd ~/x && cargo test` matched unrelated titles
/// and marked a step done that had not run.
pub fn advance_live_tasks_on_tool(tool_name: &str, target: &str) {
    if let Ok(mut t) = LIVE_TASKS.lock() {
        if t.is_empty() {
            return;
        }
        let target_lower = target.trim().to_lowercase();
        let is_shell = matches!(tool_name, "bash" | "shell" | "sh" | "run");
        // A shell step is named by its leading words (`cargo test`), not by a path stem: the
        // command is not a path, so `file_name()` returned the whole string or nothing.
        let hint = if is_shell {
            target_lower
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            std::path::Path::new(&target_lower)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string()
        };
        let is_mutating = matches!(
            tool_name,
            "edit" | "edit_file" | "write" | "write_file" | "bash" | "shell" | "sh" | "run"
        );
        if is_mutating && hint.len() >= 3 {
            let mut matched_idx = None;
            for (idx, item) in t.iter().enumerate() {
                if item.status != LiveTaskStatus::Done && item.title.to_lowercase().contains(&hint)
                {
                    matched_idx = Some(idx);
                    break;
                }
            }
            if let Some(idx) = matched_idx {
                t[idx].status = LiveTaskStatus::Done;
            }
        }
        if !t.iter().any(|i| i.status == LiveTaskStatus::InProgress) {
            if let Some(next_p) = t.iter_mut().find(|i| i.status == LiveTaskStatus::Pending) {
                next_p.status = LiveTaskStatus::InProgress;
            }
        }
    }
}

pub fn format_live_tasks_compact_bar(max_w: usize) -> Option<String> {
    let tasks = list_live_tasks();
    if tasks.is_empty() {
        return None;
    }
    let total = tasks.len();
    let done = tasks
        .iter()
        .filter(|t| t.status == LiveTaskStatus::Done)
        .count();
    let mut parts = Vec::with_capacity(tasks.len().min(4));
    for item in tasks.iter().take(4) {
        // Budget is display cells, not chars: a CJK or emoji title is twice as wide as its
        // `chars()` count, which overflowed the dock and desynchronised the erase geometry.
        let (cut, _) = hud::truncate_ansi_to_width(&item.title, 26);
        let short_t = cut.trim_end_matches(RESET);
        let s_str = match item.status {
            LiveTaskStatus::Done => format!("{}✓ {}{}", GREEN, short_t, RESET),
            LiveTaskStatus::InProgress => format!("{}{}▶ {}{}", CYAN, BOLD, short_t, RESET),
            LiveTaskStatus::Pending => format!("{}○ {}{}", DIM, short_t, RESET),
        };
        parts.push(s_str);
    }
    if total > 4 {
        parts.push(format!("{}+{}{}", DIM, total - 4, RESET));
    }
    let bar_color = if done == total { GREEN } else { YELLOW };
    let raw = format!(
        "{}│{}  {}📋 [{}/{}]{} {}",
        CYAN,
        RESET,
        bar_color,
        done,
        total,
        RESET,
        parts.join(&format!(" {}·{} ", DIM, RESET))
    );
    let (trunc, _) = hud::truncate_ansi_to_width_ellipsis(&raw, max_w);
    Some(trunc)
}

#[cfg(unix)]
extern "C" fn handle_sigint(_sig: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
extern "C" fn handle_sigwinch(_sig: i32) {
    RESIZED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
#[link(name = "c")]
unsafe extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
}

#[cfg(windows)]
extern "system" fn handle_win_ctrl(ctrl_type: u32) -> i32 {
    if ctrl_type == 0 || ctrl_type == 1 {
        INTERRUPTED.store(true, Ordering::SeqCst);
        return 1;
    }
    0
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

pub fn install_sigint_handler() {
    #[cfg(unix)]
    unsafe {
        let _ = signal(2, handle_sigint);
        let _ = signal(28, handle_sigwinch);
        // Ignore SIGTTIN (21) and SIGTTOU (22) (SIG_IGN = 1) so child shells (`zsh -i`) that
        // steal the foreground process group cannot suspend the SPAVN HUD/spinner on TTY I/O.
        let _ = signal(21, std::mem::transmute::<usize, extern "C" fn(i32)>(1));
        let _ = signal(22, std::mem::transmute::<usize, extern "C" fn(i32)>(1));
    }
    #[cfg(windows)]
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handle_win_ctrl), 1);
    }
}

pub fn take_resized() -> bool {
    RESIZED.swap(false, Ordering::SeqCst)
}

pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

pub fn trigger_interrupt() {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub fn clear_interrupted() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}

/// Asks the running turn to end once the tool batch in flight has finished. Unlike
/// [`trigger_interrupt`] it never cancels a stream or a tool mid-way.
pub fn request_soft_stop() {
    SOFT_STOP.store(true, Ordering::SeqCst);
}

/// Consumes a pending soft stop; the turn loop calls it between tool batches.
pub fn take_soft_stop() -> bool {
    SOFT_STOP.swap(false, Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked() -> std::sync::MutexGuard<'static, ()> {
        live_task_test_lock()
    }

    fn titles() -> Vec<(String, LiveTaskStatus)> {
        list_live_tasks()
            .into_iter()
            .map(|i| (i.title, i.status))
            .collect()
    }

    #[test]
    fn soft_stop_is_consumed_exactly_once() {
        assert!(!take_soft_stop());
        request_soft_stop();
        assert!(take_soft_stop());
        assert!(!take_soft_stop());
    }

    #[test]
    fn finished_list_is_dropped_for_the_next_turn() {
        let _g = locked();
        clear_live_tasks();
        sync_live_tasks_from_text("- [x] One\n- [x] Two");
        assert_eq!(titles().len(), 2);
        clear_finished_live_tasks();
        assert!(
            titles().is_empty(),
            "a fully done bar must not survive the turn"
        );

        sync_live_tasks_from_text("- [x] One\n- [ ] Two");
        clear_finished_live_tasks();
        assert_eq!(titles().len(), 2, "an unfinished list must survive");
        clear_live_tasks();
    }

    #[test]
    fn a_reexported_checklist_does_not_rewind_progress() {
        let _g = locked();
        clear_live_tasks();
        sync_live_tasks_from_text("- [ ] Update config.rs\n- [ ] Run cargo test");
        advance_live_tasks_on_tool("edit", "config.rs");
        let after_first = titles();
        assert_eq!(after_first[0].1, LiveTaskStatus::Done);
        sync_live_tasks_from_text("- [ ] Update config.rs\n- [ ] Run cargo test");
        assert_eq!(
            titles()[0].1,
            LiveTaskStatus::Done,
            "status must carry over"
        );
        clear_live_tasks();
    }

    #[test]
    fn a_quoted_checklist_cannot_hijack_the_live_plan() {
        let _g = locked();
        clear_live_tasks();
        sync_live_tasks_from_text("- [ ] Update config.rs\n- [ ] Run cargo test");
        // A README snippet with unrelated items must not replace the plan in play.
        sync_live_tasks_from_text("- [ ] Install prerequisites\n- [ ] Build the docs");
        let now: Vec<String> = titles().into_iter().map(|(t, _)| t).collect();
        assert_eq!(now, vec!["Update config.rs", "Run cargo test"]);
        // A single-item restatement of the current step is still accepted.
        advance_live_tasks_on_tool("edit", "config.rs");
        sync_live_tasks_from_text("- [ ] Run cargo test");
        assert_eq!(titles().len(), 1);
        clear_live_tasks();
    }

    #[test]
    fn bar_budget_is_measured_in_cells() {
        let _g = locked();
        clear_live_tasks();
        sync_live_tasks_from_text("- [ ] 日本語のとても長いタスク名をここに書く\n- [ ] Two");
        let bar = format_live_tasks_compact_bar(60).expect("bar");
        assert!(
            hud::visible_width(&bar) <= 60,
            "bar overflowed: {} cells",
            hud::visible_width(&bar)
        );
        clear_live_tasks();
    }
}
