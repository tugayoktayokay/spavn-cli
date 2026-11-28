//! Built-in real browser automation tool backed by local Chrome/Chromium headless + CDP.
//!
//! Zero extra Rust dependencies:
//! - Fast `open` / `text` and `screenshot` run Chrome `--headless=new` directly.
//! - Interactive `click`, `fill`, `selector`, and `eval_js` drive Chrome DevTools Protocol (CDP)
//!   over a short-lived loopback WebSocket using the host's `node` or `bun` runtime.
//! - Screenshots are attached straight to SPAVN's multimodal vision context (and temp files removed).

use super::*;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_WAIT_MS: u64 = 2500;
const MAX_WAIT_MS: u64 = 20_000;
const MAX_TEXT_CHARS: usize = 16_000;

/// Locate an installed Chromium/Chrome/Brave binary.
pub(crate) fn find_chrome_binary() -> Option<String> {
    if let Ok(env_bin) = std::env::var("SPAVN_CHROME_BIN") {
        let trimmed = env_bin.trim();
        if !trimmed.is_empty() && Path::new(trimmed).exists() {
            return Some(trimmed.to_string());
        }
    }
    const CANDIDATES: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ];
    for c in CANDIDATES {
        if Path::new(c).exists() {
            return Some((*c).to_string());
        }
    }
    for name in ["google-chrome", "chromium", "chromium-browser"] {
        if let Ok(out) = Command::new("which").arg(name).output() {
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !p.is_empty() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Validates a browser URL: allows `http://` / `https://` (including `localhost` / `127.0.0.1`
/// for local dev servers), while blocking `file://`, `javascript:`, cloud metadata (`169.254.169.254`),
/// and non-loopback private LAN targets unless explicitly permitted.
pub(crate) fn validate_browser_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(format!(
            "Browser error: only http:// and https:// URLs are allowed (got '{}').",
            trimmed
        ));
    }
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or("");
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if let Some(b) = host_port.strip_prefix('[') {
        b.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    let is_loopback = matches!(host, "localhost" | "127.0.0.1" | "::1");
    if !is_loopback && crate::guard::egress_is_blocked(trimmed) {
        return Err(format!(
            "Browser error: non-public address '{}' refused by egress policy.",
            trimmed
        ));
    }
    Ok(())
}

fn find_js_runtime() -> Option<&'static str> {
    for bin in ["node", "bun"] {
        if Command::new(bin)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(bin);
        }
    }
    None
}

const CDP_DRIVER_JS: &str = r#"
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

async function main() {
  const cfg = JSON.parse(fs.readFileSync(0, "utf8"));
  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "spavn-cdp-"));
  const chrome = spawn(cfg.chrome, [
    "--headless=new",
    "--disable-gpu",
    "--no-sandbox",
    "--disable-dev-shm-usage",
    "--remote-debugging-port=0",
    `--user-data-dir=${userDataDir}`,
    `--window-size=${cfg.width},${cfg.height}`,
    "about:blank",
  ], { stdio: ["ignore", "ignore", "pipe"] });

  let wsUrl = null;
  const deadline = Date.now() + 15000;
  await new Promise((resolve, reject) => {
    let buf = "";
    chrome.stderr.on("data", (chunk) => {
      buf += chunk.toString();
      const m = buf.match(/DevTools listening on (ws:\/\/[^\s]+)/);
      if (m) { wsUrl = m[1]; resolve(); }
    });
    chrome.on("exit", (code) => {
      if (!wsUrl) reject(new Error(`Chrome exited early (${code}): ${buf.slice(0, 300)}`));
    });
    const timer = setInterval(() => {
      if (Date.now() > deadline) { clearInterval(timer); reject(new Error("Timed out waiting for Chrome CDP")); }
    }, 200);
  });

  const port = new URL(wsUrl).port;
  const targetsRes = await fetch(`http://127.0.0.1:${port}/json/list`);
  const targets = await targetsRes.json();
  const pageTarget = targets.find((t) => t.type === "page") || targets[0];
  if (!pageTarget || !pageTarget.webSocketDebuggerUrl) {
    throw new Error("No page target available in Chrome CDP");
  }

  const ws = new WebSocket(pageTarget.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });

  let seq = 0;
  const pending = new Map();
  ws.onmessage = (ev) => {
    const msg = JSON.parse(typeof ev.data === "string" ? ev.data : ev.data.toString());
    if (msg.id && pending.has(msg.id)) {
      const { res, rej } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) rej(new Error(msg.error.message || JSON.stringify(msg.error)));
      else res(msg.result);
    }
  };
  const send = (method, params = {}) => new Promise((res, rej) => {
    const id = ++seq;
    pending.set(id, { res, rej });
    ws.send(JSON.stringify({ id, method, params }));
  });

  try {
    await send("Page.enable");
    await send("Runtime.enable");
    await send("Emulation.setDeviceMetricsOverride", {
      width: cfg.width, height: cfg.height, deviceScaleFactor: 1, mobile: false,
    });
    await send("Page.navigate", { url: cfg.url });
    await new Promise((r) => setTimeout(r, cfg.waitMs));

    if (cfg.fill && cfg.fill.selector) {
      const sel = JSON.stringify(cfg.fill.selector);
      const val = JSON.stringify(String(cfg.fill.value ?? ""));
      await send("Runtime.evaluate", {
        expression: `(() => { const el = document.querySelector(${sel}); if (!el) throw new Error('selector not found: ' + ${sel}); el.focus(); el.value = ${val}; el.dispatchEvent(new Event('input', {bubbles:true})); el.dispatchEvent(new Event('change', {bubbles:true})); return true; })()`,
        returnByValue: true,
      });
    }
    if (cfg.click) {
      const sel = JSON.stringify(cfg.click);
      await send("Runtime.evaluate", {
        expression: `(() => { const el = document.querySelector(${sel}); if (!el) throw new Error('selector not found: ' + ${sel}); el.click(); return true; })()`,
        returnByValue: true,
      });
      await new Promise((r) => setTimeout(r, Math.min(cfg.waitMs, 1500)));
    }

    const out = {};
    const meta = await send("Runtime.evaluate", {
      expression: `({ title: document.title, url: location.href })`,
      returnByValue: true,
    });
    out.title = meta?.result?.value?.title || "";
    out.url = meta?.result?.value?.url || cfg.url;

    if (cfg.op === "eval_js" && cfg.script) {
      const ev = await send("Runtime.evaluate", {
        expression: cfg.script,
        awaitPromise: true,
        returnByValue: true,
      });
      if (ev.exceptionDetails) {
        out.error = ev.exceptionDetails.text || JSON.stringify(ev.exceptionDetails);
      } else {
        out.result = ev?.result?.value;
      }
    } else if (cfg.op === "screenshot") {
      const shot = await send("Page.captureScreenshot", { format: "png" });
      out.screenshot_b64 = shot.data;
    } else {
      const selExpr = cfg.selector
        ? `(() => { const el = document.querySelector(${JSON.stringify(cfg.selector)}); return el ? el.innerText : ''; })()`
        : `(document.body ? document.body.innerText : document.documentElement.innerText || '')`;
      const txt = await send("Runtime.evaluate", { expression: selExpr, returnByValue: true });
      out.text = String(txt?.result?.value || "");
    }
    process.stdout.write(JSON.stringify(out));
  } finally {
    try { ws.close(); } catch {}
    try { chrome.kill("SIGKILL"); } catch {}
    try { fs.rmSync(userDataDir, { recursive: true, force: true }); } catch {}
  }
}

main().catch((e) => {
  process.stderr.write(String(e && e.stack ? e.stack : e));
  process.exit(1);
});
"#;

fn run_chrome_direct_screenshot(
    chrome: &str,
    url: &str,
    width: u32,
    height: u32,
    wait_ms: u64,
) -> Result<Vec<u8>, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("spavn_shot_{}.png", nonce));
    let out = Command::new(chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--hide-scrollbars",
            &format!("--window-size={},{}", width, height),
            &format!("--virtual-time-budget={}", wait_ms),
            &format!("--screenshot={}", tmp.display()),
            url,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to launch Chrome: {}", e))?;
    let bytes = fs::read(&tmp);
    let _ = fs::remove_file(&tmp);
    match bytes {
        Ok(b) if !b.is_empty() => Ok(b),
        _ => Err(format!(
            "Chrome screenshot failed (status {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(300)
                .collect::<String>()
        )),
    }
}

fn run_chrome_direct_dump_dom(chrome: &str, url: &str, wait_ms: u64) -> Result<String, String> {
    let out = Command::new(chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            &format!("--virtual-time-budget={}", wait_ms),
            "--dump-dom",
            url,
        ])
        .output()
        .map_err(|e| format!("Failed to launch Chrome: {}", e))?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(format!(
            "Chrome --dump-dom failed (status {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(300)
                .collect::<String>()
        ));
    }
    let html = String::from_utf8_lossy(&out.stdout);
    let text = super::web::strip_html_to_clean_text(&html);
    Ok(text.chars().take(MAX_TEXT_CHARS).collect())
}

pub(crate) fn exec_browser(ctx: &mut ToolContext, args: &Value) -> String {
    let op = get_str_alias(args, &["op", "action"])
        .trim()
        .to_ascii_lowercase();
    let op = if op.is_empty() { "open" } else { op.as_str() };
    let url = get_str_alias(args, &["url", "target"]).trim();
    if url.is_empty() {
        return "Browser error: missing required 'url' parameter.".to_string();
    }
    if let Err(e) = validate_browser_url(url) {
        return e;
    }
    let Some(chrome) = find_chrome_binary() else {
        return "Browser error: Chrome/Chromium not found. Install Google Chrome or set SPAVN_CHROME_BIN.".to_string();
    };
    let width = args
        .get("width")
        .and_then(|v| v.as_u64())
        .unwrap_or(1280)
        .clamp(320, 2560) as u32;
    let height = args
        .get("height")
        .and_then(|v| v.as_u64())
        .unwrap_or(900)
        .clamp(320, 2560) as u32;
    let wait_ms = args
        .get("wait_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_WAIT_MS)
        .clamp(200, MAX_WAIT_MS);
    let selector = args
        .get("selector")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let click = args
        .get("click")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let fill = args.get("fill").filter(|v| v.is_object());
    let script = args
        .get("script")
        .or_else(|| args.get("code"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if ctx.access == AccessMode::Safe
        && !prompt_user_approval(ctx, &format!("Browser ({})", op), url)
    {
        return "User rejected browser call in SAFE mode.".to_string();
    }

    let needs_cdp = op == "eval_js" || click.is_some() || fill.is_some() || selector.is_some();

    if !needs_cdp {
        if op == "screenshot" {
            return match run_chrome_direct_screenshot(&chrome, url, width, height, wait_ms) {
                Ok(bytes) => {
                    let kb = (bytes.len() as f64 / 1024.0).max(0.1);
                    let b64 = crate::router::base64_encode_bytes(&bytes);
                    let drag_img = crate::router::DraggedImage {
                        raw_span: url.to_string(),
                        path: url.to_string(),
                        filename: "browser-screenshot.png".to_string(),
                        mime: "image/png",
                        size_bytes: bytes.len(),
                        base64: b64,
                    };
                    crate::hud::render_inline_image_preview(&drag_img);
                    crate::router::register_read_image(url, drag_img);
                    format!(
                        "[SPAVN Vision Image Loaded: {} (image/png, {:.1} KB, {}x{}) — browser screenshot attached to your visual context]",
                        url, kb, width, height
                    )
                }
                Err(e) => e,
            };
        }
        return match run_chrome_direct_dump_dom(&chrome, url, wait_ms) {
            Ok(text) => format!("URL: {}\n\n{}", url, text),
            Err(e) => e,
        };
    }

    let Some(js_bin) = find_js_runtime() else {
        return "Browser error: interactive CDP mode requires `node` or `bun` in PATH.".to_string();
    };
    let payload = json!({
        "chrome": chrome,
        "url": url,
        "op": op,
        "width": width,
        "height": height,
        "waitMs": wait_ms,
        "selector": selector,
        "click": click,
        "fill": fill,
        "script": script,
    })
    .to_string();

    let mut child = match Command::new(js_bin)
        .args(["-e", CDP_DRIVER_JS])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("Browser CDP spawn error: {}", e),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return format!("Browser CDP wait error: {}", e),
    };
    if !output.status.success() {
        return format!(
            "Browser CDP failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(400)
                .collect::<String>()
        );
    }
    let parsed: Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(e) => return format!("Browser CDP JSON error: {}", e),
    };
    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        return format!("Browser JS error: {}", err);
    }
    let title = parsed.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let final_url = parsed.get("url").and_then(|v| v.as_str()).unwrap_or(url);
    if let Some(b64) = parsed.get("screenshot_b64").and_then(|v| v.as_str()) {
        let approx_kb = ((b64.len() * 3 / 4) as f64 / 1024.0).max(0.1);
        let drag_img = crate::router::DraggedImage {
            raw_span: final_url.to_string(),
            path: final_url.to_string(),
            filename: "browser-screenshot.png".to_string(),
            mime: "image/png",
            size_bytes: b64.len() * 3 / 4,
            base64: b64.to_string(),
        };
        crate::hud::render_inline_image_preview(&drag_img);
        crate::router::register_read_image(final_url, drag_img);
        return format!(
            "[SPAVN Vision Image Loaded: {} (image/png, ~{:.1} KB, {}x{}, title: '{}') — browser screenshot attached to your visual context]",
            final_url, approx_kb, width, height, title
        );
    }
    if op == "eval_js" {
        let res = parsed.get("result").cloned().unwrap_or(Value::Null);
        return format!(
            "Title: {}\nURL: {}\nResult:\n{}",
            title,
            final_url,
            serde_json::to_string_pretty(&res)
                .unwrap_or_else(|_| res.to_string())
                .chars()
                .take(MAX_TEXT_CHARS)
                .collect::<String>()
        );
    }
    let text: String = parsed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(MAX_TEXT_CHARS)
        .collect();
    format!("Title: {}\nURL: {}\n\n{}", title, final_url, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_url_validation_allows_public_and_localhost_and_blocks_metadata_and_file() {
        assert!(validate_browser_url("https://example.com").is_ok());
        assert!(validate_browser_url("http://localhost:3000/app").is_ok());
        assert!(validate_browser_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_browser_url("file:///etc/passwd").is_err());
        assert!(validate_browser_url("http://169.254.169.254/latest/meta-data/").is_err());
    }
}
