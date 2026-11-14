//! Release-update awareness.
//!
//! There is no auto-updater and no daemon that runs without you: the binary compares its own
//! `CARGO_PKG_VERSION` against the newest published tag and, when it is behind, can both *tell*
//! you which command to run (the narrow background check) and, on `/update`, *do* it — download
//! the prebuilt release asset for this platform, replace the installed binary in place, and
//! re-exec into it. Nothing is downloaded or executed on your behalf until you run `/update`.
//!
//! The check is deliberately narrow. It is one `GET` to the release endpoint derived from
//! `CARGO_PKG_REPOSITORY` (or `updateCheckUrl`), it runs detatched at startup, it is skipped
//! entirely when `updateCheck` is `false` or `SPAVN_UPDATE_CHECK=0`, and the result is cached in
//! `<spavn_dir>/update-check.json` so a cold start never touches the network to render its banner.
//! A cache that already holds a newer release stops further network checks; an "up to date"
//! answer is re-verified on the next start, because a release published a minute after the last
//! check must not stay invisible until the next interval elapses.

use crate::config::{load_user_config, set_owner_only_mode, spavn_dir, tr_en};
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
pub fn release_endpoint() -> Option<String> {
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

/// Full cargo target triple for this host, matching the asset names `install.sh` builds.
///
/// A tiny OS+ARCH map keeps `/update` zero-dependency and deterministic (no `rustc` round-trip),
/// and covers every platform whose prebuilt asset exists. `None` leaves the check-only path intact.
pub fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

/// URL of the latest release's prebuilt asset for this platform: `spavn-{target}.tar.gz`.
///
/// A deterministic URL (no GitHub API round-trip) so `/update` can download in place directly.
/// Returns `None` on platforms whose asset naming is unknown, leaving the check-only path intact.
pub fn asset_url() -> Option<String> {
    let slug = repo_slug()?;
    let target = target_triple()?;
    Some(format!(
        "https://github.com/{slug}/releases/latest/download/spavn-{target}.tar.gz"
    ))
}

/// Why [`fetch_verified_asset_binary`] did not return a binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetError {
    /// Platform has no prebuilt tarball, or the archive could not be fetched/extracted.
    Unavailable,
    /// Archive downloaded, but its SHA-256 did not match the published `.sha256` sidecar.
    ChecksumMismatch(String),
}

/// Pure-Rust FIPS 180-4 SHA-256 digest (lowercase hex), keeping archive verification
/// zero-dependency and identical across macOS, Linux, and Windows.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = String::with_capacity(64);
    for word in h {
        use std::fmt::Write;
        let _ = write!(&mut out, "{word:08x}");
    }
    out
}

fn parse_sha256_sidecar(raw: &str) -> Option<String> {
    let token = raw.split_whitespace().next()?;
    if token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(token.to_ascii_lowercase())
    } else {
        None
    }
}

/// Validates `archive` against an optional `.sha256` sidecar body.
///
/// Matches `install.sh`: when a sidecar is present, the archive must parse and match its digest;
/// when no sidecar was published (`None`), verification is skipped so custom mirrors still work.
fn verify_archive_checksum(archive: &std::path::Path, sidecar: Option<&str>) -> Result<(), String> {
    let Some(raw_sidecar) = sidecar else {
        return Ok(());
    };
    let Some(expected) = parse_sha256_sidecar(raw_sidecar) else {
        return Err(tr_en(
            "bozuk SHA-256 doğrulama dosyası alındı.",
            "received malformed SHA-256 checksum file.",
        )
        .to_string());
    };
    let bytes = std::fs::read(archive).map_err(|e| e.to_string())?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(format!(
            "{} (expected {expected}, got {actual})",
            tr_en(
                "SHA-256 doğrulaması başarısız; arşiv reddedildi",
                "SHA-256 checksum mismatch; archive rejected"
            )
        ));
    }
    Ok(())
}

/// Downloads, SHA-256-verifies, and extracts the prebuilt release asset for this platform into a
/// fresh temp dir, returning the path to the staged `spavn` binary.
pub fn fetch_verified_asset_binary() -> Result<PathBuf, AssetError> {
    let url = asset_url().ok_or(AssetError::Unavailable)?;
    let base = std::env::temp_dir().join(format!(
        "spavn-update-{}-{}-{}",
        std::env::consts::ARCH,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&base);
    let tarball = base.join("spavn.tar.gz");
    let sidecar_path = base.join("spavn.tar.gz.sha256");

    let downloaded = std::process::Command::new("curl")
        .args(["-fsSL", &url, "-o", tarball.to_string_lossy().as_ref()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !downloaded {
        let _ = std::fs::remove_dir_all(&base);
        return Err(AssetError::Unavailable);
    }

    let sidecar_url = format!("{url}.sha256");
    let sidecar_ok = std::process::Command::new("curl")
        .args([
            "-fsSL",
            &sidecar_url,
            "-o",
            sidecar_path.to_string_lossy().as_ref(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let sidecar_text = if sidecar_ok {
        std::fs::read_to_string(&sidecar_path).ok()
    } else {
        None
    };

    if let Err(reason) = verify_archive_checksum(&tarball, sidecar_text.as_deref()) {
        let _ = std::fs::remove_dir_all(&base);
        return Err(AssetError::ChecksumMismatch(reason));
    }

    let extracted = std::process::Command::new("tar")
        .args([
            "-xzf",
            tarball.to_string_lossy().as_ref(),
            "-C",
            base.to_string_lossy().as_ref(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let bin_src = if extracted {
        if let Some(found) = find_binary(&base) {
            let staged_out = std::env::temp_dir().join(format!(
                "spavn-update-bin-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            if std::fs::copy(&found, &staged_out).is_ok() {
                Ok(staged_out)
            } else {
                Err(AssetError::Unavailable)
            }
        } else {
            Err(AssetError::Unavailable)
        }
    } else {
        Err(AssetError::Unavailable)
    };

    let _ = std::fs::remove_dir_all(&base);
    bin_src
}

/// Downloads, verifies, and extracts the prebuilt release asset, returning `None` on any failure.
pub fn fetch_asset_binary() -> Option<PathBuf> {
    fetch_verified_asset_binary().ok()
}

/// Locates the extracted `spavn` binary: a flat one at the archive root first, then a recursive
/// search under nested folders. Keeps `/update` resilient to how a release tarball is laid out.
fn find_binary(dir: &std::path::Path) -> Option<PathBuf> {
    for name in ["spavn", "spawn"] {
        let flat = dir.join(name);
        if flat.exists() && flat.is_file() {
            return Some(flat);
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_binary(&path) {
                    return Some(found);
                }
            } else if matches!(entry.file_name().to_str(), Some("spavn" | "spawn")) {
                return Some(path);
            }
        }
    }
    None
}

///
/// Detected from the executable path so the advice matches reality: a Homebrew or `cargo install`
/// binary must not be told to run the curl installer, which would drop a second copy next to it.
pub fn upgrade_command() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    upgrade_command_for(&exe.to_string_lossy())
}

/// True when `exe_path` belongs to Homebrew or `cargo install`, where dropping a binary into
/// `~/.local/bin` would create a second installation instead of upgrading the running one.
pub(crate) fn is_externally_managed_path(exe_path: &str) -> bool {
    let lower = exe_path.replace('\\', "/").to_lowercase();
    lower.contains("/cellar/")
        || lower.contains("/homebrew/")
        || lower.contains("/linuxbrew/")
        || lower.contains("/opt/homebrew/")
        || lower.contains("/.cargo/bin/")
}

/// True when the running executable is owned by Homebrew or `cargo install`.
pub fn is_externally_managed() -> bool {
    std::env::current_exe()
        .ok()
        .map(|p| is_externally_managed_path(&p.to_string_lossy()))
        .unwrap_or(false)
}

/// Target path for in-place self-updates (`SPAVN_INSTALL_DIR`, else a non-`target/` running binary,
/// else `~/.local/bin/spavn`).
pub fn install_target_binary() -> PathBuf {
    if let Ok(dir) = std::env::var("SPAVN_INSTALL_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("spavn");
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let norm = exe.to_string_lossy().replace('\\', "/");
        if !is_externally_managed_path(&norm)
            && !norm.contains("/target/debug/")
            && !norm.contains("/target/release/")
            && exe.parent().is_some_and(|p| p.exists())
        {
            return exe;
        }
    }
    std::env::var("HOME")
        .map(|home| PathBuf::from(home).join(".local/bin/spavn"))
        .unwrap_or_default()
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

/// True when the startup thread must not touch the network at all.
///
/// A cache that already holds a newer release is a complete answer: the banner prints it and
/// re-asking GitHub changes nothing. A cache that says "up to date" is only certain until the
/// next release lands; treating it as fresh for a whole interval left releases published right
/// after a check invisible, so such an answer never suppresses the next check.
fn should_skip_check(cached: Option<&CachedCheck>) -> bool {
    cached.is_some_and(|c| is_newer(&c.latest, CURRENT))
}

/// Result of a headless [`upgrade_installed_binary`] attempt.
pub enum UpgradeOutcome {
    /// No newer release published.
    UpToDate,
    /// Running from Homebrew or `cargo install`; caller should show `info.command` instead of
    /// writing a duplicate copy into `~/.local/bin`.
    ManagedExternally(UpdateInfo),
    /// A newer release exists but ships no native asset for this platform.
    NoAsset(String),
    /// Downloaded, SHA-256-verified, staged on a fresh inode and probe-verified at `bin`.
    Staged {
        latest: String,
        bin: std::path::PathBuf,
    },
    /// Network, checksum, or staging failure.
    CheckError(String),
}

/// Headless in-place upgrade shared by the `spavn update` subcommand and `/update`.
///
/// Checks the release endpoint (which also refreshes the banner cache), defers to the package
/// manager for Homebrew/Cargo installs, and otherwise downloads + SHA-256-verifies the prebuilt
/// native asset, stages it onto a fresh inode, probes it and refreshes the `spvn` alias.
pub fn upgrade_installed_binary() -> UpgradeOutcome {
    if is_check_disabled() {
        return UpgradeOutcome::CheckError(
            tr_en(
                "Sürüm denetimi kapalı (updateCheck: false veya SPAVN_UPDATE_CHECK).",
                "Update check is off (updateCheck: false or SPAVN_UPDATE_CHECK).",
            )
            .to_string(),
        );
    }
    let info = match check_now() {
        Ok(Some(info)) => info,
        Ok(None) => return UpgradeOutcome::UpToDate,
        Err(e) => return UpgradeOutcome::CheckError(e),
    };
    if is_externally_managed() {
        return UpgradeOutcome::ManagedExternally(info);
    }
    let bin_src = match fetch_verified_asset_binary() {
        Ok(path) => path,
        Err(AssetError::Unavailable) => return UpgradeOutcome::NoAsset(info.latest),
        Err(AssetError::ChecksumMismatch(reason)) => return UpgradeOutcome::CheckError(reason),
    };
    let target_bin = install_target_binary();
    let staged_ok = target_bin.parent().is_some_and(|p| p.exists())
        && crate::slash_commands::stage_binary(&bin_src, &target_bin)
        && crate::slash_commands::probe_binary(&target_bin);
    let _ = std::fs::remove_file(&bin_src);
    if !staged_ok {
        return UpgradeOutcome::CheckError(
            tr_en(
                "ikili dosya yerleştirilemedi; geçerli sürümle devam ediliyor.",
                "installed binary could not be staged; continuing on the current version.",
            )
            .to_string(),
        );
    }
    crate::slash_commands::link_alias(&target_bin);
    UpgradeOutcome::Staged {
        latest: info.latest,
        bin: target_bin,
    }
}

/// Refreshes the cache in a detached thread on every startup.
///
/// The thread is never joined and never prints: the notice surfaces through [`cached_notice`]
/// on the next banner, so a slow network cannot delay the first prompt.
pub fn spawn_background_check() {
    if !check_enabled() {
        return;
    }
    if should_skip_check(read_cache().as_ref()) {
        return;
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
    fn skip_check_only_when_cache_holds_a_newer_release() {
        let now = now_unix();
        // An "up to date" answer never suppresses the next check: a release published right
        // after this cache was written must still be noticed on the next start.
        let fresh = CachedCheck {
            checked_at: now,
            latest: CURRENT.to_string(),
            url: String::new(),
        };
        assert!(!should_skip_check(Some(&fresh)));
        assert!(!should_skip_check(None));
        let newer = CachedCheck {
            checked_at: now,
            latest: "9.9.9".to_string(),
            url: String::new(),
        };
        assert!(should_skip_check(Some(&newer)));
        // A cache older than the interval is also no excuse when it holds a newer release...
        let stale_up_to_date = CachedCheck {
            checked_at: now.saturating_sub(90_000),
            latest: CURRENT.to_string(),
            url: String::new(),
        };
        assert!(!should_skip_check(Some(&stale_up_to_date)));
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

    #[cfg(unix)]
    #[test]
    fn target_triple_matches_install_sh_assets() {
        let triple = target_triple().expect("known platform");
        // Matches the TARGET values install.sh builds assets for.
        assert!(triple.starts_with(std::env::consts::ARCH));
        assert!(triple.contains("darwin") || triple.contains("linux"));
    }

    #[cfg(windows)]
    #[test]
    fn target_triple_is_none_on_windows_tarball_channel() {
        assert_eq!(target_triple(), None);
        assert_eq!(asset_url(), None);
    }

    #[cfg(unix)]
    #[test]
    fn asset_url_points_at_the_platform_asset() {
        // repo_slug() is read from CARGO_PKG_REPOSITORY at compile time; the URL must embed it and
        // the platform asset suffix so `/update` downloads the right tarball.
        let url = asset_url().expect("known platform + repo");
        assert!(url.starts_with("https://github.com/"));
        assert!(url.contains("/releases/latest/download/spavn-"));
        assert!(url.ends_with(".tar.gz"));
    }

    #[test]
    fn sha256_matches_nist_vectors_and_verifies_sidecar() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );

        let tmp = std::env::temp_dir().join(format!("spavn-sha-test-{}", std::process::id()));
        std::fs::write(&tmp, b"spavn archive payload").unwrap();
        let digest = sha256_hex(b"spavn archive payload");
        let sidecar = format!("{digest}  spavn-aarch64-apple-darwin.tar.gz\n");
        assert!(verify_archive_checksum(&tmp, Some(&sidecar)).is_ok());
        assert!(verify_archive_checksum(&tmp, None).is_ok());
        assert!(verify_archive_checksum(
            &tmp,
            Some("0000000000000000000000000000000000000000000000000000000000000000  spavn.tar.gz")
        )
        .is_err());
        assert!(verify_archive_checksum(&tmp, Some("not-a-sha256")).is_err());
        let _ = std::fs::remove_file(&tmp);

        assert!(is_externally_managed_path(
            "/opt/homebrew/Cellar/spavn/0.2.16/bin/spavn"
        ));
        assert!(is_externally_managed_path("/Users/me/.cargo/bin/spavn"));
        assert!(!is_externally_managed_path("/Users/me/.local/bin/spavn"));
    }

    #[test]
    fn find_binary_prefers_flat_archive_root() {
        let dir = std::env::temp_dir().join(format!("spavn-find-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // A nested copy must be ignored when a flat one exists.
        let nested = dir.join("pkg");
        let _ = std::fs::create_dir_all(&nested);
        let _ = std::fs::File::create(nested.join("spawn"));
        let _ = std::fs::File::create(dir.join("spawn"));
        assert_eq!(find_binary(&dir), Some(dir.join("spawn")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_binary_walks_nested_folders() {
        let dir = std::env::temp_dir().join(format!("spavn-find-nested-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("pkg"));
        let found_path = dir.join("pkg").join("spawn");
        let _ = std::fs::File::create(&found_path);
        assert_eq!(find_binary(&dir), Some(found_path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_binary_locates_spavn_binary_name() {
        let dir = std::env::temp_dir().join(format!("spavn-find-v-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("sub"));
        let nested_spavn = dir.join("sub").join("spavn");
        let _ = std::fs::File::create(&nested_spavn);
        assert_eq!(find_binary(&dir), Some(nested_spavn));
        let flat_spavn = dir.join("spavn");
        let _ = std::fs::File::create(&flat_spavn);
        assert_eq!(find_binary(&dir), Some(flat_spavn));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
