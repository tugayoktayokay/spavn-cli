//! `SPAVN_LANG` is process-wide state, so this lives in its own integration-test process instead
//! of racing the parallel unit tests.

use spavn::config::detect_os_lang_en;

#[test]
fn detect_honours_spavn_lang() {
    std::env::set_var("SPAVN_LANG", "en");
    assert!(detect_os_lang_en(), "SPAVN_LANG=en must yield EN");
    std::env::set_var("SPAVN_LANG", "tr");
    assert!(!detect_os_lang_en());
    std::env::remove_var("SPAVN_LANG");
}
