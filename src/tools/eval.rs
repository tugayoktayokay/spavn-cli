//! Persistent stateful Python (`py`) and JavaScript (`js`) REPL kernels.
//!
//! Each language keeps one long-lived child process per SPAVN process so variables, imports,
//! and helper definitions survive across tool calls (until `reset: true` or process exit).
//! Communication uses newline-delimited JSON over `stdin` / `stdout` while user `print()` /
//! `console.log()` output is captured in-memory so the wire protocol never corrupts.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;
const MAX_OUTPUT_CHARS: usize = 16_000;

const PY_KERNEL_DRIVER: &str = r#"
import ast, io, json, sys, traceback

_real_out = sys.stdout
_real_in = sys.stdin
_ns = {"__name__": "__main__"}

for _raw in _real_in:
    _raw = _raw.strip()
    if not _raw:
        continue
    try:
        _req = json.loads(_raw)
        _code = _req.get("code", "")
        _buf_out = io.StringIO()
        _buf_err = io.StringIO()
        _res_repr = None
        _err = None
        _ok = True
        sys.stdout = _buf_out
        sys.stderr = _buf_err
        try:
            _tree = ast.parse(_code, filename="<eval>", mode="exec")
            if _tree.body and isinstance(_tree.body[-1], ast.Expr):
                _last = _tree.body.pop()
                if _tree.body:
                    exec(compile(_tree, "<eval>", "exec"), _ns)
                _val = eval(compile(ast.Expression(_last.value), "<eval>", "eval"), _ns)
                if _val is not None:
                    _res_repr = repr(_val)
            else:
                exec(compile(_tree, "<eval>", "exec"), _ns)
        except Exception:
            _ok = False
            _err = traceback.format_exc()
        finally:
            sys.stdout = _real_out
            sys.stderr = sys.__stderr__
        _resp = {
            "ok": _ok,
            "stdout": _buf_out.getvalue(),
            "stderr": _buf_err.getvalue(),
            "result": _res_repr,
            "error": _err,
        }
    except Exception as _e:
        _resp = {"ok": False, "stdout": "", "stderr": "", "result": None, "error": str(_e)}
    _real_out.write(json.dumps(_resp, ensure_ascii=False) + "\n")
    _real_out.flush()
"#;

const JS_KERNEL_DRIVER: &str = r#"
const vm = require("node:vm");
const readline = require("node:readline");
const util = require("node:util");

const realOut = process.stdout;
let curOut = [];
let curErr = [];

const fmtArgs = (args) =>
  args.map((a) => (typeof a === "string" ? a : util.inspect(a, { depth: 4, colors: false }))).join(" ");

const sandbox = {
  console: {
    log: (...a) => curOut.push(fmtArgs(a)),
    info: (...a) => curOut.push(fmtArgs(a)),
    warn: (...a) => curErr.push(fmtArgs(a)),
    error: (...a) => curErr.push(fmtArgs(a)),
  },
  require,
  process,
  Buffer,
  URL,
  URLSearchParams,
  fetch: typeof fetch !== "undefined" ? fetch : undefined,
  setTimeout,
  clearTimeout,
  setInterval,
  clearInterval,
};
sandbox.globalThis = sandbox;
const ctx = vm.createContext(sandbox);

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
rl.on("line", async (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  curOut = [];
  curErr = [];
  let ok = true;
  let result = null;
  let error = null;
  try {
    const req = JSON.parse(trimmed);
    const code = String(req.code || "");
    const val = await vm.runInContext(code, ctx, { filename: "<eval>" });
    if (val !== undefined) {
      result = typeof val === "string" ? val : util.inspect(val, { depth: 4, colors: false });
    }
  } catch (e) {
    ok = false;
    error = (e && e.stack) ? String(e.stack) : String(e);
  }
  const resp = {
    ok,
    stdout: curOut.join("\n"),
    stderr: curErr.join("\n"),
    result,
    error,
  };
  realOut.write(JSON.stringify(resp) + "\n");
});
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvalLang {
    Py,
    Js,
}

impl EvalLang {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "py" | "python" | "python3" => Some(Self::Py),
            "js" | "javascript" | "node" | "bun" => Some(Self::Js),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Py => "py",
            Self::Js => "js",
        }
    }
}

struct KernelProcess {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Option<String>>,
    cwd: PathBuf,
}

impl KernelProcess {
    fn spawn(lang: EvalLang, cwd: &Path) -> Result<Self, String> {
        let mut cmd = match lang {
            EvalLang::Py => {
                let mut c = Command::new("python3");
                c.args(["-u", "-c", PY_KERNEL_DRIVER]);
                c
            }
            EvalLang::Js => {
                let runtime = if Command::new("node")
                    .arg("--version")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
                {
                    "node"
                } else {
                    "bun"
                };
                let mut c = Command::new(runtime);
                c.args(["-e", JS_KERNEL_DRIVER]);
                c
            }
        };
        cmd.current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to start {} kernel: {}", lang.label(), e))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to attach kernel stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to attach kernel stdout".to_string())?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(None);
                        break;
                    }
                    Ok(_) => {
                        if tx.send(Some(line.clone())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            rx,
            cwd: cwd.to_path_buf(),
        })
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

static PY_KERNEL: Mutex<Option<KernelProcess>> = Mutex::new(None);
static JS_KERNEL: Mutex<Option<KernelProcess>> = Mutex::new(None);

fn kernel_slot(lang: EvalLang) -> &'static Mutex<Option<KernelProcess>> {
    match lang {
        EvalLang::Py => &PY_KERNEL,
        EvalLang::Js => &JS_KERNEL,
    }
}

fn clip_output(s: &str) -> String {
    if s.chars().count() <= MAX_OUTPUT_CHARS {
        s.to_string()
    } else {
        let head: String = s.chars().take(MAX_OUTPUT_CHARS).collect();
        format!(
            "{}\n[...truncated {} chars]",
            head,
            s.chars().count() - MAX_OUTPUT_CHARS
        )
    }
}

pub(crate) fn exec_eval(ctx: &mut ToolContext, args: &Value) -> String {
    let raw_lang = get_str_alias(args, &["language", "lang"]);
    let lang = match EvalLang::parse(raw_lang) {
        Some(l) => l,
        None => {
            return format!(
                "Eval error: unsupported language '{}'. Use 'py' or 'js'.",
                raw_lang
            );
        }
    };
    let code = get_str_alias(args, &["code", "script", "source"]);
    let reset = args.get("reset").and_then(|v| v.as_bool()).unwrap_or(false);
    let timeout_secs = args
        .get("timeout_sec")
        .or_else(|| args.get("timeout"))
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);

    if ctx.access == AccessMode::Safe
        && !code.trim().is_empty()
        && !prompt_user_approval(
            ctx,
            &format!("Persistent Eval ({})", lang.label()),
            &code.chars().take(300).collect::<String>(),
        )
    {
        return "User rejected eval in SAFE mode.".to_string();
    }

    let slot = kernel_slot(lang);
    let mut guard = match slot.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    let mut notice = String::new();
    if reset {
        if let Some(old) = guard.take() {
            old.kill();
        }
        if code.trim().is_empty() {
            return format!("[eval:{}] Kernel reset.", lang.label());
        }
        notice.push_str("[kernel reset] ");
    }

    if code.trim().is_empty() {
        return "Eval error: missing 'code' parameter (or pass `reset: true`).".to_string();
    }

    let target_cwd = ctx.last_cwd.clone();
    let needs_spawn = if let Some(k) = guard.as_mut() {
        if k.is_alive() && k.cwd == target_cwd {
            false
        } else {
            if let Some(old) = guard.take() {
                old.kill();
            }
            if !reset {
                notice.push_str("[kernel restarted] ");
            }
            true
        }
    } else {
        true
    };
    if needs_spawn {
        match KernelProcess::spawn(lang, &target_cwd) {
            Ok(k) => *guard = Some(k),
            Err(e) => return format!("Eval error: {}", e),
        }
    }

    let payload = format!("{}\n", json!({ "code": code }));
    let write_res = {
        let k = guard.as_mut().unwrap();
        k.stdin
            .write_all(payload.as_bytes())
            .and_then(|_| k.stdin.flush())
    };
    if let Err(e) = write_res {
        if let Some(old) = guard.take() {
            old.kill();
        }
        return format!("Eval error: failed writing to {} kernel: {}", lang.label(), e);
    }

    let recv = {
        let k = guard.as_ref().unwrap();
        k.rx.recv_timeout(Duration::from_secs(timeout_secs))
    };

    match recv {
        Ok(Some(line)) => {
            let parsed: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => return format!("Eval protocol error: {} (raw: {})", e, clip_output(&line)),
            };
            let ok = parsed.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let stdout = parsed.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let stderr = parsed.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
            let result = parsed.get("result").and_then(|v| v.as_str());
            let error = parsed.get("error").and_then(|v| v.as_str());

            let mut out = String::new();
            if !notice.is_empty() {
                out.push_str(&notice);
                out.push('\n');
            }
            if !stdout.is_empty() {
                out.push_str(stdout.trim_end_matches('\n'));
                out.push('\n');
            }
            if !stderr.is_empty() {
                out.push_str("[stderr]\n");
                out.push_str(stderr.trim_end_matches('\n'));
                out.push('\n');
            }
            if let Some(r) = result {
                if !r.is_empty() {
                    out.push_str("=> ");
                    out.push_str(r);
                    out.push('\n');
                }
            }
            if !ok {
                if let Some(err) = error {
                    out.push_str("[error]\n");
                    out.push_str(err.trim_end_matches('\n'));
                    out.push('\n');
                }
            }
            let trimmed = out.trim();
            if trimmed.is_empty() {
                format!("[eval:{}] (ok, no output)", lang.label())
            } else {
                clip_output(trimmed)
            }
        }
        Ok(None) => {
            if let Some(old) = guard.take() {
                old.kill();
            }
            format!(
                "Eval error: {} kernel exited unexpectedly (state lost; next call starts fresh).",
                lang.label()
            )
        }
        Err(_) => {
            if let Some(old) = guard.take() {
                old.kill();
            }
            format!(
                "Eval timeout after {}s: {} kernel killed (state lost; next call starts fresh).",
                timeout_secs,
                lang.label()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::spavn_engine::AgentPhase;
    use super::*;

    #[test]
    fn python_kernel_persists_state_and_resets() {
        let dir = std::env::temp_dir();
        let mut ctx = ToolContext::new(
            AccessMode::Auto,
            "local".to_string(),
            dir,
            AgentPhase::Code,
        );
        let r1 = exec_eval(
            &mut ctx,
            &json!({ "language": "py", "code": "x_spavn_test = 21\nx_spavn_test * 2", "reset": true }),
        );
        assert!(r1.contains("=> 42"), "r1={r1}");

        let r2 = exec_eval(
            &mut ctx,
            &json!({ "language": "py", "code": "x_spavn_test + 8" }),
        );
        assert!(r2.contains("=> 29"), "r2={r2}");

        let r3 = exec_eval(
            &mut ctx,
            &json!({ "language": "py", "code": "x_spavn_test", "reset": true }),
        );
        assert!(r3.contains("NameError"), "r3={r3}");
    }

    #[test]
    fn js_kernel_persists_state_across_cells() {
        let dir = std::env::temp_dir();
        let mut ctx = ToolContext::new(
            AccessMode::Auto,
            "local".to_string(),
            dir,
            AgentPhase::Code,
        );
        let r1 = exec_eval(
            &mut ctx,
            &json!({ "language": "js", "code": "var spavnVal = 10; spavnVal * 3", "reset": true }),
        );
        assert!(r1.contains("=> 30"), "r1={r1}");

        let r2 = exec_eval(
            &mut ctx,
            &json!({ "language": "js", "code": "console.log('hi'); spavnVal + 5" }),
        );
        assert!(r2.contains("hi") && r2.contains("=> 15"), "r2={r2}");
    }
}
