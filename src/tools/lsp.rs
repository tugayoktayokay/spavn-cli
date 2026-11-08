use super::*;
use crate::config::{home_dir, load_user_config};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Encodes an LSP JSON-RPC message into Content-Length framed byte payload.
pub fn encode_lsp_message(val: &Value) -> Result<Vec<u8>, serde_json::Error> {
    let body = serde_json::to_vec(val)?;
    let mut msg = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    msg.extend_from_slice(&body);
    Ok(msg)
}

/// Decodes one complete LSP JSON-RPC message from a mutable byte buffer.
///
/// If a complete message is available, its payload is parsed as JSON, the consumed bytes
/// are removed from `buf`, and `Ok(Some(val))` is returned.
/// If `buf` contains partial data, `Ok(None)` is returned without mutating `buf`.
pub fn decode_lsp_message(buf: &mut Vec<u8>) -> Result<Option<Value>, String> {
    let (header_end, body_start) = if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
    {
        (pos, pos + 4)
    } else if let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
        (pos, pos + 2)
    } else {
        return Ok(None);
    };

    let header_str = match std::str::from_utf8(&buf[..header_end]) {
        Ok(s) => s,
        Err(e) => return Err(format!("Invalid UTF-8 in LSP headers: {}", e)),
    };

    let mut content_len: Option<usize> = None;
    for line in header_str.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            if let Ok(len) = rest.trim().parse::<usize>() {
                content_len = Some(len);
                break;
            }
        }
    }

    let len = match content_len {
        Some(l) => l,
        None => return Err("Missing Content-Length header in LSP message".to_string()),
    };

    if buf.len() < body_start + len {
        return Ok(None);
    }

    let body_bytes = &buf[body_start..body_start + len];
    let val: Value = match serde_json::from_slice(body_bytes) {
        Ok(v) => v,
        Err(e) => return Err(format!("Invalid JSON in LSP payload: {}", e)),
    };

    buf.drain(..body_start + len);
    Ok(Some(val))
}

/// Normalizes a filesystem path:
/// 1. Makes it absolute using `std::env::current_dir()` if relative.
/// 2. If the path or its parent exists on disk, uses `fs::canonicalize` to resolve symlinks and all `.` / `..`.
/// 3. Lexically eliminates any remaining `.` and `..` components.
pub fn normalize_path(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };

    // First lexical pass to eliminate `.` and `..`
    let mut lexical = Vec::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            c => lexical.push(c),
        }
    }
    let lex_path: PathBuf = lexical.into_iter().collect();

    if let Ok(canon) = fs::canonicalize(&lex_path) {
        return canon;
    }

    // Walk up until an ancestor exists on disk and canonicalize it
    let mut cur = lex_path.as_path();
    let mut suffix = Vec::new();
    while let Some(parent) = cur.parent() {
        if let Some(file_name) = cur.file_name() {
            suffix.push(file_name);
        }
        if let Ok(canon_parent) = fs::canonicalize(parent) {
            let mut result = canon_parent;
            for part in suffix.into_iter().rev() {
                result.push(part);
            }
            return result;
        }
        cur = parent;
    }

    lex_path
}

/// Converts an RFC-compliant `file://` URI to a normalized local filesystem path.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let stripped = uri.strip_prefix("file://")?;
    let path_str = stripped.strip_prefix("localhost").unwrap_or(stripped);
    let bytes = path_str.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(val) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                decoded.push(val);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    let s = String::from_utf8(decoded).ok()?;
    Some(normalize_path(Path::new(&s)))
}

/// Converts a local filesystem path to an RFC-compliant `file://` URI after path normalization.
pub fn path_to_uri(path: &Path) -> String {
    let abs_path = normalize_path(path);
    let s = abs_path.to_string_lossy();
    let mut encoded = String::from("file://");
    for b in s.as_bytes() {
        match *b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(*b as char);
            }
            byte => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{:02X}", byte);
            }
        }
    }
    encoded
}

/// Normalizes an RFC-compliant `file://` URI by parsing to path, canonicalizing, and re-encoding.
pub fn normalize_uri(uri: &str) -> String {
    if let Some(path) = uri_to_path(uri) {
        path_to_uri(&path)
    } else {
        uri.to_string()
    }
}

/// Resolves a 0-based UTF-16 character offset for the first occurrence of `symbol` on `line_text`.
pub fn resolve_symbol_column(line_text: &str, symbol: &str) -> Option<u32> {
    if symbol.is_empty() {
        return None;
    }
    let byte_idx = line_text.find(symbol)?;
    let utf16_col = line_text[..byte_idx].encode_utf16().count() as u32;
    Some(utf16_col)
}

/// Resolves 0-based (line, character) position for LSP requests.
///
/// Accepts 1-based `line_1based`, optional 1-based `col_1based`, and `symbol`.
/// If column is missing, finds the first occurrence of `symbol` on that line.
/// If line is 0 and symbol is given, searches the entire file for the first match.
pub fn resolve_line_and_col(
    content: &str,
    line_1based: u32,
    col_1based: Option<u32>,
    symbol: &str,
) -> (u32, u32) {
    if line_1based > 0 {
        let lsp_line = line_1based - 1;
        if let Some(col) = col_1based {
            if col > 0 {
                return (lsp_line, col - 1);
            }
        }
        if !symbol.is_empty() {
            if let Some(line_str) = content.lines().nth(lsp_line as usize) {
                if let Some(utf16_col) = resolve_symbol_column(line_str, symbol) {
                    return (lsp_line, utf16_col);
                }
            }
            // Search nearby lines (+/- 5) in case of line shifts
            let total = content.lines().count();
            let start = lsp_line.saturating_sub(5) as usize;
            let end = (lsp_line as usize + 6).min(total);
            for (offset, line_str) in content.lines().enumerate().skip(start).take(end - start) {
                if let Some(utf16_col) = resolve_symbol_column(line_str, symbol) {
                    return (offset as u32, utf16_col);
                }
            }
        }
        return (lsp_line, 0);
    }

    if !symbol.is_empty() {
        for (idx, line_str) in content.lines().enumerate() {
            if let Some(utf16_col) = resolve_symbol_column(line_str, symbol) {
                return (idx as u32, utf16_col);
            }
        }
    }

    (0, 0)
}

/// Maps LSP SymbolKind integer to a readable string.
pub fn symbol_kind_name(kind: u64) -> &'static str {
    match kind {
        1 => "File",
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        15 => "String",
        16 => "Number",
        17 => "Boolean",
        18 => "Array",
        19 => "Object",
        20 => "Key",
        21 => "Null",
        22 => "EnumMember",
        23 => "Struct",
        24 => "Event",
        25 => "Operator",
        26 => "TypeParameter",
        _ => "Symbol",
    }
}

/// Specification for launching a language server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub command: String,
    pub args: Vec<String>,
}

/// Returns the configured or default language server command for a given file extension.
pub fn get_server_spec_for_ext(ext: &str) -> Option<ServerSpec> {
    let cfg = load_user_config();
    if let Some(user_cmd) = cfg.lsp_servers.get(ext) {
        if !user_cmd.is_empty() {
            return Some(ServerSpec {
                command: user_cmd[0].clone(),
                args: user_cmd[1..].to_vec(),
            });
        }
    }

    match ext {
        "rs" => Some(ServerSpec {
            command: "rust-analyzer".to_string(),
            args: Vec::new(),
        }),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(ServerSpec {
            command: "typescript-language-server".to_string(),
            args: vec!["--stdio".to_string()],
        }),
        "py" => Some(ServerSpec {
            command: "pyright-langserver".to_string(),
            args: vec!["--stdio".to_string()],
        }),
        "go" => Some(ServerSpec {
            command: "gopls".to_string(),
            args: Vec::new(),
        }),
        "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" => Some(ServerSpec {
            command: "clangd".to_string(),
            args: Vec::new(),
        }),
        _ => None,
    }
}

/// Infers the default programming language extension from project root marker files.
pub fn detect_extension_from_project(root: &Path) -> Option<&'static str> {
    if root.join("Cargo.toml").exists() {
        return Some("rs");
    }
    if root.join("tsconfig.json").exists() || root.join("package.json").exists() {
        return Some("ts");
    }
    if root.join("pyproject.toml").exists()
        || root.join("requirements.txt").exists()
        || root.join("setup.py").exists()
    {
        return Some("py");
    }
    if root.join("go.mod").exists() || root.join("go.work").exists() {
        return Some("go");
    }
    if root.join("CMakeLists.txt").exists() || root.join("compile_commands.json").exists() {
        return Some("cpp");
    }
    None
}

/// Discovers the workspace root directory by walking up from `start_path` looking for project markers.
pub fn detect_workspace_root(start_path: &Path) -> PathBuf {
    let norm = normalize_path(start_path);
    let mut cur = if norm.is_file() {
        norm.parent().unwrap_or(&norm).to_path_buf()
    } else {
        norm.clone()
    };

    for _ in 0..14 {
        if cur.join("Cargo.toml").is_file()
            || cur.join("tsconfig.json").is_file()
            || cur.join("package.json").is_file()
            || cur.join("pyproject.toml").is_file()
            || cur.join("go.mod").is_file()
            || cur.join("CMakeLists.txt").is_file()
            || cur.join(".git").exists()
        {
            return cur;
        }
        if let Some(parent) = cur.parent() {
            if parent == cur {
                break;
            }
            cur = parent.to_path_buf();
        } else {
            break;
        }
    }
    norm
}

/// Resolves executable binary on PATH or standard developer directories.
pub fn resolve_binary(bin: &str) -> Option<PathBuf> {
    let p = Path::new(bin);
    if p.is_absolute() && p.is_file() {
        return Some(p.to_path_buf());
    }
    if bin.contains('/') || bin.contains('\\') {
        if let Ok(can) = fs::canonicalize(p) {
            if can.is_file() {
                return Some(can);
            }
        }
    }
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(bin);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let home = home_dir();
    let common_dirs = [
        home.join(".cargo").join("bin"),
        home.join(".local").join("bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];
    for dir in &common_dirs {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Maps file extension or path to LSP languageId identifier.
fn language_id_for_path(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        _ => "plaintext",
    }
}

/// In-memory state of an opened file in an LSP session.
struct FileState {
    version: i64,
    mtime: Option<SystemTime>,
}

/// In-flight request id → channel awaiting its response.
type PendingMap = Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value, String>>>>>;

/// Persistent LSP client managing a running server child process.
pub struct LspClient {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicI64,
    pending: PendingMap,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    diag_notify: Arc<(Mutex<bool>, Condvar)>,
    open_files: Mutex<HashMap<String, FileState>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    active_progress: Arc<Mutex<HashSet<String>>>,
    pub workspace_root: PathBuf,
    pub server_cmd: String,
}

impl LspClient {
    /// Spawns a language server process, establishes stdio framing reader, and executes initialize handshake.
    pub fn spawn(
        server_bin: &Path,
        args: &[String],
        workspace_root: &Path,
    ) -> Result<Arc<Self>, String> {
        let mut cmd = Command::new(server_bin);
        cmd.args(args);
        cmd.current_dir(workspace_root);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn {}: {}", server_bin.display(), e))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture LSP stdout".to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to open LSP stdin".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Failed to open LSP stderr".to_string())?;

        let stdin_arc = Arc::new(Mutex::new(stdin));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics = Arc::new(Mutex::new(HashMap::new()));
        let diag_notify = Arc::new((Mutex::new(false), Condvar::new()));
        let stderr_tail = Arc::new(Mutex::new(Vec::with_capacity(4096)));
        let active_progress = Arc::new(Mutex::new(HashSet::new()));

        let stderr_tail_clone = Arc::clone(&stderr_tail);
        let _stderr_reader = thread::Builder::new()
            .name("spavn-lsp-stderr".to_string())
            .spawn(move || {
                let mut chunk = [0u8; 1024];
                let mut reader = stderr;
                while let Ok(n) = reader.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut buf) = stderr_tail_clone.lock() {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > 4096 {
                            let drain_len = buf.len() - 4096;
                            buf.drain(..drain_len);
                        }
                    }
                }
            });

        let pending_clone = Arc::clone(&pending);
        let diagnostics_clone = Arc::clone(&diagnostics);
        let diag_notify_clone = Arc::clone(&diag_notify);
        let stdin_clone = Arc::clone(&stdin_arc);
        let stderr_tail_for_reader = Arc::clone(&stderr_tail);
        let active_progress_clone = Arc::clone(&active_progress);

        let _reader = thread::Builder::new()
            .name("spavn-lsp-reader".to_string())
            .spawn(move || {
                lsp_reader_loop(
                    stdout,
                    pending_clone,
                    diagnostics_clone,
                    diag_notify_clone,
                    stdin_clone,
                    stderr_tail_for_reader,
                    active_progress_clone,
                );
            });

        let client = Arc::new(Self {
            child: Mutex::new(child),
            stdin: stdin_arc,
            next_id: AtomicI64::new(1),
            pending,
            diagnostics,
            diag_notify,
            open_files: Mutex::new(HashMap::new()),
            stderr_tail,
            active_progress,
            workspace_root: workspace_root.to_path_buf(),
            server_cmd: server_bin.to_string_lossy().to_string(),
        });

        client.initialize()?;
        Ok(client)
    }

    /// Returns the most recent stderr output from the server process.
    pub fn get_stderr_tail(&self) -> String {
        self.stderr_tail
            .lock()
            .ok()
            .and_then(|b| String::from_utf8(b.clone()).ok())
            .unwrap_or_default()
    }

    /// Returns true if the language server has reported active workDoneProgress tokens (e.g. indexing).
    pub fn is_indexing(&self) -> bool {
        self.active_progress
            .lock()
            .map(|set| !set.is_empty())
            .unwrap_or(false)
    }

    /// Executes the LSP initialize handshake.
    fn initialize(&self) -> Result<(), String> {
        let root_uri = path_to_uri(&self.workspace_root);
        let folder_name = self
            .workspace_root
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "workspace".to_string());

        let init_params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "workspaceFolders": [
                {
                    "uri": root_uri,
                    "name": folder_name
                }
            ],
            "capabilities": {
                "textDocument": {
                    "synchronization": {
                        "dynamicRegistration": false,
                        "willSave": false,
                        "willSaveWaitUntil": false,
                        "didSave": false
                    },
                    "hover": {
                        "contentFormat": ["markdown", "plaintext"]
                    },
                    "definition": {
                        "dynamicRegistration": false,
                        "linkSupport": true
                    },
                    "references": {
                        "dynamicRegistration": false
                    },
                    "documentSymbol": {
                        "dynamicRegistration": false,
                        "hierarchicalDocumentSymbolSupport": true
                    },
                    "implementation": {
                        "dynamicRegistration": false,
                        "linkSupport": true
                    },
                    "typeDefinition": {
                        "dynamicRegistration": false,
                        "linkSupport": true
                    },
                    "publishDiagnostics": {
                        "relatedInformation": true
                    }
                },
                // Without this servers never send `$/progress`, so `is_indexing()` could not
                // tell a still-indexing server from a settled empty answer.
                "window": {
                    "workDoneProgress": true
                },
                "workspace": {
                    "symbol": {
                        "dynamicRegistration": false
                    },
                    "configuration": true,
                    "workspaceFolders": true
                }
            }
        });

        // Cold start (e.g. rust-analyzer loading Cargo metadata) can take up to 45s.
        if let Err(e) = self.send_request("initialize", init_params, Duration::from_secs(45)) {
            let tail = self.get_stderr_tail();
            let err_detail = format_server_exit_error(&tail);
            return Err(format!("{}: {}", e, err_detail));
        }
        self.send_notification("initialized", json!({}))?;
        Ok(())
    }

    /// Sends a JSON-RPC request and blocks until a response is received or `timeout` expires.
    pub fn send_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| "LSP pending lock poisoned")?;
            pending.insert(id, tx);
        }

        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let msg_bytes =
            encode_lsp_message(&req).map_err(|e| format!("Failed to encode LSP request: {}", e))?;

        {
            let mut stdin = self.stdin.lock().map_err(|_| "LSP stdin lock poisoned")?;
            stdin
                .write_all(&msg_bytes)
                .map_err(|e| format!("Failed to write to LSP stdin: {}", e))?;
            stdin
                .flush()
                .map_err(|e| format!("Failed to flush LSP stdin: {}", e))?;
        }

        match rx.recv_timeout(timeout) {
            Ok(res) => res,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&id);
                }
                Err(format!(
                    "LSP request '{}' timed out after {:?}",
                    method, timeout
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&id);
                }
                Err(format!(
                    "LSP server disconnected during request '{}'",
                    method
                ))
            }
        }
    }

    /// Sends a JSON-RPC notification (no id, no response expected).
    pub fn send_notification(&self, method: &str, params: Value) -> Result<(), String> {
        let notif = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        let msg_bytes = encode_lsp_message(&notif)
            .map_err(|e| format!("Failed to encode LSP notification: {}", e))?;

        let mut stdin = self.stdin.lock().map_err(|_| "LSP stdin lock poisoned")?;
        stdin
            .write_all(&msg_bytes)
            .map_err(|e| format!("Failed to write to LSP stdin: {}", e))?;
        stdin
            .flush()
            .map_err(|e| format!("Failed to flush LSP stdin: {}", e))?;
        Ok(())
    }

    /// Ensures the file is opened via `textDocument/didOpen`, or sends `textDocument/didChange` if modified.
    pub fn ensure_document_open(&self, uri: &str, file_path: &Path) -> Result<(), String> {
        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read {}: {}", file_path.display(), e))?;
        let mtime = fs::metadata(file_path).and_then(|m| m.modified()).ok();

        let mut open_files = self
            .open_files
            .lock()
            .map_err(|_| "open_files lock poisoned")?;
        if let Some(state) = open_files.get_mut(uri) {
            if state.mtime != mtime {
                state.version += 1;
                state.mtime = mtime;
                let version = state.version;
                drop(open_files);

                self.send_notification(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "version": version
                        },
                        "contentChanges": [
                            {
                                "text": content
                            }
                        ]
                    }),
                )?;
            }
        } else {
            let lang_id = language_id_for_path(file_path);
            open_files.insert(uri.to_string(), FileState { version: 1, mtime });
            drop(open_files);

            self.send_notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": lang_id,
                        "version": 1,
                        "text": content
                    }
                }),
            )?;
        }
        Ok(())
    }

    /// Checks if the underlying server child process is still running.
    pub fn is_alive(&self) -> bool {
        if let Ok(mut child) = self.child.lock() {
            matches!(child.try_wait(), Ok(None))
        } else {
            false
        }
    }

    /// Gracefully sends shutdown and exit to the server.
    pub fn shutdown_best_effort(&self) {
        let _ = self.send_request("shutdown", Value::Null, Duration::from_secs(2));
        let _ = self.send_notification("exit", Value::Null);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.shutdown_best_effort();
    }
}

/// Formats server exit error, checking for missing rustup components.
fn format_server_exit_error(stderr_tail: &str) -> String {
    let trimmed = stderr_tail.trim();
    if trimmed.is_empty() {
        return "LSP server process terminated unexpectedly".to_string();
    }
    if trimmed.contains("Unknown binary 'rust-analyzer'")
        || trimmed.contains("rustup component add")
    {
        return format!(
            "rustup proxy error: rust-analyzer component is not installed in the active toolchain. Run `rustup component add rust-analyzer`.\nstderr: {}",
            trimmed
        );
    }
    format!("LSP server process terminated: {}", trimmed)
}
/// Returns true if an LSP error is transient (e.g. content modified while indexing).
fn is_transient_lsp_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("content modified")
        || lower.contains("cancelled")
        || lower.contains("canceled")
        || lower.contains("not initialized")
        || lower.contains("server is busy")
}

/// Background loop that reads from server stdout and routes responses/notifications.
fn lsp_reader_loop(
    mut stdout: std::process::ChildStdout,
    pending: PendingMap,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    diag_notify: Arc<(Mutex<bool>, Condvar)>,
    stdin: Arc<Mutex<ChildStdin>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    active_progress: Arc<Mutex<HashSet<String>>>,
) {
    let mut buf = Vec::with_capacity(32768);
    let mut chunk = [0u8; 8192];

    loop {
        loop {
            match decode_lsp_message(&mut buf) {
                Ok(Some(msg)) => {
                    handle_incoming_message(
                        &msg,
                        &pending,
                        &diagnostics,
                        &diag_notify,
                        &stdin,
                        &active_progress,
                    );
                }
                Ok(None) => break,
                Err(_) => {
                    // Skip ahead on corrupted framing bytes
                    if !buf.is_empty() {
                        buf.remove(0);
                    }
                }
            }
        }

        match stdout.read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => {
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }

    // Process any remaining messages in buffer
    while let Ok(Some(msg)) = decode_lsp_message(&mut buf) {
        handle_incoming_message(
            &msg,
            &pending,
            &diagnostics,
            &diag_notify,
            &stdin,
            &active_progress,
        );
    }

    // Server process terminated: notify all pending callers with detailed stderr
    let tail = stderr_tail
        .lock()
        .ok()
        .and_then(|b| String::from_utf8(b.clone()).ok())
        .unwrap_or_default();
    let err_msg = format_server_exit_error(&tail);
    if let Ok(mut map) = pending.lock() {
        for (_, tx) in map.drain() {
            let _ = tx.send(Err(err_msg.clone()));
        }
    }
}

/// Dispatches an incoming LSP message: correlations, server-to-client requests, and notifications.
fn handle_incoming_message(
    msg: &Value,
    pending: &PendingMap,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Value>>>>,
    diag_notify: &Arc<(Mutex<bool>, Condvar)>,
    stdin: &Arc<Mutex<ChildStdin>>,
    active_progress: &Arc<Mutex<HashSet<String>>>,
) {
    let id_opt = msg.get("id").and_then(|v| v.as_i64());
    let method_opt = msg.get("method").and_then(|m| m.as_str());

    // 1. Response to client request (has ID, no method)
    if let (Some(id), None) = (id_opt, method_opt) {
        let tx_opt = if let Ok(mut map) = pending.lock() {
            map.remove(&id)
        } else {
            None
        };
        if let Some(tx) = tx_opt {
            if let Some(err) = msg.get("error") {
                let err_msg = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("LSP request returned an error");
                let _ = tx.send(Err(err_msg.to_string()));
            } else {
                let res = msg.get("result").cloned().unwrap_or(Value::Null);
                let _ = tx.send(Ok(res));
            }
        }
        return;
    }

    // 2. Server-to-client request (has ID and method)
    if let (Some(id), Some(method)) = (id_opt, method_opt) {
        let resp = match method {
            "workspace/configuration" => {
                let count = msg
                    .get("params")
                    .and_then(|p| p.get("items"))
                    .and_then(|it| it.as_array())
                    .map(|a| a.len())
                    .unwrap_or(1);
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": vec![Value::Null; count]
                })
            }
            "client/registerCapability" | "window/workDoneProgress/create" => {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": Value::Null
                })
            }
            _ => {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": Value::Null
                })
            }
        };

        if let Ok(bytes) = encode_lsp_message(&resp) {
            if let Ok(mut writer) = stdin.lock() {
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
        }
        return;
    }

    // 3. Server notification (has method, no ID)
    if let Some(method) = method_opt {
        if method == "$/progress" {
            if let Some(params) = msg.get("params") {
                let token = params.get("token").map(|t| t.to_string());
                if let Some(tok) = token {
                    let kind = params
                        .get("value")
                        .and_then(|v| v.get("kind"))
                        .and_then(|k| k.as_str())
                        .unwrap_or("");
                    if let Ok(mut set) = active_progress.lock() {
                        if kind == "begin" {
                            set.insert(tok);
                        } else if kind == "end" {
                            set.remove(&tok);
                        }
                    }
                }
            }
        } else if method == "textDocument/publishDiagnostics" {
            if let Some(params) = msg.get("params") {
                if let Some(uri) = params.get("uri").and_then(|u| u.as_str()) {
                    let norm_key = normalize_uri(uri);
                    let diags = params
                        .get("diagnostics")
                        .and_then(|d| d.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if let Ok(mut store) = diagnostics.lock() {
                        store.insert(norm_key, diags);
                    }
                    let (lock, cvar) = &**diag_notify;
                    if let Ok(mut flag) = lock.lock() {
                        *flag = true;
                        cvar.notify_all();
                    }
                }
            }
        }
    }
}

type ClientKey = (String, PathBuf);
static CLIENT_CACHE: Mutex<Option<HashMap<ClientKey, Arc<LspClient>>>> = Mutex::new(None);

/// Retrieves or spawns an LSP client cached by (server_binary, workspace_root).
fn get_or_spawn_client(
    server_bin: &Path,
    args: &[String],
    workspace_root: &Path,
) -> Result<Arc<LspClient>, String> {
    let key = (
        server_bin.to_string_lossy().to_string(),
        workspace_root.to_path_buf(),
    );
    let mut guard = CLIENT_CACHE
        .lock()
        .map_err(|_| "LSP client cache lock poisoned")?;
    let map = guard.get_or_insert_with(HashMap::new);

    if let Some(existing) = map.get(&key) {
        if existing.is_alive() {
            return Ok(Arc::clone(existing));
        } else {
            map.remove(&key);
        }
    }

    let client = LspClient::spawn(server_bin, args, workspace_root)?;
    map.insert(key, Arc::clone(&client));
    Ok(client)
}

fn wait_for_diagnostics(client: &LspClient, uri: &str, timeout: Duration) -> Vec<Value> {
    let start = Instant::now();
    let (lock, cvar) = &*client.diag_notify;
    let norm_target = normalize_uri(uri);
    let target_path = uri_to_path(uri);

    loop {
        {
            if let Ok(store) = client.diagnostics.lock() {
                if let Some(diags) = store.get(&norm_target).or_else(|| store.get(uri)) {
                    return diags.clone();
                }
                if let Some(ref tp) = target_path {
                    for (k, v) in store.iter() {
                        if let Some(kp) = uri_to_path(k) {
                            if &kp == tp {
                                return v.clone();
                            }
                        }
                    }
                }
            }
        }

        let elapsed = start.elapsed();
        if elapsed >= timeout {
            break;
        }
        let remaining = timeout - elapsed;

        if let Ok(flag_guard) = lock.lock() {
            let _ = cvar.wait_timeout(flag_guard, remaining.min(Duration::from_millis(200)));
        }
    }

    if let Ok(store) = client.diagnostics.lock() {
        if let Some(diags) = store.get(&norm_target).or_else(|| store.get(uri)) {
            return diags.clone();
        }
        if let Some(ref tp) = target_path {
            for (k, v) in store.iter() {
                if let Some(kp) = uri_to_path(k) {
                    if &kp == tp {
                        return v.clone();
                    }
                }
            }
        }
    }
    Vec::new()
}

fn format_diagnostics(diags: &[Value], uri: &str, workspace_root: &Path) -> String {
    let norm_workspace = normalize_path(workspace_root);
    let rel_path = uri_to_path(uri)
        .and_then(|p| {
            let norm_p = normalize_path(&p);
            norm_p
                .strip_prefix(&norm_workspace)
                .ok()
                .map(|r| r.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| {
            uri_to_path(uri)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| uri.to_string())
        });

    if diags.is_empty() {
        return format!("(No diagnostics reported for {})", rel_path);
    }
    let mut lines = Vec::new();
    for d in diags {
        let range = d.get("range");
        let start_line = range
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(|l| l.as_u64())
            .unwrap_or(0) as u32;
        let start_char = range
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("character"))
            .and_then(|c| c.as_u64())
            .unwrap_or(0) as u32;

        let severity = match d.get("severity").and_then(|s| s.as_u64()).unwrap_or(1) {
            1 => "error",
            2 => "warning",
            3 => "info",
            4 => "hint",
            _ => "diagnostic",
        };

        let message = d
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .trim();

        lines.push(format!(
            "{}:{}:{} [{}] {}",
            rel_path,
            start_line + 1,
            start_char + 1,
            severity,
            message
        ));
    }
    lines.join("\n")
}

/// Extracts a single-line trimmed source preview from a file at 0-based `line_0based`.
fn get_line_preview(uri: &str, line_0based: u32) -> Option<String> {
    let path = uri_to_path(uri)?;
    let content = fs::read_to_string(&path).ok()?;
    let line = content.lines().nth(line_0based as usize)?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() > 120 {
        let truncated: String = trimmed.chars().take(117).collect();
        Some(format!("{}...", truncated))
    } else {
        Some(trimmed.to_string())
    }
}

/// Formats a single location as `path:line:col: preview`.
fn format_location(
    loc_uri: &str,
    line_0based: u32,
    char_0based: u32,
    workspace_root: &Path,
) -> String {
    let line_1based = line_0based + 1;
    let col_1based = char_0based + 1;
    let norm_workspace = normalize_path(workspace_root);
    let display_path = if let Some(path) = uri_to_path(loc_uri) {
        let norm_path = normalize_path(&path);
        if let Ok(rel) = norm_path.strip_prefix(&norm_workspace) {
            rel.to_string_lossy().to_string()
        } else {
            norm_path.to_string_lossy().to_string()
        }
    } else {
        loc_uri.to_string()
    };

    let preview = get_line_preview(loc_uri, line_0based);
    if let Some(prev) = preview {
        format!("{}:{}:{}: {}", display_path, line_1based, col_1based, prev)
    } else {
        format!("{}:{}:{}", display_path, line_1based, col_1based)
    }
}

/// Parses locations from LSP response (Location, Location[], or LocationLink[]).
fn parse_locations(res: &Value) -> Vec<(String, u32, u32)> {
    let mut locs = Vec::new();
    match res {
        Value::Object(map) => {
            if let (Some(uri), Some(range)) =
                (map.get("uri").and_then(|u| u.as_str()), map.get("range"))
            {
                let line = range
                    .get("start")
                    .and_then(|s| s.get("line"))
                    .and_then(|l| l.as_u64())
                    .unwrap_or(0) as u32;
                let character = range
                    .get("start")
                    .and_then(|s| s.get("character"))
                    .and_then(|c| c.as_u64())
                    .unwrap_or(0) as u32;
                locs.push((uri.to_string(), line, character));
            }
        }
        Value::Array(items) => {
            for item in items {
                if let Some(target_uri) = item.get("targetUri").and_then(|u| u.as_str()) {
                    let range = item
                        .get("targetSelectionRange")
                        .or_else(|| item.get("targetRange"));
                    let line = range
                        .and_then(|r| r.get("start"))
                        .and_then(|s| s.get("line"))
                        .and_then(|l| l.as_u64())
                        .unwrap_or(0) as u32;
                    let character = range
                        .and_then(|r| r.get("start"))
                        .and_then(|s| s.get("character"))
                        .and_then(|c| c.as_u64())
                        .unwrap_or(0) as u32;
                    locs.push((target_uri.to_string(), line, character));
                } else if let Some(uri) = item.get("uri").and_then(|u| u.as_str()) {
                    let range = item.get("range");
                    let line = range
                        .and_then(|r| r.get("start"))
                        .and_then(|s| s.get("line"))
                        .and_then(|l| l.as_u64())
                        .unwrap_or(0) as u32;
                    let character = range
                        .and_then(|r| r.get("start"))
                        .and_then(|s| s.get("character"))
                        .and_then(|c| c.as_u64())
                        .unwrap_or(0) as u32;
                    locs.push((uri.to_string(), line, character));
                }
            }
        }
        _ => {}
    }
    locs
}

/// Formats a list of locations, capping at `cap` items.
fn format_location_results(
    locs: &[(String, u32, u32)],
    workspace_root: &Path,
    cap: usize,
) -> String {
    if locs.is_empty() {
        return "(No results found)".to_string();
    }
    let total = locs.len();
    let display_count = total.min(cap);
    let mut lines = Vec::new();
    for (uri, line, col) in &locs[..display_count] {
        lines.push(format_location(uri, *line, *col, workspace_root));
    }
    if total > cap {
        lines.push(format!("[Showing first {} of {} results]", cap, total));
    }
    lines.join("\n")
}

/// Flattens hierarchical DocumentSymbol responses recursively with lines and previews.
fn flatten_document_symbols(items: &[Value], uri: &str, depth: usize, out: &mut Vec<String>) {
    for item in items {
        let name = item
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("<unnamed>");
        let kind_num = item.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);
        let kind_str = symbol_kind_name(kind_num);

        let sel_range = item.get("selectionRange").or_else(|| item.get("range"));
        let line_0 = sel_range
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("line"))
            .and_then(|l| l.as_u64())
            .unwrap_or(0) as u32;
        let char_0 = sel_range
            .and_then(|r| r.get("start"))
            .and_then(|s| s.get("character"))
            .and_then(|c| c.as_u64())
            .unwrap_or(0) as u32;

        let indent = "  ".repeat(depth);
        let preview = get_line_preview(uri, line_0).unwrap_or_default();
        let prev_suffix = if preview.is_empty() {
            String::new()
        } else {
            format!(": {}", preview)
        };

        out.push(format!(
            "{}{:>4}:{:>2} [{}] {}{}",
            indent,
            line_0 + 1,
            char_0 + 1,
            kind_str,
            name,
            prev_suffix
        ));

        if let Some(children) = item.get("children").and_then(|c| c.as_array()) {
            flatten_document_symbols(children, uri, depth + 1, out);
        }
    }
}

/// Formats flat SymbolInformation responses (from documentSymbol or workspace/symbol).
fn format_flat_symbols(items: &[Value], workspace_root: &Path, out: &mut Vec<String>) {
    for item in items {
        let name = item
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("<unnamed>");
        let kind_num = item.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);
        let kind_str = symbol_kind_name(kind_num);
        let container = item
            .get("containerName")
            .and_then(|c| c.as_str())
            .unwrap_or("");

        if let Some(loc) = item.get("location") {
            let uri = loc.get("uri").and_then(|u| u.as_str()).unwrap_or("");
            let range = loc.get("range");
            let line_0 = range
                .and_then(|r| r.get("start"))
                .and_then(|s| s.get("line"))
                .and_then(|l| l.as_u64())
                .unwrap_or(0) as u32;
            let char_0 = range
                .and_then(|r| r.get("start"))
                .and_then(|s| s.get("character"))
                .and_then(|c| c.as_u64())
                .unwrap_or(0) as u32;

            let norm_workspace = normalize_path(workspace_root);
            let path_str = uri_to_path(uri)
                .and_then(|p| {
                    let norm_p = normalize_path(&p);
                    norm_p
                        .strip_prefix(&norm_workspace)
                        .ok()
                        .map(|r| r.to_string_lossy().to_string())
                })
                .unwrap_or_else(|| uri.to_string());

            let container_suffix = if container.is_empty() {
                String::new()
            } else {
                format!(" ({})", container)
            };

            let preview = get_line_preview(uri, line_0).unwrap_or_default();
            let prev_suffix = if preview.is_empty() {
                String::new()
            } else {
                format!(": {}", preview)
            };

            out.push(format!(
                "{}:{}:{} [{}] {}{}{}",
                path_str,
                line_0 + 1,
                char_0 + 1,
                kind_str,
                name,
                container_suffix,
                prev_suffix
            ));
        }
    }
}

/// Formats LSP hover contents.
fn format_hover_content(hover_res: &Value) -> String {
    if hover_res.is_null() {
        return "(No hover information available)".to_string();
    }
    let contents = match hover_res.get("contents") {
        Some(c) => c,
        None => return "(No hover contents)".to_string(),
    };

    let mut parts = Vec::new();
    match contents {
        Value::String(s) => parts.push(s.clone()),
        Value::Object(obj) => {
            if let Some(val) = obj.get("value").and_then(|v| v.as_str()) {
                parts.push(val.to_string());
            }
        }
        Value::Array(items) => {
            for it in items {
                match it {
                    Value::String(s) => parts.push(s.clone()),
                    Value::Object(obj) => {
                        if let Some(val) = obj.get("value").and_then(|v| v.as_str()) {
                            parts.push(val.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    if parts.is_empty() {
        "(No hover information available)".to_string()
    } else {
        parts.join("\n\n")
    }
}

/// Fallback to ripgrep regex heuristics when no language server binary is available.
fn exec_lsp_heuristic_fallback(
    ctx: &mut ToolContext,
    op: &str,
    raw_path: &str,
    symbol: &str,
    ext: &str,
    reason: &str,
) -> String {
    let label = if ext.is_empty() {
        format!("[heuristic: no language server available ({})]\n", reason)
    } else {
        format!(
            "[heuristic: no language server for .{} ({})]\n",
            ext, reason
        )
    };

    if op == "symbols" || (symbol.is_empty() && !raw_path.is_empty() && raw_path != ".") {
        let target = ctx.resolve_local_path(raw_path);
        if target.is_file() {
            if let Ok(content) = fs::read_to_string(&target) {
                let mut syms = Vec::new();
                for (idx, line) in content.lines().enumerate() {
                    if is_structural_outline_line(line) {
                        syms.push(format!("{:>4}: {}", idx + 1, line.trim()));
                    }
                }
                if syms.is_empty() {
                    return format!("{}(No structural symbols found in file)", label);
                }
                return format!("{}{}", label, syms.join("\n"));
            }
        }
        return format!(
            "{}{}",
            label,
            build_live_project_signatures(&target.to_string_lossy())
        );
    }

    if symbol.is_empty() {
        return format!("{}LSP error: missing 'symbol' parameter.", label);
    }

    let is_def = op == "definition" || op == "def";
    let pattern = if is_def {
        format!(
            r"(fn|struct|enum|trait|type|const|let|static|def|class|function|interface|func)\s+{}\b",
            symbol
        )
    } else {
        format!(r"\b{}\b", symbol)
    };

    let search_res = exec_search(
        ctx,
        &json!({
            "pattern": pattern,
            "path": raw_path,
            "context": if is_def { 6 } else { 0 }
        }),
    );
    format!("{}{}", label, search_res)
}

/// Executes Language Server Protocol operations: definition, references, hover, symbols, diagnostics, etc.
pub fn exec_lsp(ctx: &mut ToolContext, args: &Value) -> String {
    let op = get_str_alias(args, &["op", "action", "mode"]).to_ascii_lowercase();
    let symbol = get_str_alias(args, &["symbol", "name", "query", "identifier"]);
    let raw_path = {
        let p = get_str_alias(args, &["path", "file", "dir"]);
        if p.is_empty() {
            "."
        } else {
            p
        }
    };

    let line_arg = args
        .get("line")
        .and_then(|v| {
            v.as_u64()
                .map(|n| n as u32)
                .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
        })
        .unwrap_or(0);
    let col_arg = args
        .get("column")
        .or_else(|| args.get("col"))
        .and_then(|v| {
            v.as_u64()
                .map(|n| n as u32)
                .or_else(|| v.as_str().and_then(|s| s.parse::<u32>().ok()))
        });

    let target_path = normalize_path(&ctx.resolve_local_path(raw_path));
    let workspace_root = normalize_path(&detect_workspace_root(&target_path));

    let ext = if target_path.is_file() {
        target_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        detect_extension_from_project(&workspace_root)
            .unwrap_or("")
            .to_string()
    };

    let server_spec = get_server_spec_for_ext(&ext);
    let (server_cmd, server_args) = match server_spec {
        Some(spec) => (spec.command, spec.args),
        None => {
            return exec_lsp_heuristic_fallback(
                ctx,
                &op,
                raw_path,
                symbol,
                &ext,
                "unsupported extension",
            );
        }
    };

    let resolved_bin = match resolve_binary(&server_cmd) {
        Some(b) => b,
        None => {
            return exec_lsp_heuristic_fallback(
                ctx,
                &op,
                raw_path,
                symbol,
                &ext,
                &format!("binary '{}' not found on PATH", server_cmd),
            );
        }
    };

    let client = match get_or_spawn_client(&resolved_bin, &server_args, &workspace_root) {
        Ok(c) => c,
        Err(e) => {
            return exec_lsp_heuristic_fallback(
                ctx,
                &op,
                raw_path,
                symbol,
                &ext,
                &format!("failed to initialize server: {}", e),
            );
        }
    };

    // Special op: workspace_symbol
    if op == "workspace_symbol" || op == "workspace_symbols" {
        let query = if !symbol.is_empty() { symbol } else { raw_path };
        return match client.send_request(
            "workspace/symbol",
            json!({ "query": query }),
            Duration::from_secs(20),
        ) {
            Ok(res) => {
                let mut out = Vec::new();
                if let Some(items) = res.as_array() {
                    format_flat_symbols(items, &workspace_root, &mut out);
                }
                if out.is_empty() {
                    "(No matching workspace symbols found)".to_string()
                } else {
                    if out.len() > 100 {
                        let total = out.len();
                        out.truncate(100);
                        out.push(format!("[Showing first 100 of {} results]", total));
                    }
                    out.join("\n")
                }
            }
            Err(e) => format!("LSP error (workspace/symbol): {}", e),
        };
    }

    // Special case: definition or references with only symbol and no path or line
    let needs_symbol_resolution =
        (op == "definition" || op == "def" || op == "references" || op == "refs")
            && !symbol.is_empty()
            && (line_arg == 0 && (raw_path == "." || !target_path.is_file()));

    let (effective_path, effective_line, effective_col) = if needs_symbol_resolution {
        match client.send_request(
            "workspace/symbol",
            json!({ "query": symbol }),
            Duration::from_secs(20),
        ) {
            Ok(sym_res) => {
                let mut best_loc: Option<(PathBuf, u32, u32)> = None;
                if let Some(items) = sym_res.as_array() {
                    for it in items {
                        let name = it.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        if let Some(loc) = it.get("location") {
                            let uri = loc.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                            let range = loc.get("range");
                            let l = range
                                .and_then(|r| r.get("start"))
                                .and_then(|s| s.get("line"))
                                .and_then(|n| n.as_u64())
                                .unwrap_or(0) as u32;
                            let c = range
                                .and_then(|r| r.get("start"))
                                .and_then(|s| s.get("character"))
                                .and_then(|n| n.as_u64())
                                .unwrap_or(0) as u32;
                            if let Some(p) = uri_to_path(uri) {
                                if name == symbol {
                                    best_loc = Some((p, l + 1, c + 1));
                                    break;
                                } else if best_loc.is_none() {
                                    best_loc = Some((p, l + 1, c + 1));
                                }
                            }
                        }
                    }
                }
                match best_loc {
                    Some((p, l, c)) => (p, l, Some(c)),
                    None => (target_path.clone(), line_arg, col_arg),
                }
            }
            Err(_) => (target_path.clone(), line_arg, col_arg),
        }
    } else {
        (target_path.clone(), line_arg, col_arg)
    };
    let effective_path = normalize_path(&effective_path);
    if !effective_path.is_file() {
        return format!(
            "LSP error: target path '{}' is not a file. For workspace searches use op='workspace_symbol'.",
            effective_path.display()
        );
    }

    let uri = path_to_uri(&effective_path);
    if let Err(e) = client.ensure_document_open(&uri, &effective_path) {
        return format!("LSP error opening document: {}", e);
    }

    // Diagnostics operation
    if op == "diagnostics" || op == "diag" {
        let wait_sec = args
            .get("wait_sec")
            .or_else(|| args.get("timeout_sec"))
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(15);
        let diags = wait_for_diagnostics(&client, &uri, Duration::from_secs(wait_sec));
        return format_diagnostics(&diags, &uri, &workspace_root);
    }

    // Document symbols
    if op == "symbols" || op == "document_symbols" {
        return match client.send_request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
            Duration::from_secs(20),
        ) {
            Ok(res) => {
                let mut out = Vec::new();
                if let Some(items) = res.as_array() {
                    let is_hierarchical = items.iter().any(|it| it.get("children").is_some());
                    if is_hierarchical {
                        flatten_document_symbols(items, &uri, 0, &mut out);
                    } else {
                        format_flat_symbols(items, &workspace_root, &mut out);
                    }
                }
                if out.is_empty() {
                    "(No symbols found in file)".to_string()
                } else {
                    out.join("\n")
                }
            }
            Err(e) => format!("LSP error (documentSymbol): {}", e),
        };
    }

    // Position-based queries
    let content = match fs::read_to_string(&effective_path) {
        Ok(c) => c,
        Err(e) => return format!("LSP error reading file: {}", e),
    };

    let (lsp_line, lsp_char) =
        resolve_line_and_col(&content, effective_line, effective_col, symbol);
    let position_param = json!({
        "textDocument": { "uri": uri },
        "position": {
            "line": lsp_line,
            "character": lsp_char
        }
    });

    match op.as_str() {
        "definition" | "def" => {
            let start = Instant::now();
            let mut locs = Vec::new();
            let mut last_err = None;

            loop {
                match client.send_request(
                    "textDocument/definition",
                    position_param.clone(),
                    Duration::from_secs(20),
                ) {
                    Ok(res) => {
                        locs = parse_locations(&res);
                        if !locs.is_empty() {
                            last_err = None;
                            break;
                        }
                    }
                    Err(e) => {
                        if is_transient_lsp_error(&e) {
                            last_err = None;
                        } else {
                            last_err = Some(e);
                        }
                    }
                }

                let elapsed = start.elapsed();
                if elapsed >= Duration::from_secs(60) {
                    break;
                }
                // A permanent error or a settled empty answer ends the wait once indexing is done.
                if !client.is_indexing()
                    && (last_err.is_some() || elapsed >= Duration::from_secs(8))
                {
                    break;
                }
                thread::sleep(Duration::from_millis(500));
            }

            if let Some(e) = last_err {
                if locs.is_empty() {
                    return format!("LSP error (definition): {}", e);
                }
            }

            if locs.is_empty() && needs_symbol_resolution {
                format_location(
                    &path_to_uri(&effective_path),
                    effective_line.saturating_sub(1),
                    effective_col.unwrap_or(1).saturating_sub(1),
                    &workspace_root,
                )
            } else {
                format_location_results(&locs, &workspace_root, 100)
            }
        }
        "references" | "refs" => {
            let mut req_params = position_param;
            req_params["context"] = json!({ "includeDeclaration": true });
            let start = Instant::now();
            let mut locs = Vec::new();
            let mut last_err = None;

            loop {
                match client.send_request(
                    "textDocument/references",
                    req_params.clone(),
                    Duration::from_secs(20),
                ) {
                    Ok(res) => {
                        locs = parse_locations(&res);
                        if !locs.is_empty() {
                            last_err = None;
                            break;
                        }
                    }
                    Err(e) => {
                        if is_transient_lsp_error(&e) {
                            last_err = None;
                        } else {
                            last_err = Some(e);
                        }
                    }
                }

                let elapsed = start.elapsed();
                if elapsed >= Duration::from_secs(60) {
                    break;
                }
                // A permanent error or a settled empty answer ends the wait once indexing is done.
                if !client.is_indexing()
                    && (last_err.is_some() || elapsed >= Duration::from_secs(8))
                {
                    break;
                }
                thread::sleep(Duration::from_millis(500));
            }

            if let Some(e) = last_err {
                if locs.is_empty() {
                    return format!("LSP error (references): {}", e);
                }
            }

            format_location_results(&locs, &workspace_root, 100)
        }
        "hover" => {
            match client.send_request(
                "textDocument/hover",
                position_param,
                Duration::from_secs(20),
            ) {
                Ok(res) => format_hover_content(&res),
                Err(e) => format!("LSP error (hover): {}", e),
            }
        }
        "implementation" => {
            match client.send_request(
                "textDocument/implementation",
                position_param,
                Duration::from_secs(20),
            ) {
                Ok(res) => {
                    let locs = parse_locations(&res);
                    format_location_results(&locs, &workspace_root, 100)
                }
                Err(e) => format!("LSP error (implementation): {}", e),
            }
        }
        "type_definition" | "typedef" => {
            match client.send_request(
                "textDocument/typeDefinition",
                position_param,
                Duration::from_secs(20),
            ) {
                Ok(res) => {
                    let locs = parse_locations(&res);
                    format_location_results(&locs, &workspace_root, 100)
                }
                Err(e) => format!("LSP error (typeDefinition): {}", e),
            }
        }
        _ => {
            format!(
                "Unknown LSP op '{}'. Supported ops: definition, references, hover, symbols, diagnostics, implementation, type_definition, workspace_symbol.",
                op
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_lsp_framing() {
        let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "test" });
        let bytes = encode_lsp_message(&msg).expect("encode");

        let mut buf = Vec::new();
        // Feed partial bytes
        buf.extend_from_slice(&bytes[..10]);
        assert_eq!(decode_lsp_message(&mut buf).unwrap(), None);

        // Feed remaining bytes
        buf.extend_from_slice(&bytes[10..]);
        let decoded = decode_lsp_message(&mut buf).unwrap().expect("decode");
        assert_eq!(decoded["id"], 1);
        assert_eq!(decoded["method"], "test");
        assert!(buf.is_empty());
    }

    #[test]
    fn test_multiple_messages_in_one_buffer() {
        let msg1 = json!({ "id": 1, "result": "one" });
        let msg2 = json!({ "id": 2, "result": "two" });

        let mut buf = encode_lsp_message(&msg1).unwrap();
        buf.extend(encode_lsp_message(&msg2).unwrap());

        let d1 = decode_lsp_message(&mut buf)
            .unwrap()
            .expect("first message");
        assert_eq!(d1["id"], 1);
        assert_eq!(d1["result"], "one");

        let d2 = decode_lsp_message(&mut buf)
            .unwrap()
            .expect("second message");
        assert_eq!(d2["id"], 2);
        assert_eq!(d2["result"], "two");
        assert!(buf.is_empty());
    }

    #[test]
    fn test_uri_path_roundtrip_with_spaces() {
        let path = PathBuf::from("/Users/test user/my project/file.rs");
        let uri = path_to_uri(&path);
        assert!(uri.contains("%20"));
        let roundtrip = uri_to_path(&uri).expect("roundtrip");
        assert_eq!(roundtrip, path);
    }

    #[test]
    fn test_normalize_path_and_uri() {
        let p = Path::new("/tmp/test/./sub/../file.rs");
        let norm = normalize_path(p);
        assert!(!norm.to_string_lossy().contains("/./"));
        assert!(!norm.to_string_lossy().contains("/../"));

        let uri = "file:///tmp/test/./sub/../file.rs";
        let norm_u = normalize_uri(uri);
        assert!(!norm_u.contains("/./"));
        assert!(!norm_u.contains("/../"));
    }

    #[test]
    fn test_resolve_line_and_col_by_symbol() {
        let content = "fn main() {\n    let my_var = 42;\n    println!(\"{}\", my_var);\n}\n";
        // Line 2 has 'my_var' starting at column 9 (4 spaces + 'let ')
        let (lsp_line, lsp_char) = resolve_line_and_col(content, 2, None, "my_var");
        assert_eq!(lsp_line, 1);
        assert_eq!(lsp_char, 8); // 0-based character index

        // Search across entire file without line
        let (found_line, found_char) = resolve_line_and_col(content, 0, None, "println");
        assert_eq!(found_line, 2);
        assert_eq!(found_char, 4);
    }

    #[test]
    fn test_format_location_preview() {
        let temp_dir = std::env::temp_dir().join(format!("spavn_test_loc_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("sample.rs");
        fs::write(
            &file_path,
            "line one\npub fn calculate_total() -> u64 {\n    0\n}",
        )
        .unwrap();

        let uri = path_to_uri(&file_path);
        let loc_str = format_location(&uri, 1, 7, &temp_dir);
        assert!(loc_str.contains("sample.rs:2:8: pub fn calculate_total()"));

        let _ = fs::remove_dir_all(&temp_dir);
    }
    #[test]
    fn test_format_diagnostics() {
        let uri = "file:///tmp/test/src/lib.rs";
        let workspace = Path::new("/tmp/test");
        let diags = vec![
            json!({
                "range": { "start": { "line": 4, "character": 10 } },
                "severity": 1,
                "message": "mismatched types"
            }),
            json!({
                "range": { "start": { "line": 9, "character": 2 } },
                "severity": 2,
                "message": "unused variable"
            }),
        ];
        let out = format_diagnostics(&diags, uri, workspace);
        assert!(out.contains("src/lib.rs:5:11 [error] mismatched types"));
        assert!(out.contains("src/lib.rs:10:3 [warning] unused variable"));

        let empty = format_diagnostics(&[], uri, workspace);
        assert_eq!(empty, "(No diagnostics reported for src/lib.rs)");
    }

    #[test]
    #[ignore]
    fn test_rust_analyzer_integration() {
        let temp_dir = std::env::temp_dir().join(format!("spavn_lsp_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("src")).unwrap();

        let cargo_toml = r#"[package]
name = "test_lsp_proj"
version = "0.1.0"
edition = "2021"
"#;
        let lib_rs = r#"pub fn greet_user(name: &str) -> String {
    format!("Hello, {}!", name)
}
"#;
        let main_rs = r#"use test_lsp_proj::greet_user;

fn main() {
    let msg = greet_user("world");
    println!("{}", msg);
    let bad_type: u32 = "type_error_test";
}
"#;
        fs::write(temp_dir.join("Cargo.toml"), cargo_toml).unwrap();
        fs::write(temp_dir.join("src").join("lib.rs"), lib_rs).unwrap();
        fs::write(temp_dir.join("src").join("main.rs"), main_rs).unwrap();

        let bin = resolve_binary("rust-analyzer");
        if bin.is_none() {
            let _ = fs::remove_dir_all(&temp_dir);
            return;
        }
        let ra_bin = bin.unwrap();

        let client = LspClient::spawn(&ra_bin, &[], &temp_dir)
            .expect("LspClient::spawn rust-analyzer failed");

        let main_path = temp_dir.join("src").join("main.rs");
        let main_uri = path_to_uri(&main_path);
        client.ensure_document_open(&main_uri, &main_path).unwrap();

        let (lsp_line, lsp_char) = resolve_line_and_col(main_rs, 4, None, "greet_user");
        let start = Instant::now();
        let mut locs = Vec::new();
        while locs.is_empty() && start.elapsed() < Duration::from_secs(30) {
            if let Ok(def_res) = client.send_request(
                "textDocument/definition",
                json!({
                    "textDocument": { "uri": main_uri },
                    "position": { "line": lsp_line, "character": lsp_char }
                }),
                Duration::from_secs(10),
            ) {
                locs = parse_locations(&def_res);
                if !locs.is_empty() {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(500));
        }

        assert!(!locs.is_empty(), "Should find definition for greet_user");
        let formatted = format_location_results(&locs, &temp_dir, 100);
        assert!(
            formatted.contains("lib.rs"),
            "Definition should be in lib.rs"
        );

        // 2. Diagnostics: assert deliberate type error is reported
        let diags = wait_for_diagnostics(&client, &main_uri, Duration::from_secs(25));
        let diag_str = format_diagnostics(&diags, &main_uri, &temp_dir);
        println!("Temp project diagnostics:\n{}", diag_str);
        assert!(
            diag_str.contains("mismatched types") || diag_str.contains("expected `u32`"),
            "Diagnostics should contain type mismatch error, got: {}",
            diag_str
        );
        assert!(
            diag_str.contains("main.rs:6"),
            "Diagnostics should point to main.rs line 6, got: {}",
            diag_str
        );
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    #[ignore]
    fn test_spavn_repo_integration() {
        let repo_root = detect_workspace_root(Path::new("src/turn.rs"));
        let mut ctx = ToolContext::new(
            AccessMode::Sovereign,
            "local".to_string(),
            repo_root,
            crate::spavn_engine::AgentPhase::Code,
        );

        // 1. Definition of evaluate_loop_guard in src/turn.rs
        let def_res = exec_lsp(
            &mut ctx,
            &json!({
                "op": "definition",
                "path": "src/turn.rs",
                "line": 900,
                "symbol": "evaluate_loop_guard"
            }),
        );
        println!("Definition result:\n{}", def_res);
        assert!(
            def_res.contains("src/harness.rs"),
            "evaluate_loop_guard should resolve to src/harness.rs, got: {}",
            def_res
        );

        // 2. Diagnostics for src/harness.rs
        let diag_res = exec_lsp(
            &mut ctx,
            &json!({
                "op": "diagnostics",
                "path": "src/harness.rs",
                "timeout_sec": 10
            }),
        );
        println!("Diagnostics result:\n{}", diag_res);
        assert!(!diag_res.is_empty());
        assert!(
            !diag_res.contains("/./"),
            "Diagnostics result should not contain /./, got: {}",
            diag_res
        );
        assert!(
            diag_res.contains("src/harness.rs"),
            "Diagnostics result should contain relative path src/harness.rs, got: {}",
            diag_res
        );
    }
}
