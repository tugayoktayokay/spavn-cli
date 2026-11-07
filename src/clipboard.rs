//! Zero-dependency clipboard write via the platform's CLI tool (`pbcopy`,
//! `wl-copy`, `xclip`, `xsel`, `clip`). Used by the `/copy` slash command to
//! copy regions of the last assistant turn into the system clipboard.

use std::io::Write;
use std::process::{Command, Stdio};

fn candidates() -> Vec<(&'static str, Vec<&'static str>)> {
    #[cfg(target_os = "macos")]
    {
        vec![("pbcopy", vec![])]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec![
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ]
    }
    #[cfg(windows)]
    {
        vec![("clip", vec![])]
    }
    #[cfg(not(any(unix, windows)))]
    {
        Vec::new()
    }
}

/// Writes `text` to the system clipboard. Returns an error only when no
/// clipboard tool is available or every candidate failed.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    for (cmd, args) in candidates() {
        let mut child = match Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(mut stdin) = child.stdin.take() {
            if stdin.write_all(text.as_bytes()).is_err() {
                let _ = child.kill();
                continue;
            }
        }
        drop(child.stdin.take());
        match child.wait() {
            Ok(status) if status.success() => return Ok(()),
            _ => continue,
        }
    }
    Err(crate::config::tr_en(
        "panoya kopyalama aracı bulunamadı (pbcopy/wl-copy/xclip/xsel)",
        "no clipboard tool found (pbcopy/wl-copy/xclip/xsel)",
    )
    .to_string())
}
