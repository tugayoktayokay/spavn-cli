//! SPAVN orchestration constants and the pure decision logic that drives the unbounded
//! turn loop in [`crate::turn`].
//!
//! The agent loop (`run_agent_turn`) is large because it interleaves streaming, tool
//! dispatch, compaction, recovery retries, and verification gates. To keep that loop
//! readable and testable, every tunable threshold lives here as a named constant and every
//! branching decision is extracted into a pure function with its own unit tests. This means
//! the loop body only *calls* decisions — it never scatters magic numbers.
//!
//! All values are defaults; they can be surfaced to user config later without touching the
//! loop, since callers reference these constants by name.

use crate::spavn_engine::AgentPhase;

/// Local-model context guard: max read-only file reads executed per agent turn. Beyond this
/// budget the turn stops dispatching reads and asks the model to analyze what it already
/// gathered, avoiding local KV-cache prefill stalls on huge parallel reads.
pub const LOCAL_READ_BUDGET: usize = 5;

/// Repeat tool-call signatures before the loop escalates the current step to the cloud model.
/// A single repeat is noise; two identical steps mean the local model is stuck in a loop.
pub const MAX_REPEATS_BEFORE_ESCALATE: usize = 2;

/// Per-tier streaming/compaction token caps, applied by endpoint cost tier.
pub const FREE_FLASH_TOKEN_CAP: usize = 64_000;
pub const HIGH_CONTEXT_TOKEN_CAP: usize = 240_000;
pub const STANDARD_TOKEN_CAP: usize = 120_000;

/// Context-window size (tokens) above which an endpoint is treated as "high context".
pub const HIGH_CONTEXT_THRESHOLD: usize = 500_000;

/// Percentage of the context window at which a hard compaction runs (`80%`).
pub const HARD_COMPACT_RATIO_PCT: usize = 80;

/// Token usage that triggers turn-start Context Shaking (OMP-style spill to recoverable files).
pub const FIRST_TURN_SHAKE_TOKENS: usize = 24_000;

/// Prune budget for the "compact old tool messages" gate inside `compact_messages_for_model`.
/// Fallback when context is unknown, otherwise ~25% of the context window clamped to a sane range.
pub const COMPACT_PRUNE_BUDGET_FALLBACK: usize = 16_000;
pub const COMPACT_PRUNE_BUDGET_MIN: usize = 4_000;
pub const COMPACT_PRUNE_BUDGET_MAX: usize = 64_000;

/// Compute that prune budget. Unknown context (0) falls back to a fixed small budget.
pub fn compact_prune_budget(max_ctx_tokens: usize) -> usize {
    if max_ctx_tokens == 0 {
        COMPACT_PRUNE_BUDGET_FALLBACK
    } else {
        (max_ctx_tokens / 4).clamp(COMPACT_PRUNE_BUDGET_MIN, COMPACT_PRUNE_BUDGET_MAX)
    }
}

/// Message-count limits feeding the full-compaction gate.
pub const MESSAGE_COUNT_SOFT_LIMIT: usize = 160;
pub const MESSAGE_COUNT_HARD_LIMIT: usize = 6;

/// Turn-recovery retry ceilings for each auto-continuation gate.
pub const MAX_LENGTH_STOP_RETRIES: usize = 3;
pub const MAX_EMPTY_STOP_RETRIES: usize = 2;
pub const MAX_PREMATURE_STOP_RETRIES: usize = 2;

/// Maximum autonomous Zero-Stop Verification-Gate auto-fix rounds after file edits. An identical
/// diagnostic means the previous round changed nothing relevant, so the gate stops nudging
/// instead of burning another API turn on the same output.
pub const MAX_VERIFY_ROUNDS: usize = 4;

/// Whether the local loop should escalate this step to the cloud model.
pub fn should_escalate_to_cloud(is_local: bool, repeat_count: usize) -> bool {
    is_local && repeat_count >= MAX_REPEATS_BEFORE_ESCALATE
}

/// Per-tier token cap for streaming/compaction decisions.
pub fn tier_token_cap(is_free_or_flash: bool, max_ctx_tokens: usize) -> usize {
    if is_free_or_flash {
        FREE_FLASH_TOKEN_CAP
    } else if max_ctx_tokens >= HIGH_CONTEXT_THRESHOLD {
        HIGH_CONTEXT_TOKEN_CAP
    } else {
        STANDARD_TOKEN_CAP
    }
}

/// Hard-compaction threshold in tokens for a given endpoint, capped by its tier token cap.
pub fn hard_compact_threshold(max_ctx_tokens: usize, token_cap: usize) -> usize {
    ((max_ctx_tokens * HARD_COMPACT_RATIO_PCT) / 100).min(token_cap)
}

/// Whether a full compaction should run at the current turn position. At turn start
/// (`total_turn_steps == 0`) either the token threshold or the soft message-count limit can
/// trigger it; mid-turn only the token threshold applies, so we never destroy context merely
/// because `messages.len()` grew during an active edit pass.
pub fn should_full_compact(
    total_turn_steps: usize,
    effective_tok: usize,
    message_count: usize,
    compact_threshold: usize,
) -> bool {
    if total_turn_steps == 0 {
        (effective_tok >= compact_threshold || message_count > MESSAGE_COUNT_SOFT_LIMIT)
            && message_count > MESSAGE_COUNT_HARD_LIMIT
    } else {
        effective_tok >= compact_threshold && message_count > MESSAGE_COUNT_HARD_LIMIT
    }
}

/// Retry-limit checks for the turn-recovery gates. Each is a tiny predicate so the loop reads
/// as an intent list rather than a wall of `< N` comparisons.
pub fn length_stop_allowed(retries: usize) -> bool {
    retries < MAX_LENGTH_STOP_RETRIES
}
pub fn empty_stop_allowed(retries: usize) -> bool {
    retries < MAX_EMPTY_STOP_RETRIES
}
pub fn premature_stop_allowed(retries: usize) -> bool {
    retries < MAX_PREMATURE_STOP_RETRIES
}
pub fn verify_rounds_allowed(rounds: usize) -> bool {
    rounds < MAX_VERIFY_ROUNDS
}

/// When a model output announces an upcoming action (`"let me ..."`, `"I will now ...`,
/// `"şimdi ... yapacağım"`) or ends with a trailing colon, but stops without calling a tool.
pub fn is_premature_stop(text: &str) -> bool {
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
/// mutations, and dumped a multi-line fenced code block with file/symbol markers instead of
/// calling `edit`/`write`.
pub fn is_phantom_code_block_stop(
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

/// Whether the user's latest genuine instruction asked for findings only ("sadece söyle bana",
/// "report only", "don't apply", ...). Recovery gates that force a tool call must stay dormant
/// then: after an explicit report-only directive a missing edit is compliance, not a stall.
pub fn user_asked_report_only(user_text: &str) -> bool {
    // to_lowercase maps İ to i + U+0307; strip the combining dot and normalize the curly
    // apostrophe so "İşlem yapma" / "don’t edit" still match their markers.
    let t = user_text
        .to_lowercase()
        .replace('\u{307}', "")
        .replace('\u{2019}', "'");
    const TR: &[&str] = &[
        "sadece söyle",
        "sadece anlat",
        "sadece bak",
        "sadece göster",
        "işlem yapma",
        "değişiklik yapma",
        "sadece cevapla",
    ];
    const EN: &[&str] = &[
        "just tell",
        "just report",
        "report only",
        "don't apply",
        "do not apply",
        "don't modify",
        "do not modify",
        "don't edit",
        "no edits",
    ];
    TR.iter().chain(EN.iter()).any(|m| t.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalate_only_when_local_and_repeat_threshold_reached() {
        assert!(should_escalate_to_cloud(true, MAX_REPEATS_BEFORE_ESCALATE));
        assert!(!should_escalate_to_cloud(
            true,
            MAX_REPEATS_BEFORE_ESCALATE - 1
        ));
        // A single repeat (below threshold) must NOT escalate.
        assert!(!should_escalate_to_cloud(true, 1));
        // Cloud endpoints never re-escalate (already cloud).
        assert!(!should_escalate_to_cloud(false, 99));
    }

    #[test]
    fn tier_cap_selects_by_cost_tier_and_context() {
        assert_eq!(tier_token_cap(true, 120_000), FREE_FLASH_TOKEN_CAP);
        assert_eq!(
            tier_token_cap(false, HIGH_CONTEXT_THRESHOLD),
            HIGH_CONTEXT_TOKEN_CAP
        );
        assert_eq!(
            tier_token_cap(false, HIGH_CONTEXT_THRESHOLD + 1),
            HIGH_CONTEXT_TOKEN_CAP
        );
        // Standard tier: below the high-context boundary.
        assert_eq!(tier_token_cap(false, 200_000), STANDARD_TOKEN_CAP);
    }

    #[test]
    fn compact_threshold_is_ratio_capped_by_tier_cap() {
        // 1M context * 80% = 800k, but standard tier caps at 120k.
        assert_eq!(
            hard_compact_threshold(1_000_000, STANDARD_TOKEN_CAP),
            STANDARD_TOKEN_CAP
        );
        // 500k context * 80% = 400k, high-context tier caps at 240k.
        assert_eq!(
            hard_compact_threshold(500_000, HIGH_CONTEXT_TOKEN_CAP),
            HIGH_CONTEXT_TOKEN_CAP
        );
        // Small context: ratio dominates (no cap hit).
        assert_eq!(hard_compact_threshold(100_000, STANDARD_TOKEN_CAP), 80_000);
    }

    #[test]
    fn full_compact_respects_turn_position_and_limits() {
        let thr = 80_000;
        // Turn start: token threshold triggers.
        assert!(should_full_compact(0, 90_000, 10, thr));
        // Turn start: soft message-count limit triggers even below the token threshold.
        assert!(should_full_compact(
            0,
            10_000,
            MESSAGE_COUNT_SOFT_LIMIT + 1,
            thr
        ));
        // Turn start: both limits low → no compaction.
        assert!(!should_full_compact(0, 10_000, 10, thr));
        // Mid-turn: only the token threshold can trigger (soft count limit ignored).
        assert!(should_full_compact(
            5,
            90_000,
            MESSAGE_COUNT_SOFT_LIMIT + 1,
            thr
        ));
        assert!(!should_full_compact(
            5,
            10_000,
            MESSAGE_COUNT_SOFT_LIMIT + 1,
            thr
        ));
    }

    #[test]
    fn compact_prune_budget_uses_fallback_then_ratio() {
        // Unknown context → fixed fallback.
        assert_eq!(compact_prune_budget(0), COMPACT_PRUNE_BUDGET_FALLBACK);
        // 200k * 25% = 50k, within the clamp range.
        assert_eq!(compact_prune_budget(200_000), 50_000);
        // Tiny context clamps up to the minimum.
        assert_eq!(compact_prune_budget(8_000), COMPACT_PRUNE_BUDGET_MIN);
        // Huge context clamps down to the maximum.
        assert_eq!(compact_prune_budget(1_000_000), COMPACT_PRUNE_BUDGET_MAX);
    }

    #[test]
    fn recovery_gate_retries_are_capped() {
        assert!(length_stop_allowed(0));
        assert!(length_stop_allowed(MAX_LENGTH_STOP_RETRIES - 1));
        assert!(!length_stop_allowed(MAX_LENGTH_STOP_RETRIES));

        assert!(empty_stop_allowed(0));
        assert!(!empty_stop_allowed(MAX_EMPTY_STOP_RETRIES));

        assert!(premature_stop_allowed(0));
        assert!(!premature_stop_allowed(MAX_PREMATURE_STOP_RETRIES));

        assert!(verify_rounds_allowed(0));
        assert!(!verify_rounds_allowed(MAX_VERIFY_ROUNDS));
    }

    #[test]
    fn premature_stop_detection_covers_real_model_output_variants() {
        // These strings are appended as the final user message, so these cases exercise the
        // real failover decision path.
        let agent = [
            "Let me start by reading the target file.",
            "I'll now refactor the module.",
            "Şimdi dosyayı inceleyeceğim",
            "Şimdi düzenleyeceğim",
            "I will now examine the flow:",
        ];
        for t in agent {
            assert!(is_premature_stop(t), "should be premature stop for: {t}");
        }

        // Terminal completions and questions must NOT trigger recovery.
        let no = [
            "Done. The refactoring compiles and all tests pass.",
            "What should I prioritize next?",
            "```\nfn main() {}\n```",
            "",
            "First, I want to understand the flow",
        ];
        for n in no {
            assert!(
                !is_premature_stop(n),
                "should NOT be premature stop for: {n}"
            );
        }
    }

    #[test]
    fn report_only_directive_is_detected_for_recovery_gate() {
        assert!(user_asked_report_only("neyi var onun sadece söyle bana"));
        assert!(user_asked_report_only("İşlem yapma, sadece rapor ver"));
        assert!(user_asked_report_only(
            "Just tell me what's wrong, don't apply anything"
        ));
        assert!(!user_asked_report_only("kardeş kaldır ozaman o modelleri"));
        assert!(!user_asked_report_only(
            "genel bi ak bakalım sorun olabilecek yerlere"
        ));
    }

    #[test]
    fn phantom_code_block_detection_requires_investigation_and_mutations() {
        // No investigation steps → not a dump.
        let short = r#"```rust
fn main() {}"#;
        assert!(!is_phantom_code_block_stop(
            short,
            AgentPhase::Code,
            0,
            false
        ));

        // Investigation + mutations → real edit, not a phantom dump.
        let dump = r#"```rust
fn compute(x: i32) -> i32 {
    x * 2 + 1
}

fn main() {
    println!("{}", compute(42));
}
"#;
        assert!(!is_phantom_code_block_stop(dump, AgentPhase::Code, 5, true));

        // Investigation, zero mutations, fence with signature → phantom dump.
        assert!(is_phantom_code_block_stop(dump, AgentPhase::Code, 3, false));
    }

    #[test]
    fn phantom_code_block_detection_ignores_questions_and_unclosed_fences() {
        // A question is never a dump.
        let q = "Should I refactor the module?";
        assert!(!is_phantom_code_block_stop(q, AgentPhase::Code, 2, false));

        // An open fence without a signature is not a dump.
        let open = r#"```rust
let x = 1;
"#;
        assert!(!is_phantom_code_block_stop(
            open,
            AgentPhase::Code,
            2,
            false
        ));
    }
}
