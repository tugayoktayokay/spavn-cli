//! Release-update awareness.
//!
//! There is no auto-updater and no daemon: the binary compares its own
//! `CARGO_PKG_VERSION` against the newest published tag and, when it is behind, tells the user
//! which command to run. Nothing is downloaded or executed on the user's behalf.
//!
//! The check is deliberately narrow. It is one `GET` to the release endpoint derived from
//! `CARGO_PKG_REPOSITORY` (or `updateCheckUrl`), it runs at most once per
//! `updateCheckIntervalHours`, it is skipped entirely when `updateCheck` is `false` or
//! `SPAVN_UPDATE_CHECK=0`, and the result is cached in `<spavn_dir>/update-check.json` so a cold
//! start never touches the network to render its banner.

use crate::config::{load_user_config, set_owner_only_mode, spavn_dir};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Version this binary was built from, as stamped by Cargo.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

const CACHE_FILE: &str = "update-check.json";

/// Notice produced by the background thread and not yet shown.
///
/// The input loop owns the terminal, so the checker may not print. It parks the result here and
/// the loop drains it on its next tick, erasing the dock first so the notice lands on a clean line.
static PENDING_NOTICE: Mutex<Option<UpdateInfo>> = Mutex::new(None);

/// Takes the parked notice, if the background check found one. Idempotent: drains once.
pub fn take_pending_notice() -> Option<UpdateInfo> {
    PENDING_NOTICE.lock().ok()?.take()
}

#[cfg(test)]
fn park_notice_for_test(info: UpdateInfo) {
    if let Ok(mut slot) = PENDING_NOTICE.lock() {
        *slot = Some(info);
    }
}

/// A newer release than the one running, plus the command that installs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub url: String,
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedCheck {
    checked_at: u64,
    latest: String,
    #[serde(default)]
    url: String,
}

fn cache_path() -> PathBuf {
    spavn_dir().join(CACHE_FILE)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `owner/repo` for the repository this binary was built from.
///
/// Read from Cargo metadata instead of a literal so a fork points at its own releases without a
/// code change, and so nothing here hardcodes `tugayoktayokay/spavn-cli`.
pub fn repo_slug() -> Option<&'static str> {
    let repo = env!("CARGO_PKG_REPOSITORY").trim().trim_end_matches('/');
    let rest = repo
        .strip_prefix("https://github.com/")
        .or_else(|| repo.strip_prefix("http://github.com/"))
        .or_else(|| repo.strip_prefix("git@github.com:"))
        .or_else(|| repo.strip_prefix("https://api.github.com/repos/"))?;
    let rest = rest.trim_end_matches(".git").trim_end_matches('/');
    if rest.split('/').count() == 2 && !rest.is_empty() {
        Some(rest)
    } else {
        None
    }
}

/// Release endpoint used by the background check: `updateCheckUrl`, else the GitHub API for the
/// packaged repository. `None` means this build has nowhere to ask, so the check stays off.
fn release_endpoint() -> Option<String> {
    if let Some(custom) = load_user_config().update_check_url {
        let custom = custom.trim().to_string();
        return if custom.is_empty() {
            None
        } else {
            Some(custom)
        };
    }
    repo_slug().map(|slug| format!("https://api.github.com/repos/{slug}/releases/latest"))
}

fn check_enabled() -> bool {
    if std::env::var("SPAVN_UPDATE_CHECK").is_ok_and(|v| {
        let v = v.trim();
        v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    }) {
        return false;
    }
    load_user_config().update_check
}

/// Splits `1.2.3-rc1` into numeric parts, ignoring pre-release and build metadata.
///
/// Comparison is numeric per component so `0.10.0` sorts above `0.9.0`; a component that is not a
/// number counts as `0` rather than aborting the whole comparison.
fn version_parts(raw: &str) -> Vec<u64> {
    let core = raw.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or("");
    let mut parts: Vec<u64> = core
        .split('.')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0))
        .collect();
    while parts.len() > 1 && parts.last() == Some(&0) {
        parts.pop();
    }
    parts
}

/// Orders two version strings component-wise, padding the shorter one with zeros.
pub fn compare_versions(candidate: &str, current: &str) -> Ordering {
    let a = version_parts(candidate);
    let b = version_parts(current);
    let len = a.len().max(b.len());
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

/// True when `candidate` is a strictly newer release than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    compare_versions(candidate, current) == Ordering::Greater
}

/// Upgrade command for the install channel this binary was launched from.
///
/// Detected from the executable path so the advice matches reality: a Homebrew or `cargo install`
/// binary must not be told to run the curl installer, which would drop a second copy next to it.
pub fn upgrade_command() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    upgrade_command_for(&exe.to_string_lossy())
}

/// [`upgrade_command`] for an explicit executable path, so the channel mapping is testable
/// without moving the running binary into a Homebrew prefix.
fn upgrade_command_for(exe_path: &str) -> String {
    let path = exe_path.replace('\\', "/");
    let lower = path.to_lowercase();
    if lower.contains("/cellar/")
        || lower.contains("/homebrew/")
        || lower.contains("/linuxbrew/")
        || lower.contains("/opt/homebrew/")
    {
        return "brew upgrade spavn".to_string();
    }
    if lower.contains("/.cargo/bin/") {
        return "cargo install spavn --locked --force".to_string();
    }
    match repo_slug() {
        Some(slug) => {
            format!("curl -fsSL https://raw.githubusercontent.com/{slug}/main/install.sh | sh")
        }
        None => "see your package manager for the upgrade command".to_string(),
    }
}

fn build_info(latest: &str, url: &str) -> UpdateInfo {
    UpdateInfo {
        current: CURRENT.to_string(),
        latest: latest.to_string(),
        url: url.to_string(),
        command: upgrade_command(),
    }
}

fn write_cache(latest: &str, url: &str) {
    let entry = CachedCheck {
        checked_at: now_unix(),
        latest: latest.to_string(),
        url: url.to_string(),
    };
    let Ok(raw) = serde_json::to_string(&entry) else {
        return;
    };
    let path = cache_path();
    if fs::write(&path, raw).is_ok() {
        set_owner_only_mode(&path, 0o600);
    }
}

fn read_cache() -> Option<CachedCheck> {
    let raw = fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str::<CachedCheck>(&raw).ok()
}

/// Newer release recorded by an earlier check, without touching the network.
///
/// The startup banner calls this so it prints instantly; the freshness decision belongs to
/// [`spawn_background_check`], which refreshes the cache behind the prompt.
pub fn cached_notice() -> Option<UpdateInfo> {
    if !check_enabled() {
        return None;
    }
    let cached = read_cache()?;
    if !is_newer(&cached.latest, CURRENT) {
        return None;
    }
    Some(build_info(&cached.latest, &cached.url))
}

/// True when the automatic check is switched off, either by config or by `SPAVN_UPDATE_CHECK`.
///
/// Callers that act on an explicit request (`/update`) use this to say so out loud instead of
/// reporting "up to date", which would be a lie when nothing was asked.
pub fn is_check_disabled() -> bool {
    !check_enabled()
}

/// Fetches the latest published tag and, when it is newer, returns the notice.
///
/// `Err` carries a user-facing reason; a failed check is never fatal and never blocks a turn.
pub fn check_now() -> Result<Option<UpdateInfo>, String> {
    if !check_enabled() {
        return Ok(None);
    }
    let endpoint = release_endpoint()
        .ok_or_else(|| "no release endpoint: set `updateCheckUrl` in the config".to_string())?;
    let headers = [
        ("Accept", "application/vnd.github+json"),
        ("User-Agent", concat!("spavn/", env!("CARGO_PKG_VERSION"))),
    ];
    let (status, body) = crate::http::get_text_public(&endpoint, &headers, 5)
        .map_err(|e| format!("could not reach {endpoint}: {e}"))?;
    if status != 200 {
        return Err(format!("{endpoint} answered HTTP {status}"));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("unreadable release JSON: {e}"))?;
    let tag = parsed
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("{endpoint} returned no `tag_name`"))?;
    let latest = tag.trim_start_matches('v').to_string();
    let url = parsed
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if !is_newer(&latest, CURRENT) {
        write_cache(CURRENT, &url);
        return Ok(None);
    }
    write_cache(&latest, &url);
    Ok(Some(build_info(&latest, &url)))
}

/// True when a freshly found release still has to be announced.
///
/// The banner already printed whatever the cache knew before this run, so parking the same version
/// again would show the user the same notice twice.
fn should_park(already_surfaced: Option<&str>, latest: &str) -> bool {
    already_surfaced != Some(latest)
}

/// Refreshes the cache in a detached thread, at most once per `updateCheckIntervalHours`.
///
/// Called once at startup. The thread is never joined and never prints: the notice surfaces
/// through [`cached_notice`] on the next banner, so a slow network cannot delay the first prompt.
pub fn spawn_background_check() {
    if !check_enabled() {
        return;
    }
    let interval = load_user_config().update_check_interval_hours.max(1) * 3_600;
    if let Some(cached) = read_cache() {
        if now_unix().saturating_sub(cached.checked_at) < interval {
            return;
        }
    }
    let _ = std::thread::Builder::new()
        .name("spavn-update-check".to_string())
        .spawn(|| {
            // What the cache already knew before this check. The banner has just printed that.
            let already_surfaced = read_cache().map(|c| c.latest);
            if let Ok(Some(info)) = check_now() {
                if !should_park(already_surfaced.as_deref(), &info.latest) {
                    return;
                }
                if let Ok(mut slot) = PENDING_NOTICE.lock() {
                    *slot = Some(info);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_components_order_like_versions_not_like_strings() {
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.1"));
    }

    #[test]
    fn tag_prefixes_and_padding_are_ignored() {
        assert!(is_newer("v0.2", "0.1.9"));
        assert!(!is_newer("v0.1.0", "0.1"));
        assert_eq!(compare_versions("1.0.0", "1"), Ordering::Equal);
        assert_eq!(compare_versions("0.1.0", "0.1.0"), Ordering::Equal);
    }

    #[test]
    fn prerelease_patch_is_not_mistaken_for_newer() {
        // `0.2.0-rc1` releases before `0.2.0`; the numeric core still decides the ordering, so
        // the rc does not look newer than the final release and vice versa.
        assert_eq!(compare_versions("0.2.0-rc1", "0.2.0"), Ordering::Equal);
        assert!(is_newer("0.2.0", "0.1.0"));
    }

    #[test]
    fn garbage_components_never_panic() {
        assert!(!is_newer("", CURRENT));
        assert!(!is_newer("not-a-version", CURRENT));
        assert!(is_newer("999.0.0", CURRENT));
    }

    #[test]
    fn repo_slug_comes_from_cargo_metadata() {
        let slug = repo_slug().expect("Cargo.toml must set a github repository");
        assert_eq!(slug.split('/').count(), 2, "{slug}");
        assert!(!slug.ends_with(".git"), "{slug}");
    }

    #[test]
    fn upgrade_command_matches_the_install_channel() {
        // Telling a package-managed binary to run the curl installer would drop a second copy
        // beside it, so each prefix must map to its own package manager.
        for cellar in [
            "/opt/homebrew/Cellar/spavn/0.1.0/bin/spavn",
            "/usr/local/Cellar/spavn/0.1.0/bin/spavn",
            "/home/linuxbrew/.linuxbrew/bin/spavn",
        ] {
            assert_eq!(
                upgrade_command_for(cellar),
                "brew upgrade spavn",
                "{cellar}"
            );
        }
        assert_eq!(
            upgrade_command_for("/Users/me/.cargo/bin/spavn"),
            "cargo install spavn --locked --force"
        );
        let bare = upgrade_command_for("/Users/me/.local/bin/spavn");
        assert!(bare.contains("install.sh"), "{bare}");
    }

    #[test]
    fn the_parked_notice_is_announced_exactly_once() {
        assert!(take_pending_notice().is_none(), "slot starts empty");
        park_notice_for_test(UpdateInfo {
            current: "0.1.0".into(),
            latest: "0.2.0".into(),
            url: String::new(),
            command: "brew upgrade spavn".into(),
        });
        assert_eq!(
            take_pending_notice().map(|i| i.latest),
            Some("0.2.0".to_string())
        );
        assert!(
            take_pending_notice().is_none(),
            "the dock must not repeat the line on the next tick"
        );
    }

    #[test]
    fn a_version_the_banner_already_showed_is_not_announced_twice() {
        assert!(!should_park(Some("0.2.0"), "0.2.0"));
        assert!(should_park(Some("0.2.0"), "0.3.0"));
        assert!(should_park(None, "0.2.0"));
    }
}
