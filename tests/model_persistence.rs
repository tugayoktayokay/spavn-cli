//! End-to-end checks for session-scoped model selection and the config cache.
//!
//! Integration tests run in their own process, so pointing `SPAVN_HOME` at a scratch directory
//! cannot disturb the unit tests. All tests share one directory and one `config.json`, so every
//! test that reads or writes it takes `config_lock()` and runs one at a time.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use spavn::config::{load_user_config, save_user_config};
use spavn::session::{load_session_model, save_session_model};

fn home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("spavn-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SPAVN_HOME", &dir);
        std::env::remove_var("SPAVN_MODEL");
        dir
    })
}

fn config_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn session_model_switch_never_touches_the_default() {
    let _guard = config_lock();
    let home = home();
    let mut cfg = load_user_config();
    cfg.default_model = "pinned-default".to_string();
    save_user_config(&cfg).unwrap();

    // Terminal A switches model mid-session, terminal B is a different session.
    let a = home.join("session-a.jsonl");
    let b = home.join("session-b.jsonl");
    save_session_model(&a, "anthropic:claude-opus-5-5");

    assert_eq!(
        load_session_model(&a).as_deref(),
        Some("anthropic:claude-opus-5-5")
    );
    assert_eq!(
        load_session_model(&b),
        None,
        "a new session must not inherit another session's model"
    );
    assert_eq!(
        load_user_config().default_model,
        "pinned-default",
        "a /model switch must not rewrite the default"
    );

    // Re-opening session A later still finds its own model.
    save_session_model(&a, "local:qwen3");
    assert_eq!(load_session_model(&a).as_deref(), Some("local:qwen3"));
}

#[test]
fn config_cache_sees_saves_and_outside_edits() {
    let _guard = config_lock();
    let home = home();
    let path = home.join("config.json");

    let mut cfg = load_user_config();
    cfg.default_model = "first-model".to_string();
    save_user_config(&cfg).unwrap();
    assert_eq!(load_user_config().default_model, "first-model");

    // A save is visible on the very next load, never a stale cached copy.
    cfg.default_model = "second-model".to_string();
    save_user_config(&cfg).unwrap();
    assert_eq!(load_user_config().default_model, "second-model");

    // An edit made outside the process (different length, so the stamp changes) is picked up.
    let raw = std::fs::read_to_string(&path).unwrap();
    let edited = raw.replace("second-model", "externally-edited-model");
    assert_ne!(raw.len(), edited.len());
    std::fs::write(&path, edited).unwrap();
    assert_eq!(load_user_config().default_model, "externally-edited-model");
}

fn spavn(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_spavn"))
        .args(args)
        .env("SPAVN_HOME", home())
        .env("SPAVN_UPDATE_CHECK", "0")
        .env_remove("SPAVN_MODEL")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spavn binary runs")
}

#[test]
fn binary_prints_version() {
    let out = spavn(&["--version"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "got: {text}");
}

#[test]
fn binary_help_reports_the_configured_default_model() {
    let _guard = config_lock();
    let home = home();
    let mut cfg = load_user_config();
    cfg.default_model = "help-visible-default".to_string();
    save_user_config(&cfg).unwrap();
    assert!(home.join("config.json").exists());

    let out = spavn(&["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("help-visible-default"),
        "--help should show the saved default model, got: {text}"
    );
}

#[test]
fn last_selected_model_seeds_new_terminals_without_touching_default() {
    let _guard = config_lock();
    let _ = home();
    let mut cfg = load_user_config();
    cfg.default_model = "pinned-default".to_string();
    save_user_config(&cfg).unwrap();

    // A scratch path outside `sessions_dir()` must not touch the cross-terminal default.
    save_session_model(&std::env::temp_dir().join("scratch.jsonl"), "scratch-model");
    assert_ne!(
        spavn::config::load_last_model().as_deref(),
        Some("scratch-model")
    );

    save_session_model(&spavn::session::new_session_path(), "model-x");
    assert_eq!(spavn::config::load_last_model().as_deref(), Some("model-x"));
    assert_eq!(load_user_config().default_model, "pinned-default");
}

#[test]
fn concurrent_config_updates_do_not_lose_each_other() {
    let _guard = config_lock();
    let _ = home();
    let workers: Vec<_> = (0..8)
        .map(|n| {
            std::thread::spawn(move || {
                spavn::config::update_user_config(|c| {
                    c.language = format!("lang-{n}");
                    c.default_model = format!("model-{n}");
                })
                .unwrap();
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    // Whole-file consistency: language and model come from the same writer, file still parses.
    let cfg = load_user_config();
    let n = cfg.language.trim_start_matches("lang-");
    assert_eq!(cfg.default_model, format!("model-{n}"));
    assert!(!home().join("config.json.lock").exists(), "lock released");
}

#[cfg(unix)]
#[test]
fn session_files_and_dir_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = config_lock();
    let _ = home();
    let path = spavn::session::new_session_path();
    let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    let dir_mode = std::fs::metadata(spavn::session::sessions_dir())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600, "session file must be 0600");
    assert_eq!(dir_mode, 0o700, "sessions dir must be 0700");
    let _ = std::fs::remove_file(&path);

    spavn::config::upsert_spavn_secret("TEST_SECRET_KEY", "sk-test-value").unwrap();
    let secret_mode = std::fs::metadata(home().join("secrets.env"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(secret_mode, 0o600, "secrets.env must be 0600");
}
