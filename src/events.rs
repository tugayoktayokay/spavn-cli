//! The turn's facts, published as data.
//!
//! Rendering used to be the only consumer of what a turn does: a tool's exit code, its duration,
//! the token budget were painted on the terminal and then dropped. [`emit`] publishes the same
//! facts as one JSON object per line, in the envelope the desktop shell binds to real surfaces
//! instead of scraping ANSI (`docs/PROTOCOL.md` §2 in the desktop repo).
//!
//! Emission is strictly additive: with no sink installed every call returns immediately, so the
//! TUI's byte output is unchanged.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Where serialised events go. A sink must not block: the bridge hands each line to a writer
/// thread and returns.
pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

/// Wire version of the envelope. A host that sees another value must renegotiate, not guess.
pub const VERSION: u32 = 1;

static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(1);
static TURN: AtomicU64 = AtomicU64::new(0);

fn slot() -> &'static Mutex<Option<Sink>> {
    SINK.get_or_init(|| Mutex::new(None))
}

/// Installs the process-wide sink. Later calls replace it; the bridge owns the single one.
pub fn install(sink: Sink) {
    // A panicking sink would poison the slot and silence the rest of the run; the lock is only
    // held to swap an `Option`, so recovering the guard is safe here.
    let mut guard = match slot().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(sink);
}

/// True once a host asked for events — callers that would otherwise format for a human can skip
/// the work when nobody is listening.
pub fn is_installed() -> bool {
    match slot().lock() {
        Ok(g) => g.is_some(),
        Err(poisoned) => poisoned.into_inner().is_some(),
    }
}

/// Marks the 1-based turn the following events belong to. `0` means session-scoped.
pub fn set_turn(turn: u64) {
    TURN.store(turn, Ordering::Relaxed);
}

/// The envelope's `turn` field, as last set by [`set_turn`].
pub fn current_turn() -> u64 {
    TURN.load(Ordering::Relaxed)
}

/// `seq` of the last event published, `0` before the first one. `ping` reports it so a host can
/// tell a quiet turn from lost frames.
pub fn current_seq() -> u64 {
    SEQ.load(Ordering::Relaxed).saturating_sub(1)
}

/// One fact about the turn. `data` shapes are the wire contract; renaming a key breaks the shell,
/// so the JSON is spelled out by hand instead of derived from Rust field names.
#[derive(Debug, Clone)]
pub enum Event {
    /// First event after the handshake: what the session is and what it runs on.
    SessionReady {
        session: String,
        cwd: String,
        model: String,
        endpoint_local: bool,
        phase: String,
        access: String,
        ctx_tokens: usize,
        ctx_limit: usize,
        ctx_pct: u8,
    },
    /// A prompt was accepted and the agent loop started.
    TurnStarted { prompt: String, skills: Vec<String> },
    /// Reasoning deltas; only when thinking is on.
    ThinkingDelta { text: String },
    /// Append-only answer text within a turn.
    MessageDelta { text: String },
    /// Authoritative answer text; the host reconciles its deltas against this.
    MessageFinal { text: String },
    /// A tool call is about to run.
    ToolStarted {
        id: String,
        name: String,
        target: String,
        host: String,
        args_summary: String,
    },
    /// Partial tool output (bash/lsp/mcp), coalesced by the caller.
    ToolOutput {
        id: String,
        chunk: String,
        truncated: bool,
    },
    /// A tool finished: `ok` is false for a non-zero exit or a blocked call.
    ToolFinished {
        id: String,
        ok: bool,
        exit_code: Option<i32>,
        duration_ms: u64,
        added: Option<usize>,
        removed: Option<usize>,
        rows: Option<usize>,
    },
    /// The access gate is blocking on a human answer.
    ApprovalRequest {
        id: String,
        action: String,
        detail: String,
        access: String,
        options: Vec<&'static str>,
    },
    /// The gate resolved — by human answer, timeout, or disconnect.
    ApprovalResolved {
        id: String,
        answer: String,
        access: String,
    },
    /// The agent's task list changed.
    TasksUpdated { items: Vec<(String, String)> },
    /// Plan/code phase changed mid-session.
    PhaseChanged { from: String, to: String },
    /// Token accounting after an API response.
    Usage {
        prompt_tokens: usize,
        completion_tokens: usize,
        ctx_tokens: usize,
        ctx_pct: u8,
        quota: Option<String>,
    },
    /// A line the human would otherwise have read on the terminal.
    Notice { level: &'static str, text: String },
    /// A recoverable failure; the turn may continue.
    Error {
        message: String,
        retryable: bool,
        tool_call_id: Option<String>,
    },
    /// The turn ended. `ok` is false on an error or an interrupt.
    TurnFinished {
        ok: bool,
        steps: usize,
        duration_ms: u64,
    },
    /// Last event the child sends before the socket closes.
    BridgeClosed {
        reason: &'static str,
        code: Option<i32>,
    },
}

impl Event {
    /// The `type` field. Spelled here so the match is exhaustive: a new variant cannot ship
    /// without a name on the wire.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::SessionReady { .. } => "session.ready",
            Event::TurnStarted { .. } => "turn.started",
            Event::ThinkingDelta { .. } => "thinking.delta",
            Event::MessageDelta { .. } => "message.delta",
            Event::MessageFinal { .. } => "message.final",
            Event::ToolStarted { .. } => "tool.started",
            Event::ToolOutput { .. } => "tool.output",
            Event::ToolFinished { .. } => "tool.finished",
            Event::ApprovalRequest { .. } => "approval.request",
            Event::ApprovalResolved { .. } => "approval.resolved",
            Event::TasksUpdated { .. } => "tasks.updated",
            Event::PhaseChanged { .. } => "phase.changed",
            Event::Usage { .. } => "usage",
            Event::Notice { .. } => "notice",
            Event::Error { .. } => "error",
            Event::TurnFinished { .. } => "turn.finished",
            Event::BridgeClosed { .. } => "bridge.closed",
        }
    }

    /// The `data` object. Absent optionals are omitted rather than sent as `null`, so the host can
    /// use `key in data` for "present".
    pub fn data(&self) -> Value {
        match self {
            Event::SessionReady {
                session,
                cwd,
                model,
                endpoint_local,
                phase,
                access,
                ctx_tokens,
                ctx_limit,
                ctx_pct,
            } => json!({
                "session": session,
                "cwd": cwd,
                "model": model,
                "endpoint": { "key": model, "local": endpoint_local },
                "phase": phase,
                "access": access,
                "ctx": { "tokens": ctx_tokens, "limit": ctx_limit, "pct": ctx_pct },
            }),
            Event::TurnStarted { prompt, skills } => json!({ "prompt": prompt, "skills": skills }),
            Event::ThinkingDelta { text } => json!({ "text": text }),
            Event::MessageDelta { text } => json!({ "text": text }),
            Event::MessageFinal { text } => json!({ "text": text }),
            Event::ToolStarted {
                id,
                name,
                target,
                host,
                args_summary,
            } => json!({
                "id": id,
                "name": name,
                "target": target,
                "host": host,
                "args_summary": args_summary,
            }),
            Event::ToolOutput {
                id,
                chunk,
                truncated,
            } => json!({ "id": id, "chunk": chunk, "truncated": truncated }),
            Event::ToolFinished {
                id,
                ok,
                exit_code,
                duration_ms,
                added,
                removed,
                rows,
            } => {
                let mut data = json!({ "id": id, "ok": ok, "duration_ms": duration_ms });
                if let Some(code) = exit_code {
                    data["exit_code"] = json!(code);
                }
                if added.is_some() || removed.is_some() {
                    data["diff"] = json!({
                        "added": added.unwrap_or(0),
                        "removed": removed.unwrap_or(0),
                    });
                }
                if let Some(rows) = rows {
                    data["rows"] = json!(rows);
                }
                data
            }
            Event::ApprovalRequest {
                id,
                action,
                detail,
                access,
                options,
            } => json!({
                "id": id,
                "action": action,
                "detail": detail,
                "access": access,
                "options": options,
            }),
            Event::ApprovalResolved { id, answer, access } => {
                json!({ "id": id, "answer": answer, "access": access })
            }
            Event::TasksUpdated { items } => json!({
                "items": items
                    .iter()
                    .map(|(title, status)| json!({ "title": title, "status": status }))
                    .collect::<Vec<_>>(),
            }),
            Event::PhaseChanged { from, to } => json!({ "from": from, "to": to }),
            Event::Usage {
                prompt_tokens,
                completion_tokens,
                ctx_tokens,
                ctx_pct,
                quota,
            } => {
                let mut data = json!({
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "ctx_tokens": ctx_tokens,
                    "ctx_pct": ctx_pct,
                });
                if let Some(quota) = quota.as_ref().filter(|q| !q.is_empty()) {
                    data["quota"] = json!(quota);
                }
                data
            }
            Event::Notice { level, text } => json!({ "level": level, "text": text }),
            Event::Error {
                message,
                retryable,
                tool_call_id,
            } => {
                let mut data = json!({ "message": message, "retryable": retryable });
                if let Some(id) = tool_call_id {
                    data["tool_call_id"] = json!(id);
                }
                data
            }
            Event::TurnFinished {
                ok,
                steps,
                duration_ms,
            } => json!({ "ok": ok, "steps": steps, "duration_ms": duration_ms }),
            Event::BridgeClosed { reason, code } => {
                let mut data = json!({ "reason": reason });
                if let Some(code) = code {
                    data["code"] = json!(code);
                }
                data
            }
        }
    }

    /// Serialises to a single line: envelope plus data, no embedded newline.
    pub fn line(&self) -> Option<String> {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let envelope = json!({
            "type": self.kind(),
            "v": VERSION,
            "seq": seq,
            "ts": rfc3339_now(),
            "turn": current_turn(),
            "data": self.data(),
        });
        serde_json::to_string(&envelope).ok()
    }
}

/// Publish `event`. Cheap and silent when no host is attached.
pub fn emit(event: Event) {
    if !is_installed() {
        return;
    }
    let sink = match slot().lock() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    if let Some(sink) = sink {
        if let Some(line) = event.line() {
            sink(&line);
        }
    }
}

/// Human-facing access name, as the shell's chip spells it.
pub fn access_label(mode: crate::tools::AccessMode) -> String {
    match mode {
        crate::tools::AccessMode::Safe => "safe",
        crate::tools::AccessMode::Auto => "auto",
        crate::tools::AccessMode::Sovereign => "sovereign",
    }
    .to_string()
}

/// The local UTC offset in minutes, read once. A wrong clock offset must not silence telemetry, so
/// failure degrades to UTC rather than to an error.
fn local_offset_minutes() -> i32 {
    static OFFSET: OnceLock<i32> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let raw = std::process::Command::new("/bin/date")
            .arg("+%z")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        parse_offset(&raw).unwrap_or(0)
    })
}

/// `+0300` → 180, `-0730` → -450, anything else → `None`.
fn parse_offset(raw: &str) -> Option<i32> {
    let bytes = raw.as_bytes();
    if bytes.len() != 5 {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours: i32 = raw.get(1..3)?.parse().ok()?;
    let minutes: i32 = raw.get(3..5)?.parse().ok()?;
    Some(sign * (hours * 60 + minutes))
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's `civil_from_days`; the standard
/// library has no local-time formatting and pulling a crate in for it would double the dep count.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// RFC 3339 with the local offset, second-free millisecond precision: `2026-11-06T00:14:03.221+03:00`.
pub fn rfc3339_now() -> String {
    let offset = local_offset_minutes() as i64 * 60;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    rfc3339_from_unix(now, offset)
}

fn rfc3339_from_unix(unix_secs: i64, offset_secs: i64) -> String {
    let local = unix_secs + offset_secs;
    let days = local.div_euclid(86_400);
    let secs_of_day = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let abs = offset_secs.abs();
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis())
        .unwrap_or(0);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{}{:02}:{:02}",
        year,
        month,
        day,
        hour,
        minute,
        second,
        millis,
        sign,
        abs / 3600,
        (abs % 3600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_nothing_without_a_sink() {
        // No `install` in this process: the call must not panic and must not format.
        emit(Event::Notice {
            level: "info",
            text: "hello".into(),
        });
    }

    #[test]
    fn current_seq_reports_the_last_published_frame() {
        let line = Event::Notice {
            level: "info",
            text: "x".into(),
        }
        .line()
        .expect("serialises");
        let value: Value = serde_json::from_str(&line).expect("json line");
        // Other tests publish in parallel, so the counter may already be past this frame.
        assert!(current_seq() >= value["seq"].as_u64().unwrap());
    }

    #[test]
    fn envelope_matches_the_wire_contract() {
        let line = Event::ToolFinished {
            id: "1".into(),
            ok: true,
            exit_code: Some(0),
            duration_ms: 12,
            added: Some(2),
            removed: None,
            rows: None,
        }
        .line()
        .expect("serialises");
        assert!(line.starts_with('{') && !line.contains('\n'), "{line}");
        let value: Value = serde_json::from_str(&line).expect("json line");
        assert_eq!(value["type"], "tool.finished");
        assert_eq!(value["v"], VERSION);
        assert_eq!(value["data"]["ok"], true);
        assert_eq!(value["data"]["exit_code"], 0);
        assert_eq!(value["data"]["diff"]["added"], 2);
        // Absent optionals are omitted, not nulled.
        assert!(value["data"].get("rows").is_none(), "{line}");
    }

    #[test]
    fn every_kind_is_in_the_protocol_catalogue() {
        // `PROTOCOL.md` §2.1 in the desktop repo. `usage`, `notice` and `error` are undotted there.
        const CATALOGUE: [&str; 17] = [
            "session.ready",
            "turn.started",
            "thinking.delta",
            "message.delta",
            "message.final",
            "tool.started",
            "tool.output",
            "tool.finished",
            "approval.request",
            "approval.resolved",
            "tasks.updated",
            "phase.changed",
            "usage",
            "notice",
            "error",
            "turn.finished",
            "bridge.closed",
        ];
        let events = [
            Event::SessionReady {
                session: "/tmp/s.jsonl".into(),
                cwd: "/tmp".into(),
                model: "m".into(),
                endpoint_local: true,
                phase: "code".into(),
                access: "safe".into(),
                ctx_tokens: 1,
                ctx_limit: 2,
                ctx_pct: 50,
            },
            Event::TurnStarted {
                prompt: "p".into(),
                skills: vec![],
            },
            Event::ThinkingDelta { text: "t".into() },
            Event::MessageDelta { text: "d".into() },
            Event::MessageFinal { text: "f".into() },
            Event::ToolStarted {
                id: "1".into(),
                name: "read".into(),
                target: "a.rs".into(),
                host: "local".into(),
                args_summary: "read a.rs".into(),
            },
            Event::ToolOutput {
                id: "1".into(),
                chunk: "x".into(),
                truncated: false,
            },
            Event::ToolFinished {
                id: "1".into(),
                ok: true,
                exit_code: Some(0),
                duration_ms: 3,
                added: None,
                removed: None,
                rows: None,
            },
            Event::ApprovalRequest {
                id: "a".into(),
                action: "bash".into(),
                detail: "ls".into(),
                access: "safe".into(),
                options: vec!["approve", "deny"],
            },
            Event::ApprovalResolved {
                id: "a".into(),
                answer: "approve".into(),
                access: "safe".into(),
            },
            Event::TasksUpdated {
                items: vec![("write bridge.rs".into(), "pending".into())],
            },
            Event::PhaseChanged {
                from: "plan".into(),
                to: "code".into(),
            },
            Event::Usage {
                prompt_tokens: 10,
                completion_tokens: 2,
                ctx_tokens: 12,
                ctx_pct: 1,
                quota: None,
            },
            Event::Notice {
                level: "info",
                text: "n".into(),
            },
            Event::Error {
                message: "boom".into(),
                retryable: true,
                tool_call_id: None,
            },
            Event::TurnFinished {
                ok: true,
                steps: 1,
                duration_ms: 5,
            },
            Event::BridgeClosed {
                reason: "shutdown",
                code: Some(0),
            },
        ];
        assert_eq!(events.len(), CATALOGUE.len(), "one sample per variant");
        for event in events {
            assert!(CATALOGUE.contains(&event.kind()), "{}", event.kind());
            serde_json::to_string(&event.data()).expect("data serialises");
        }
    }

    #[test]
    fn seq_increases_and_turn_is_stamped() {
        set_turn(7);
        let a: Value = serde_json::from_str(
            &Event::Notice {
                level: "info",
                text: "a".into(),
            }
            .line()
            .unwrap(),
        )
        .unwrap();
        let b: Value = serde_json::from_str(
            &Event::Notice {
                level: "info",
                text: "b".into(),
            }
            .line()
            .unwrap(),
        )
        .unwrap();
        assert!(b["seq"].as_u64().unwrap() > a["seq"].as_u64().unwrap());
        assert_eq!(a["turn"], 7);
        set_turn(0);
    }

    #[test]
    fn install_routes_every_event_to_the_sink() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        install(Arc::new(move |line: &str| {
            sink_seen.lock().unwrap().push(line.to_string());
        }));
        emit(Event::PhaseChanged {
            from: "plan".into(),
            to: "code".into(),
        });
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(is_installed());
    }

    #[test]
    fn rfc3339_carries_local_offset_and_civil_date() {
        // 2026-11-06T00:14:03+03:00 is 2026-11-05T21:14:03Z.
        let stamped = rfc3339_from_unix(1_793_913_243, 3 * 3600);
        assert!(stamped.starts_with("2026-11-06T00:14:03."), "{stamped}");
        assert!(stamped.ends_with("+03:00"), "{stamped}");
        assert_eq!(parse_offset("+0300"), Some(180));
        assert_eq!(parse_offset("-0730"), Some(-450));
        assert_eq!(parse_offset("junk"), None);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }
}
