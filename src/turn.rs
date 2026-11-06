use spavn::config::{load_user_config, tr_en};
use spavn::hud::{
    render_assistant_footer, render_assistant_header, term_size, truncate_ansi_to_width,
    truncate_ansi_to_width_middle, visible_width, LiveSpinner, MarkdownStreamRenderer,
};
use spavn::router::{resolve_endpoint, stream_chat_completion, ChatMessage, ModelEndpoint};
use spavn::session::{
    append_message, evict_old_skill_blocks, perform_manual_compact, rewrite_session,
    safe_utf8_prefix, safe_utf8_suffix, summarize_tool_args, tool_progress_target,
};
use spavn::spavn_engine::AgentPhase;
use spavn::state::{
    advance_live_tasks_on_tool, clear_finished_live_tasks, clear_interrupted, is_interrupted,
    pop_steer_messages, sync_live_tasks_from_text,
};
use spavn::tools::{execute_tool, AccessMode, ToolContext};
use spavn::{hud, tools};
use std::cell::RefCell;
use std::path::PathBuf;

pub(crate) const CYAN: &str = "\x1b[36m";
pub(crate) const DIM: &str = "\x1b[2m";
pub(crate) const BOLD: &str = "\x1b[1m";
pub(crate) const GREEN: &str = "\x1b[32m";
pub(crate) const YELLOW: &str = "\x1b[33m";
pub(crate) const MAGENTA: &str = "\x1b[35m";
pub(crate) const RED: &str = "\x1b[31m";
pub(crate) const RESET: &str = "\x1b[0m";

/// Local model context guard: max read-only file reads executed per agent turn.
/// Beyond this budget the turn stops dispatching reads and asks the model to analyze
/// what it already gathered, avoiding local KV-cache prefill stalls on huge parallel reads.
pub(crate) const LOCAL_READ_BUDGET: usize = 5;

pub(crate) fn is_read_tool_name(name: &str) -> bool {
    matches!(
        name.trim().to_lowercase().as_str(),
        "read" | "read_file" | "cat" | "view"
    )
}

pub(crate) fn is_fast_inline_tool(name: &str) -> bool {
    matches!(
        name.trim().to_lowercase().as_str(),
        "read"
            | "read_file"
            | "cat"
            | "view"
            | "edit"
            | "edit_file"
            | "replace"
            | "patch"
            | "write"
            | "write_file"
            | "search"
            | "grep"
            | "rg"
            | "glob"
            | "find_files"
            | "list_files"
            | "file_glob"
            | "lsp"
    )
}

/// A call may join a parallel batch only when it is read-only *and* cannot reach the approval
/// prompt: two simultaneous prompts read the same stdin and swallow each other's keystrokes.
pub(crate) fn is_batchable(name: &str, ctx: &ToolContext) -> bool {
    ctx.access == AccessMode::Sovereign && tools::parallel_safe_tool(name)
}

/// Extracts `(normalized_path, span_key)` from a `read`, `edit`, or `write` tool call's JSON args
/// so `compact_messages_for_model` can supersede stale reads of the same file/region before `cutoff`.
pub(crate) fn extract_file_op_key(tool_name: &str, raw_args: &str) -> Option<(String, String)> {
    let kind = match tool_name.trim() {
        "read" | "read_file" | "cat" | "view" => "read",
        "edit" | "edit_file" | "replace" | "patch" | "write" | "write_file" => "mutate",
        _ => return None,
    };
    let val = serde_json::from_str::<serde_json::Value>(raw_args).ok()?;
    let raw_path = val
        .get("path")
        .or_else(|| val.get("file_path"))
        .or_else(|| val.get("file"))
        .and_then(|v| v.as_str())?
        .trim();
    if raw_path.is_empty() {
        return None;
    }
    let (base_path, inline_span) = match raw_path.rsplit_once(':') {
        Some((b, s))
            if !b.is_empty()
                && s.chars()
                    .next()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false) =>
        {
            (b.trim(), s.trim())
        }
        _ => (raw_path, ""),
    };
    if kind == "mutate" {
        return Some((base_path.to_string(), "*".to_string()));
    }
    let start = val.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0);
    let end = val.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0);
    let span_key = if !inline_span.is_empty() {
        inline_span.to_string()
    } else if start == 0 && end == 0 {
        "*".to_string()
    } else {
        format!("{}-{}", start, end)
    };
    Some((base_path.to_string(), span_key))
}

pub(crate) fn compact_messages_for_model(
    messages: &[ChatMessage],
    is_local: bool,
    max_ctx_tokens: usize,
) -> Vec<ChatMessage> {
    let raw_tok = hud::estimate_messages_tokens(messages);
    let prune_budget = if max_ctx_tokens == 0 {
        16_000
    } else {
        (max_ctx_tokens / 4).clamp(4_000, 64_000)
    };
    let prune_old_tools = raw_tok >= prune_budget;
    let len = messages.len();
    let last_user_idx = messages
        .iter()
        .rposition(|m| {
            m.role == "user"
                && !m
                    .content
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("[SPAVN VERIFICATION GATE]")
        })
        .unwrap_or_else(|| {
            messages
                .iter()
                .rposition(|m| m.role == "user")
                .unwrap_or(len)
        });
    // Anchor pruning & reasoning eviction to `last_user_idx` (stepping only every 16 messages
    // within marathon turns) so the prompt prefix stays 100% byte-identical across consecutive
    // tool calls and `llama-server` KV-cache never re-evaluates the active turn prefix.
    let turn_anchor = last_user_idx + ((len.saturating_sub(last_user_idx)) / 16) * 12;
    let cutoff = if prune_old_tools {
        if is_local {
            turn_anchor
        } else if raw_tok < (max_ctx_tokens * 60) / 100 {
            // Never shake tool outputs inside the active turn on large-window models while under 60% capacity.
            last_user_idx.min(len.saturating_sub(20))
        } else {
            len.saturating_sub(20).min(turn_anchor)
        }
    } else {
        0
    };
    // Keep `messages[0]` static for local models so KV-cache from token 0 is never invalidated.
    let live_sigs = if !is_local && prune_old_tools {
        tools::build_live_project_signatures(".")
    } else {
        String::new()
    };
    let keep_reasoning_from = if is_local { turn_anchor } else { last_user_idx };
    // Supersede Pruning: identify `read` tool calls before `turn_anchor` whose target
    // file/region was later re-read or mutated (`edit`/`write`). Anchored to `turn_anchor` so
    // the KV-cache prefix stays byte-identical between anchor steps.
    let supersede_cutoff = if is_local {
        turn_anchor
    } else {
        turn_anchor.max(cutoff)
    };
    let mut read_calls: Vec<(usize, String, String, String)> = Vec::new();
    let mut latest_full_op: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut latest_span_op: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for (idx, m) in messages.iter().enumerate() {
        if m.role == "assistant" {
            if let Some(tcs) = &m.tool_calls {
                for tc in tcs {
                    let tname = tc.function.name.trim();
                    if let Some((base_path, span_key)) =
                        extract_file_op_key(tname, &tc.function.arguments)
                    {
                        if span_key == "*" {
                            latest_full_op.insert(base_path.clone(), idx);
                        }
                        latest_span_op.insert((base_path.clone(), span_key.clone()), idx);
                        if is_read_tool_name(tname) && idx < supersede_cutoff {
                            read_calls.push((idx, tc.id.clone(), base_path, span_key));
                        }
                    }
                }
            }
        }
    }
    let mut superseded_ids: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (call_idx, call_id, base_path, span_key) in read_calls {
        let by_full = latest_full_op
            .get(&base_path)
            .map(|&later| later > call_idx)
            .unwrap_or(false);
        let by_span = latest_span_op
            .get(&(base_path.clone(), span_key))
            .map(|&later| later > call_idx)
            .unwrap_or(false);
        if by_full || by_span {
            superseded_ids.insert(call_id, base_path);
        }
    }

    messages
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let mut cloned = m.clone();
            if idx == 0 && m.role == "system" && !live_sigs.is_empty() {
                if let Some(ref sys_text) = cloned.content {
                    if !sys_text.contains("[SPAVN LIVE PROJECT SIGNATURES]") {
                        cloned.content = Some(format!(
                            "{}\n\n[SPAVN LIVE PROJECT SIGNATURES — Use exact symbol/type names below across files]:\n{}",
                            sys_text, live_sigs
                        ));
                    }
                }
            }

            if idx < keep_reasoning_from {
                cloned.reasoning_content = None;
            }

            if m.role == "user" && idx != last_user_idx {
                if let Some(ref content) = cloned.content {
                    if content.contains("<active_skill") {
                        cloned.content = Some(evict_old_skill_blocks(content));
                    }
                }
            }
            if m.role == "tool" && idx < supersede_cutoff {
                if let Some(tc_id) = m.tool_call_id.as_deref() {
                    if let Some(path_label) = superseded_ids.get(tc_id) {
                        cloned.content = Some(format!(
                            "[SPAVN Supersede Prune: Older read of `{}` superseded by newer read/edit in context]",
                            path_label
                        ));
                        return cloned;
                    }
                }
            }
            if idx > 0 && idx < cutoff {
                if m.role == "tool" {
                    if let Some(content) = &m.content {
                        if content.len() > 600 {
                            let head = safe_utf8_prefix(content, 260);
                            let tail = safe_utf8_suffix(content, 160);
                            let skeleton = extract_compact_ast_skeleton(content);
                            cloned.content = Some(format!(
                                "{}\n...[SPAVN Context Compactor: {} bytes pruned from older turn{}]...\n{}",
                                head,
                                content.len().saturating_sub(420),
                                skeleton,
                                tail
                            ));
                        }
                    }
                } else if m.role == "assistant" {
                    if let Some(tcs) = &mut cloned.tool_calls {
                        for tc in tcs.iter_mut() {
                            if tc.function.arguments.len() > 800 {
                                let path_hint = serde_json::from_str::<serde_json::Value>(
                                    &tc.function.arguments,
                                )
                                .ok()
                                .and_then(|v| {
                                    v.get("path")
                                        .or_else(|| v.get("command"))
                                        .and_then(|s| s.as_str())
                                        .map(|s| safe_utf8_prefix(s, 120).to_string())
                                })
                                .unwrap_or_default();
                                tc.function.arguments = match tc.function.name.as_str() {
                                    "edit" => serde_json::json!({
                                        "path": path_hint,
                                        "old_text": "/* compacted */",
                                        "new_text": "/* compacted */"
                                    }),
                                    "write" => serde_json::json!({
                                        "path": path_hint,
                                        "content": "/* compacted */"
                                    }),
                                    "bash" => serde_json::json!({
                                        "command": path_hint
                                    }),
                                    _ => serde_json::json!({
                                        "path": path_hint
                                    }),
                                }
                                .to_string();
                            }
                        }
                    }
                }
            }
            cloned
        })
        .collect()
}

/// Canonicalizes a tool call into `name:canonical_json_args` so key-order permutations
/// and whitespace differences match identically. Transparent meta-tools (`memory`, `skill`)
/// return `None` so they never reset the consecutive repeat chain.
pub(crate) fn canonical_tool_call_sig(name: &str, raw_args: &str) -> Option<String> {
    let trimmed_name = name.trim();
    if matches!(trimmed_name, "memory" | "skill" | "recall_memory") {
        return None;
    }
    let canon_args = serde_json::from_str::<serde_json::Value>(raw_args.trim())
        .ok()
        .and_then(|v| serde_json::to_string(&v).ok())
        .unwrap_or_else(|| raw_args.trim().to_string());
    Some(format!("{}:{}", trimmed_name, canon_args))
}

/// Extracts up to 8 structural AST signature lines (`fn`, `struct`, `def`, `class`, `interface`)
/// from the middle of a large tool output before it gets compacted so symbol names and line numbers survive.
fn extract_compact_ast_skeleton(content: &str) -> String {
    let mut sigs = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        let body = match trimmed.split_once(':') {
            Some((num, rest)) if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) => {
                rest.trim()
            }
            _ => trimmed,
        };
        if body.starts_with("pub fn ")
            || body.starts_with("fn ")
            || body.starts_with("pub async fn ")
            || body.starts_with("async fn ")
            || body.starts_with("pub struct ")
            || body.starts_with("struct ")
            || body.starts_with("pub enum ")
            || body.starts_with("enum ")
            || body.starts_with("pub trait ")
            || body.starts_with("trait ")
            || body.starts_with("def ")
            || body.starts_with("async def ")
            || body.starts_with("class ")
            || body.starts_with("export function ")
            || body.starts_with("function ")
            || body.starts_with("export interface ")
            || body.starts_with("interface ")
        {
            let short: String = trimmed.chars().take(80).collect();
            sigs.push(short);
            if sigs.len() >= 8 {
                break;
            }
        }
    }
    if sigs.is_empty() {
        String::new()
    } else {
        format!(" | AST Signatures: {}", sigs.join(" ; "))
    }
}

/// True when the reply announces an action ("let me read...", "şimdi ... okuyacağım", trailing `:`)
/// but stops without calling a tool. Questions and "let me know" sign-offs are final answers.
pub(crate) fn is_premature_stop(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('?') || trimmed.ends_with("```") {
        return false;
    }
    let tail_lines: Vec<&str> = trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let last_line = tail_lines.last().copied().unwrap_or("");
    let lower_tail = tail_lines.join(" ").to_lowercase();
    let announces_en = (lower_tail.contains("let me ") && !lower_tail.contains("let me know"))
        || lower_tail.contains("i will now ")
        || lower_tail.contains("i'll now ");
    let announces_tr = lower_tail.contains("şimdi ")
        && (lower_tail.contains("yapacağım")
            || lower_tail.contains("düzenleyeceğim")
            || lower_tail.contains("okuyacağım")
            || lower_tail.contains("inceleyeceğim"));
    last_line.ends_with(':') || announces_tr || announces_en
}

/// Detects when a model in CODE phase investigated files (`total_turn_steps > 0`), made zero file
/// mutations, and dumped a multi-line fenced code block with file/symbol markers instead of calling `edit`/`write`.
pub(crate) fn is_phantom_code_block_stop(
    text: &str,
    phase: AgentPhase,
    total_turn_steps: usize,
    has_mutations: bool,
) -> bool {
    if phase != AgentPhase::Code || total_turn_steps == 0 || has_mutations {
        return false;
    }
    let trimmed = text.trim();
    if trimmed.contains('?') {
        return false;
    }
    let mut in_fence = false;
    let mut code_lines = 0usize;
    let mut has_code_signature = false;
    for line in trimmed.lines() {
        let t = line.trim();
        if let Some(lang) = t.strip_prefix("```") {
            if !in_fence {
                in_fence = true;
                let l = lang.trim().to_ascii_lowercase();
                if matches!(
                    l.as_str(),
                    "rust"
                        | "rs"
                        | "go"
                        | "python"
                        | "py"
                        | "ts"
                        | "tsx"
                        | "js"
                        | "jsx"
                        | "c"
                        | "cpp"
                ) {
                    has_code_signature = true;
                }
            } else {
                in_fence = false;
            }
            continue;
        }
        if in_fence {
            code_lines += 1;
            if t.starts_with("fn ")
                || t.starts_with("pub fn ")
                || t.starts_with("func ")
                || t.starts_with("def ")
                || t.starts_with("export ")
            {
                has_code_signature = true;
            }
        }
    }
    has_code_signature && code_lines >= 6
}

pub(crate) fn run_agent_turn(
    endpoint: &ModelEndpoint,
    ctx: &mut ToolContext,
    messages: &mut Vec<ChatMessage>,
    session_file: &PathBuf,
    show_thinking: bool,
    quota_badge: Option<&str>,
    quiet: bool,
) -> Result<(), String> {
    // The window title carries the turn: the state glyph animates while this guard lives and
    // falls back to the static glyph on every exit path, including the `?` early returns.
    let _title = hud::WorkingGuard::new();
    clear_interrupted();
    // A bar left at `[3/3]` from the previous request describes finished work; keeping it up
    // made the next, unrelated turn look like it was still mid-plan.
    clear_finished_live_tasks();
    hud::set_sticky_dock_status(
        endpoint,
        ctx.access,
        &ctx.phase,
        &ctx.default_host,
        quota_badge.unwrap_or("aktif"),
    );
    let turn_prompt = messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let tools_spec = tools::tool_definitions_for_turn(ctx.phase, endpoint.is_local(), &turn_prompt);
    let mut last_tool_sig = String::new();
    let mut repeat_count = 0usize;
    let mut canonical_sig_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut unverified_edits = false;
    let mut compiler_verified_in_edit = false;
    let mut verification_nudged = false;
    let mut modified_files_in_turn: Vec<String> = Vec::new();
    let mut auto_verify_rounds = 0usize;
    // Last diagnostic handed to the model by the zero-stop gate; a repeat means the previous
    // round changed nothing relevant and re-sending it only costs an API turn.
    let mut last_gate_err: Option<String> = None;
    let mut empty_stop_retries = 0usize;
    let mut length_stop_retries = 0usize;
    let mut premature_stop_retries = 0usize;
    let mut last_observed_total_tokens = 0usize;
    let mut consecutive_investigation_steps = 0usize;
    let mut total_turn_steps = 0usize;
    let mut focus_gate_nudged = false;
    let mut file_read_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let cfg = load_user_config();
    // Unpaired Tool-Call Recovery: if the last message in history is an assistant
    // message with tool_calls (e.g. from an interrupted session), resolve them before calling LLM.
    if let Some(last_msg) = messages.last().cloned() {
        if last_msg.role == "assistant" {
            if let Some(pending_tcs) = last_msg.tool_calls {
                for tc in pending_tcs {
                    let recovery_msg = ChatMessage::tool_result(
                        tc.id,
                        tc.function.name,
                        "[SPAVN Turn Recovery: Previous tool execution was interrupted before completion.]".to_string(),
                    );
                    append_message(session_file, &recovery_msg);
                    messages.push(recovery_msg);
                }
            }
        }
    }

    // Unbounded Turn Loop: continues until all tool calls, verification gates,
    // recovery retries, and queued steering/follow-up messages settle.
    loop {
        if is_interrupted() {
            if !quiet {
                println!("\n{}⚡ [İşlem Esc/Ctrl+C ile durduruldu]{}", YELLOW, RESET);
            }
            return Ok(());
        }

        // Inject any live steering messages typed by the user while the turn was running!
        let steered = pop_steer_messages();
        if !steered.is_empty() {
            for sm in steered {
                println!(
                    "\n{}⚡ [Canlı Müdahale (Steer) Aktif Tura Enjekte Edildi]: {}{}{}",
                    YELLOW, BOLD, sm, RESET
                );
                let steer_msg = ChatMessage::user(format!(
                    "[LIVE USER STEERING INSTRUCTION — Prioritize this immediately]: {}",
                    sm
                ));
                append_message(session_file, &steer_msg);
                messages.push(steer_msg);
            }
        }

        let escalated_cloud = endpoint.is_local() && repeat_count >= 2;
        let step_endpoint = if escalated_cloud {
            if !quiet {
                println!(
                    "\n{}⚡ [SPAVN Hibrit Escalation: Yerel model döngüye girdi -> Bu adım {} ile çözülüyor...]{}",
                    MAGENTA, cfg.fallback_cloud_model, RESET
                );
            }
            resolve_endpoint(&cfg.fallback_cloud_model)
        } else {
            endpoint.clone()
        };

        let max_ctx_tokens = step_endpoint.context_window_tokens();
        let mut view_messages =
            compact_messages_for_model(messages, step_endpoint.is_local(), max_ctx_tokens);
        let mut view_tok = hud::estimate_messages_tokens(&view_messages);
        let effective_tok = view_tok.max(last_observed_total_tokens);
        let is_free_or_flash = step_endpoint.is_low_cost_tier();
        let token_cap = if is_free_or_flash {
            64_000
        } else if max_ctx_tokens >= 500_000 {
            240_000
        } else {
            120_000
        };
        let compact_threshold = ((max_ctx_tokens * 80) / 100).min(token_cap);
        // Never run destructive full compaction in the middle of an active turn (`total_turn_steps > 0`)
        // merely because `messages.len()` grew; only compact at turn start (`total_turn_steps == 0`) or
        // if token usage hits the hard threshold. During turn start, shake older turns' oversized tool
        // outputs to recoverable spill files first (OMP-style Context Shaking).
        if total_turn_steps == 0 && effective_tok >= 24_000 {
            let active_u_idx = messages
                .iter()
                .rposition(|m| {
                    m.role == "user" && !m.content.as_deref().unwrap_or("").starts_with("[SPAVN ")
                })
                .unwrap_or(messages.len());
            let mut shaken_any = false;
            for (idx, m) in messages.iter_mut().enumerate().skip(1) {
                if idx >= active_u_idx || m.role != "tool" {
                    continue;
                }
                if let Some(c) = &m.content {
                    if c.len() > 1200
                        && !c.contains("[SPAVN Shaken")
                        && !c.contains("...[Compacted")
                    {
                        let est_tok = c.len() / 4 + 1;
                        let head = safe_utf8_prefix(c, 240);
                        let tail = safe_utf8_suffix(c, 140);
                        let spill_note = spavn::spill::spill_output("shaken-turn", c)
                            .map(|p| format!(" — recover: {}", p.display()))
                            .unwrap_or_default();
                        m.content = Some(format!(
                            "{}\n...[SPAVN Shaken ~{} tokens{}]...\n{}",
                            head, est_tok, spill_note, tail
                        ));
                        shaken_any = true;
                    }
                }
            }
            if shaken_any {
                rewrite_session(session_file, messages);
                view_messages =
                    compact_messages_for_model(messages, step_endpoint.is_local(), max_ctx_tokens);
                view_tok = hud::estimate_messages_tokens(&view_messages);
            }
        }
        let should_full_compact = if total_turn_steps == 0 {
            (effective_tok >= compact_threshold || messages.len() > 160) && messages.len() > 6
        } else {
            effective_tok >= compact_threshold && messages.len() > 6
        };
        if should_full_compact {
            let raw_before_tok = hud::estimate_messages_tokens(messages).max(effective_tok);
            perform_manual_compact(&step_endpoint, messages, None);
            rewrite_session(session_file, messages);
            last_observed_total_tokens = 0;
            view_messages =
                compact_messages_for_model(messages, step_endpoint.is_local(), max_ctx_tokens);
            view_tok = hud::estimate_messages_tokens(&view_messages);
            if !quiet {
                println!(
                    "{}╰─ ⚡ {}: {} → {}{}",
                    GREEN,
                    tr_en(
                        "[SPAVN Auto-Compact sonrası işlem otomatik devam ediyor]",
                        "[SPAVN Auto-Continuing after context compaction]"
                    ),
                    hud::format_token_short(raw_before_tok),
                    hud::format_token_short(view_tok),
                    RESET
                );
            }
        }
        hud::set_context_window_usage(view_tok.max(last_observed_total_tokens), max_ctx_tokens);
        let mut spinner = if quiet {
            LiveSpinner::silent()
        } else {
            LiveSpinner::start(tr_en("Düşünülüyor", "Thinking"))
        };
        let content_started = std::cell::Cell::new(false);
        let content_chars = std::cell::Cell::new(0usize);
        let partial_content = RefCell::new(String::new());
        let md_renderer = RefCell::new(MarkdownStreamRenderer::new());
        total_turn_steps += 1;
        let _ = total_turn_steps;
        let result = match stream_chat_completion(
            &step_endpoint,
            &view_messages,
            &tools_spec,
            |think_delta| {
                if !quiet {
                    hud::stream_thinking_delta(think_delta, show_thinking);
                }
            },
            |content_delta| {
                partial_content.borrow_mut().push_str(content_delta);
                if quiet {
                    return;
                }
                content_chars.set(content_chars.get() + content_delta.chars().count());
                hud::set_spinner_label(&format!(
                    "✍️ {} ({} {})",
                    tr_en("Yanıt yazılıyor", "Writing response"),
                    content_chars.get(),
                    tr_en("krk", "ch")
                ));
                if !content_started.get() {
                    content_started.set(true);
                    render_assistant_header(&step_endpoint.key);
                }
                md_renderer.borrow_mut().push(content_delta);
            },
        ) {
            Ok(r) => r,
            Err(err) => {
                if content_started.get() {
                    md_renderer.borrow_mut().finish();
                }
                hud::close_thinking_box_if_open();
                spinner.stop();
                if content_started.get() && !quiet {
                    println!("{}╰─{}", CYAN, RESET);
                }
                let partial_trimmed = partial_content.borrow().trim().to_string();
                if !partial_trimmed.is_empty() {
                    let saved_partial = ChatMessage {
                        role: "assistant".to_string(),
                        content: Some(format!("{}\n\n*[Esc ile durduruldu]*", partial_trimmed)),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                        thought_signature: None,
                        reasoning_content: None,
                    };
                    append_message(session_file, &saved_partial);
                    messages.push(saved_partial);
                }
                if is_interrupted() {
                    println!(
                        "\n{}⚡ {}{}",
                        YELLOW,
                        tr_en(
                            "[İşlem Esc/Ctrl+C ile iptal edildi]",
                            "[Operation cancelled via Esc/Ctrl+C]"
                        ),
                        RESET
                    );
                    return Ok(());
                }
                // A reconnect replays the whole turn, so it is only safe while nothing has
                // been printed yet; otherwise the partial answer would appear twice.
                if step_endpoint.is_local() && partial_content.borrow().is_empty() {
                    if !quiet {
                        println!(
                            "\n{}🔄 {} ({}) — {}{}",
                            CYAN,
                            tr_en(
                                "Yerel model bağlantısı yenileniyor",
                                "Reconnecting to local model"
                            ),
                            step_endpoint.key,
                            err,
                            RESET
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    let mut retry_local_spinner = if quiet {
                        LiveSpinner::silent()
                    } else {
                        LiveSpinner::start(tr_en(
                            "Yerel model yeniden deneniyor",
                            "Retrying local model",
                        ))
                    };
                    let local_retry_res = stream_chat_completion(
                        &step_endpoint,
                        &view_messages,
                        &tools_spec,
                        |think_delta| {
                            if !quiet {
                                hud::stream_thinking_delta(think_delta, show_thinking);
                            }
                        },
                        |c| {
                            partial_content.borrow_mut().push_str(c);
                            if quiet {
                                return;
                            }
                            content_chars.set(content_chars.get() + c.chars().count());
                            hud::set_spinner_label(&format!(
                                "✍️ {} ({} {})",
                                tr_en("Yanıt yazılıyor", "Writing response"),
                                content_chars.get(),
                                tr_en("krk", "ch")
                            ));
                            if !content_started.get() {
                                content_started.set(true);
                                render_assistant_header(&step_endpoint.key);
                            }
                            md_renderer.borrow_mut().push(c);
                        },
                    );
                    if content_started.get() {
                        md_renderer.borrow_mut().finish();
                    }
                    if !quiet {
                        hud::close_thinking_box_if_open();
                    }
                    retry_local_spinner.stop();

                    match local_retry_res {
                        Ok(ok_local) => ok_local,
                        Err(local_err2) => {
                            let fb_key = cfg.fallback_cloud_model.trim();
                            if fb_key.is_empty()
                                || fb_key.eq_ignore_ascii_case("none")
                                || fb_key == step_endpoint.key
                            {
                                return Err(local_err2);
                            }
                            if !quiet {
                                println!(
                                    "\n{}⚠️  {} ({}) -> {} {}{}",
                                    YELLOW,
                                    tr_en("Yerel model yanıt vermedi", "Local model unreachable"),
                                    local_err2,
                                    fb_key,
                                    tr_en(
                                        "modeline otomatik geçiliyor...",
                                        "failing over automatically..."
                                    ),
                                    RESET
                                );
                            }
                            let fallback_ep = resolve_endpoint(fb_key);
                            let mut fb_spinner = if quiet {
                                LiveSpinner::silent()
                            } else {
                                LiveSpinner::start(tr_en("Düşünülüyor", "Thinking"))
                            };
                            let fb_res = stream_chat_completion(
                                &fallback_ep,
                                &view_messages,
                                &tools_spec,
                                |think_delta| {
                                    if !quiet {
                                        hud::stream_thinking_delta(think_delta, show_thinking);
                                    }
                                },
                                |c| {
                                    partial_content.borrow_mut().push_str(c);
                                    if quiet {
                                        return;
                                    }
                                    content_chars.set(content_chars.get() + c.chars().count());
                                    hud::set_spinner_label(&format!(
                                        "✍️ {} ({} {})",
                                        tr_en("Yanıt yazılıyor", "Writing response"),
                                        content_chars.get(),
                                        tr_en("krk", "ch")
                                    ));
                                    if !content_started.get() {
                                        content_started.set(true);
                                        render_assistant_header(&fallback_ep.key);
                                    }
                                    md_renderer.borrow_mut().push(c);
                                },
                            );
                            if content_started.get() {
                                md_renderer.borrow_mut().finish();
                            }
                            if !quiet {
                                hud::close_thinking_box_if_open();
                            }
                            fb_spinner.stop();
                            fb_res?
                        }
                    }
                } else {
                    return Err(err);
                }
            }
        };

        if content_started.get() {
            md_renderer.borrow_mut().finish();
        }
        if !quiet {
            hud::close_thinking_box_if_open();
        }
        spinner.stop();

        let assistant_msg = result.message;
        if let Some(ref c_text) = assistant_msg.content {
            sync_live_tasks_from_text(c_text);
        }
        append_message(session_file, &assistant_msg);
        messages.push(assistant_msg.clone());
        let est_after = hud::estimate_messages_tokens(messages);
        last_observed_total_tokens = result.usage.total_tokens as usize;
        let actual_used = last_observed_total_tokens.max(est_after);
        hud::set_context_window_usage(actual_used, step_endpoint.context_window_tokens());
        let secs = (result.elapsed_ms as f64) / 1000.0;
        let decode_secs = (result.elapsed_ms.saturating_sub(result.ttft_ms).max(1) as f64) / 1000.0;
        let tok_s = if decode_secs > 0.0 && result.usage.completion_tokens > 0 {
            (result.usage.completion_tokens as f64) / decode_secs
        } else {
            0.0
        };

        if let Some(tool_calls) = assistant_msg.tool_calls {
            consecutive_investigation_steps += 1;
            let mut local_read_count = 0usize;
            let mut batch_sigs: Vec<String> = tool_calls
                .iter()
                .filter_map(|tc| canonical_tool_call_sig(&tc.function.name, &tc.function.arguments))
                .collect();
            batch_sigs.sort();
            let mut max_sig_seen_in_turn = 0usize;
            for sig in &batch_sigs {
                let entry = canonical_sig_counts.entry(sig.clone()).or_insert(0);
                *entry += 1;
                if *entry > max_sig_seen_in_turn {
                    max_sig_seen_in_turn = *entry;
                }
            }
            if !batch_sigs.is_empty() {
                let turn_batch_sig = batch_sigs.join("||");
                if turn_batch_sig == last_tool_sig {
                    repeat_count += 1;
                } else {
                    last_tool_sig = turn_batch_sig;
                    repeat_count = max_sig_seen_in_turn.saturating_sub(1);
                }
            }
            // stay exactly as they were in the serial path.
            let batch_calls: Vec<(String, String)> = tool_calls
                .iter()
                .map(|tc| {
                    (
                        tc.function.name.trim().to_string(),
                        tc.function.arguments.clone(),
                    )
                })
                .collect();
            let mut prefetched: std::collections::HashMap<usize, (String, u128)> =
                std::collections::HashMap::new();
            let mut budget_used = 0usize;
            let mut batch_idx = 0usize;
            while batch_idx < batch_calls.len() {
                if !is_batchable(&batch_calls[batch_idx].0, ctx) {
                    batch_idx += 1;
                    continue;
                }
                let start = batch_idx;
                while batch_idx < batch_calls.len() && is_batchable(&batch_calls[batch_idx].0, ctx)
                {
                    if endpoint.is_local()
                        && is_read_tool_name(&batch_calls[batch_idx].0)
                        && budget_used >= LOCAL_READ_BUDGET
                    {
                        break;
                    }
                    if endpoint.is_local() && is_read_tool_name(&batch_calls[batch_idx].0) {
                        budget_used += 1;
                    }
                    batch_idx += 1;
                }
                if batch_idx - start > 1 {
                    let batch_started = std::time::Instant::now();
                    let out = tools::execute_read_only_batch(ctx, &batch_calls[start..batch_idx]);
                    // A batch shares one wall clock: charging each member the serial-looking
                    // duration would report a cost the user never paid.
                    let batch_ms = batch_started.elapsed().as_millis();
                    for (off, res) in out.into_iter().enumerate() {
                        prefetched.insert(start + off, (res, batch_ms));
                    }
                }
            }
            for (idx, tc) in tool_calls.into_iter().enumerate() {
                let tool_name = tc.function.name.trim().to_string();
                if is_read_tool_name(&tool_name) {
                    if let Some((base_path, _)) =
                        extract_file_op_key(&tool_name, &tc.function.arguments)
                    {
                        let count = file_read_counts.entry(base_path.clone()).or_insert(0);
                        *count += 1;
                        if *count > 3 {
                            let tool_msg = ChatMessage::tool_result(
                                tc.id,
                                tool_name,
                                format!(
                                    "[SPAVN FILE-READ CAP]: You have already read `{}` 3 times in this turn and its Structural Outline is in your context. Stop paging through this file; use `search` with `context: 3` or proceed to your edit/answer.",
                                    base_path
                                ),
                            );
                            append_message(session_file, &tool_msg);
                            messages.push(tool_msg);
                            continue;
                        }
                    }
                }
                if endpoint.is_local()
                    && is_read_tool_name(&tool_name)
                    && local_read_count >= LOCAL_READ_BUDGET
                {
                    let tool_msg = ChatMessage::tool_result(
                        tc.id,
                        tool_name,
                        format!(
                            "[SPAVN Local Context Budget]: {} files read in this step; analyze these first before reading more.",
                            LOCAL_READ_BUDGET
                        ),
                    );
                    append_message(session_file, &tool_msg);
                    messages.push(tool_msg);
                    continue;
                }
                if endpoint.is_local() && is_read_tool_name(&tool_name) {
                    local_read_count += 1;
                }

                let summary_for_task =
                    summarize_tool_args(&tool_name, &tc.function.arguments, &ctx.default_host);
                let progress_target = tool_progress_target(&tool_name, &tc.function.arguments);
                let pre_hunk_starts = if matches!(tool_name.as_str(), "edit" | "write") {
                    hud::toolcard::resolve_hunk_start_lines(&tool_name, &tc.function.arguments)
                } else {
                    Vec::new()
                };
                let pre_change_rows = if !quiet && matches!(tool_name.as_str(), "edit" | "write") {
                    hud::toolcard::change_rows(
                        &tool_name,
                        &tc.function.arguments,
                        hud::toolcard::CHANGE_MAX_ROWS,
                    )
                } else {
                    Vec::new()
                };
                let mut batched_ms: Option<u128> = None;
                let (mut tool_output, tool_ms) = if quiet {
                    let t0 = std::time::Instant::now();
                    let (out, elapsed) = match prefetched.remove(&idx) {
                        Some((out, ms)) => (out, ms),
                        None => (
                            execute_tool(ctx, &tool_name, &tc.function.arguments),
                            t0.elapsed().as_millis(),
                        ),
                    };
                    (out, elapsed)
                } else {
                    let summary = summary_for_task.clone();
                    let (_, cols) = term_size();
                    // The header must never wrap: a wrapped line desynchronises the inline
                    // dock's erase math, which then eats the rows above it.
                    let head_w = cols.saturating_sub(2).max(20);
                    // Cut the middle of the summary, not its tail: `cd <path> && <cmd>` is the
                    // common shape and the command at the end is the informative half.
                    let prefix = format!("{}╭─ ⚡ {}{}{} {}", YELLOW, BOLD, tool_name, RESET, DIM);
                    let budget = head_w.saturating_sub(visible_width(&prefix)).max(8);
                    let (shown, used) = truncate_ansi_to_width_middle(&summary, budget);
                    let shown = if ctx.default_host == "local"
                        && matches!(tool_name.as_str(), "read" | "write" | "edit" | "lsp")
                    {
                        if let Some(raw_path) =
                            serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                                .ok()
                                .and_then(|v| {
                                    v.get("path")
                                        .and_then(|s| s.as_str())
                                        .map(|s| s.to_string())
                                })
                        {
                            spavn::ansi::osc8_file_link(
                                &raw_path,
                                pre_hunk_starts.first().copied(),
                                &shown,
                            )
                        } else {
                            shown
                        }
                    } else {
                        shown
                    };
                    // The diffstat rides on the header, so the size of a change is visible
                    // before any body row is read.
                    let stat = if matches!(tool_name.as_str(), "edit" | "write") {
                        hud::toolcard::diff_stat(&tool_name, &tc.function.arguments)
                    } else {
                        None
                    };
                    let stat_line = match stat {
                        Some((add, del)) => format!(
                            " {}(+{}{}{} -{}{}{}){}",
                            DIM, GREEN, add, RESET, RED, del, RESET, DIM
                        ),
                        None => String::new(),
                    };
                    let _ = used;
                    println!("{}{}{}{}", prefix, shown, stat_line, RESET);
                    let tool_started = std::time::Instant::now();
                    let out = if let Some((out, ms)) = prefetched.remove(&idx) {
                        batched_ms = Some(ms);
                        out
                    } else if is_fast_inline_tool(&tool_name) && ctx.default_host == "local" {
                        execute_tool(ctx, &tool_name, &tc.function.arguments)
                    } else {
                        let mut tool_spinner =
                            LiveSpinner::start(&format!("⚡ {} çalıştırılıyor", tool_name));
                        let res = execute_tool(ctx, &tool_name, &tc.function.arguments);
                        tool_spinner.stop();
                        res
                    };
                    (
                        out,
                        batched_ms.unwrap_or_else(|| tool_started.elapsed().as_millis()),
                    )
                };

                if ctx.phase == AgentPhase::Code
                    && matches!(
                        tool_name.as_str(),
                        "edit" | "edit_file" | "replace" | "write" | "write_file"
                    )
                {
                    if tool_output.starts_with("Successfully ") {
                        consecutive_investigation_steps = 0;
                        focus_gate_nudged = false;
                        unverified_edits = true;
                        compiler_verified_in_edit = tool_output
                            .contains("[Compiler Check: cargo check OK]")
                            || tool_output.contains("[Compiler Check: tsc OK]");
                        if let Ok(v) =
                            serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                        {
                            let mut touched = Vec::new();
                            if let Some(p) = v
                                .get("path")
                                .or_else(|| v.get("file_path"))
                                .or_else(|| v.get("file"))
                                .and_then(|s| s.as_str())
                            {
                                touched.push(p.trim().to_string());
                            }
                            if let Some(files_arr) = v.get("files").and_then(|a| a.as_array()) {
                                for item in files_arr {
                                    if let Some(fp) = item
                                        .get("path")
                                        .or_else(|| item.get("file_path"))
                                        .and_then(|s| s.as_str())
                                    {
                                        touched.push(fp.trim().to_string());
                                    }
                                }
                            }
                            for p_str in touched {
                                file_read_counts.remove(&p_str);
                                let resolved_p = ctx.resolve_target_arg(&p_str);
                                file_read_counts.remove(&resolved_p);
                                if !resolved_p.is_empty()
                                    && !modified_files_in_turn.contains(&resolved_p)
                                {
                                    modified_files_in_turn.push(resolved_p);
                                }
                            }
                        }
                    }
                } else if matches!(
                    tool_name.as_str(),
                    "bash" | "run_command" | "shell" | "exec"
                ) {
                    let verify_cmd = tools::is_verification_shell_command(&tc.function.arguments);
                    let succeeded = !tool_output.contains("(exit code ")
                        && !tool_output.contains("<auto_error_context>")
                        && !tools::has_compiler_or_test_error_signature(&tool_output);
                    if verify_cmd && succeeded {
                        unverified_edits = false;
                        modified_files_in_turn.clear();
                    }
                }
                if repeat_count >= 2 || tool_output.starts_with("[EOF STOP:") {
                    tool_output.push_str(&format!(
                        "\n\n[SPAVN LOOP GUARD (count={})]: Stop repeating `{}` or reading past EOF. Proceed directly to your analysis or edit using the context already gathered.",
                        repeat_count + 1,
                        tool_name
                    ));
                }

                let is_err_tool = tool_output.starts_with("Edit error:")
                    || tool_output.starts_with("Read error")
                    || tool_output.starts_with("Write error")
                    || tool_output.starts_with("BLOCKED")
                    || tool_output.starts_with("Execution error:");
                if !is_err_tool {
                    advance_live_tasks_on_tool(&tool_name, &progress_target);
                }

                if !quiet {
                    let preview_line = tool_output
                        .lines()
                        .next()
                        .unwrap_or("(tamamlandı)")
                        .chars()
                        .take(80)
                        .collect::<String>();
                    let (_, cols) = term_size();
                    let full_w = cols.saturating_sub(2).max(20);
                    // Show what actually changed for edits instead of a byte tally, and a
                    // head+tail slice of the output for everything else.
                    let is_diff_body =
                        !is_err_tool && matches!(tool_name.as_str(), "edit" | "write");
                    let body = if is_err_tool {
                        Vec::new()
                    } else if is_diff_body {
                        if !pre_change_rows.is_empty() {
                            pre_change_rows
                        } else {
                            let changes = hud::toolcard::change_rows(
                                &tool_name,
                                &tc.function.arguments,
                                hud::toolcard::CHANGE_MAX_ROWS,
                            );
                            if changes.is_empty() {
                                hud::toolcard::body_rows(&tool_output, hud::toolcard::BODY_MAX_ROWS)
                            } else {
                                changes
                            }
                        }
                    } else {
                        let max_body = if batched_ms.is_some() && is_read_tool_name(&tool_name) {
                            2
                        } else {
                            hud::toolcard::BODY_MAX_ROWS
                        };
                        hud::toolcard::body_rows(&tool_output, max_body)
                    };
                    if is_diff_body && !body.is_empty() {
                        for row in
                            hud::toolcard::numbered_diff_rows_at(&body, full_w, &pre_hunk_starts)
                        {
                            println!("{}", row);
                        }
                    } else {
                        for row in &body {
                            println!("{}", hud::toolcard::rail_row(row, full_w));
                        }
                    }
                    let raw_footer = hud::toolcard::footer_line(
                        !is_err_tool,
                        // Only echo the output when the body is empty: otherwise the preview
                        // would just repeat the first rail row the reader already sees.
                        if body.is_empty() { &preview_line } else { "" },
                        // A diff body is the visible change, not the one-line executor result:
                        // counting `Successfully …` as "1 line" under five drawn rows lies.
                        if is_diff_body {
                            body.iter()
                                .filter(|r| !hud::toolcard::is_marker_row(r))
                                .count()
                        } else {
                            tool_output.lines().count()
                        },
                        tool_ms,
                    );
                    let (trunc_footer, _) = truncate_ansi_to_width(&raw_footer, full_w);
                    println!("{}", trunc_footer);
                }

                let tool_msg = ChatMessage::tool_result(tc.id, tool_name, tool_output);
                append_message(session_file, &tool_msg);
                messages.push(tool_msg);
            }
            let inv_budget = if step_endpoint.is_local() { 6 } else { 10 };
            if consecutive_investigation_steps >= inv_budget && !focus_gate_nudged {
                focus_gate_nudged = true;
                if let Some(last_tool_msg) = messages.last_mut() {
                    if last_tool_msg.role == "tool" {
                        let nudge_note = format!(
                            "\n\n[SPAVN FOCUS GATE ({} consecutive read/search steps without an edit)]: Stop broad exploration (do NOT read `node_modules`, compiled bundles, or unrelated repositories). Synthesize your root-cause hypothesis from the evidence already gathered and either apply the surgical edit now or present your findings to the user.",
                            consecutive_investigation_steps
                        );
                        if let Some(c) = &mut last_tool_msg.content {
                            c.push_str(&nudge_note);
                        }
                    }
                }
            }
            continue;
        }

        // If user injected a live steer message while the final text was streaming, continue the loop immediately!
        let post_steered = pop_steer_messages();
        if !post_steered.is_empty() {
            for sm in post_steered {
                println!(
                    "\n{}⚡ [Canlı Müdahale (Steer) Aktif Tura Enjekte Edildi]: {}{}{}",
                    YELLOW, BOLD, sm, RESET
                );
                let steer_msg = ChatMessage::user(format!(
                    "[LIVE USER STEERING INSTRUCTION — Prioritize this immediately]: {}",
                    sm
                ));
                append_message(session_file, &steer_msg);
                messages.push(steer_msg);
            }
            continue;
        }
        // 1. Max-Tokens (`finish_reason == "length"`) Auto-Continuation
        if result.finish_reason.as_deref() == Some("length") && length_stop_retries < 3 {
            length_stop_retries += 1;
            if !quiet {
                println!(
                    "\n{}⚡ [SPAVN Turn Recovery: Çıktı token sınırına ulaştı ({}/3) -> Kaldığı yerden devam ediliyor...]{}",
                    YELLOW, length_stop_retries, RESET
                );
            }
            let cont_msg = ChatMessage::user(
                "[SPAVN LENGTH-STOP RECOVERY]: Your previous response hit the output token limit (`finish_reason=length`). Continue immediately from the exact point where you stopped without repeating prior output.",
            );
            append_message(session_file, &cont_msg);
            messages.push(cont_msg);
            continue;
        }

        // 2. Empty Assistant Stop Recovery
        let text_trimmed = assistant_msg.content.as_deref().unwrap_or("").trim();
        let reasoning_trimmed = assistant_msg
            .reasoning_content
            .as_deref()
            .unwrap_or("")
            .trim();
        if text_trimmed.is_empty() && !reasoning_trimmed.is_empty() {
            let recovered_from_reasoning = hud::strip_internal_tags_for_display(reasoning_trimmed);
            if !recovered_from_reasoning.is_empty() {
                if !quiet && !content_started.get() {
                    content_started.set(true);
                    render_assistant_header(&step_endpoint.key);
                    let mut r = MarkdownStreamRenderer::new();
                    r.push(&recovered_from_reasoning);
                    r.finish();
                }
                if let Some(last_m) = messages.last_mut() {
                    last_m.content = Some(recovered_from_reasoning.clone());
                }
                rewrite_session(session_file, messages);
                if !quiet {
                    render_assistant_footer(
                        &step_endpoint.key,
                        ctx.access.short_badge(),
                        result.ttft_ms,
                        secs,
                        tok_s,
                    );
                }
                break;
            }
        }
        if text_trimmed.is_empty() && empty_stop_retries < 2 {
            empty_stop_retries += 1;
            if !quiet {
                println!(
                    "\n{}⚡ [SPAVN Turn Recovery: Boş yanıt tespit edildi ({}/2) -> İşlem otomatik devam ediyor...]{}",
                    YELLOW, empty_stop_retries, RESET
                );
            }
            let empty_nudge = ChatMessage::user(
                "[SPAVN EMPTY-STOP RECOVERY]: You returned an empty response with no tool call and no text. Call the next required tool or provide the final answer now.",
            );
            append_message(session_file, &empty_nudge);
            messages.push(empty_nudge);
            continue;
        }

        // 3. Premature Action-Announcement Stop Recovery
        let looks_like_premature_stop = is_premature_stop(text_trimmed);
        let looks_like_phantom_code = is_phantom_code_block_stop(
            text_trimmed,
            ctx.phase,
            total_turn_steps,
            !modified_files_in_turn.is_empty(),
        );
        if (looks_like_premature_stop || looks_like_phantom_code) && premature_stop_retries < 2 {
            premature_stop_retries += 1;
            if !quiet {
                println!(
                    "\n{}⚡ [SPAVN Turn Recovery: Yarım kalan aksiyon tespit edildi ({}/2) -> Araç çağrısı tetikleniyor...]{}",
                    YELLOW, premature_stop_retries, RESET
                );
            }
            let u_nudge = if looks_like_phantom_code {
                ChatMessage::user(
                    "[SPAVN PHANTOM-EDIT RECOVERY]: You printed a code block in chat during CODE phase without applying it to disk. Call `edit` or `write` now to apply your changes.",
                )
            } else {
                ChatMessage::user(
                    "[SPAVN UNEXPECTED-STOP RECOVERY]: You announced an action you were about to take but stopped without calling a tool. Call the concrete tool now.",
                )
            };
            append_message(session_file, &u_nudge);
            messages.push(u_nudge);
            continue;
        }

        // 4. Autonomous Zero-Stop Verification Gate after file edits (up to 4 auto-fix rounds).
        // An identical diagnostic means the previous round changed nothing relevant; re-sending
        // it just burns a whole API turn, so the gate stops nudging instead of looping.
        if !modified_files_in_turn.is_empty() && auto_verify_rounds < 4 {
            let eff_cwd = if ctx.last_cwd.is_dir() {
                &ctx.last_cwd
            } else {
                &ctx.policy.cwd
            };
            let cwd_str = eff_cwd.to_string_lossy();
            let gate_res = if compiler_verified_in_edit {
                tools::check_unfinished_placeholders(&modified_files_in_turn)
            } else {
                tools::run_project_verification_gate(&cwd_str, &modified_files_in_turn)
            };
            if let Some(gate_err) = gate_res {
                auto_verify_rounds += 1;
                unverified_edits = false;
                compiler_verified_in_edit = false;
                if last_gate_err.as_deref() == Some(gate_err.as_str()) {
                    // Byte-identical diagnostic: the last round produced no relevant change, so
                    // another nudge would be a wasted API turn with the same outcome.
                    last_gate_err = None;
                    auto_verify_rounds = 4;
                } else {
                    last_gate_err = Some(gate_err.clone());
                    if !quiet {
                        println!(
                            "\n{}⚡ [SPAVN Zero-Stop Doğrulama Kapısı ({}/4): Derleme/eksik kod tespit edildi -> Otomatik düzeltme tetikleniyor...]{}",
                            YELLOW, auto_verify_rounds, RESET
                        );
                    }
                    let gate_msg = ChatMessage::user(gate_err);
                    append_message(session_file, &gate_msg);
                    messages.push(gate_msg);
                    continue;
                }
            } else if compiler_verified_in_edit
                || ctx.policy.cwd.join("Cargo.toml").is_file()
                || ctx.policy.cwd.join("tsconfig.json").is_file()
            {
                unverified_edits = false;
            }
        }
        if unverified_edits && !verification_nudged {
            verification_nudged = true;
            unverified_edits = false;
            if !quiet {
                println!(
                    "{}⚡ [SPAVN Otonom Doğrulama Kapısı: Dosya değişikliği sonrası derleme/test kontrolü tetikleniyor...]{}",
                    YELLOW, RESET
                );
            }
            let nudge = ChatMessage::user(
                "[SPAVN VERIFICATION GATE]: You modified file(s) via edit/write without running a verification check (`bash` build/test). Run a verification command now, then report the final result.",
            );
            append_message(session_file, &nudge);
            messages.push(nudge);
            continue;
        }

        if !quiet && (content_started.get() || result.usage.total_tokens > 0) {
            render_assistant_footer(
                &step_endpoint.key,
                ctx.access.short_badge(),
                result.ttft_ms,
                secs,
                tok_s,
            );
        }

        break;
    }

    Ok(())
}

#[cfg(test)]
mod premature_stop_tests {
    use super::is_premature_stop;

    #[test]
    fn detects_announcements_not_final_answers() {
        for yes in [
            "Let me read the file first.",
            "Şimdi dosyayı okuyacağım.",
            "I'll now run the tests",
            "Next step:",
        ] {
            assert!(is_premature_stop(yes), "{yes}");
        }
        for no in [
            "Done. Let me know if you need anything else.",
            "Bitti. Başka bir şey ister misin?",
            "Tamamlandı, testler geçiyor.",
            "Which file should I edit?",
            "### Değişiklikler:\n- `src/main.rs` güncellendi.\n- `src/router.rs` düzeltildi.",
            "Örnek:\n```rust\nfn main() {}\n```",
        ] {
            assert!(!is_premature_stop(no), "{no}");
        }
    }

    #[test]
    fn queue_and_steer_pop_last_for_live_edit_and_cancel() {
        spavn::state::clear_queued_messages();
        spavn::state::clear_steer_messages();
        spavn::state::push_queued_message("ilk görev".to_string());
        spavn::state::push_queued_message("ikinci görev".to_string());
        assert_eq!(
            spavn::state::peek_last_pending_message(),
            Some((false, "ikinci görev".to_string()))
        );
        assert_eq!(
            spavn::state::pop_last_pending_message(),
            Some((false, "ikinci görev".to_string()))
        );
        spavn::state::push_steer_message("acil müdahale".to_string());
        assert_eq!(
            spavn::state::pop_last_pending_message(),
            Some((true, "acil müdahale".to_string()))
        );
        assert_eq!(
            spavn::state::pop_last_pending_message(),
            Some((false, "ilk görev".to_string()))
        );
        assert_eq!(spavn::state::pop_last_pending_message(), None);
    }

    #[test]
    fn local_compact_messages_preserves_kv_prefix_across_turn_steps() {
        use spavn::router::ChatMessage;
        let mut msgs = vec![
            ChatMessage::system("system prompt".to_string()),
            ChatMessage::user("first task".to_string()),
        ];
        let make_assistant = |txt: String, reasoning: String| ChatMessage {
            role: "assistant".to_string(),
            content: Some(txt),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            thought_signature: None,
            reasoning_content: Some(reasoning),
        };
        for i in 0..8 {
            msgs.push(make_assistant(format!("step {i}"), "r".repeat(4000)));
            msgs.push(ChatMessage::tool_result(
                format!("call_{i}"),
                "read",
                "x".repeat(6000),
            ));
        }
        msgs.push(ChatMessage::user(
            "second task: audit commit and fix test".to_string(),
        ));
        for step in 0..5 {
            msgs.push(make_assistant(
                format!("active step {step}"),
                format!("thinking {step}"),
            ));
            msgs.push(ChatMessage::tool_result(
                format!("active_call_{step}"),
                "read",
                "y".repeat(1200),
            ));
        }
        let view_a = super::compact_messages_for_model(&msgs, true, 131_072);
        // Append one more assistant + tool step in the same turn
        msgs.push(make_assistant(
            "active step 5".to_string(),
            "thinking 5".to_string(),
        ));
        msgs.push(ChatMessage::tool_result(
            "active_call_5".to_string(),
            "read",
            "y".repeat(1200),
        ));
        let view_b = super::compact_messages_for_model(&msgs, true, 131_072);
        assert_eq!(view_a.len() + 2, view_b.len());
        for i in 0..view_a.len() {
            assert_eq!(
                view_a[i].content, view_b[i].content,
                "KV prefix content mutated at message index {i}"
            );
            assert_eq!(
                view_a[i].reasoning_content, view_b[i].reasoning_content,
                "KV prefix reasoning_content mutated at message index {i}"
            );
        }
    }

    #[test]
    fn supersede_prune_replaces_stale_reads_before_turn_anchor() {
        use spavn::router::{ChatMessage, ToolCall, ToolFunction};
        let call = |id: &str, name: &str, args: &str| ChatMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: id.to_string(),
                call_type: "function".to_string(),
                function: ToolFunction {
                    name: name.to_string(),
                    arguments: args.to_string(),
                },
                thought_signature: None,
            }]),
            tool_call_id: None,
            name: None,
            thought_signature: None,
            reasoning_content: None,
        };
        let msgs = vec![
            ChatMessage::system("sys".to_string()),
            ChatMessage::user("first turn".to_string()),
            call(
                "c1",
                "read",
                r#"{"path":"src/foo.rs","start_line":1,"end_line":50}"#,
            ),
            ChatMessage::tool_result("c1", "read", "old file body".repeat(100)),
            ChatMessage::user("second turn".to_string()),
            call(
                "c2",
                "edit",
                r#"{"path":"src/foo.rs","old_text":"a","new_text":"b"}"#,
            ),
            ChatMessage::tool_result("c2", "edit", "Successfully edited src/foo.rs".to_string()),
        ];
        let compacted = super::compact_messages_for_model(&msgs, true, 131_072);
        assert!(
            compacted[3]
                .content
                .as_deref()
                .unwrap_or("")
                .contains("Supersede Prune"),
            "expected stale read to be superseded: {:?}",
            compacted[3].content
        );
    }

    #[test]
    fn canonical_tool_call_sig_normalizes_json_and_skips_transparent_tools() {
        let s1 = super::canonical_tool_call_sig("read", r#"{"path":"a.rs","start_line":1}"#);
        let s2 = super::canonical_tool_call_sig("read", r#"{"start_line": 1, "path": "a.rs"}"#);
        assert_eq!(s1, s2);
        assert!(
            super::canonical_tool_call_sig("memory", r#"{"op":"recall","query":"x"}"#).is_none()
        );
        assert!(super::canonical_tool_call_sig("skill", r#"{"name":"rust"}"#).is_none());
    }
}
