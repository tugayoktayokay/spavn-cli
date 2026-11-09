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

/// Token usage that triggers turn-start Context Shaking (spill to recoverable files).
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

/// Maximum claim-gate retry rounds allowed per turn before accepting with a visible warning footer.
pub const MAX_CLAIM_GATE_ROUNDS: usize = 2;

/// Repeat count thresholds for the tool-call Loop Guard state machine.
/// Count 3: Keep warning the model in the tool result.
pub const LOOP_GUARD_WARN_THRESHOLD: usize = 3;
/// Count 4: Hard block — do NOT execute the tool, return a BLOCKED tool result.
pub const LOOP_GUARD_BLOCK_THRESHOLD: usize = 4;
/// Count 6: Hard stop — abort the agent turn and show a bilingual loop stop message.
pub const LOOP_GUARD_STOP_THRESHOLD: usize = 6;

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
pub fn claim_gate_rounds_allowed(rounds: usize) -> bool {
    rounds < MAX_CLAIM_GATE_ROUNDS
}

/// State machine actions for repetitive or looping tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopGuardDecision {
    /// Count < 3: normal tool execution.
    Allow,
    /// Count == 3: execute the tool, but append a prominent warning to the tool result.
    Warn,
    /// Count 4..=5: do NOT execute the tool; return a BLOCKED tool result explaining the block.
    Block,
    /// Count >= 6: do NOT execute the tool; abort the turn and notify the user.
    StopTurn,
}

/// Evaluates the loop guard state machine given the observed execution count.
pub fn evaluate_loop_guard(count: usize) -> LoopGuardDecision {
    if count >= LOOP_GUARD_STOP_THRESHOLD {
        LoopGuardDecision::StopTurn
    } else if count >= LOOP_GUARD_BLOCK_THRESHOLD {
        LoopGuardDecision::Block
    } else if count >= LOOP_GUARD_WARN_THRESHOLD {
        LoopGuardDecision::Warn
    } else {
        LoopGuardDecision::Allow
    }
}

/// Hierarchy of verification evidence observed during an agent turn.
/// Stronger levels strictly outrank weaker levels:
/// `None < Compile < Test < Runtime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VerificationLevel {
    /// No verification command or file check was executed after the last edit.
    None = 0,
    /// Compiler / syntax checks (`cargo check`, `tsc`, `go vet`, `py_compile`, `node --check`).
    /// Proves type and syntax correctness only; never counts as behavioral proof.
    Compile = 1,
    /// Automated test suites (`cargo test`, `pytest`, `go test`, `npm test`, `jest`, `vitest`).
    Test = 2,
    /// Explicit runtime execution of program, script, curl, or smoke checks.
    Runtime = 3,
}

impl VerificationLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationLevel::None => "None",
            VerificationLevel::Compile => "Compile",
            VerificationLevel::Test => "Test",
            VerificationLevel::Runtime => "Runtime",
        }
    }

    /// Turkish and English badge text for user-visible footers.
    pub fn display_badge_tr_en(&self) -> (&'static str, &'static str) {
        match self {
            VerificationLevel::None => (
                "yapılmadı (davranış test edilmedi)",
                "none (behavior not tested)",
            ),
            VerificationLevel::Compile => (
                "yalnızca derleme (davranış test edilmedi)",
                "compile only (behavior not tested)",
            ),
            VerificationLevel::Test => ("testler çalıştırıldı", "tests executed"),
            VerificationLevel::Runtime => (
                "çalışma zamanı / smoke kontrolü yapıldı",
                "runtime / smoke check executed",
            ),
        }
    }
}

/// Splits a shell command line on top-level compound operators (`&&`, `||`, `;`, `|`),
/// respecting single/double quotes and stopping before any heredoc body (`<<EOF`).
fn split_compound_command(cmd: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = cmd.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < len {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'\\' && i + 1 < len {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        if b == b'\\' && i + 1 < len {
            i += 2;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_double = true;
            i += 1;
            continue;
        }
        // Heredoc (`<<EOF`, `<<'EOF'`, `<<-EOF`, excluding `<<<` here-string):
        // Everything after the current line's newline is heredoc payload, not shell subcommands.
        if b == b'<' && i + 1 < len && bytes[i + 1] == b'<' && (i + 2 >= len || bytes[i + 2] != b'<')
        {
            let line_end = cmd[i..].find('\n').map(|off| i + off).unwrap_or(len);
            parts.push(&cmd[start..line_end]);
            return parts;
        }
        if b == b';' || b == b'\n' {
            parts.push(&cmd[start..i]);
            start = i + 1;
            i += 1;
        } else if matches!(b, b'&' | b'|') && i + 1 < len && bytes[i + 1] == b {
            parts.push(&cmd[start..i]);
            start = i + 2;
            i += 2;
        } else if b == b'|' {
            parts.push(&cmd[start..i]);
            start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    if start < len {
        parts.push(&cmd[start..]);
    }
    parts
}

/// Strips quoted string literals (`'...'` and `"..."`) so arguments like
/// `git commit -m "fix cargo test"` do not false-match runner keywords.
fn strip_quoted_literals(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < len {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                in_single = false;
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'\\' && i + 1 < len {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_double = false;
                out.push(' ');
            }
            i += 1;
            continue;
        }
        if b == b'\\' && i + 1 < len {
            i += 2;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_double = true;
            i += 1;
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    out
}

fn has_word_token(haystack: &str, token: &str) -> bool {
    haystack.split_whitespace().any(|w| {
        w == token
            || w.strip_prefix("./").unwrap_or(w) == token
            || w.rsplit('/').next().unwrap_or(w) == token
    })
}

/// Strips leading environment variable assignments (`KEY=val`) and execution wrappers
/// (`sudo`, `env`, `nohup`, `time`, `nice`, `stdbuf`, `timeout <duration>`).
fn strip_env_vars_and_wrappers(mut s: &str) -> &str {
    loop {
        s = s.trim();
        for prefix in ["sudo ", "env ", "nohup ", "time "] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest.trim_start();
                continue;
            }
        }
        if let Some(rest) = s.strip_prefix("nice ") {
            let mut r = rest.trim_start();
            if let Some(after_n) = r.strip_prefix("-n ") {
                if let Some((_, after_val)) = after_n.trim_start().split_once(char::is_whitespace) {
                    r = after_val;
                }
            } else if r.starts_with('-') {
                if let Some((_, after_flag)) = r.split_once(char::is_whitespace) {
                    r = after_flag;
                }
            }
            s = r;
            continue;
        }
        if let Some(rest) = s.strip_prefix("stdbuf ") {
            let mut r = rest.trim_start();
            while r.starts_with('-') {
                if let Some((_, after_flag)) = r.split_once(char::is_whitespace) {
                    r = after_flag.trim_start();
                } else {
                    break;
                }
            }
            s = r;
            continue;
        }
        if let Some(rest) = s.strip_prefix("timeout ") {
            let mut r = rest.trim_start();
            while r.starts_with('-') {
                if let Some((flag, after_flag)) = r.split_once(char::is_whitespace) {
                    r = after_flag.trim_start();
                    if matches!(flag, "-k" | "-s" | "--kill-after" | "--signal") {
                        if let Some((_, after_val)) = r.split_once(char::is_whitespace) {
                            r = after_val.trim_start();
                        }
                    }
                } else {
                    break;
                }
            }
            // Strip the duration token (e.g., `240`, `240s`, `5m`)
            if let Some((dur, after_dur)) = r.split_once(char::is_whitespace) {
                if dur.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    s = after_dur.trim_start();
                    continue;
                }
            }
        }
        if let Some((first_word, rest)) = s.split_once(char::is_whitespace) {
            if matches!(
                first_word,
                "sudo" | "env" | "nohup" | "time"
            ) {
                s = rest;
                continue;
            }
            if first_word.contains('=')
                && !first_word.starts_with("./")
                && !first_word.starts_with('/')
                && !first_word.starts_with('-')
            {
                s = rest;
                continue;
            }
        }
        break;
    }
    s.trim()
}

/// Classifies a single shell command without compound operators.
fn classify_single_subcommand(subcmd: &str, modified_files: &[String]) -> VerificationLevel {
    let raw_clean = strip_env_vars_and_wrappers(subcmd);
    if raw_clean.is_empty() {
        return VerificationLevel::None;
    }
    let unquoted = strip_quoted_literals(raw_clean);
    let lower = unquoted.to_ascii_lowercase();
    let first_word = lower
        .split_whitespace()
        .next()
        .map(|w| w.rsplit('/').next().unwrap_or(w))
        .unwrap_or("");

    // Non-verification shell/VCS/file utilities never count as Test or Compile even if their
    // arguments mention test runner names (e.g. `git commit -m "fix cargo test"`).
    if matches!(
        first_word,
        "git"
            | "echo"
            | "printf"
            | "grep"
            | "rg"
            | "sed"
            | "awk"
            | "cat"
            | "head"
            | "tail"
            | "ls"
            | "cp"
            | "mv"
            | "rm"
            | "mkdir"
            | "touch"
            | "chmod"
            | "chown"
            | "install"
            | "gh"
            | "brew"
            | "tar"
            | "find"
            | "cd"
            | "pwd"
            | "export"
            | "alias"
            | "true"
            | "false"
    ) {
        return VerificationLevel::None;
    }

    // 1. Automated test suites (VerificationLevel::Test)
    // Rust test
    let is_rust_test = lower.contains("cargo test")
        || lower.contains("cargo nextest")
        || (first_word == "cargo" && has_word_token(&lower, "test"));
    // Python test
    let is_py_test = has_word_token(&lower, "pytest")
        || has_word_token(&lower, "py.test")
        || lower.contains("python -m unittest")
        || lower.contains("python3 -m unittest")
        || lower.contains("python -m pytest")
        || lower.contains("python3 -m pytest");
    // Go test
    let is_go_test = lower.contains("go test");
    // JS/TS test
    let is_js_test = lower.contains("npm test")
        || lower.contains("npm run test")
        || lower.contains("npm t ")
        || lower.ends_with("npm t")
        || lower.contains("pnpm test")
        || lower.contains("pnpm run test")
        || lower.contains("pnpm t ")
        || lower.ends_with("pnpm t")
        || lower.contains("yarn test")
        || lower.contains("yarn run test")
        || lower.contains("bun test")
        || has_word_token(&lower, "vitest")
        || has_word_token(&lower, "jest")
        || lower.contains("node --test")
        || lower.contains("tsx --test")
        || lower.contains("deno test");
    // Swift / Other tests
    let is_other_test = lower.contains("swift test")
        || has_word_token(&lower, "ctest")
        || lower.contains("mix test")
        || lower.contains("mvn test")
        || lower.contains("gradle test")
        || has_word_token(&lower, "rspec");

    if is_rust_test || is_py_test || is_go_test || is_js_test || is_other_test {
        return VerificationLevel::Test;
    }

    // 2. Compilation / syntax / static analysis checks (VerificationLevel::Compile)
    // Rust compile/lint
    let is_rust_compile = lower.contains("cargo check")
        || lower.contains("cargo clippy")
        || lower.contains("cargo build");
    // TypeScript compile
    let is_ts_compile = lower == "tsc"
        || lower.starts_with("tsc ")
        || lower.ends_with(" tsc")
        || lower.contains(" tsc ")
        || lower.contains("npx tsc")
        || lower.contains("pnpm tsc")
        || lower.contains("yarn tsc")
        || lower.contains("bun x tsc");
    // Go compile/vet
    let is_go_compile = lower.contains("go vet") || lower.contains("go build");
    // Python syntax check
    let is_py_compile = lower.contains("py_compile")
        || lower.contains("mypy")
        || lower.contains("flake8")
        || lower.contains("ruff check");
    // Node syntax check
    let is_node_compile = lower.contains("node --check") || lower.contains("node -c ");
    // Generic builds
    let is_generic_build = lower.contains("npm run build")
        || lower.contains("pnpm build")
        || lower.contains("pnpm run build")
        || lower.contains("yarn build")
        || lower.contains("bun run build")
        || lower.contains("swift build")
        || lower.contains("javac ")
        || lower.contains("mvn compile")
        || lower.contains("gradle compile")
        || lower.contains("cmake --build")
        || lower.contains("ninja");

    if is_rust_compile
        || is_ts_compile
        || is_go_compile
        || is_py_compile
        || is_node_compile
        || is_generic_build
    {
        return VerificationLevel::Compile;
    }

    // 3. Runtime / smoke execution (VerificationLevel::Runtime)
    // HTTP endpoints check
    let is_curl = lower.starts_with("curl ") || lower.contains(" curl ");
    // Running built binary directly
    let is_binary_run = lower.contains("cargo run")
        || lower.starts_with("./target/debug/")
        || lower.starts_with("./target/release/")
        || lower.starts_with("./bin/")
        || lower.contains("/target/debug/")
        || lower.contains("/target/release/");
    // Direct script interpreter execution (excluding the syntax/test checks handled above)
    let is_script_run = (lower.starts_with("python ") || lower.starts_with("python3 "))
        || (lower.starts_with("node ") && !lower.contains("--check") && !lower.contains("--test"))
        || (lower.starts_with("tsx ") && !lower.contains("--test"))
        || lower.starts_with("ts-node ")
        || (lower.starts_with("bun run ") && !lower.contains("build") && !lower.contains("test"))
        || lower.starts_with("bash ")
        || lower.starts_with("sh ");
    // Explicit smoke test commands
    let is_smoke_label =
        lower.contains("smoke") || lower.contains("# smoke") || lower.contains("--smoke");
    // Check if the command explicitly runs any file mutated in this turn
    let runs_modified_file = modified_files.iter().any(|f| {
        let f_lower = f.to_ascii_lowercase();
        let f_name = std::path::Path::new(&f_lower)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        (!f_name.is_empty() && lower.contains(f_name)) || lower.contains(&f_lower)
    });

    if is_curl || is_binary_run || is_script_run || is_smoke_label || runs_modified_file {
        return VerificationLevel::Runtime;
    }

    VerificationLevel::None
}

/// Classifies a shell command into the strongest [`VerificationLevel`] it exercises.
///
/// Compound commands chained with `&&`, `||`, `;`, or `|` are decomposed into individual
/// subcommands, and the strongest level observed is returned.
pub fn classify_verification_command(
    cmd_text: &str,
    modified_files: &[String],
) -> VerificationLevel {
    let mut strongest = VerificationLevel::None;
    for part in split_compound_command(cmd_text) {
        let level = classify_single_subcommand(part, modified_files);
        if level > strongest {
            strongest = level;
            if strongest == VerificationLevel::Runtime {
                break;
            }
        }
    }
    strongest
}

/// Pulls the command string out of raw shell tool arguments (JSON or a bare string).
fn shell_command_text(raw_args: &str) -> (String, Option<serde_json::Value>) {
    let trimmed = raw_args.trim();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return (trimmed.to_string(), None);
    };
    let cmd = ["command", "cmd", "script", "shell"]
        .iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_string();
    (cmd, Some(v))
}

/// Extracts the command from raw tool arguments (JSON or raw string) and classifies it.
/// If the model explicitly labeled the call as a smoke test via JSON argument fields,
/// elevates the verification level to [`VerificationLevel::Runtime`].
pub fn classify_shell_tool_call(raw_args: &str, modified_files: &[String]) -> VerificationLevel {
    let (cmd, parsed) = shell_command_text(raw_args);
    if let Some(ref v) = parsed {
        if let Some(lang) = v.get("lang").and_then(|x| x.as_str()) {
            if matches!(
                lang.trim().to_ascii_lowercase().as_str(),
                "py" | "python" | "js" | "node" | "ts"
            ) && !cmd.trim().is_empty()
            {
                return VerificationLevel::Runtime;
            }
        }
    }
    let mut level = classify_verification_command(&cmd, modified_files);
    if let Some(v) = parsed {
        let is_smoke_flag = v.get("smoke").and_then(|x| x.as_bool()).unwrap_or(false)
            || v.get("label")
                .and_then(|x| x.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("smoke"))
            || v.get("type")
                .and_then(|x| x.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("smoke"));
        if is_smoke_flag && !cmd.trim().is_empty() && level < VerificationLevel::Runtime {
            level = VerificationLevel::Runtime;
        }
    }
    level
}

/// Test commands inside one shell call, as whitespace tokens with wrappers, redirections,
/// and non-scope display/profile flags dropped (`nohup timeout 240 cargo test --release 2>&1` → `["cargo", "test"]`).
fn test_invocations(cmd: &str, modified_files: &[String]) -> Vec<Vec<String>> {
    split_compound_command(cmd)
        .into_iter()
        .filter(|part| classify_single_subcommand(part, modified_files) == VerificationLevel::Test)
        .map(|part| {
            let cleaned = strip_env_vars_and_wrappers(part);
            cleaned
                .split_whitespace()
                .filter(|t| {
                    !t.contains('>')
                        && !t.contains('<')
                        && !matches!(
                            *t,
                            "--" | "--lib"
                                | "--bins"
                                | "--tests"
                                | "--all-targets"
                                | "--workspace"
                                | "--quiet"
                                | "-q"
                                | "--nocapture"
                                | "--show-output"
                                | "--release"
                                | "-r"
                                | "--locked"
                                | "--offline"
                                | "--frozen"
                                | "--no-fail-fast"
                                | "--color=always"
                                | "--color=never"
                                | "--color=auto"
                        )
                })
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|tokens| !tokens.is_empty())
        .collect()
}

/// Detects whether a compound command (`cargo test && git push ...`) that exited non-zero
/// actually passed its test subcommand(s) and only failed in a subsequent non-test subcommand.
fn compound_test_subcommand_passed(
    cmd: &str,
    tool_output: &str,
    modified_files: &[String],
) -> bool {
    if tool_output.is_empty()
        || tool_output.contains("[SPAVN Timeout:")
        || crate::tools::has_compiler_or_test_error_signature(tool_output)
    {
        return false;
    }
    let parts = split_compound_command(cmd);
    if parts.len() < 2 {
        return false;
    }
    // Check that there is at least one non-test subcommand after a test subcommand
    let mut saw_test = false;
    let mut saw_non_test_after_test = false;
    for part in parts {
        let lvl = classify_single_subcommand(part, modified_files);
        if lvl == VerificationLevel::Test {
            saw_test = true;
        } else if saw_test && !part.trim().is_empty() {
            saw_non_test_after_test = true;
        }
    }
    if !saw_non_test_after_test {
        return false;
    }
    // Verify the output contains an explicit test suite pass marker and zero failures
    let has_rust_pass = tool_output.contains("test result: ok.") && !tool_output.contains("FAILED");
    let has_pytest_or_jest_pass = (tool_output.contains(" passed") || tool_output.contains("PASS "))
        && !tool_output.contains(" failed")
        && !tool_output.contains("FAIL ");
    let has_go_pass = tool_output.contains("\nok  \t") && !tool_output.contains("--- FAIL:");
    has_rust_pass || has_pytest_or_jest_pass || has_go_pass
}

/// Test runs that failed during a turn and have not been re-run green since.
///
/// A failing test is evidence the change is broken. A later green run only clears it when it
/// covers at least as much: the same command or a broader one, i.e. one whose tokens are a
/// prefix of the failed command (`cargo test` clears a failed `cargo test parser`, while
/// `cargo test parser` does not clear a failed `cargo test`). While any failure stays open, no
/// shell call earns more than [`VerificationLevel::Compile`].
#[derive(Debug, Default)]
pub struct FailedTestRuns {
    open: Vec<Vec<String>>,
}

impl FailedTestRuns {
    /// Feeds one finished shell call and returns the verification level it earns.
    pub fn observe(
        &mut self,
        raw_args: &str,
        succeeded: bool,
        modified_files: &[String],
    ) -> VerificationLevel {
        self.observe_with_output(raw_args, succeeded, "", modified_files)
    }

    /// Feeds one finished shell call along with its captured output and returns the
    /// verification level it earns. Inspects `tool_output` so that compound commands like
    /// `cargo test && git push` where `cargo test` passed (`test result: ok.`) and `git push`
    /// failed do not falsely poison `FailedTestRuns`.
    pub fn observe_with_output(
        &mut self,
        raw_args: &str,
        succeeded: bool,
        tool_output: &str,
        modified_files: &[String],
    ) -> VerificationLevel {
        let (cmd, parsed) = shell_command_text(raw_args);
        if let Some(ref v) = parsed {
            if let Some(lang) = v.get("lang").and_then(|x| x.as_str()) {
                if matches!(
                    lang.trim().to_ascii_lowercase().as_str(),
                    "py" | "python" | "js" | "node" | "ts"
                ) {
                    if !succeeded {
                        return VerificationLevel::None;
                    }
                    let level = VerificationLevel::Runtime;
                    return if self.is_open() {
                        level.min(VerificationLevel::Compile)
                    } else {
                        level
                    };
                }
            }
        }
        let runs = test_invocations(&cmd, modified_files);
        let tests_effectively_succeeded =
            succeeded || (!runs.is_empty() && compound_test_subcommand_passed(&cmd, tool_output, modified_files));
        if !tests_effectively_succeeded {
            for run in runs {
                if !self.open.contains(&run) {
                    self.open.push(run);
                }
            }
            return VerificationLevel::None;
        }
        for run in &runs {
            self.open.retain(|failed| !failed.starts_with(run));
        }
        let level = classify_shell_tool_call(raw_args, modified_files);
        if self.is_open() {
            level.min(VerificationLevel::Compile)
        } else {
            level
        }
    }

    pub fn is_open(&self) -> bool {
        !self.open.is_empty()
    }

    /// The still-failing commands, joined for a gate message (`cargo test; pytest`).
    pub fn describe(&self) -> String {
        self.open
            .iter()
            .map(|r| r.join(" "))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The bilingual footer appended to an accepted answer whose verification is below Test.
/// Names any test that failed and was never re-run green.
pub fn verification_footer(level: VerificationLevel, failed: &FailedTestRuns) -> String {
    if failed.is_open() {
        return format!(
            "\n\n⚠ Doğrulama: test başarısız, aynı ya da daha geniş komutla tekrar geçmedi / Verification: a test failed and never passed again with the same or a broader command\n⚠ Hâlâ başarısız / Still failing: `{}`",
            failed.describe()
        );
    }
    let (badge_tr, badge_en) = level.display_badge_tr_en();
    format!("\n\n⚠ Doğrulama: {badge_tr} / Verification: {badge_en}")
}

/// Extracts a normalized signature for a failing tool execution to identify repeating errors.
pub fn extract_tool_error_signature(
    tool_name: &str,
    raw_args: &str,
    output: &str,
) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    let is_err = trimmed.starts_with("Edit error:")
        || trimmed.starts_with("Read error")
        || trimmed.starts_with("Write error")
        || trimmed.starts_with("Execution error:")
        || trimmed.starts_with("BLOCKED")
        || trimmed.contains("No such file or directory")
        || trimmed.contains("not found")
        || trimmed.contains("(exit code ");

    if !is_err {
        return None;
    }

    let target = if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw_args) {
        v.get("path")
            .or_else(|| v.get("file_path"))
            .or_else(|| v.get("command"))
            .or_else(|| v.get("cmd"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        String::new()
    };

    let first_line = trimmed.lines().next().unwrap_or("").trim();
    let err_norm: String = first_line.chars().take(80).collect();

    Some(format!("{}:{}:{}", tool_name.trim(), target, err_norm))
}

/// Strips fenced code blocks (```...```) and inline code spans (`...`) from markdown text.
fn strip_markdown_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut in_backtick = false;
        for ch in line.chars() {
            if ch == '`' {
                in_backtick = !in_backtick;
            } else if !in_backtick {
                out.push(ch);
            }
        }
        out.push('\n');
    }
    out
}

/// Returns true if the text explicitly and honestly discloses that tests or verification
/// were not run, or that behavior remains untested.
pub fn discloses_untested_status(text: &str) -> bool {
    let lower = text.to_lowercase().replace('\u{307}', "");
    const UNTESTED_PHRASES: &[&str] = &[
        "test edilmedi",
        "test edilmemis",
        "test edilmemiş",
        "test edilmemistir",
        "test edilmemiştir",
        "dogrulanmadi",
        "doğrulanmadı",
        "dogrulanmamis",
        "doğrulanmamış",
        "yurutulmedi",
        "yürütülmedi",
        "yurutulmemis",
        "yürütülmemiş",
        "calistirilmadi",
        "çalıştırılmadı",
        "komut calistirilmadi",
        "komut çalıştırılmadı",
        "test calistirilmadi",
        "test çalıştırılmadı",
        "davranissal test",
        "davranışsal test",
        "davranis dogrulanmadi",
        "davranış doğrulanmadı",
        "davranissal dogrulama yurutulmedi",
        "davranışsal doğrulama yürütülmedi",
        "test kosulmadi",
        "test koşulmadı",
        "not tested",
        "untested",
        "behavior not verified",
        "behavior not tested",
        "behavior untested",
        "tests were not run",
        "no tests run",
        "tests not run",
        "without running tests",
        "without verification",
    ];

    UNTESTED_PHRASES.iter().any(|p| lower.contains(p))
}

/// Returns true if the user's prompt explicitly forbids running commands or tests.
pub fn prompt_forbids_execution(prompt: &str) -> bool {
    let lower = prompt.to_lowercase().replace('\u{307}', "");
    const FORBID_PHRASES: &[&str] = &[
        "komut calistirma",
        "komut çalıştırma",
        "test calistirma",
        "test çalıştırma",
        "test kosma",
        "test koşma",
        "hicbir komut",
        "hiçbir komut",
        "hicbir test",
        "hiçbir test",
        "sadece dosya",
        "sadece dosyayi",
        "sadece dosyayı",
        "don't run",
        "dont run",
        "do not run",
        "no tests",
        "no command",
        "without running",
        "without tests",
        "edit only",
        "only edit",
    ];

    FORBID_PHRASES.iter().any(|p| lower.contains(p))
}

/// Detects success claims in model output text (TR + EN) outside code blocks.
///
/// Design decision:
/// - Explicit success claims (e.g. `düzelttim`, `düzeldi`, `çözüldü`, `çözdüm`, `kalıcı olarak düzelttim`,
///   `kalıcı çözüm`, `doğrulandı`, `sorun kalmadı`, `artık çalışıyor`, `canlıda`, `fixed`, `resolved`,
///   `works now`, `verified`, `deployed`, `all tests pass`) require Test or Runtime verification
///   when files were mutated in the turn.
/// - Bare words like `kalıcı` or `çalışıyor` are NOT claims by themselves; they require claim phrases.
/// - Explicit disclosure of untested status (`test edilmedi`, `yürütülmedi`, `not tested`) is treated
///   as an honest rewrite and returns `None` so the gate does not over-fire.
pub fn detect_success_claim(text: &str) -> Option<String> {
    if discloses_untested_status(text) {
        return None;
    }
    let cleaned = strip_markdown_code(text);
    if cleaned.trim().is_empty() {
        return None;
    }
    let norm = cleaned
        .to_lowercase()
        .replace('\u{307}', "") // Turkish combining dot above (from İ)
        .replace('\u{2019}', "'");

    // 1. Multi-word phrase matches
    const PHRASES: &[(&str, &str)] = &[
        ("sorun kalmadı", "sorun kalmadı"),
        ("sorun kalmadi", "sorun kalmadı"),
        ("problem kalmadı", "problem kalmadı"),
        ("problem kalmadi", "problem kalmadı"),
        ("hata kalmadı", "hata kalmadı"),
        ("hata kalmadi", "hata kalmadı"),
        ("kalıcı olarak düzelt", "kalıcı olarak düzelttim"),
        ("kalici olarak duzelt", "kalıcı olarak düzelttim"),
        ("kalıcı olarak çöz", "kalıcı olarak çözüldü"),
        ("kalici olarak coz", "kalıcı olarak çözüldü"),
        ("kalıcı çözüm", "kalıcı çözüm"),
        ("kalici cozum", "kalıcı çözüm"),
        ("sorun kalıcı olarak", "sorun kalıcı olarak"),
        ("sorun kalici olarak", "sorun kalıcı olarak"),
        ("artık kalıcı", "kalıcı"),
        ("artik kalici", "kalıcı"),
        ("artık çalışıyor", "çalışıyor"),
        ("artik calisiyor", "çalışıyor"),
        ("düzgün çalışıyor", "çalışıyor"),
        ("duzgun calisiyor", "çalışıyor"),
        ("başarıyla çalışıyor", "çalışıyor"),
        ("basariyla calisiyor", "çalışıyor"),
        ("sorunsuz çalışıyor", "çalışıyor"),
        ("sorunsuz calisiyor", "çalışıyor"),
        ("hatasız çalışıyor", "çalışıyor"),
        ("hatasiz calisiyor", "çalışıyor"),
        ("başarıyla tamamlandı", "tamamlandı"),
        ("basariyla tamamlandi", "tamamlandı"),
        ("canlıya alındı", "canlıda"),
        ("canliya alindi", "canlıda"),
        ("all tests pass", "all tests pass"),
        ("all tests passed", "all tests passed"),
        ("all tests are passing", "all tests pass"),
        ("works now", "works now"),
        ("working now", "working now"),
        ("now works", "now works"),
        ("it works", "it works"),
        ("issue is resolved", "resolved"),
        ("issue resolved", "resolved"),
        ("bug is resolved", "resolved"),
        ("bug resolved", "resolved"),
        ("bug is fixed", "fixed"),
        ("bug fixed", "fixed"),
        ("problem solved", "problem solved"),
        ("problem is solved", "problem solved"),
    ];

    for &(pat, display_claim) in PHRASES {
        if norm.contains(pat) {
            return Some(display_claim.to_string());
        }
    }

    // 2. Token-level stem matching with word-start boundary
    for raw_token in norm.split(|c: char| !c.is_alphanumeric() && c != '-') {
        let token = raw_token.trim();
        if token.is_empty() {
            continue;
        }

        // Exclude technical compounds and non-claim words
        if token.starts_with("fixed-")
            || token == "fixture"
            || token == "fixtures"
            || token == "prefix"
            || token == "suffix"
            || token == "affixed"
            || token == "transfixed"
            || token == "unresolved"
            || token == "unverified"
            || token == "undeployed"
        {
            continue;
        }

        // Exclude Turkish suggestion / infinitive / negative forms
        if token.starts_with("duzeltme")
            || token.starts_with("düzeltme")
            || token.starts_with("duzeltilme")
            || token.starts_with("düzeltilme")
            || token.starts_with("duzeltmek")
            || token.starts_with("düzeltmek")
            || token.starts_with("duzeltilmeli")
            || token.starts_with("düzeltilmeli")
            || token.starts_with("duzeltilmedi")
            || token.starts_with("düzeltilmedi")
            || token.starts_with("duzeltmedim")
            || token.starts_with("düzeltmedim")
            || token.starts_with("duzelmedi")
            || token.starts_with("düzelmedi")
            || token.starts_with("cozulmedi")
            || token.starts_with("çözülmedi")
            || token.starts_with("cozulme")
            || token.starts_with("çözülme")
            || token.starts_with("cozmedim")
            || token.starts_with("çözmedim")
            || token.starts_with("cozulmeli")
            || token.starts_with("çözülmeli")
            || token.starts_with("dogrulanmadi")
            || token.starts_with("doğrulanmadı")
            || token.starts_with("dogrulanma")
            || token.starts_with("doğrulanma")
            || token.starts_with("calismiyor")
            || token.starts_with("çalışmıyor")
            || token.starts_with("calisiyorum")
            || token.starts_with("çalışıyorum")
        {
            continue;
        }

        // Positive Turkish claim stems
        if token.starts_with("duzeltt")
            || token.starts_with("düzeltt")
            || token.starts_with("duzelti")
            || token.starts_with("düzelti")
        {
            return Some("düzelttim".to_string());
        }
        if token.starts_with("duzeldi")
            || token.starts_with("düzeldi")
            || token.starts_with("duzelmis")
            || token.starts_with("düzelmiş")
        {
            return Some("düzeldi".to_string());
        }
        if token.starts_with("cozdum")
            || token.starts_with("çözdüm")
            || token.starts_with("cozduk")
            || token.starts_with("çözdük")
        {
            return Some("çözdüm".to_string());
        }
        if token.starts_with("cozuldu")
            || token.starts_with("çözüldü")
            || token.starts_with("cozulmus")
            || token.starts_with("çözülmüş")
        {
            return Some("çözüldü".to_string());
        }
        if token.starts_with("dogrulandi")
            || token.starts_with("doğrulandı")
            || token.starts_with("dogruladim")
            || token.starts_with("doğruladım")
            || token.starts_with("dogruladık")
            || token.starts_with("doğruladık")
            || token.starts_with("dogrulanmis")
            || token.starts_with("doğrulanmış")
        {
            return Some("doğrulandı".to_string());
        }
        if token == "canlida" || token == "canlıda" {
            return Some("canlıda".to_string());
        }

        // Positive English claim words
        if token == "fixed" {
            if norm.contains("not fixed")
                || norm.contains("never fixed")
                || norm.contains("unfixed")
            {
                continue;
            }
            return Some("fixed".to_string());
        }
        if token == "resolved" {
            if norm.contains("not resolved") || norm.contains("unresolved") {
                continue;
            }
            return Some("resolved".to_string());
        }
        if token == "verified" {
            if norm.contains("not verified") || norm.contains("unverified") {
                continue;
            }
            return Some("verified".to_string());
        }
        if token == "deployed" {
            if norm.contains("not deployed") || norm.contains("undeployed") {
                continue;
            }
            return Some("deployed".to_string());
        }
    }

    None
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

    #[test]
    fn verification_level_ordering_and_display() {
        assert!(VerificationLevel::None < VerificationLevel::Compile);
        assert!(VerificationLevel::Compile < VerificationLevel::Test);
        assert!(VerificationLevel::Test < VerificationLevel::Runtime);

        assert_eq!(VerificationLevel::None.as_str(), "None");
        assert_eq!(VerificationLevel::Compile.as_str(), "Compile");
        assert_eq!(VerificationLevel::Test.as_str(), "Test");
        assert_eq!(VerificationLevel::Runtime.as_str(), "Runtime");

        let (tr, en) = VerificationLevel::Compile.display_badge_tr_en();
        assert!(tr.contains("yalnızca derleme"));
        assert!(en.contains("compile only"));
    }

    #[test]
    fn failed_test_is_only_cleared_by_the_same_or_a_broader_green_run() {
        let mods: [String; 0] = [];
        let call = |cmd: &str| serde_json::json!({ "command": cmd }).to_string();
        let mut runs = FailedTestRuns::default();

        assert_eq!(
            runs.observe(&call("cargo test 2>&1 | tail -5"), false, &mods),
            VerificationLevel::None
        );
        // A green narrower subset does not prove the failing suite passes.
        assert_eq!(
            runs.observe(&call("cargo test parser"), true, &mods),
            VerificationLevel::Compile
        );
        assert!(runs.is_open());
        assert_eq!(runs.describe(), "cargo test");
        // Nor does a runtime smoke while the failure stays open.
        assert_eq!(
            runs.observe(&call("cargo run -- --help"), true, &mods),
            VerificationLevel::Compile
        );
        // Re-running the failed command green clears it.
        assert_eq!(
            runs.observe(&call("cd crate && cargo test"), true, &mods),
            VerificationLevel::Test
        );
        assert!(!runs.is_open());

        // A broader green run clears a narrower failure.
        runs.observe(&call("pytest tests/test_a.py -k slow"), false, &mods);
        assert_eq!(
            runs.observe(&call("pytest"), true, &mods),
            VerificationLevel::Test
        );
        assert!(!runs.is_open());

        // Non-test failures never open the ledger.
        runs.observe(&call("ls missing"), false, &mods);
        assert!(!runs.is_open());

        // Runner display/target flags (--lib, --nocapture, --) normalize so full suite clears filtered runs.
        runs.observe(
            &call("cargo test --manifest-path Cargo.toml -- --nocapture mask_secret_preview"),
            false,
            &mods,
        );
        runs.observe(
            &call("cargo test --manifest-path Cargo.toml hud::pickers::menu_tests"),
            false,
            &mods,
        );
        assert_eq!(
            runs.observe(
                &call("cargo test --manifest-path Cargo.toml --lib"),
                true,
                &mods
            ),
            VerificationLevel::Test
        );
        assert!(!runs.is_open());

        // 1. Wrappers (`nohup`, `timeout 240`, `--release`) normalize so a plain `cargo test` clears them.
        runs.observe(
            &call("nohup timeout 240 cargo test --release"),
            false,
            &mods,
        );
        assert!(runs.is_open());
        assert_eq!(runs.describe(), "cargo test");
        assert_eq!(
            runs.observe(&call("cargo test"), true, &mods),
            VerificationLevel::Test
        );
        assert!(!runs.is_open());

        // 2. Quoted strings, heredocs, git commit messages, and inline Python/Rust snippets
        // containing `#[cfg(test)]` or `cargo test` never poison FailedTestRuns.
        runs.observe(
            &call("python3 -c 's = \"} } #[cfg(test)] mod en { cargo test }\"; raise SystemExit(1)'"),
            false,
            &mods,
        );
        assert!(!runs.is_open());
        runs.observe(
            &call("git commit -m \"fix cargo test && pytest\" && git push origin main --tags"),
            false,
            &mods,
        );
        assert!(!runs.is_open());
        runs.observe(
            &serde_json::json!({
                "lang": "py",
                "command": "# } } #[cfg(test)] mod en\n# cargo test\nraise RuntimeError('boom')"
            })
            .to_string(),
            false,
            &mods,
        );
        assert!(!runs.is_open());

        // 3. Compound command where `cargo test` passes (`test result: ok.`) and a subsequent
        // non-test command (`git push`) fails clears open `cargo test` and earns VerificationLevel::Test.
        runs.observe(&call("cargo test --quiet"), false, &mods);
        assert!(runs.is_open());
        let compound_out = "running 342 tests\ntest result: ok. 339 passed; 0 failed; 3 ignored\n! [rejected] v0.1.0 -> v0.1.0 (already exists)\n(exit code 1)";
        assert_eq!(
            runs.observe_with_output(
                &call("cargo test --quiet && cargo build --release && git push origin main --tags"),
                false,
                compound_out,
                &mods,
            ),
            VerificationLevel::Test
        );
        assert!(!runs.is_open());
    }

    #[test]
    fn classify_verification_command_representative_and_compounds() {
        let no_mods: [String; 0] = [];
        assert_eq!(
            classify_verification_command("cargo check", &no_mods),
            VerificationLevel::Compile
        );
        assert_eq!(
            classify_verification_command("cargo test", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("npx tsx --test a.ts && npm run build", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("ls", &no_mods),
            VerificationLevel::None
        );
        assert_eq!(
            classify_verification_command("cargo clippy --workspace", &no_mods),
            VerificationLevel::Compile
        );
        assert_eq!(
            classify_verification_command("cargo build --release", &no_mods),
            VerificationLevel::Compile
        );
        assert_eq!(
            classify_verification_command("tsc --noEmit", &no_mods),
            VerificationLevel::Compile
        );
        assert_eq!(
            classify_verification_command("go vet ./...", &no_mods),
            VerificationLevel::Compile
        );
        assert_eq!(
            classify_verification_command("go build ./...", &no_mods),
            VerificationLevel::Compile
        );
        assert_eq!(
            classify_verification_command("python3 -m py_compile foo.py", &no_mods),
            VerificationLevel::Compile
        );
        assert_eq!(
            classify_verification_command("node --check server.js", &no_mods),
            VerificationLevel::Compile
        );
        assert_eq!(
            classify_verification_command("pytest tests/", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("go test ./...", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("npm test", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("pnpm test", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("yarn test", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("bun test", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("vitest run", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("jest", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("node --test test.js", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("deno test", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("swift test", &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_verification_command("curl -s http://localhost:8080/health", &no_mods),
            VerificationLevel::Runtime
        );
        assert_eq!(
            classify_verification_command("python3 main.py", &no_mods),
            VerificationLevel::Runtime
        );
        assert_eq!(
            classify_verification_command("node server.js", &no_mods),
            VerificationLevel::Runtime
        );
        assert_eq!(
            classify_verification_command("cargo run --bin app", &no_mods),
            VerificationLevel::Runtime
        );
        assert_eq!(
            classify_verification_command("./target/debug/mybin", &no_mods),
            VerificationLevel::Runtime
        );
        assert_eq!(
            classify_verification_command("npm run smoke", &no_mods),
            VerificationLevel::Runtime
        );
        assert_eq!(
            classify_verification_command("git status", &no_mods),
            VerificationLevel::None
        );
        assert_eq!(
            classify_verification_command("cat README.md", &no_mods),
            VerificationLevel::None
        );
    }

    #[test]
    fn classify_shell_tool_call_json_and_smoke_label() {
        let no_mods: [String; 0] = [];
        assert_eq!(
            classify_shell_tool_call(r#"{"cmd":"cargo check"}"#, &no_mods),
            VerificationLevel::Compile
        );
        assert_eq!(
            classify_shell_tool_call(r#"{"command":"cargo test"}"#, &no_mods),
            VerificationLevel::Test
        );
        assert_eq!(
            classify_shell_tool_call(r#"{"command":"python3 script.py","smoke":true}"#, &no_mods),
            VerificationLevel::Runtime
        );
        assert_eq!(
            classify_shell_tool_call(r#"{"command":"./run.sh","label":"smoke"}"#, &no_mods),
            VerificationLevel::Runtime
        );
    }

    #[test]
    fn detect_success_claim_positive_tr_and_en() {
        assert_eq!(
            detect_success_claim("Sorunu düzelttim, sorunsuz derleniyor.").as_deref(),
            Some("düzelttim")
        );
        assert_eq!(
            detect_success_claim("Bug düzeldi.").as_deref(),
            Some("düzeldi")
        );
        assert_eq!(
            detect_success_claim("Tüm sorunlar çözüldü.").as_deref(),
            Some("çözüldü")
        );
        assert_eq!(
            detect_success_claim("Hatayı çözdüm.").as_deref(),
            Some("çözdüm")
        );
        assert_eq!(
            detect_success_claim("Sorunu kalıcı olarak düzelttim.").as_deref(),
            Some("kalıcı olarak düzelttim")
        );
        assert_eq!(
            detect_success_claim("Kalıcı çözüm uygulandı.").as_deref(),
            Some("kalıcı çözüm")
        );
        assert_eq!(
            detect_success_claim("İşlem doğrulandı.").as_deref(),
            Some("doğrulandı")
        );
        assert_eq!(
            detect_success_claim("Artık sorun kalmadı.").as_deref(),
            Some("sorun kalmadı")
        );
        assert_eq!(
            detect_success_claim("Sistem artık çalışıyor.").as_deref(),
            Some("çalışıyor")
        );
        assert_eq!(
            detect_success_claim("Değişiklik canlıda.").as_deref(),
            Some("canlıda")
        );

        assert_eq!(
            detect_success_claim("I have fixed the issue.").as_deref(),
            Some("fixed")
        );
        assert_eq!(
            detect_success_claim("The bug has been resolved.").as_deref(),
            Some("resolved")
        );
        assert_eq!(
            detect_success_claim("Everything works now.").as_deref(),
            Some("works now")
        );
        assert_eq!(
            detect_success_claim("Verified that behavior matches specification.").as_deref(),
            Some("verified")
        );
        assert_eq!(
            detect_success_claim("Successfully deployed.").as_deref(),
            Some("deployed")
        );
        assert_eq!(
            detect_success_claim("All tests pass successfully.").as_deref(),
            Some("all tests pass")
        );
    }

    #[test]
    fn detect_success_claim_negatives_and_code_blocks() {
        // Suggestions and future intent do not count as success claims
        assert!(detect_success_claim("Bu bir düzeltme önerisi olarak incelenebilir.").is_none());
        assert!(detect_success_claim("düzeltme planı hazırladım").is_none());
        assert!(detect_success_claim("Bunu düzeltmek için şu adımı izleyin").is_none());
        assert!(detect_success_claim("Bu hatanın çözülmesi gerekiyor").is_none());
        assert!(detect_success_claim("Bu sorunun düzeltilmesi önerilir").is_none());

        // Explicit negations
        assert!(detect_success_claim("Hata henüz düzeltilmedi").is_none());
        assert!(detect_success_claim("Sorun çözülmedi").is_none());
        assert!(detect_success_claim("Doğrulanmadı").is_none());
        assert!(detect_success_claim("Sistem çalışmıyor").is_none());
        assert!(detect_success_claim("The bug is not fixed yet").is_none());
        assert!(detect_success_claim("Unresolved issue in parser").is_none());
        assert!(detect_success_claim("Unverified modifications").is_none());

        // Technical compounds and unrelated words
        assert!(detect_success_claim("Here is a prefix tree with fixed-width layout.").is_none());
        assert!(detect_success_claim("Using a test fixture in tests/mod.rs").is_none());

        // Code blocks are ignored
        let code_block = "```rust\n// Bug is fixed here\nfn fixed() {}\n```";
        assert!(detect_success_claim(code_block).is_none());

        // Inline backticks are ignored
        assert!(detect_success_claim("Set `is_fixed = true` in config").is_none());

        // Honest disclosures of untested status
        assert!(detect_success_claim("yalnızca statik dosya düzenlemesi kalıcı olarak yapıldı; davranışsal testler yürütülmemiş olarak bırakıldı").is_none());
        assert!(detect_success_claim("davranışsal doğrulama yürütülmedi").is_none());
        assert!(detect_success_claim("test edilmedi").is_none());
        assert!(detect_success_claim("not tested").is_none());
        assert!(detect_success_claim("behavior untested").is_none());

        // Bare words without claim phrases
        assert!(detect_success_claim("kalıcı ayarlar dosyaya yazıldı").is_none());
        assert!(detect_success_claim("burada bir arka plan servisi çalışıyor").is_none());
    }
    #[test]
    fn loop_guard_state_machine_thresholds() {
        assert_eq!(evaluate_loop_guard(0), LoopGuardDecision::Allow);
        assert_eq!(evaluate_loop_guard(1), LoopGuardDecision::Allow);
        assert_eq!(evaluate_loop_guard(2), LoopGuardDecision::Allow);
        assert_eq!(
            evaluate_loop_guard(LOOP_GUARD_WARN_THRESHOLD),
            LoopGuardDecision::Warn
        );
        assert_eq!(
            evaluate_loop_guard(LOOP_GUARD_BLOCK_THRESHOLD),
            LoopGuardDecision::Block
        );
        assert_eq!(evaluate_loop_guard(5), LoopGuardDecision::Block);
        assert_eq!(
            evaluate_loop_guard(LOOP_GUARD_STOP_THRESHOLD),
            LoopGuardDecision::StopTurn
        );
        assert_eq!(evaluate_loop_guard(10), LoopGuardDecision::StopTurn);
    }

    #[test]
    fn extract_tool_error_signature_identifies_repeating_failures() {
        let sig = extract_tool_error_signature(
            "read",
            r#"{"path":"foo.rs"}"#,
            "Read error: No such file or directory",
        );
        assert_eq!(
            sig.as_deref(),
            Some("read:foo.rs:Read error: No such file or directory")
        );

        let sig2 = extract_tool_error_signature(
            "edit",
            r#"{"path":"bar.rs"}"#,
            "Edit error: Old text not found",
        );
        assert_eq!(
            sig2.as_deref(),
            Some("edit:bar.rs:Edit error: Old text not found")
        );

        let ok = extract_tool_error_signature("read", r#"{"path":"foo.rs"}"#, "fn main() {}");
        assert!(ok.is_none());
    }
    #[test]
    fn prompt_forbids_execution_detection() {
        assert!(prompt_forbids_execution("calc.py içindeki average fonksiyonundaki +1 hatasını düzelt. Hiçbir komut ya da test çalıştırma, sadece dosyayı düzenle"));
        assert!(prompt_forbids_execution("don't run tests, edit only"));
        assert!(prompt_forbids_execution("do not run any commands"));
        assert!(prompt_forbids_execution("hicbir test kosma"));
        assert!(!prompt_forbids_execution("lütfen testleri çalıştır"));
        assert!(!prompt_forbids_execution("run cargo test and fix errors"));
    }
}
