use super::*;
use crate::ansi::GREEN;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MIN_STEPS: usize = 1;
pub const MAX_STEPS_HARD_CAP: usize = 80;
pub const DEFAULT_SCOUT_STEPS: usize = 24;
pub const DEFAULT_WORKER_STEPS: usize = 40;
pub const MAX_CONCURRENT_TASKS: usize = 8;
pub const DEFAULT_TIMEOUT_SECS: u64 = 15 * 60; // 15 minutes
pub const LOOP_GUARD_THRESHOLD: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Scout,
    Worker,
}

impl AgentKind {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "worker" | "work" | "code" | "write" | "edit" => AgentKind::Worker,
            _ => AgentKind::Scout,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Scout => "scout",
            AgentKind::Worker => "worker",
        }
    }

    pub fn default_steps(&self) -> usize {
        match self {
            AgentKind::Scout => DEFAULT_SCOUT_STEPS,
            AgentKind::Worker => DEFAULT_WORKER_STEPS,
        }
    }
}

pub fn clamp_steps(raw: Option<usize>, kind: AgentKind) -> usize {
    match raw {
        Some(steps) => steps.clamp(MIN_STEPS, MAX_STEPS_HARD_CAP),
        None => kind.default_steps(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentStatus {
    Completed,
    Failed(String),
    BudgetExhausted,
    Timeout,
}

impl SubagentStatus {
    pub fn failed(msg: impl Into<String>) -> Self {
        SubagentStatus::Failed(msg.into())
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, SubagentStatus::Failed(_))
    }

    pub fn as_str(&self) -> String {
        match self {
            SubagentStatus::Completed => "completed".to_string(),
            SubagentStatus::Failed(reason) => {
                let trimmed = reason.trim();
                if trimmed.is_empty() {
                    "failed".to_string()
                } else {
                    format!("failed: {}", trimmed)
                }
            }
            SubagentStatus::BudgetExhausted => "budget_exhausted".to_string(),
            SubagentStatus::Timeout => "failed (timeout)".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubagentTask {
    pub task: String,
    pub context: String,
    pub expected_output: String,
    pub kind: AgentKind,
    pub max_steps: usize,
    pub model: Option<String>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub struct SubagentResult {
    pub task_index: usize,
    pub task_label: String,
    pub kind: AgentKind,
    pub status: SubagentStatus,
    pub steps_used: usize,
    pub final_text: String,
    pub applied_files: Vec<String>,
    pub patch_conflict_path: Option<PathBuf>,
}

impl SubagentResult {
    pub fn format_report(&self) -> String {
        let mut header = format!(
            "### Sub-Agent #{} [{}] ({})\n- Status: {}\n- Steps used: {}",
            self.task_index + 1,
            self.kind.as_str(),
            self.task_label,
            self.status.as_str(),
            self.steps_used
        );
        if !self.applied_files.is_empty() {
            header.push_str(&format!(
                "\n- Applied files: {}",
                self.applied_files.join(", ")
            ));
        }
        if let Some(patch_path) = &self.patch_conflict_path {
            header.push_str(&format!(
                "\n- Patch conflict: saved to {} (not half-applied)",
                patch_path.display()
            ));
        }
        format!("{}\n\n{}", header, self.final_text)
    }
}

/// Collapses a sub-agent task into one short, single-line header label.
pub fn one_line_task(task: &str) -> String {
    let flat = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 72 {
        return flat;
    }
    let mut cut = flat.chars().take(72).collect::<String>();
    cut.push('…');
    cut
}

fn git_cmd() -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.fsmonitor=false"]);
    cmd
}

/// Checks whether `path` is inside an initialized git working tree.
pub fn is_git_repo(path: &Path) -> bool {
    let output = git_cmd()
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output();
    match output {
        Ok(out) => out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true",
        Err(_) => false,
    }
}

/// Extracts touched file paths from a `git diff` patch header.
pub fn extract_patched_files(diff_text: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            if let Some((a_file, _b_file)) = rest.split_once(" b/") {
                let trimmed = a_file.trim();
                if !trimmed.is_empty() && !files.iter().any(|f: &String| f == trimmed) {
                    files.push(trimmed.to_string());
                }
            }
        }
    }
    files
}

/// Temporary git worktree guard ensuring isolated changes and deterministic cleanup.
pub struct WorktreeGuard {
    pub worktree_path: PathBuf,
    pub main_repo_path: PathBuf,
    pub base_commit: String,
    pub id: String,
    pub active: bool,
}

impl WorktreeGuard {
    pub fn setup(main_repo: &Path, id: &str) -> Result<Self, String> {
        let wt_path = std::env::temp_dir().join(format!("spavn-wt-{}-{}", id, std::process::id()));
        if wt_path.exists() {
            let _ = std::fs::remove_dir_all(&wt_path);
        }

        // 1. git worktree add --detach <wt_path> HEAD
        let out = git_cmd()
            .args(["worktree", "add", "--detach"])
            .arg(&wt_path)
            .arg("HEAD")
            .current_dir(main_repo)
            .output()
            .map_err(|e| format!("Failed to execute git worktree add: {}", e))?;

        if !out.status.success() {
            return Err(format!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }

        // 2. Snapshot parent's current uncommitted tracked changes: git diff HEAD --binary
        let diff_out = git_cmd()
            .args(["diff", "HEAD", "--binary"])
            .current_dir(main_repo)
            .output()
            .map_err(|e| format!("Failed to get git diff HEAD: {}", e))?;

        if diff_out.status.success() && !diff_out.stdout.is_empty() {
            let dirty_patch = wt_path.join(".spavn-dirty.patch");
            if std::fs::write(&dirty_patch, &diff_out.stdout).is_ok() {
                let _ = git_cmd()
                    .args(["apply", "--whitespace=nowarn", ".spavn-dirty.patch"])
                    .current_dir(&wt_path)
                    .stdin(std::process::Stdio::null())
                    .output();
                let _ = std::fs::remove_file(&dirty_patch);
            }
        }

        // 3. Snapshot parent's untracked files
        let untracked_out = git_cmd()
            .args(["ls-files", "--others", "--exclude-standard", "-z"])
            .current_dir(main_repo)
            .output();

        if let Ok(untracked) = untracked_out {
            if untracked.status.success() {
                for item in untracked.stdout.split(|&b| b == 0) {
                    if item.is_empty() {
                        continue;
                    }
                    if let Ok(rel_str) = std::str::from_utf8(item) {
                        let src_file = main_repo.join(rel_str);
                        let dst_file = wt_path.join(rel_str);
                        if src_file.is_file() {
                            if let Some(parent) = dst_file.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            let _ = std::fs::copy(&src_file, &dst_file);
                        }
                    }
                }
            }
        }

        // 4. Commit baseline snapshot in worktree so diff against base isolates worker changes
        let _ = git_cmd()
            .args(["add", "-A"])
            .current_dir(&wt_path)
            .output();

        let _ = git_cmd()
            .args([
                "-c",
                "user.name=spavn",
                "-c",
                "user.email=spavn@local",
                "commit",
                "-m",
                "spavn-worker-base",
                "--allow-empty",
                "--no-gpg-sign",
            ])
            .current_dir(&wt_path)
            .output();

        // 5. Get base commit hash
        let rev_out = git_cmd()
            .args(["rev-parse", "HEAD"])
            .current_dir(&wt_path)
            .output()
            .map_err(|e| format!("Failed to get worktree HEAD: {}", e))?;

        let base_commit = String::from_utf8_lossy(&rev_out.stdout).trim().to_string();

        Ok(Self {
            worktree_path: wt_path,
            main_repo_path: main_repo.to_path_buf(),
            base_commit,
            id: id.to_string(),
            active: true,
        })
    }

    pub fn compute_patch(&self) -> Result<String, String> {
        if !self.active {
            return Ok(String::new());
        }
        // git -C wt add -A && git -C wt diff --cached --binary <base>
        let add_out = git_cmd()
            .args(["add", "-A"])
            .current_dir(&self.worktree_path)
            .output()
            .map_err(|e| format!("Failed to stage worktree changes: {}", e))?;
        if !add_out.status.success() {
            return Err(format!(
                "git add -A failed in worktree: {}",
                String::from_utf8_lossy(&add_out.stderr).trim()
            ));
        }

        let diff_out = git_cmd()
            .args(["diff", "--cached", "--binary", &self.base_commit])
            .current_dir(&self.worktree_path)
            .output()
            .map_err(|e| format!("Failed to diff against base commit: {}", e))?;

        if !diff_out.status.success() {
            return Err(format!(
                "git diff --cached in worktree failed: {}",
                String::from_utf8_lossy(&diff_out.stderr).trim()
            ));
        }

        Ok(String::from_utf8_lossy(&diff_out.stdout).to_string())
    }

    pub fn cleanup(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let _ = git_cmd()
            .args(["worktree", "remove", "--force"])
            .arg(&self.worktree_path)
            .current_dir(&self.main_repo_path)
            .output();

        if self.worktree_path.exists() {
            let _ = std::fs::remove_dir_all(&self.worktree_path);
        }

        let _ = git_cmd()
            .args(["worktree", "prune"])
            .current_dir(&self.main_repo_path)
            .output();
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[derive(Debug, Clone)]
pub struct PatchApplyOutcome {
    pub applied_files: Vec<String>,
    pub conflict_patch_file: Option<PathBuf>,
    pub error: Option<String>,
}

/// Applies a worker's patch to the main workspace, all or nothing.
///
/// `git apply --check` dry-runs first; only a clean patch is applied, and plain `git apply`
/// is itself atomic. A conflicting patch touches nothing: it is saved under
/// `~/.spavn/patches/<id>.patch` and reported. The workspace is never reset or cleaned —
/// it holds the user's uncommitted work and earlier workers' applied patches.
pub fn apply_patch_sequentially(
    workspace_root: &Path,
    patch_id: &str,
    patch_text: &str,
) -> PatchApplyOutcome {
    let patches_dir = home_dir().join(".spavn").join("patches");
    apply_patch_sequentially_with_patches_dir(workspace_root, patch_id, patch_text, &patches_dir)
}

pub fn apply_patch_sequentially_with_patches_dir(
    workspace_root: &Path,
    patch_id: &str,
    patch_text: &str,
    patches_dir: &Path,
) -> PatchApplyOutcome {
    if patch_text.trim().is_empty() {
        return PatchApplyOutcome {
            applied_files: Vec::new(),
            conflict_patch_file: None,
            error: None,
        };
    }

    let files = extract_patched_files(patch_text);

    let temp_patch_file = std::env::temp_dir().join(format!("spavn-apply-{}.patch", patch_id));
    if let Err(e) = std::fs::write(&temp_patch_file, patch_text) {
        return PatchApplyOutcome {
            applied_files: Vec::new(),
            conflict_patch_file: None,
            error: Some(format!("Failed to write temporary patch file: {}", e)),
        };
    }

    let git_apply = |extra: &[&str]| {
        git_cmd()
            .arg("apply")
            .args(extra)
            .arg(&temp_patch_file)
            .current_dir(workspace_root)
            .output()
    };
    let stderr_of = |out: &std::io::Result<std::process::Output>| match out {
        Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
        Err(e) => e.to_string(),
    };
    let check = git_apply(&["--check"]);
    let applied = match &check {
        Ok(o) if o.status.success() => git_apply(&[]),
        _ => check,
    };
    let _ = std::fs::remove_file(&temp_patch_file);

    if matches!(&applied, Ok(o) if o.status.success()) {
        return PatchApplyOutcome {
            applied_files: files,
            conflict_patch_file: None,
            error: None,
        };
    }
    let reason = stderr_of(&applied);
    let _ = std::fs::create_dir_all(patches_dir);
    let patch_path = patches_dir.join(format!("{}.patch", patch_id));
    let saved = std::fs::write(&patch_path, patch_text).is_ok();
    PatchApplyOutcome {
        applied_files: Vec::new(),
        conflict_patch_file: saved.then_some(patch_path),
        error: Some(format!(
            "patch does not apply cleanly; workspace left untouched: {}",
            reason
        )),
    }
}

/// Parses and validates subagent tasks from incoming tool JSON arguments.
pub fn parse_subagent_tasks(
    args: &Value,
    _default_model: &str,
) -> Result<Vec<SubagentTask>, String> {
    let shared_ctx = get_str_alias(args, &["context", "files", "background"]).trim();
    let shared_model = get_str_alias(args, &["model", "endpoint"]).trim();
    let shared_model_opt = if shared_model.is_empty() {
        None
    } else {
        Some(shared_model.to_string())
    };

    if let Some(tasks_arr) = args.get("tasks").and_then(|v| v.as_array()) {
        if tasks_arr.is_empty() {
            return Err("Subagent error: missing 'task' parameter in tasks array.".to_string());
        }
        if tasks_arr.len() > MAX_CONCURRENT_TASKS {
            return Err(format!(
                "Subagent error: maximum {} tasks allowed per batch (received {}).",
                MAX_CONCURRENT_TASKS,
                tasks_arr.len()
            ));
        }

        let mut tasks = Vec::with_capacity(tasks_arr.len());
        for (idx, item) in tasks_arr.iter().enumerate() {
            let t = get_str_alias(item, &["task", "prompt", "query", "objective"]).trim();
            if t.is_empty() {
                return Err(format!(
                    "Subagent error: missing 'task' parameter in task #{}.",
                    idx + 1
                ));
            }
            let item_ctx = get_str_alias(item, &["context", "files", "background"]).trim();
            let combined_ctx = match (shared_ctx.is_empty(), item_ctx.is_empty()) {
                (false, false) => format!("{}\n\n{}", shared_ctx, item_ctx),
                (false, true) => shared_ctx.to_string(),
                (true, false) => item_ctx.to_string(),
                (true, true) => String::new(),
            };

            let expected = get_str_alias(item, &["expected_output", "format", "output"])
                .trim()
                .to_string();

            let kind_str = get_str_alias(item, &["kind", "agent", "type"]);
            let kind = if kind_str.is_empty() {
                let top_kind_str = get_str_alias(args, &["kind", "agent", "type"]);
                AgentKind::parse(top_kind_str)
            } else {
                AgentKind::parse(kind_str)
            };

            let raw_steps = item
                .get("max_steps")
                .and_then(|v| v.as_u64())
                .or_else(|| args.get("max_steps").and_then(|v| v.as_u64()))
                .map(|v| v as usize);
            let max_steps = clamp_steps(raw_steps, kind);

            let item_model = get_str_alias(item, &["model", "endpoint"]).trim();
            let model = if !item_model.is_empty() {
                Some(item_model.to_string())
            } else {
                shared_model_opt.clone()
            };

            let timeout_secs = item
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .or_else(|| args.get("timeout_secs").and_then(|v| v.as_u64()))
                .unwrap_or(DEFAULT_TIMEOUT_SECS);

            tasks.push(SubagentTask {
                task: t.to_string(),
                context: combined_ctx,
                expected_output: expected,
                kind,
                max_steps,
                model,
                timeout_secs,
            });
        }
        return Ok(tasks);
    }

    let task = get_str_alias(args, &["task", "prompt", "query", "objective"]).trim();
    if task.is_empty() {
        return Err("Subagent error: missing 'task' parameter.".to_string());
    }

    let expected = get_str_alias(args, &["expected_output", "format", "output"])
        .trim()
        .to_string();
    let kind_str = get_str_alias(args, &["kind", "agent", "type"]);
    let kind = AgentKind::parse(kind_str);

    let raw_steps = args
        .get("max_steps")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let max_steps = clamp_steps(raw_steps, kind);

    let timeout_secs = args
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    Ok(vec![SubagentTask {
        task: task.to_string(),
        context: shared_ctx.to_string(),
        expected_output: expected,
        kind,
        max_steps,
        model: shared_model_opt,
        timeout_secs,
    }])
}

/// Builds a compressed transcript summary of tool operations and observations.
pub fn summarize_transcript(msgs: &[crate::router::ChatMessage]) -> String {
    let mut lines = Vec::new();
    for m in msgs {
        if m.role == "tool" {
            let tname = m.name.as_deref().unwrap_or("tool");
            let content = m.content.as_deref().unwrap_or("");
            let preview = content
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>();
            lines.push(format!("- Tool `{}`: {}", tname, preview));
        } else if m.role == "assistant" {
            if let Some(c) = &m.content {
                let first_line = c
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .collect::<String>();
                if !first_line.is_empty() {
                    lines.push(format!("- Note: {}", first_line));
                }
            }
        }
    }
    if lines.is_empty() {
        "(no tool activity recorded)".to_string()
    } else {
        lines.join("\n")
    }
}

/// Formats an array of SubagentResult in original input order into a markdown report.
pub fn format_results(results: &[SubagentResult]) -> String {
    results
        .iter()
        .map(|r| r.format_report())
        .collect::<Vec<_>>()
        .join("\n\n")
}
/// Resolves the actual model key to use for a subagent task.
///
/// Priority:
/// 1. Explicit task/batch override `model_override` (if non-empty and != "default").
/// 2. Parent context `current_model` (if non-empty and != "default").
/// 3. Configured `default_model` from user configuration (e.g. "flash-medium").
///
/// Also handles "local" keyword by resolving through local discovery.
pub fn resolve_subagent_model(model_override: Option<&str>, current_model: &str) -> String {
    let cfg = crate::config::load_user_config();
    let raw = match model_override {
        Some(m) if !m.trim().is_empty() && !m.trim().eq_ignore_ascii_case("default") => m.trim(),
        _ if !current_model.trim().is_empty()
            && !current_model.trim().eq_ignore_ascii_case("default") =>
        {
            current_model.trim()
        }
        _ => cfg.default_model.trim(),
    };

    if raw.is_empty() || raw.eq_ignore_ascii_case("default") {
        cfg.default_model
    } else if raw.eq_ignore_ascii_case("local") {
        cfg.local_discovery_url
            .as_deref()
            .and_then(crate::router::detect_active_local_model)
            .unwrap_or(cfg.default_model)
    } else {
        raw.to_string()
    }
}

/// Executes a single subagent loop (scout or worker).
pub fn run_single_subagent(
    task_spec: &SubagentTask,
    task_idx: usize,
    target_model: &str,
    default_host: &str,
    parent_ctx: &ToolContext,
    workspace_path: &Path,
) -> SubagentResult {
    let task_label = one_line_task(&task_spec.task);
    let resolved_model = resolve_subagent_model(task_spec.model.as_deref(), target_model);
    let ep = crate::router::resolve_endpoint(&resolved_model);

    // Create subagent tool context. Never more permissive than the parent: `exec_subagent`
    // refuses workers under SAFE, since a background thread cannot ask for approval.
    let access = parent_ctx.access;
    let mut sub_ctx = ToolContext::new(
        access,
        default_host.to_string(),
        workspace_path.to_path_buf(),
        if task_spec.kind == AgentKind::Worker {
            crate::spavn_engine::AgentPhase::Code
        } else {
            crate::spavn_engine::AgentPhase::Plan
        },
    );
    sub_ctx.policy.protected = parent_ctx.policy.protected.clone();
    sub_ctx.policy.home = parent_ctx.policy.home.clone();
    sub_ctx.can_prompt = false;
    sub_ctx.current_model = resolved_model.clone();
    // Filter tools for this agent kind
    let sub_tools = match task_spec.kind {
        AgentKind::Scout => {
            let mut tools = tool_definitions_for_phase(crate::spavn_engine::AgentPhase::Plan);
            if let Value::Array(items) = &mut tools {
                items.retain(|it| {
                    let name = it
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    matches!(
                        name,
                        "read" | "search" | "glob" | "lsp" | "memory" | "web" | "skill"
                    )
                });
            }
            tools
        }
        AgentKind::Worker => {
            let mut tools = tool_definitions_for_phase(crate::spavn_engine::AgentPhase::Code);
            if let Value::Array(items) = &mut tools {
                items.retain(|it| {
                    let name = it
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    !matches!(name, "subagent" | "spawn_submodels" | "task" | "delegate")
                });
            }
            tools
        }
    };

    let system_prompt = match task_spec.kind {
        AgentKind::Scout => {
            "You are a focused, read-only SPAVN Sub-Agent. Use `read`, `search`, `glob`, `lsp`, \
             `memory`, `web`, and `skill` to investigate the codebase and return a concise, \
             high-signal summary with exact file:line references. Do not speculate."
        }
        AgentKind::Worker => {
            "You are a SPAVN Sub-Agent Worker responsible for implementing code changes.\n\
             - Own the change end-to-end.\n\
             - No stubs, placeholders, or partial implementations; finish all reachable work.\n\
             - Run a verification check (build, test, lint, or syntax check via `bash` or inspection) before finishing.\n\
             - In your final response, explicitly report:\n  \
               1. Changed files (paths).\n  \
               2. Verification exercised (exact command run and output status).\n  \
               3. Concise summary of the solution."
        }
    };

    let mut user_prompt = format!("## Sub-Agent Task\n{}", task_spec.task.trim());
    if !task_spec.context.trim().is_empty() {
        user_prompt.push_str(&format!("\n\n## Context\n{}", task_spec.context.trim()));
    }
    if !task_spec.expected_output.trim().is_empty() {
        user_prompt.push_str(&format!(
            "\n\n## Expected Output\n{}",
            task_spec.expected_output.trim()
        ));
    }

    let mut msgs = vec![
        crate::router::ChatMessage::system(system_prompt),
        crate::router::ChatMessage::user(user_prompt),
    ];

    let start_time = Instant::now();
    let timeout = Duration::from_secs(task_spec.timeout_secs);
    let mut steps_used = 0usize;
    let mut call_counts: HashMap<String, usize> = HashMap::new();
    let mut loop_halted: Option<String> = None;
    let mut last_error: Option<String> = None;
    let mut status = SubagentStatus::Completed;
    // Parent-visible progress: agent start line (non-quiet mode)
    if crate::hud::stdin_is_tty() {
        crate::hud::spinner_print(&format!(
            "\n{}🐝 Sub-agent #{} [{}] started: {}{}\n",
            CYAN,
            task_idx + 1,
            task_spec.kind.as_str(),
            task_label,
            RESET
        ));
    }

    for step in 0..task_spec.max_steps {
        steps_used = step + 1;

        if crate::is_interrupted() {
            let res = SubagentResult {
                task_index: task_idx,
                task_label: task_label.clone(),
                kind: task_spec.kind,
                status: SubagentStatus::failed("cancelled via Esc/Ctrl+C"),
                steps_used,
                final_text: "[Subagent cancelled via Esc/Ctrl+C]".to_string(),
                applied_files: Vec::new(),
                patch_conflict_path: None,
            };
            if crate::hud::stdin_is_tty() {
                crate::hud::spinner_print(&format!(
                    "{}✗ Sub-agent #{} [{}] cancelled ({} steps){}\n",
                    YELLOW,
                    task_idx + 1,
                    task_spec.kind.as_str(),
                    steps_used,
                    RESET
                ));
            }
            return res;
        }

        if start_time.elapsed() >= timeout {
            let partial = summarize_transcript(&msgs);
            let res = SubagentResult {
                task_index: task_idx,
                task_label: task_label.clone(),
                kind: task_spec.kind,
                status: SubagentStatus::Timeout,
                steps_used,
                final_text: format!(
                    "[Subagent timed out after {:?}. Partial transcript summary:\n{}]",
                    start_time.elapsed(),
                    partial
                ),
                applied_files: Vec::new(),
                patch_conflict_path: None,
            };
            if crate::hud::stdin_is_tty() {
                crate::hud::spinner_print(&format!(
                    "{}✗ Sub-agent #{} [{}] timed out ({} steps){}\n",
                    YELLOW,
                    task_idx + 1,
                    task_spec.kind.as_str(),
                    steps_used,
                    RESET
                ));
            }
            return res;
        }

        crate::hud::set_spinner_label(&format!(
            "🐝 Sub-agent #{} [{}] working (step {}/{})",
            task_idx + 1,
            task_spec.kind.as_str(),
            step + 1,
            task_spec.max_steps
        ));

        let completion_res =
            crate::router::stream_chat_completion(&ep, &msgs, &sub_tools, |_| {}, |_| {});
        let res = match completion_res {
            Ok(r) => r,
            Err(e) => {
                let err_msg = format!("model call failed (endpoint: {}): {}", ep.key, e);
                status = SubagentStatus::failed(err_msg.clone());
                last_error = Some(err_msg);
                break;
            }
        };
        let msg = res.message;
        msgs.push(msg.clone());

        if let Some(tcs) = msg.tool_calls {
            if !tcs.is_empty() {
                for tc in tcs.into_iter().take(8) {
                    let tname = tc.function.name.trim().to_string();

                    // Loop guard check: identical call 4x -> stop agent with a note
                    let sig = format!("{}:{}", tname, tc.function.arguments.trim());
                    let count = call_counts.entry(sig).or_insert(0);
                    *count += 1;
                    if *count >= LOOP_GUARD_THRESHOLD {
                        let note = format!(
                            "[Subagent halted: loop detected. Tool '{}' called with identical arguments {} times.]",
                            tname, LOOP_GUARD_THRESHOLD
                        );
                        msgs.push(crate::router::ChatMessage::tool_result(
                            tc.id,
                            tname.clone(),
                            note.clone(),
                        ));
                        loop_halted = Some(note);
                        break;
                    }

                    let out = match task_spec.kind {
                        AgentKind::Scout => {
                            if matches!(
                                tname.as_str(),
                                "read" | "search" | "glob" | "memory" | "web" | "lsp" | "skill"
                            ) {
                                execute_tool(&mut sub_ctx, &tname, &tc.function.arguments)
                            } else {
                                format!(
                                    "Subagent error: only read-only tools (read, search, glob, memory, web, lsp, skill) are allowed for scout agents; cannot run '{}'.",
                                    tname
                                )
                            }
                        }
                        AgentKind::Worker => {
                            if matches!(
                                tname.as_str(),
                                "subagent" | "spawn_submodels" | "task" | "delegate"
                            ) {
                                "Subagent error: subagents cannot recursively spawn subagents."
                                    .to_string()
                            } else {
                                execute_tool(&mut sub_ctx, &tname, &tc.function.arguments)
                            }
                        }
                    };
                    msgs.push(crate::router::ChatMessage::tool_result(tc.id, tname, out));
                }

                if let Some(note) = loop_halted {
                    let fail_reason = format!("loop detected: {}", note);
                    status = SubagentStatus::failed(fail_reason);
                    let partial = summarize_transcript(&msgs);
                    let res = SubagentResult {
                        task_index: task_idx,
                        task_label: task_label.clone(),
                        kind: task_spec.kind,
                        status,
                        steps_used,
                        final_text: format!("{}\n\nPartial transcript:\n{}", note, partial),
                        applied_files: Vec::new(),
                        patch_conflict_path: None,
                    };
                    if crate::hud::stdin_is_tty() {
                        crate::hud::spinner_print(&format!(
                            "{}✗ Sub-agent #{} [{}] loop detected ({} steps){}\n",
                            YELLOW,
                            task_idx + 1,
                            task_spec.kind.as_str(),
                            steps_used,
                            RESET
                        ));
                    }
                    return res;
                }
                continue;
            }
        }

        let ans = msg
            .content
            .or(msg.reasoning_content)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !ans.is_empty() {
            let res = SubagentResult {
                task_index: task_idx,
                task_label: task_label.clone(),
                kind: task_spec.kind,
                status: SubagentStatus::Completed,
                steps_used,
                final_text: ans,
                applied_files: Vec::new(),
                patch_conflict_path: None,
            };
            if crate::hud::stdin_is_tty() {
                crate::hud::spinner_print(&format!(
                    "{}✓ Sub-agent #{} [{}] finished: completed ({} steps){}\n",
                    GREEN,
                    task_idx + 1,
                    task_spec.kind.as_str(),
                    steps_used,
                    RESET
                ));
            }
            return res;
        }
        break;
    }

    if steps_used >= task_spec.max_steps && !status.is_failed() {
        status = SubagentStatus::BudgetExhausted;
    }

    let final_ans = if let Some(ref err) = last_error {
        let partial = summarize_transcript(&msgs);
        if partial == "(no tool activity recorded)" {
            format!("[Subagent failed: {}]", err)
        } else {
            format!(
                "[Subagent failed: {}]\n\nActivity before failure:\n{}",
                err, partial
            )
        }
    } else {
        msgs.iter()
            .rev()
            .find(|m| m.role == "assistant")
            .and_then(|m| m.content.clone().or_else(|| m.reasoning_content.clone()))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "(Sub-agent finished [{}] after {} steps without explicit final summary. Activity:\n{})",
                    status.as_str(),
                    steps_used,
                    summarize_transcript(&msgs)
                )
            })
    };

    let res = SubagentResult {
        task_index: task_idx,
        task_label: task_label.clone(),
        kind: task_spec.kind,
        status: status.clone(),
        steps_used,
        final_text: final_ans,
        applied_files: Vec::new(),
        patch_conflict_path: None,
    };

    if crate::hud::stdin_is_tty() {
        let (mark, color) = if status == SubagentStatus::Completed {
            ("✓", GREEN)
        } else {
            ("✗", YELLOW)
        };
        crate::hud::spinner_print(&format!(
            "{}{} Sub-agent #{} [{}] finished: {} ({} steps){}\n",
            color,
            mark,
            task_idx + 1,
            task_spec.kind.as_str(),
            status.as_str(),
            steps_used,
            RESET
        ));
    }

    res
}

/// Main entry point for the `subagent` tool.
pub fn exec_subagent(ctx: &mut ToolContext, args: &Value) -> String {
    let default_model_key = &ctx.current_model;
    let tasks = match parse_subagent_tasks(args, default_model_key) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if ctx.access == AccessMode::Safe && tasks.iter().any(|t| t.kind == AgentKind::Worker) {
        return crate::config::tr_en(
            "subagent: worker ajanları SAFE modda çalışamaz (arka plandaki ajan onay isteyemez). Erişimi AUTO/SOVEREIGN yapın veya kind=scout kullanın.",
            "subagent: worker agents cannot run in SAFE mode (a background agent cannot ask for approval). Switch access to AUTO/SOVEREIGN or use kind=scout.",
        )
        .to_string();
    }

    let workspace_is_git = is_git_repo(&ctx.policy.cwd);
    let worker_count = tasks.iter().filter(|t| t.kind == AgentKind::Worker).count();

    // Non-git workspace: workers run in place only when tasks <= 1; refuse if >1 worker tasks
    if !workspace_is_git && worker_count > 1 {
        return "Subagent error: multiple worker tasks in a non-git workspace are not supported because concurrent file modifications cannot be isolated. Run workers sequentially or initialize a git repository.".to_string();
    }

    let batch_id = format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        std::process::id()
    );

    // Set up worktrees for workers if git repo
    let mut worktrees: HashMap<usize, WorktreeGuard> = HashMap::new();
    if workspace_is_git {
        for (idx, task_spec) in tasks.iter().enumerate() {
            if task_spec.kind == AgentKind::Worker {
                let wt_id = format!("{}-t{}", batch_id, idx + 1);
                match WorktreeGuard::setup(&ctx.policy.cwd, &wt_id) {
                    Ok(guard) => {
                        worktrees.insert(idx, guard);
                    }
                    Err(e) => {
                        return format!(
                            "Subagent error: failed to initialize isolated worktree for worker #{}: {}",
                            idx + 1,
                            e
                        );
                    }
                }
            }
        }
    }

    // Run tasks concurrently up to 8 threads
    let default_host = ctx.default_host.clone();
    let current_model = ctx.current_model.clone();
    let parent_ctx = ctx.clone();
    let main_cwd = ctx.policy.cwd.clone();

    // Map worktree paths so they can be referenced inside the thread scope
    let wt_paths: HashMap<usize, PathBuf> = worktrees
        .iter()
        .map(|(idx, wt)| (*idx, wt.worktree_path.clone()))
        .collect();

    let mut results: Vec<SubagentResult> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(tasks.len());
        for (idx, task_spec) in tasks.iter().enumerate() {
            let task_spec = task_spec.clone();
            let parent_ctx = parent_ctx.clone();
            let default_host = default_host.clone();
            let main_cwd = main_cwd.clone();
            let wt_path = wt_paths.get(&idx).cloned();

            let target_model = resolve_subagent_model(task_spec.model.as_deref(), &current_model);

            let handle = s.spawn(move || {
                let workspace = match &wt_path {
                    Some(p) => p.as_path(),
                    None => main_cwd.as_path(),
                };
                run_single_subagent(
                    &task_spec,
                    idx,
                    &target_model,
                    &default_host,
                    &parent_ctx,
                    workspace,
                )
            });
            handles.push(handle);
        }

        handles
            .into_iter()
            .enumerate()
            .map(|(idx, h)| match h.join() {
                Ok(res) => res,
                Err(_) => SubagentResult {
                    task_index: idx,
                    task_label: one_line_task(&tasks[idx].task),
                    kind: tasks[idx].kind,
                    status: SubagentStatus::failed("subagent thread panicked"),
                    steps_used: 0,
                    final_text: "[Subagent panicked; its result is missing. Re-run it alone with `subagent`.]".to_string(),
                    applied_files: Vec::new(),
                    patch_conflict_path: None,
                },
            })
            .collect()
    });

    // Sequentially apply patches for workers in input order
    for (idx, task_spec) in tasks.iter().enumerate() {
        if task_spec.kind == AgentKind::Worker {
            if let Some(mut wt) = worktrees.remove(&idx) {
                let patch = match wt.compute_patch() {
                    Ok(p) => p,
                    Err(e) => {
                        let err_msg = format!("failed to compute worker patch: {}", e);
                        results[idx].status = SubagentStatus::failed(err_msg.clone());
                        results[idx]
                            .final_text
                            .push_str(&format!("\n\n[{}]", err_msg));
                        wt.cleanup();
                        continue;
                    }
                };

                wt.cleanup();

                if !patch.trim().is_empty() {
                    let outcome = apply_patch_sequentially(&ctx.policy.cwd, &wt.id, &patch);
                    results[idx].applied_files = outcome.applied_files;
                    if let Some(conflict_path) = outcome.conflict_patch_file {
                        results[idx].patch_conflict_path = Some(conflict_path);
                    }
                    if let Some(err) = outcome.error {
                        results[idx].status = SubagentStatus::failed(err.clone());
                        results[idx].final_text.push_str(&format!("\n\n[{}]", err));
                    }
                }
            }
        }
    }

    // Clean up any remaining worktrees
    for (_, mut wt) in worktrees {
        wt.cleanup();
    }

    format_results(&results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_parsing_defaults() {
        let args = json!({
            "task": "Investigate memory usage in router"
        });
        let tasks = parse_subagent_tasks(&args, "default-model").expect("parsing should succeed");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task, "Investigate memory usage in router");
        assert_eq!(tasks[0].kind, AgentKind::Scout);
        assert_eq!(tasks[0].max_steps, DEFAULT_SCOUT_STEPS);
        assert_eq!(tasks[0].timeout_secs, DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn test_task_parsing_worker_and_step_caps() {
        let args = json!({
            "task": "Refactor router endpoints",
            "kind": "worker",
            "max_steps": 200
        });
        let tasks = parse_subagent_tasks(&args, "default-model").expect("parsing should succeed");
        assert_eq!(tasks[0].kind, AgentKind::Worker);
        assert_eq!(tasks[0].max_steps, MAX_STEPS_HARD_CAP); // clamped to 80

        let args_zero = json!({
            "task": "Refactor router endpoints",
            "kind": "worker",
            "max_steps": 0
        });
        let tasks_zero = parse_subagent_tasks(&args_zero, "default-model").unwrap();
        assert_eq!(tasks_zero[0].max_steps, MIN_STEPS); // clamped to 1
    }

    #[test]
    fn test_task_parsing_max_concurrency_rejection() {
        let task_items: Vec<Value> = (0..9)
            .map(|i| {
                json!({
                    "task": format!("Sub-task {}", i + 1)
                })
            })
            .collect();
        let args = json!({
            "tasks": task_items
        });
        let err = parse_subagent_tasks(&args, "default-model").unwrap_err();
        assert!(
            err.contains("maximum 8 tasks allowed"),
            "Expected max 8 tasks error, got: {}",
            err
        );
    }

    #[test]
    fn test_ordered_result_assembly() {
        let r1 = SubagentResult {
            task_index: 0,
            task_label: "Inspect schema".to_string(),
            kind: AgentKind::Scout,
            status: SubagentStatus::Completed,
            steps_used: 5,
            final_text: "Schema verified.".to_string(),
            applied_files: Vec::new(),
            patch_conflict_path: None,
        };
        let r2 = SubagentResult {
            task_index: 1,
            task_label: "Exhaust budget".to_string(),
            kind: AgentKind::Worker,
            status: SubagentStatus::BudgetExhausted,
            steps_used: 40,
            final_text: "Ran out of steps.".to_string(),
            applied_files: vec!["src/foo.rs".to_string()],
            patch_conflict_path: None,
        };
        let r3 = SubagentResult {
            task_index: 2,
            task_label: "Broken task".to_string(),
            kind: AgentKind::Scout,
            status: SubagentStatus::failed("Encountered fatal error."),
            steps_used: 2,
            final_text: "Encountered fatal error.".to_string(),
            applied_files: Vec::new(),
            patch_conflict_path: None,
        };

        let formatted = format_results(&[r1, r2, r3]);
        assert!(formatted.contains("### Sub-Agent #1 [scout] (Inspect schema)"));
        assert!(formatted.contains("- Status: completed"));
        assert!(formatted.contains("### Sub-Agent #2 [worker] (Exhaust budget)"));
        assert!(formatted.contains("- Status: budget_exhausted"));
        assert!(formatted.contains("- Applied files: src/foo.rs"));
        assert!(formatted.contains("### Sub-Agent #3 [scout] (Broken task)"));
        assert!(formatted.contains("- Status: failed: Encountered fatal error."));

        let idx1 = formatted.find("#1").unwrap();
        let idx2 = formatted.find("#2").unwrap();
        let idx3 = formatted.find("#3").unwrap();
        assert!(idx1 < idx2 && idx2 < idx3, "Results must stay in order");
    }

    #[test]
    fn test_resolve_subagent_model_fallback_when_default_or_empty() {
        let default_cfg = crate::config::load_user_config().default_model;

        // None + "default" -> cfg.default_model
        let m1 = resolve_subagent_model(None, "default");
        assert_eq!(m1, default_cfg);

        // None + "" -> cfg.default_model
        let m2 = resolve_subagent_model(None, "");
        assert_eq!(m2, default_cfg);

        // Some("default") + "default" -> cfg.default_model
        let m3 = resolve_subagent_model(Some("default"), "default");
        assert_eq!(m3, default_cfg);

        // Some("custom") + "default" -> "custom"
        let m4 = resolve_subagent_model(Some("custom-model"), "default");
        assert_eq!(m4, "custom-model");

        // None + "parent-model" -> "parent-model"
        let m5 = resolve_subagent_model(None, "parent-model");
        assert_eq!(m5, "parent-model");

        // Endpoint resolved from default_model must not have key or model named "default"
        let ep = crate::router::resolve_endpoint(&m1);
        assert_ne!(ep.key.to_lowercase(), "default");
        assert_ne!(ep.model.to_lowercase(), "default");
    }

    #[test]
    fn test_subagent_status_format_preserves_error_details() {
        let res = SubagentResult {
            task_index: 0,
            task_label: "Failing query".to_string(),
            kind: AgentKind::Scout,
            status: SubagentStatus::failed(
                "model call failed (endpoint: flash-medium): 404 Not Found",
            ),
            steps_used: 1,
            final_text:
                "[Subagent failed: model call failed (endpoint: flash-medium): 404 Not Found]"
                    .to_string(),
            applied_files: Vec::new(),
            patch_conflict_path: None,
        };
        let report = res.format_report();
        assert!(report.contains(
            "- Status: failed: model call failed (endpoint: flash-medium): 404 Not Found"
        ));
        assert!(report.contains(
            "[Subagent failed: model call failed (endpoint: flash-medium): 404 Not Found]"
        ));
    }
    #[test]
    fn test_non_git_multi_worker_refusal() {
        let temp_dir = std::env::temp_dir().join(format!(
            "spavn-test-nongit-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&temp_dir);

        let mut c = ToolContext::new(
            AccessMode::Auto,
            "local".to_string(),
            temp_dir.clone(),
            crate::spavn_engine::AgentPhase::Plan,
        );

        let args = json!({
            "tasks": [
                { "task": "worker 1", "kind": "worker" },
                { "task": "worker 2", "kind": "worker" }
            ]
        });

        let out = exec_subagent(&mut c, &args);
        assert!(
            out.contains("multiple worker tasks in a non-git workspace are not supported"),
            "Expected refusal for multi-worker in non-git workspace, got: {}",
            out
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn workers_are_refused_under_safe_access() {
        // A background worker cannot prompt for approval; running it with AUTO powers
        // would silently exceed the parent's SAFE mode.
        let mut c = ToolContext::new(
            AccessMode::Safe,
            "local".to_string(),
            std::env::temp_dir(),
            crate::spavn_engine::AgentPhase::Code,
        );
        let out = exec_subagent(&mut c, &json!({ "task": "edit x", "kind": "worker" }));
        assert!(out.contains("SAFE"), "expected SAFE refusal, got: {}", out);
    }

    #[test]
    fn test_patch_apply_different_files_and_conflict_handling() {
        let test_root = std::env::temp_dir().join(format!(
            "spavn-git-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let repo_dir = test_root.join("repo");
        let patches_dir = test_root.join("patches");
        let _ = std::fs::create_dir_all(&repo_dir);
        let _ = std::fs::create_dir_all(&patches_dir);

        // Initialize git repo
        let _ = git_cmd()
            .args(["init", "-b", "main"])
            .current_dir(&repo_dir)
            .output();
        let _ = git_cmd()
            .args(["config", "core.fsmonitor", "false"])
            .current_dir(&repo_dir)
            .output();
        let _ = git_cmd()
            .args(["config", "user.name", "spavn-test"])
            .current_dir(&repo_dir)
            .output();
        let _ = git_cmd()
            .args(["config", "user.email", "test@spavn.local"])
            .current_dir(&repo_dir)
            .output();

        let f1 = repo_dir.join("file1.txt");
        let f2 = repo_dir.join("file2.txt");
        let _ = std::fs::write(&f1, "initial file1\n");
        let _ = std::fs::write(&f2, "initial file2\n");

        let _ = git_cmd()
            .args(["add", "."])
            .current_dir(&repo_dir)
            .output();
        let _ = git_cmd()
            .args(["commit", "-m", "init", "--no-gpg-sign"])
            .current_dir(&repo_dir)
            .output();

        // 1. Two worktrees editing different files -> both apply cleanly
        let mut wt1 = WorktreeGuard::setup(&repo_dir, "t1").expect("wt1 setup failed");
        let mut wt2 = WorktreeGuard::setup(&repo_dir, "t2").expect("wt2 setup failed");

        let _ = std::fs::write(
            wt1.worktree_path.join("file1.txt"),
            "initial file1\nworker1 add\n",
        );
        let _ = std::fs::write(
            wt2.worktree_path.join("file2.txt"),
            "initial file2\nworker2 add\n",
        );

        let p1 = wt1.compute_patch().expect("compute patch 1");
        let p2 = wt2.compute_patch().expect("compute patch 2");

        let outcome1 =
            apply_patch_sequentially_with_patches_dir(&repo_dir, "w1", &p1, &patches_dir);
        assert!(outcome1.conflict_patch_file.is_none());
        assert_eq!(outcome1.applied_files, vec!["file1.txt"]);

        let outcome2 =
            apply_patch_sequentially_with_patches_dir(&repo_dir, "w2", &p2, &patches_dir);
        assert!(outcome2.conflict_patch_file.is_none());
        assert_eq!(outcome2.applied_files, vec!["file2.txt"]);

        wt1.cleanup();
        wt2.cleanup();

        // Commit those changes in main
        let _ = git_cmd()
            .args(["commit", "-am", "applied wt1 and wt2", "--no-gpg-sign"])
            .current_dir(&repo_dir)
            .output();

        // 2. Same lines -> second saved as patch and reported
        let mut wt3 = WorktreeGuard::setup(&repo_dir, "t3").expect("wt3 setup failed");
        let mut wt4 = WorktreeGuard::setup(&repo_dir, "t4").expect("wt4 setup failed");

        let _ = std::fs::write(wt3.worktree_path.join("file1.txt"), "worker3 overwrite\n");
        let _ = std::fs::write(wt4.worktree_path.join("file1.txt"), "worker4 conflict\n");

        let p3 = wt3.compute_patch().expect("compute patch 3");
        let p4 = wt4.compute_patch().expect("compute patch 4");

        let outcome3 =
            apply_patch_sequentially_with_patches_dir(&repo_dir, "w3", &p3, &patches_dir);
        assert!(outcome3.conflict_patch_file.is_none());

        let outcome4 =
            apply_patch_sequentially_with_patches_dir(&repo_dir, "w4", &p4, &patches_dir);
        assert!(
            outcome4.conflict_patch_file.is_some(),
            "Expected conflict for worker 4"
        );
        let saved_patch = outcome4.conflict_patch_file.unwrap();
        assert!(saved_patch.exists(), "Conflict patch must exist on disk");

        // Workspace must NOT be left half-applied with conflict markers
        let content1 = std::fs::read_to_string(&f1).unwrap();
        assert!(
            !content1.contains("<<<<<<<"),
            "Workspace must not have conflict markers; got: {}",
            content1
        );

        wt3.cleanup();
        wt4.cleanup();

        let _ = std::fs::remove_dir_all(&test_root);
    }
}
