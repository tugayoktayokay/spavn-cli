//! Spill-over storage for oversized tool output.
//!
//! `bash` truncates the middle of a large result before it reaches the model — cheap
//! for the context window, but destructive: the dropped bytes are gone for good, so the
//! model can only re-run the command (often expensive) to see them again. Spilling the
//! full text to disk keeps the context small *and* leaves the output recoverable: the
//! truncation marker carries a path the model can `read`, `search`, or `grep` later.
//!
//! Files live in `<spavn_dir>/tool-output/` so they never mix with the knowledge
//! artifacts in `<spavn_dir>/artifacts/` (those are `<project>.jsonl` and are parsed).

use crate::config::{set_owner_only_mode, spavn_dir};
use crate::session::safe_utf8_prefix;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Hard ceiling for a single spill file. A runaway command (`yes`, a huge build log)
/// must never fill the disk just because its output was large.
pub const SPILL_MAX_BYTES: usize = 8 * 1024 * 1024;

/// How long a spill stays recoverable. Shaken session turns persist their `recover:`
/// path in the session file, so the file must outlive the turn that wrote it — a
/// count cap did not: one shake pass writes dozens of spills at once and evicted
/// files other live sessions had just been told to read.
pub const SPILL_MAX_AGE_MS: u128 = 7 * 24 * 60 * 60 * 1000;

/// Disk ceiling for the whole spill directory. Only when it is exceeded are the
/// oldest spills evicted before their age limit, so a burst never touches recent files.
pub const SPILL_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

const SPILL_DIR: &str = "tool-output";

/// Distinguishes spills created inside the same millisecond by the same process.
/// Millisecond stamps alone collide: two parallel tool calls with the same label used to
/// resolve to one filename, so the second silently overwrote the first.
static SPILL_SEQ: AtomicU64 = AtomicU64::new(0);

fn spill_dir() -> PathBuf {
    spavn_dir().join(SPILL_DIR)
}

/// Writes `text` to a fresh spill file and returns its path.
///
/// `label` is slugified into the filename so `ls ~/.spavn/tool-output` stays readable.
/// Returns `None` on any I/O failure — a spill is best-effort, and a failed spill must
/// never turn a working command into an error.
pub fn spill_output(label: &str, text: &str) -> Option<PathBuf> {
    spill_output_in(&spill_dir(), label, text)
}

/// Same as [`spill_output`], but writes into an explicit directory.
///
/// The directory is a parameter rather than an env lookup so tests (and any future
/// per-session spill root) never have to mutate the global `SPAVN_HOME`.
pub fn spill_output_in(dir: &Path, label: &str, text: &str) -> Option<PathBuf> {
    let dir = dir.to_path_buf();
    fs::create_dir_all(&dir).ok()?;
    set_owner_only_mode(&dir, 0o700);

    let slug: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(40)
        .collect();
    let slug = if slug.is_empty() {
        "output".to_string()
    } else {
        slug
    };

    let stamp = now_ms();
    let seq = SPILL_SEQ.fetch_add(1, Ordering::Relaxed);
    // Zero-padded name parts: `prune_spills_in` orders by name, and a bare `seq`
    // would sort "10" before "9" and evict the newest file.
    let path = dir.join(format!(
        "{:013}-{:06}-{:06}-{}.txt",
        stamp,
        std::process::id() % 1_000_000,
        seq,
        slug
    ));

    let body = if text.len() > SPILL_MAX_BYTES {
        let cut = safe_utf8_prefix(text, SPILL_MAX_BYTES).len();
        format!(
            "{}\n\n... [spill truncated: kept {} of {} bytes] ...\n",
            &text[..cut],
            cut,
            text.len()
        )
    } else {
        text.to_string()
    };

    let mut f = fs::File::create(&path).ok()?;
    set_owner_only_mode(&path, 0o600);
    f.write_all(body.as_bytes()).ok()?;
    f.flush().ok()?;

    prune_spills_in(&dir, stamp, SPILL_MAX_AGE_MS, SPILL_BUDGET_BYTES);
    Some(path)
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The creation stamp embedded in a spill name (`<13-digit ms>-…`).
fn spill_stamp(path: &Path) -> Option<u128> {
    let name = path.file_name()?.to_str()?;
    name.get(..13)?.parse().ok()
}

/// Deletes spills older than `max_age_ms`, then — only if the directory still exceeds
/// `budget_bytes` — evicts the oldest until it fits, never the newest file.
///
/// Names embed a monotonic millisecond stamp and a padded counter, so ordering by
/// name is exact. Ordering by mtime was not: files written inside the same
/// filesystem timestamp tick compare equal.
fn prune_spills_in(dir: &Path, now_ms: u128, max_age_ms: u128, budget_bytes: u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        if !path.extension().is_some_and(|x| x == "txt") {
            continue;
        }
        let expired = spill_stamp(&path).is_some_and(|t| now_ms.saturating_sub(t) > max_age_ms);
        if expired {
            let _ = fs::remove_file(&path);
            continue;
        }
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        files.push((path, size));
    }
    let mut total: u64 = files.iter().map(|(_, s)| s).sum();
    if total <= budget_bytes {
        return;
    }
    files.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
    let newest = files.len().saturating_sub(1);
    for (path, size) in files.into_iter().take(newest) {
        if total <= budget_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

/// The truncation marker: tells the model *where* the missing bytes went and how to
/// get them back, instead of silently dropping content.
pub fn truncation_marker(
    dropped: usize,
    total: usize,
    spilled: Option<&std::path::Path>,
) -> String {
    match spilled {
        Some(path) => format!(
            "... [SPAVN truncated {} of {} bytes from the middle. Full output saved to `{}` — \
             read it with `read` (offset/limit), or search it with `search`/`bash grep`.] ...",
            dropped,
            total,
            path.display()
        ),
        None => format!(
            "... [SPAVN truncated {} of {} bytes of middle output] ...",
            dropped, total
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private spill root: the suite shares one process, and another test flips the
    /// global `SPAVN_HOME` then deletes it, so a shared directory is unusable here.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "spavn-spill-{}-{}-{}",
                tag,
                std::process::id(),
                SPILL_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::create_dir_all(&dir);
            TempRoot(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn marker_names_the_spill_path_when_one_exists() {
        let m = truncation_marker(500, 9000, Some(std::path::Path::new("/tmp/x.txt")));
        assert!(m.contains("500") && m.contains("9000"));
        assert!(m.contains("/tmp/x.txt"));
    }

    #[test]
    fn marker_degrades_when_the_spill_failed() {
        let m = truncation_marker(500, 9000, None);
        assert!(m.contains("500") && m.contains("9000"));
        assert!(!m.contains("saved to"));
    }

    #[test]
    fn slug_is_filesystem_safe_and_bounded() {
        let root = TempRoot::new("slug");
        let path = spill_output_in(root.path(), "rm -rf /tmp/$(whoami) ; echo \"hi\"", "body")
            .expect("spill");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains('/') && !name.contains(';') && !name.contains('$'));
        assert!(name.len() < 80, "name too long: {name}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "body");
    }

    fn name(stamp: u128, seq: usize) -> String {
        format!("{:013}-000001-{:06}-x.txt", stamp, seq)
    }

    #[test]
    fn a_burst_of_fresh_spills_evicts_nothing() {
        // One shake pass wrote 52 spills and the old 40-file cap deleted the rest,
        // including files another live session had just been handed.
        let root = TempRoot::new("burst");
        let dir = root.path();
        let paths: Vec<_> = (0..120)
            .map(|i| spill_output_in(dir, "shaken-turn", &format!("turn {i}")).expect("spill"))
            .collect();
        for p in &paths {
            assert!(p.exists(), "fresh spill evicted: {}", p.display());
        }
    }

    #[test]
    fn spills_past_the_age_limit_are_deleted_and_younger_ones_kept() {
        let root = TempRoot::new("age");
        let dir = root.path();
        let now = 10_000_000u128;
        let old = dir.join(name(now - 5_001, 0));
        let edge = dir.join(name(now - 5_000, 1));
        let young = dir.join(name(now - 10, 2));
        for p in [&old, &edge, &young] {
            fs::write(p, "b").unwrap();
        }
        prune_spills_in(dir, now, 5_000, u64::MAX);
        assert!(!old.exists(), "expired spill must be deleted");
        assert!(
            edge.exists(),
            "spill exactly at the age limit is still recoverable"
        );
        assert!(young.exists());
    }

    #[test]
    fn over_budget_evicts_oldest_first_and_never_the_newest() {
        let root = TempRoot::new("budget");
        let dir = root.path();
        let now = 10_000_000u128;
        let files: Vec<_> = (0..5)
            .map(|i| dir.join(name(now - 100 + i, i as usize)))
            .collect();
        for p in &files {
            fs::write(p, "x".repeat(10)).unwrap();
        }
        // 50 bytes on disk, 25 allowed: the three oldest go, the two newest stay.
        prune_spills_in(dir, now, u128::MAX, 25);
        let alive: Vec<bool> = files.iter().map(|p| p.exists()).collect();
        assert_eq!(alive, vec![false, false, false, true, true]);

        // A single file bigger than the whole budget is the newest: it survives.
        let huge = dir.join(name(now, 9));
        fs::write(&huge, "x".repeat(100)).unwrap();
        prune_spills_in(dir, now, u128::MAX, 25);
        assert!(huge.exists(), "the newest spill must never be evicted");
    }

    #[test]
    fn concurrent_spills_with_the_same_label_never_collide() {
        let root = TempRoot::new("concurrent");
        let dir = root.path();
        let big = "a".repeat(4096);
        let out: Vec<_> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8)
                .map(|_| s.spawn(|| spill_output_in(dir, "big", &big)))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let paths: Vec<_> = out
            .into_iter()
            .map(|p| p.expect("spill under contention"))
            .collect();
        let mut uniq: Vec<_> = paths.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(
            uniq.len(),
            paths.len(),
            "two spills shared a file: {:?}",
            paths
        );
    }

    #[test]
    fn oversized_body_is_capped_and_says_so() {
        let root = TempRoot::new("oversized");
        let big = "a".repeat(SPILL_MAX_BYTES + 1024);
        let path = spill_output_in(root.path(), "big", &big).expect("spill");
        let written = fs::read_to_string(&path).expect("spill file must be readable");
        assert!(written.len() <= SPILL_MAX_BYTES + 200);
        assert!(written.contains("spill truncated: kept"));
    }
}
