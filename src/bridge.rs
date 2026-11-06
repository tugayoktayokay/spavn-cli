//! The desktop bridge: the CLI as a headless child of the app.
//!
//! The app owns a Unix socket; the CLI connects as a client and speaks the NDJSON protocol in
//! `apps/spavn-desktop/docs/PROTOCOL.md` (v1). This module owns the transport, the request
//! handling that does not need the agent loop, and the [`Approver`] that turns the access gate
//! into an `approval.request` event. The agent loop itself lives in the binary (`turn.rs`): the
//! driver polls this module for prompts and streams its facts back through [`crate::events`].
//!
//! Two threads, one owner each: the reader thread parses and answers every request, the writer
//! thread drains an mpsc queue so a hung socket can never block the agent loop.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::approval::{ApprovalAnswer, Approver};
use crate::config::{byok_catalog, load_user_config};
use crate::events::{self, Event};
use crate::memory::cached_system_prompt;
use crate::router::{ChatMessage, ModelEndpoint};
use crate::session::{
    append_message, fork_session, list_recent_sessions, load_session, new_session_path,
};
use crate::slash_commands::{dispatch_slash_command, SlashOutcome};
use crate::spavn_engine::{AgentPhase, AgentRoleKey};
use crate::state::{list_live_tasks, LiveTaskStatus};
use crate::tools::{AccessMode, ToolContext};

/// How long the app may take to say hello before the CLI gives up and exits non-zero.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the gate waits for a human before failing closed.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Everything a turn owns. The agent loop mutates it under one lock, so a request that needs to
/// read it (`state.get`) can never observe a half-applied change.
pub struct Shared {
    pub endpoint: ModelEndpoint,
    pub ctx: ToolContext,
    pub messages: Vec<ChatMessage>,
    pub session: PathBuf,
    pub session_name: String,
    pub cwd: String,
    pub phase: AgentPhase,
    pub role: AgentRoleKey,
    pub show_thinking: bool,
    pub quota_badge: String,
    pub prefill_input: Option<String>,
    pub turns: u64,
    /// Cached `state.get` payload; rebuilt after every state-mutating request and after each turn.
    pub snapshot: Value,
}

impl Shared {
    pub fn new(endpoint: ModelEndpoint, ctx: ToolContext, messages: Vec<ChatMessage>, session: PathBuf) -> Self {
        let cwd = ctx.default_host.clone();
        let _ = &cwd;
        let mut shared = Self {
            endpoint,
            ctx,
            messages,
            session: session.clone(),
            session_name: session
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            cwd: crate::memory::current_workdir(),
            phase: AgentPhase::Code,
            role: AgentRoleKey::Auto,
            show_thinking: true,
            quota_badge: String::new(),
            prefill_input: None,
            turns: 0,
            snapshot: Value::Null,
        };
        shared.phase = shared.ctx.phase;
        shared.snapshot = snapshot(&shared);
        shared
    }

    /// Context utilisation for the session chip: `(tokens, limit, pct)`.
    pub fn ctx_usage(&self) -> (usize, usize, u8) {
        let tokens = crate::hud::estimate_messages_tokens(&self.messages);
        let limit = self.endpoint.context_window_tokens();
        (tokens, limit, percent(tokens, limit))
    }

    /// What `session.ready` carries. Sent once, after the handshake.
    pub fn session_ready(&mut self) -> Event {
        let (ctx_tokens, ctx_limit, ctx_pct) = self.ctx_usage();
        Event::SessionReady {
            session: self.session.to_string_lossy().to_string(),
            cwd: self.cwd.clone(),
            model: self.endpoint.key.clone(),
            endpoint_local: self.endpoint.is_local(),
            phase: self.phase.as_str().to_string(),
            access: events::access_label(self.ctx.access),
            ctx_tokens,
            ctx_limit,
            ctx_pct,
        }
    }
}

fn percent(used: usize, limit: usize) -> u8 {
    if limit == 0 {
        return 0;
    }
    (((used as f64 / limit as f64) * 100.0).round() as u8).min(100)
}

fn status_str(status: &LiveTaskStatus) -> &'static str {
    match status {
        LiveTaskStatus::Pending => "pending",
        LiveTaskStatus::InProgress => "inProgress",
        LiveTaskStatus::Done => "done",
    }
}

/// The `state.get` payload — the same shape `/state` renders, but as data.
pub fn snapshot(shared: &Shared) -> Value {
    let (ctx_tokens, ctx_limit, ctx_pct) = shared.ctx_usage();
    let cfg = load_user_config();
    let sessions = list_recent_sessions(20)
        .into_iter()
        .map(|s| {
            json!({
                "path": s.path.to_string_lossy(),
                "filename": s.filename,
                "age": s.age_label,
                "message_count": s.message_count,
                "preview": s.preview,
            })
        })
        .collect::<Vec<_>>();
    let models = byok_catalog(&cfg)
        .iter()
        .map(|group| {
            json!({
                "key": group.id,
                "label": group.title,
                "local": group.is_local,
                "models": group
                    .models
                    .iter()
                    .map(|m| {
                        json!({
                            "key": m.key,
                            "label": m.label,
                            "badge": m.badge,
                            "context_window": m.context_window,
                        })
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let tasks = list_live_tasks()
        .iter()
        .map(|t| json!({ "title": t.title, "status": status_str(&t.status) }))
        .collect::<Vec<_>>();
    json!({
        "session": shared.session.to_string_lossy(),
        "cwd": shared.cwd,
        "model": shared.endpoint.key,
        "endpoint": {
            "key": shared.endpoint.key,
            "label": shared.endpoint.label,
            "local": shared.endpoint.is_local(),
        },
        "phase": shared.phase.as_str(),
        "access": events::access_label(shared.ctx.access),
        "ctx": { "tokens": ctx_tokens, "limit": ctx_limit, "pct": ctx_pct },
        "turns": shared.turns,
        "sessions": sessions,
        "models": models,
        "tasks": tasks,
        // The CLI has no goals, pins or gates store; the keys stay so the host's shape check
        // passes and the panels render empty instead of guessing.
        "goals": [],
        "pins": [],
        "gates": [],
    })
}

/// A prompt the reader accepted and the driver has not started yet.
pub struct PendingPrompt {
    pub turn: u64,
    pub text: String,
}

/// State shared between the socket threads and the agent loop; deliberately lock-free where a
/// blocking turn must not be able to stall a request.
struct Ctl {
    busy: AtomicBool,
    shutdown: AtomicBool,
    prompt_queue: Mutex<VecDeque<PendingPrompt>>,
    approvals: Mutex<HashMap<String, Sender<String>>>,
    approval_seq: AtomicU64,
    snapshot: Mutex<Value>,
    /// Action titles the human approved for the rest of the session (`answer: "session"`).
    session_ok: Mutex<HashSet<String>>,
}

/// The CLI's end of the bridge.
pub struct Bridge {
    shared: Arc<Mutex<Shared>>,
    ctl: Arc<Ctl>,
    out: Sender<String>,
}

impl Bridge {
    pub fn shared(&self) -> Arc<Mutex<Shared>> {
        self.shared.clone()
    }

    /// Locks the session for a read-modify-write by the driver.
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        match self.shared.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// The next queued prompt, if the app submitted one.
    pub fn take_prompt(&self) -> Option<PendingPrompt> {
        let mut q = match self.ctl.prompt_queue.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        q.pop_front()
    }

    /// Marks the turn in flight so `prompt.submit` can answer `busy` instead of queueing behind it.
    pub fn set_busy(&self, busy: bool) {
        self.ctl.busy.store(busy, Ordering::SeqCst);
    }

    pub fn shutdown_requested(&self) -> bool {
        self.ctl.shutdown.load(Ordering::SeqCst)
    }

    /// Refreshes the cached `state.get` payload after a mutation the reader did not make itself.
    pub fn refresh_snapshot(&self) {
        let fresh = snapshot(&self.lock());
        let mut cache = match self.ctl.snapshot.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *cache = fresh;
    }

    /// Serialises one frame straight onto the writer queue (used for `bridge.closed`).
    pub fn send_raw(&self, line: String) {
        let _ = self.out.send(line);
    }

    /// The approver the turn's [`ToolContext`] must use.
    pub fn approver(&self) -> Arc<dyn Approver> {
        Arc::new(BridgeApprover {
            ctl: self.ctl.clone(),
        })
    }
}

/// The access gate, answered by the app's card instead of a terminal line.
///
/// Fail-closed, as the trait demands: no answer, a closed socket, or a timeout is a deny.
struct BridgeApprover {
    ctl: Arc<Ctl>,
}

impl Approver for BridgeApprover {
    fn ask(&self, action_title: &str, detail: &str, access: AccessMode) -> ApprovalAnswer {
        let label = events::access_label(access);
        if let Ok(ok) = self.ctl.session_ok.lock() {
            if ok.contains(action_title) {
                return ApprovalAnswer::Approve;
            }
        }
        let id = format!("ap-{}", self.ctl.approval_seq.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::channel::<String>();
        if let Ok(mut map) = self.ctl.approvals.lock() {
            map.insert(id.clone(), tx);
        } else {
            return ApprovalAnswer::Deny;
        }
        events::emit(Event::ApprovalRequest {
            id: id.clone(),
            action: action_title.to_string(),
            detail: detail.to_string(),
            access: label.clone(),
            options: vec!["approve", "deny", "sovereign", "session"],
        });
        let answer = rx.recv_timeout(APPROVAL_TIMEOUT).unwrap_or_else(|_| "deny".into());
        if let Ok(mut map) = self.ctl.approvals.lock() {
            map.remove(&id);
        }
        if answer == "session" {
            if let Ok(mut ok) = self.ctl.session_ok.lock() {
                ok.insert(action_title.to_string());
            }
        }
        events::emit(Event::ApprovalResolved {
            id,
            answer: answer.clone(),
            access: label,
        });
        match answer.as_str() {
            "approve" | "session" => ApprovalAnswer::Approve,
            "sovereign" => ApprovalAnswer::Sovereign,
            _ => ApprovalAnswer::Deny,
        }
    }

    fn notice(&self, message: &str) {
        events::emit(Event::Notice {
            level: "info",
            text: message.to_string(),
        });
    }
}

/// Reads the handshake token the app wrote next to its socket.
///
/// `SPAVN_BRIDGE_TOKEN` wins; otherwise `<dir>/bridge.env` is parsed for it, which is how the app
/// pairs the two processes without putting a secret on a command line.
fn read_token(socket: &Path) -> Option<String> {
    if let Ok(token) = std::env::var("SPAVN_BRIDGE_TOKEN") {
        if !token.trim().is_empty() {
            return Some(token.trim().to_string());
        }
    }
    let env_path = socket.parent()?.join("bridge.env");
    let raw = std::fs::read_to_string(env_path).ok()?;
    for line in raw.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("SPAVN_BRIDGE_TOKEN=") {
            let value = value.trim().trim_matches('"').trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Length-independent comparison: the token is a secret, so a timing side channel is not free.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn reply(id: &Value, data: Value) -> String {
    json!({ "v": events::VERSION, "method": "reply", "id": id, "ok": true, "data": data }).to_string()
}

fn error_reply(id: &Value, code: &str, message: impl Into<String>) -> String {
    json!({
        "v": events::VERSION,
        "method": "reply",
        "id": id,
        "ok": false,
        "err": { "code": code, "message": message.into() },
    })
    .to_string()
}

/// Connects, handshakes, and starts the reader and writer threads.
///
/// Returns only once the app's hello has been accepted, so the driver can emit `session.ready`
/// with the sink already installed and no event can jump ahead of the handshake.
pub fn connect(socket: &str, shared: Shared) -> Result<Bridge, String> {
    let path = PathBuf::from(socket);
    let stream = UnixStream::connect(&path)
        .map_err(|e| format!("bridge: connect {socket}: {e}"))?;
    let token = read_token(&path);
    let shared = Arc::new(Mutex::new(shared));
    let ctl = Arc::new(Ctl {
        busy: AtomicBool::new(false),
        shutdown: AtomicBool::new(false),
        prompt_queue: Mutex::new(VecDeque::new()),
        approvals: Mutex::new(HashMap::new()),
        approval_seq: AtomicU64::new(1),
        snapshot: Mutex::new(Value::Null),
        session_ok: Mutex::new(HashSet::new()),
    });

    let (out_tx, out_rx) = mpsc::channel::<String>();
    let writer_stream = stream
        .try_clone()
        .map_err(|e| format!("bridge: clone socket: {e}"))?;
    spawn_writer(writer_stream, out_rx);

    let (hs_tx, hs_rx) = mpsc::channel::<Result<(), String>>();
    let reader_stream = stream
        .try_clone()
        .map_err(|e| format!("bridge: clone socket: {e}"))?;
    spawn_reader(reader_stream, shared.clone(), ctl.clone(), out_tx.clone(), token, hs_tx);
    drop(stream);

    match hs_rx.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(err),
        Err(_) => return Err("bridge: handshake timed out".to_string()),
    }

    let sink_tx = out_tx.clone();
    events::install(Arc::new(move |line: &str| {
        let _ = sink_tx.send(line.to_string());
    }));

    let bridge = Bridge {
        shared: shared.clone(),
        ctl,
        out: out_tx,
    };
    {
        let fresh = snapshot(&bridge.lock());
        if let Ok(mut cache) = bridge.ctl.snapshot.lock() {
            *cache = fresh;
        }
    }
    Ok(bridge)
}

fn spawn_writer(stream: UnixStream, out_rx: Receiver<String>) {
    std::thread::Builder::new()
        .name("spavn-bridge-writer".into())
        .spawn(move || {
            let mut stream = stream;
            for line in out_rx {
                if stream.write_all(line.as_bytes()).is_err() {
                    break;
                }
                if stream.write_all(b"\n").is_err() || stream.flush().is_err() {
                    break;
                }
            }
        })
        .ok();
}

fn spawn_reader(
    stream: UnixStream,
    shared: Arc<Mutex<Shared>>,
    ctl: Arc<Ctl>,
    out: Sender<String>,
    token: Option<String>,
    hs: Sender<Result<(), String>>,
) {
    std::thread::Builder::new()
        .name("spavn-bridge-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(stream);
            if let Err(err) = handshake(&mut reader, &out, token.as_deref()) {
                let _ = hs.send(Err(err));
                return;
            }
            let _ = hs.send(Ok(()));
            serve(reader, shared, ctl, out);
        })
        .ok();
}

/// Reads the app's hello, verifies it, answers `hello_ok`.
fn handshake(
    reader: &mut BufReader<UnixStream>,
    out: &Sender<String>,
    token: Option<&str>,
) -> Result<(), String> {
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .map_err(|e| format!("bridge: read hello: {e}"))?;
    if n == 0 {
        return Err("bridge: app closed before hello".to_string());
    }
    let hello: Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("bridge: bad hello: {e}"))?;
    if hello.get("method").and_then(Value::as_str) != Some("hello") {
        return Err("bridge: expected hello".to_string());
    }
    let version = hello.get("v").and_then(Value::as_u64).unwrap_or(0);
    if version != events::VERSION as u64 {
        return Err(format!(
            "bridge: version mismatch (app {version}, cli {})",
            events::VERSION
        ));
    }
    if hello.get("role").and_then(Value::as_str) != Some("app") {
        return Err("bridge: hello role must be \"app\"".to_string());
    }
    let presented = hello.get("token").and_then(Value::as_str).unwrap_or("");
    match token {
        Some(expected) if ct_eq(expected, presented) => {}
        Some(_) => return Err("bridge: handshake rejected: wrong token".to_string()),
        None => return Err("bridge: no token: set SPAVN_BRIDGE_TOKEN or <dir>/bridge.env".to_string()),
    }
    let hello_ok = json!({
        "v": events::VERSION,
        "method": "hello_ok",
        "pid": std::process::id(),
        "cli": env!("CARGO_PKG_VERSION"),
    })
    .to_string();
    let _ = out.send(hello_ok);
    Ok(())
}

/// The request loop. Every branch answers exactly one frame; nothing here blocks on the agent
/// loop except by taking the session lock, and commands that would need a quiet session answer
/// `busy` from the atomic flag first.
fn serve(
    reader: BufReader<UnixStream>,
    shared: Arc<Mutex<Shared>>,
    ctl: Arc<Ctl>,
    out: Sender<String>,
) {
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(err) => {
                let _ = out.send(error_reply(&Value::Null, "bad_request", format!("bad json: {err}")));
                continue;
            }
        };
        let id = frame.get("id").cloned().unwrap_or(Value::Null);
        let method = frame.get("method").and_then(Value::as_str).unwrap_or("");
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        let busy = ctl.busy.load(Ordering::SeqCst);
        let answer = handle(method, &params, &shared, &ctl, busy);
        let _ = out.send(match answer {
            Ok(data) => reply(&id, data),
            Err((code, message)) => error_reply(&id, code, message),
        });
        if ctl.shutdown.load(Ordering::SeqCst) {
            break;
        }
    }
    // The app closed the socket: the driver polls this and exits without a further write.
    ctl.shutdown.store(true, Ordering::SeqCst);
}

type Handled = Result<Value, (&'static str, String)>;

fn handle(
    method: &str,
    params: &Value,
    shared: &Arc<Mutex<Shared>>,
    ctl: &Arc<Ctl>,
    busy: bool,
) -> Handled {
    match method {
        "ping" => Ok(json!({ "pong": true, "seq": events::current_seq() })),
        "state.get" => {
            // A turn owns the session lock; the cached snapshot is the answer while it runs.
            if busy {
                let cache = ctl.snapshot.lock().map(|g| g.clone()).unwrap_or(Value::Null);
                return Ok(cache);
            }
            let mut guard = lock(shared);
            *ctl.snapshot.lock().unwrap_or_else(|p| p.into_inner()) = snapshot(&guard);
            Ok(ctl.snapshot.lock().map(|g| g.clone()).unwrap_or(Value::Null))
        }
        "prompt.submit" => {
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                return Err(("bad_request", "prompt.submit needs a non-empty text".into()));
            }
            if busy {
                return Err(("busy", "a turn is already running".into()));
            }
            if let Some(spec) = params.get("session").and_then(Value::as_str) {
                if let Some(path) = resolve_session(spec) {
                    resume_locked(&mut lock(shared), &path)?;
                } else {
                    return Err(("not_found", format!("no session {spec}")));
                }
            }
            let turn = {
                let mut guard = lock(shared);
                guard.turns += 1;
                guard.turns
            };
            ctl.prompt_queue
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push_back(PendingPrompt {
                    turn,
                    text: text.clone(),
                });
            refresh(shared, ctl);
            Ok(json!({ "turn": turn, "accepted": true }))
        }
        "prompt.steer" => {
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                return Err(("bad_request", "prompt.steer needs a non-empty text".into()));
            }
            if !busy {
                return Err(("no_turn", "nothing to steer: no turn is running".into()));
            }
            crate::state::push_steer_message(text);
            Ok(json!({ "queued": crate::state::steer_message_count() }))
        }
        "turn.interrupt" => {
            let mode = params.get("mode").and_then(Value::as_str).unwrap_or("soft");
            match mode {
                "soft" => crate::state::request_soft_stop(),
                "hard" => crate::state::trigger_interrupt(),
                other => return Err(("bad_request", format!("unknown interrupt mode {other}"))),
            }
            Ok(json!({ "mode": mode }))
        }
        "approval.answer" => {
            let id = params.get("id").and_then(Value::as_str).unwrap_or("");
            let answer = params.get("answer").and_then(Value::as_str).unwrap_or("");
            if !matches!(answer, "approve" | "deny" | "sovereign" | "session") {
                return Err(("bad_request", format!("unknown answer {answer}")));
            }
            let sender = ctl
                .approvals
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(id)
                .cloned();
            match sender {
                Some(tx) => {
                    let _ = tx.send(answer.to_string());
                    Ok(json!({ "id": id, "answer": answer }))
                }
                None => Err(("unknown_request", format!("no pending approval {id}"))),
            }
        }
        "session.new" => {
            if busy {
                return Err(("busy", "a turn is running".into()));
            }
            let path = new_session_path();
            let mut guard = lock(shared);
            let sys = ChatMessage::system(cached_system_prompt(
                &guard.cwd,
                guard.endpoint.wants_compact_prompt(),
            ));
            append_message(&path, &sys);
            guard.messages = vec![sys];
            guard.session = path.clone();
            guard.session_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            guard.turns = 0;
            drop(guard);
            refresh(shared, ctl);
            Ok(json!({ "session": path.to_string_lossy() }))
        }
        "session.resume" => {
            if busy {
                return Err(("busy", "a turn is running".into()));
            }
            let spec = params.get("path").and_then(Value::as_str).unwrap_or("");
            let path = resolve_session(spec)
                .ok_or(("not_found", format!("no session {spec}")))?;
            {
                let mut guard = lock(shared);
                resume_locked(&mut guard, &path)?;
            }
            refresh(shared, ctl);
            Ok(json!({ "session": path.to_string_lossy() }))
        }
        "session.fork" => {
            if busy {
                return Err(("busy", "a turn is running".into()));
            }
            let forked = {
                let mut guard = lock(shared);
                let forked = fork_session(&guard.messages);
                guard.session = forked.clone();
                guard.turns = 0;
                forked
            };
            refresh(shared, ctl);
            Ok(json!({ "session": forked.to_string_lossy() }))
        }
        "slash.exec" => {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if !command.starts_with('/') {
                return Err(("bad_request", "slash.exec needs a /command".into()));
            }
            let outcome = {
                let mut guard = lock(shared);
                let mut guard = &mut *guard;
                dispatch_slash_command(
                    &command,
                    &guard.cwd,
                    &mut guard.endpoint,
                    &mut guard.ctx,
                    &mut guard.phase,
                    &mut guard.role,
                    &mut guard.show_thinking,
                    &mut guard.quota_badge,
                    &mut guard.prefill_input,
                    &mut guard.session,
                    &mut guard.session_name,
                    &mut guard.messages,
                )
            };
            refresh(shared, ctl);
            match outcome {
                SlashOutcome::ExitLoop => {
                    ctl.shutdown.store(true, Ordering::SeqCst);
                    Ok(json!({ "handled": true, "exit": true }))
                }
                SlashOutcome::Handled | SlashOutcome::NotSlash => {
                    Ok(json!({ "handled": true, "exit": false }))
                }
            }
        }
        "phase.set" => {
            if busy {
                return Err(("busy", "a turn is running".into()));
            }
            let raw = params.get("phase").and_then(Value::as_str).unwrap_or("");
            let next = AgentPhase::parse_alias(raw)
                .ok_or(("bad_request", format!("unknown phase {raw}")))?;
            let from = {
                let mut guard = lock(shared);
                let from = guard.phase;
                guard.phase = next;
                guard.ctx.phase = next;
                from
            };
            events::emit(Event::PhaseChanged {
                from: from.as_str().to_string(),
                to: next.as_str().to_string(),
            });
            refresh(shared, ctl);
            Ok(json!({ "phase": next.as_str() }))
        }
        "access.set" => {
            if busy {
                return Err(("busy", "a turn is running".into()));
            }
            let raw = params.get("access").and_then(Value::as_str).unwrap_or("");
            let mode = match raw {
                "safe" => AccessMode::Safe,
                "auto" => AccessMode::Auto,
                "sovereign" => AccessMode::Sovereign,
                other => return Err(("bad_request", format!("unknown access {other}"))),
            };
            {
                let mut guard = lock(shared);
                guard.ctx.access = mode;
            }
            events::emit(Event::Notice {
                level: "info",
                text: format!("access → {}", events::access_label(mode)),
            });
            refresh(shared, ctl);
            Ok(json!({ "access": events::access_label(mode) }))
        }
        "shutdown" => {
            ctl.shutdown.store(true, Ordering::SeqCst);
            Ok(json!({ "bye": true }))
        }
        other => Err(("unknown_method", format!("unknown method {other}"))),
    }
}

fn lock(shared: &Arc<Mutex<Shared>>) -> std::sync::MutexGuard<'_, Shared> {
    match shared.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn refresh(shared: &Arc<Mutex<Shared>>, ctl: &Arc<Ctl>) {
    let fresh = snapshot(&lock(shared));
    let mut cache = ctl.snapshot.lock().unwrap_or_else(|p| p.into_inner());
    *cache = fresh;
}

/// Resolves a session spec the way `--resume` does: a path, a filename, or `latest`/empty.
fn resolve_session(spec: &str) -> Option<PathBuf> {
    let spec = spec.trim();
    let dir = crate::session::sessions_dir();
    if spec.is_empty() || spec.eq_ignore_ascii_case("latest") {
        return crate::session::latest_session_path();
    }
    let direct = PathBuf::from(spec);
    if direct.exists() {
        return Some(direct);
    }
    for candidate in [dir.join(spec), dir.join(format!("{spec}.jsonl"))] {
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn resume_locked(shared: &mut Shared, path: &Path) -> Result<(), (&'static str, String)> {
    let loaded = load_session(&path.to_path_buf());
    if loaded.is_empty() {
        return Err(("not_found", format!("session {} has no messages", path.display())));
    }
    shared.messages = loaded;
    shared.session = path.to_path_buf();
    shared.session_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    shared.turns = 0;
    if let Some(first) = shared.messages.first_mut().filter(|m| m.role == "system") {
        *first = ChatMessage::system(cached_system_prompt(
            &shared.cwd,
            shared.endpoint.wants_compact_prompt(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shared() -> Shared {
        let endpoint = crate::router::resolve_endpoint("local");
        let ctx = ToolContext::new(
            AccessMode::Safe,
            "local".to_string(),
            PathBuf::from("/tmp"),
            AgentPhase::Code,
        );
        let session = PathBuf::from("/tmp/spavn-test.jsonl");
        Shared::new(endpoint, ctx, vec![ChatMessage::system("sys")], session)
    }

    #[test]
    fn token_comparison_is_exact_and_length_aware() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("", "x"));
    }

    #[test]
    fn snapshot_has_every_key_the_shell_reads() {
        let shared = test_shared();
        let snap = snapshot(&shared);
        for key in [
            "session", "cwd", "model", "endpoint", "phase", "access", "ctx", "turns", "sessions",
            "models", "tasks", "goals", "pins", "gates",
        ] {
            assert!(snap.get(key).is_some(), "missing {key} in {snap}");
        }
        assert_eq!(snap["phase"], "code");
        assert_eq!(snap["access"], "safe");
        assert!(snap["goals"].as_array().is_some_and(|a| a.is_empty()));
    }

    #[test]
    fn session_ready_reports_the_running_endpoint() {
        let mut shared = test_shared();
        let event = shared.session_ready();
        assert_eq!(event.kind(), "session.ready");
        let data = event.data();
        assert_eq!(data["phase"], "code");
        assert!(data["ctx"]["pct"].as_u64().is_some());
    }

    #[test]
    fn busy_and_failing_requests_answer_with_a_code() {
        let shared = Arc::new(Mutex::new(test_shared()));
        let ctl = Arc::new(Ctl {
            busy: AtomicBool::new(true),
            shutdown: AtomicBool::new(false),
            prompt_queue: Mutex::new(VecDeque::new()),
            approvals: Mutex::new(HashMap::new()),
            approval_seq: AtomicU64::new(1),
            snapshot: Mutex::new(json!({ "session": "cached" })),
            session_ok: Mutex::new(HashSet::new()),
        });
        let err = handle("prompt.submit", &json!({"text": "hi"}), &shared, &ctl, true)
            .expect_err("busy");
        assert_eq!(err.0, "busy");
        let err = handle("nope", &Value::Null, &shared, &ctl, false).expect_err("unknown");
        assert_eq!(err.0, "unknown_method");
        // While a turn runs, state.get serves the cached payload instead of blocking on the lock.
        let cached = handle("state.get", &Value::Null, &shared, &ctl, true).expect("cached");
        assert_eq!(cached["session"], "cached");
    }

    #[test]
    fn prompt_queue_feeds_the_driver_in_order() {
        let shared = Arc::new(Mutex::new(test_shared()));
        let ctl = Arc::new(Ctl {
            busy: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            prompt_queue: Mutex::new(VecDeque::new()),
            approvals: Mutex::new(HashMap::new()),
            approval_seq: AtomicU64::new(1),
            snapshot: Mutex::new(Value::Null),
            session_ok: Mutex::new(HashSet::new()),
        });
        handle("prompt.submit", &json!({"text": "one"}), &shared, &ctl, false).expect("ok");
        handle("prompt.submit", &json!({"text": "two"}), &shared, &ctl, false).expect("ok");
        let mut q = ctl.prompt_queue.lock().unwrap();
        assert_eq!(q.pop_front().unwrap().text, "one");
        assert_eq!(q.pop_front().unwrap().turn, 2);
    }

    #[test]
    fn phase_and_access_requests_move_the_shared_state() {
        let shared = Arc::new(Mutex::new(test_shared()));
        let ctl = Arc::new(Ctl {
            busy: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            prompt_queue: Mutex::new(VecDeque::new()),
            approvals: Mutex::new(HashMap::new()),
            approval_seq: AtomicU64::new(1),
            snapshot: Mutex::new(Value::Null),
            session_ok: Mutex::new(HashSet::new()),
        });
        handle("phase.set", &json!({"phase": "plan"}), &shared, &ctl, false).expect("ok");
        handle("access.set", &json!({"access": "sovereign"}), &shared, &ctl, false).expect("ok");
        let guard = shared.lock().unwrap();
        assert_eq!(guard.phase, AgentPhase::Plan);
        assert_eq!(guard.ctx.access, AccessMode::Sovereign);
        assert!(handle("access.set", &json!({"access": "root"}), &shared, &ctl, false).is_err());
    }
}
