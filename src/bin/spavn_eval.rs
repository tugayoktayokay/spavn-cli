//! `spavn-eval` — runs the agent against a directory of small tasks and reports a pass rate.
//!
//! Each task lives in `evals/tasks/<name>/` as a `task.json` plus a `repo/` fixture:
//!
//! ```json
//! { "prompt": "Fix the failing test.", "verify": "python3 -m unittest -q", "tags": ["bugfix"] }
//! ```
//!
//! The fixture is copied to a scratch directory, `spavn -j` runs there as a separate process
//! (the agent keeps process-wide state, so one process per task is the only safe isolation),
//! and then `verify` decides pass or fail. A task that already passes before the agent runs is
//! reported as `broken-fixture` and does not count, so a bad task cannot inflate the score.
//! Results go to stdout as a table and, with `--out`, to a JSONL file for comparing runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const USAGE: &str = "spavn-eval — run the agent against evals/tasks and report a pass rate\n\n\
Usage: spavn-eval [options]\n\n\
Options:\n\
\x20 -m, --model <key>      Model passed to spavn (default: spavn's own default)\n\
\x20 -t, --tasks <dir>      Tasks directory (default: evals/tasks)\n\
\x20 -f, --filter <text>    Only tasks whose name or tags contain <text>\n\
\x20 -T, --timeout <secs>   Per-task agent timeout (default: 600)\n\
\x20 -n, --repeat <count>   Run every task this many times (default: 1)\n\
\x20 -o, --out <file>       Append one JSON line per task to <file>\n\
\x20     --spavn <path>     spavn binary (default: next to this binary, else PATH)\n\
\x20     --list             List the selected tasks and exit\n\
\x20 -h, --help             Print this help\n";

struct Options {
    model: Option<String>,
    tasks_dir: PathBuf,
    filter: Option<String>,
    timeout: Duration,
    repeat: usize,
    out: Option<PathBuf>,
    spavn: PathBuf,
    list_only: bool,
}

struct Task {
    name: String,
    dir: PathBuf,
    prompt: String,
    verify: String,
    tags: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Pass,
    Fail,
    Timeout,
    AgentError,
    BrokenFixture,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Fail => "fail",
            Outcome::Timeout => "timeout",
            Outcome::AgentError => "agent-error",
            Outcome::BrokenFixture => "broken-fixture",
        }
    }
}

fn default_spavn() -> PathBuf {
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let sibling = dir.join("spavn");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("spavn")
}

fn parse_args() -> Result<Options, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = Options {
        model: None,
        tasks_dir: PathBuf::from("evals/tasks"),
        filter: None,
        timeout: Duration::from_secs(600),
        repeat: 1,
        out: None,
        spavn: default_spavn(),
        list_only: false,
    };
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "-h" || flag == "--help" {
            print!("{USAGE}");
            std::process::exit(0);
        }
        if flag == "--list" {
            opts.list_only = true;
            i += 1;
            continue;
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{flag} needs a value"))?
            .clone();
        match flag {
            "-m" | "--model" => opts.model = Some(value),
            "-t" | "--tasks" => opts.tasks_dir = PathBuf::from(value),
            "-f" | "--filter" => opts.filter = Some(value.to_lowercase()),
            "-T" | "--timeout" => {
                let secs: u64 = value
                    .parse()
                    .map_err(|_| format!("--timeout expects whole seconds, got '{value}'"))?;
                opts.timeout = Duration::from_secs(secs);
            }
            "-n" | "--repeat" => {
                opts.repeat = value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| format!("--repeat expects a number >= 1, got '{value}'"))?;
            }
            "-o" | "--out" => opts.out = Some(PathBuf::from(value)),
            // The agent starts in a scratch directory, so a relative path would stop resolving.
            "--spavn" => {
                let path = PathBuf::from(&value);
                opts.spavn = fs::canonicalize(&path).unwrap_or(path);
            }
            other => return Err(format!("unknown option '{other}'")),
        }
        i += 2;
    }
    Ok(opts)
}

fn load_tasks(opts: &Options) -> Result<Vec<Task>, String> {
    let entries = fs::read_dir(&opts.tasks_dir)
        .map_err(|e| format!("cannot read {}: {e}", opts.tasks_dir.display()))?;
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let manifest = dir.join("task.json");
        if !manifest.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let raw =
            fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
        let doc: Value =
            serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", manifest.display()))?;
        let field = |key: &str| -> Result<String, String> {
            doc.get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("{}: missing string field '{key}'", manifest.display()))
        };
        let tags: Vec<String> = doc
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(f) = &opts.filter {
            let hit = name.to_lowercase().contains(f)
                || tags.iter().any(|t| t.to_lowercase().contains(f));
            if !hit {
                continue;
            }
        }
        if !dir.join("repo").is_dir() {
            return Err(format!("{}: missing repo/ fixture", dir.display()));
        }
        tasks.push(Task {
            name,
            prompt: field("prompt")?,
            verify: field("verify")?,
            tags,
            dir,
        });
    }
    tasks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tasks)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Runs `verify` in `dir`; true when it exits 0.
fn verify_passes(dir: &Path, verify: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(verify)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

struct AgentRun {
    timed_out: bool,
    exit_ok: bool,
    reply: Value,
}

fn run_agent(opts: &Options, task: &Task, work: &Path, scratch: &Path) -> Result<AgentRun, String> {
    let stdout_path = scratch.join("agent.stdout");
    let stderr_path = scratch.join("agent.stderr");
    let open = |p: &Path| fs::File::create(p).map_err(|e| format!("{}: {e}", p.display()));
    let mut cmd = Command::new(&opts.spavn);
    cmd.arg("-j").arg("-a").arg("auto");
    if let Some(model) = &opts.model {
        cmd.arg("-m").arg(model);
    }
    cmd.arg("-p")
        .arg(&task.prompt)
        .current_dir(work)
        .env("SPAVN_UPDATE_CHECK", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(open(&stdout_path)?))
        .stderr(Stdio::from(open(&stderr_path)?));
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", opts.spavn.display()))?;

    let deadline = Instant::now() + opts.timeout;
    let (timed_out, exit_ok) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (false, status.success()),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break (true, false);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(format!("waiting for agent: {e}")),
        }
    };

    // `-j` prints a single JSON object as its last stdout line.
    let stdout = fs::read_to_string(&stdout_path).unwrap_or_default();
    let reply = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .unwrap_or(Value::Null);
    Ok(AgentRun {
        timed_out,
        exit_ok,
        reply,
    })
}

fn run_task(opts: &Options, task: &Task, scratch_root: &Path) -> (Outcome, f64, Value) {
    let started = Instant::now();
    let scratch = scratch_root.join(&task.name);
    let work = scratch.join("work");
    let finish =
        |outcome: Outcome, detail: Value| (outcome, started.elapsed().as_secs_f64(), detail);

    if let Err(e) = copy_dir(&task.dir.join("repo"), &work) {
        return finish(Outcome::AgentError, json!({ "error": e.to_string() }));
    }
    if verify_passes(&work, &task.verify) {
        return finish(Outcome::BrokenFixture, Value::Null);
    }
    let run = match run_agent(opts, task, &work, &scratch) {
        Ok(r) => r,
        Err(e) => return finish(Outcome::AgentError, json!({ "error": e })),
    };
    let detail = json!({
        "status": run.reply.get("status"),
        "model": run.reply.get("model"),
        "messages": run.reply.get("messages_count"),
    });
    if run.timed_out {
        return finish(Outcome::Timeout, detail);
    }
    // Judge on the files, not on the agent's own claim; a crashed run may still have fixed it.
    if verify_passes(&work, &task.verify) {
        return finish(Outcome::Pass, detail);
    }
    if !run.exit_ok {
        return finish(Outcome::AgentError, detail);
    }
    finish(Outcome::Fail, detail)
}

fn main() {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("spavn-eval: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let tasks = match load_tasks(&opts) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("spavn-eval: {e}");
            std::process::exit(2);
        }
    };
    if tasks.is_empty() {
        eprintln!(
            "spavn-eval: no tasks matched in {}",
            opts.tasks_dir.display()
        );
        std::process::exit(2);
    }
    if opts.list_only {
        for t in &tasks {
            println!("{:<28} [{}]  {}", t.name, t.tags.join(","), t.prompt);
        }
        return;
    }

    let scratch_root = std::env::temp_dir().join(format!("spavn-eval-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch_root);
    if let Err(e) = fs::create_dir_all(&scratch_root) {
        eprintln!("spavn-eval: {}: {e}", scratch_root.display());
        std::process::exit(2);
    }

    let mut out_file = opts.out.as_ref().map(|p| {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap_or_else(|e| {
                eprintln!("spavn-eval: {}: {e}", p.display());
                std::process::exit(2);
            })
    });

    println!("{:<28} {:<15} {:>8}", "TASK", "RESULT", "SECONDS");
    let (mut passed, mut counted, mut broken) = (0usize, 0usize, 0usize);
    let mut total_secs = 0.0;
    // (task name, passes, counted attempts) in run order, for the per-task summary.
    let mut per_task: Vec<(String, usize, usize)> = Vec::new();
    let attempts = tasks
        .iter()
        .flat_map(|t| (1..=opts.repeat).map(move |n| (t, n)));
    for (task, attempt) in attempts {
        // A fresh scratch directory per attempt, so one run never sees another's edits.
        let attempt_root = scratch_root.join(format!("attempt-{attempt}"));
        let (outcome, secs, detail) = run_task(&opts, task, &attempt_root);
        let label = if opts.repeat > 1 {
            format!("{}#{attempt}", task.name)
        } else {
            task.name.clone()
        };
        println!("{:<28} {:<15} {:>8.1}", label, outcome.as_str(), secs);
        if per_task
            .last()
            .map(|(n, _, _)| n != &task.name)
            .unwrap_or(true)
        {
            per_task.push((task.name.clone(), 0, 0));
        }
        if outcome == Outcome::BrokenFixture {
            broken += 1;
        } else {
            counted += 1;
            total_secs += secs;
            if let Some(entry) = per_task.last_mut() {
                entry.2 += 1;
                if outcome == Outcome::Pass {
                    entry.1 += 1;
                }
            }
            if outcome == Outcome::Pass {
                passed += 1;
            }
        }
        if let Some(f) = out_file.as_mut() {
            use std::io::Write;
            let line = json!({
                "task": task.name,
                "attempt": attempt,
                "tags": task.tags,
                "model": opts.model.clone().or_else(|| {
                    detail.get("model").and_then(Value::as_str).map(str::to_string)
                }),
                "outcome": outcome.as_str(),
                "seconds": secs,
                "detail": detail,
            });
            let _ = writeln!(f, "{line}");
        }
    }
    let _ = fs::remove_dir_all(&scratch_root);

    let rate = if counted == 0 {
        0.0
    } else {
        100.0 * passed as f64 / counted as f64
    };
    if opts.repeat > 1 {
        println!("\n{:<28} {:>10}", "TASK", "PASS RATE");
        for (name, ok, total) in &per_task {
            println!("{name:<28} {:>10}", format!("{ok}/{total}"));
        }
    }
    println!(
        "\n{passed}/{counted} passed ({rate:.0}%), {total_secs:.0}s total{}",
        if broken > 0 {
            format!(", {broken} broken fixture(s) excluded")
        } else {
            String::new()
        }
    );
    if broken > 0 || passed < counted {
        std::process::exit(1);
    }
}
