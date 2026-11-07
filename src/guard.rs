use crate::config::{home_dir, load_user_config, tr_en};
use std::path::{Component, Path, PathBuf};

const SYSTEM_WRITE_DENY: &[&str] = &[
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/etc",
    "/private/etc",
    "/var",
    "/Library",
    "/Applications",
    "/opt",
    "/dev",
    "/proc",
    "/sys",
    "/boot",
];

const SYSTEM_ROOTS: &[&str] = &[
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/etc",
    "/private/etc",
    "/var",
    "/Library",
    "/Applications",
    "/opt",
    "/dev",
    "/proc",
    "/sys",
    "/boot",
];

const EXACT_CRITICAL: &[&str] = &["/Users", "/home", "/private", "/Volumes"];

const SECRET_HOME: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".netrc",
    ".spavn/secrets.env",
    "Library/Keychains",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".docker/config.json",
    ".npmrc",
    ".kube",
    ".config/gcloud",
];

const SENSITIVE_HOME: &[&str] = &[
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".config",
    "Library/LaunchAgents",
    "Library/LaunchDaemons",
    ".spavn/hooks",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathRisk {
    Allow,
    Ask {
        reason: String,
        even_sovereign: bool,
    },
    Deny {
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct PathPolicy {
    pub cwd: PathBuf,
    pub workspace_roots: Vec<PathBuf>,
    pub protected: Vec<PathBuf>,
    pub allow: Vec<PathBuf>,
    pub home: PathBuf,
}

impl PathPolicy {
    pub fn new(workspace_root: &Path) -> Self {
        let cfg = load_user_config();
        let home = home_dir();
        let cwd = canonicalize_soft(workspace_root);
        let mut workspace_roots = vec![cwd.clone()];
        let mut allow = Vec::new();
        for raw in &cfg.allowed_paths {
            if raw.trim().is_empty() {
                continue;
            }
            let resolved = canonicalize_soft(&expand_path(raw, &home, &cwd));
            workspace_roots.push(resolved.clone());
            allow.push(resolved);
        }
        let protected = cfg
            .protected_paths
            .iter()
            .filter(|p| !p.trim().is_empty())
            .map(|p| canonicalize_soft(&expand_path(p, &home, &cwd)))
            .collect();
        Self {
            cwd,
            workspace_roots,
            protected,
            allow,
            home,
        }
    }

    #[cfg(test)]
    pub fn for_test(cwd: &str, workspace: &str, protected: Vec<&str>) -> Self {
        let home = home_dir();
        Self {
            cwd: PathBuf::from(cwd),
            workspace_roots: vec![PathBuf::from(workspace)],
            protected: protected.into_iter().map(PathBuf::from).collect(),
            allow: Vec::new(),
            home,
        }
    }

    pub fn resolve(&self, raw: &str) -> PathBuf {
        canonicalize_soft(&expand_path(raw, &self.home, &self.cwd))
    }

    pub fn is_protected(&self, path: &Path) -> bool {
        under_any(path, &self.protected)
    }

    pub fn check(&self, raw: &str, write: bool) -> PathRisk {
        let raw = raw.trim();
        if raw.is_empty() {
            return PathRisk::Allow;
        }
        if looks_remote(raw) {
            return if write {
                PathRisk::Ask {
                    reason: "writing on remote host".to_string(),
                    even_sovereign: false,
                }
            } else {
                PathRisk::Allow
            };
        }

        let path = self.resolve(raw);

        if under_any(&path, &self.allow) {
            return PathRisk::Allow;
        }

        for rel in SECRET_HOME {
            let secret = self.home.join(rel);
            if path == secret || path.starts_with(&secret) {
                return PathRisk::Ask {
                    reason: format!("hassas dosya ({}), onay gerekli", rel),
                    even_sovereign: true,
                };
            }
        }

        for rel in SENSITIVE_HOME {
            let sensitive = self.home.join(rel);
            if path == sensitive || path.starts_with(&sensitive) {
                return PathRisk::Ask {
                    reason: format!("sensitive user file ({}), approval required", rel),
                    even_sovereign: false,
                };
            }
        }

        if under_any(&path, &self.workspace_roots) || is_temp_path(&path) {
            return PathRisk::Allow;
        }

        if write {
            for sys in SYSTEM_WRITE_DENY {
                let root = PathBuf::from(sys);
                if path == root || path.starts_with(&root) {
                    return PathRisk::Deny {
                        reason: format!("system directory ({}) write is protected", sys),
                    };
                }
            }
            if path.parent().is_none() {
                return PathRisk::Deny {
                    reason: "root directory write is protected".to_string(),
                };
            }
        }

        PathRisk::Ask {
            reason: "outside the working area".to_string(),
            even_sovereign: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellRisk {
    None,
    Destructive(String),
    Catastrophic(String),
}

#[derive(Debug, Clone)]
struct Tok {
    text: String,
    dynamic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Critical,
    Protected,
    Dynamic,
    Plain,
}

pub fn assess_shell_command(cmd: &str, policy: &PathPolicy) -> ShellRisk {
    let mut worst = ShellRisk::None;
    // `cd <dir> && rm -rf x` must judge `x` relative to <dir>, not the workspace.
    let mut policy = policy.clone();
    for (seg_idx, segment) in tokenize(cmd).iter().enumerate() {
        if let Some(risk) = indirect_exec_risk(segment, seg_idx) {
            escalate(&mut worst, risk);
        }
        if segment.first().map(|t| basename(&t.text)) == Some("cd") {
            if let Some(dir) = segment
                .get(1)
                .filter(|t| !t.dynamic && !t.text.contains('$'))
            {
                policy.cwd = policy.resolve(&dir.text);
            }
        }
        let policy = &policy;
        if let Some(risk) = analyze_segment(segment, policy) {
            escalate(&mut worst, risk);
        }
        for idx in 0..segment.len() {
            let is_redirect = matches!(segment[idx].text.as_str(), ">" | ">>" | "&>");
            if is_redirect {
                if let Some(target) = segment.get(idx + 1) {
                    if target.text == "/dev/null" {
                        continue;
                    }
                    let mut out: Option<ShellRisk> = None;
                    match classify_target(policy, target) {
                        TargetKind::Critical => push(
                            &mut out,
                            ShellRisk::Catastrophic(
                                tr_en(
                                    "kritik dosyaya yönlendirme: {0}",
                                    "critical file redirection: {0}",
                                )
                                .replace("{0}", &target.text),
                            ),
                        ),
                        TargetKind::Protected => push(
                            &mut out,
                            ShellRisk::Destructive(
                                tr_en(
                                    "korunan dosyaya yönlendirme: {0}",
                                    "protected file redirection: {0}",
                                )
                                .replace("{0}", &target.text),
                            ),
                        ),
                        _ => {}
                    }
                    if let Some(risk) = out {
                        escalate(&mut worst, risk);
                    }
                }
            }
        }
    }
    worst
}

fn analyze_segment(tokens: &[Tok], policy: &PathPolicy) -> Option<ShellRisk> {
    let mut idx = 0;
    let mut saw_sudo = false;

    while idx < tokens.len() {
        let text = &tokens[idx].text;
        let base = basename(text);
        if is_env_assignment(text) {
            idx += 1;
            continue;
        }
        if base == "sudo" || base == "doas" {
            saw_sudo = true;
            idx += 1;
            while idx < tokens.len() && tokens[idx].text.starts_with('-') {
                idx += 1;
            }
            continue;
        }
        if matches!(
            base,
            "env" | "command" | "builtin" | "nohup" | "time" | "caffeinate" | "nice" | "xargs"
        ) {
            idx += 1;
            while idx < tokens.len() && tokens[idx].text.starts_with('-') {
                idx += 1;
            }
            continue;
        }
        break;
    }

    let mut out: Option<ShellRisk> = None;
    if idx < tokens.len() {
        let cmd = basename(&tokens[idx].text).to_string();
        let args = &tokens[idx + 1..];
        analyze_command(&cmd, args, policy, &mut out);
        if let Some(inner) = nested_shell_script(&cmd, args) {
            let risk = assess_shell_command(&inner, policy);
            if risk != ShellRisk::None {
                push(&mut out, risk);
            }
        }
        if let Some(arg) = args.iter().find(|a| {
            !a.text.starts_with('-') && is_secret_only_path(policy, &policy.resolve(&a.text))
        }) {
            push(
                &mut out,
                ShellRisk::Destructive(format!("hidden file access: {}", arg.text)),
            );
        }
    }
    if saw_sudo {
        push(
            &mut out,
            ShellRisk::Destructive("privilege escalation via sudo".to_string()),
        );
    }
    out
}

fn analyze_command(cmd: &str, args: &[Tok], policy: &PathPolicy, out: &mut Option<ShellRisk>) {
    match cmd {
        "rm" => {
            let mut recursive = false;
            let mut force = false;
            let mut after_separator = false;
            let mut targets: Vec<&Tok> = Vec::new();
            for arg in args {
                if !after_separator {
                    if arg.text == "--" {
                        after_separator = true;
                        continue;
                    }
                    if arg.text.starts_with('-') && arg.text.len() > 1 {
                        if arg.text == "--recursive" {
                            recursive = true;
                        }
                        if arg.text == "--force" {
                            force = true;
                        }
                        if !arg.text.starts_with("--") {
                            if arg.text.contains('r') || arg.text.contains('R') {
                                recursive = true;
                            }
                            if arg.text.contains('f') {
                                force = true;
                            }
                        }
                        continue;
                    }
                }
                targets.push(arg);
            }
            for target in targets {
                let wildcard_all = matches!(target.text.as_str(), "*" | "./*" | ".*" | "./.*");
                let kind = if wildcard_all {
                    TargetKind::Critical
                } else {
                    classify_target(policy, target)
                };
                match kind {
                    TargetKind::Critical if recursive => push(
                        out,
                        ShellRisk::Catastrophic(format!(
                            "root/working-area recursive deletion: {}",
                            target.text
                        )),
                    ),
                    TargetKind::Protected if recursive => push(
                        out,
                        ShellRisk::Catastrophic(
                            tr_en(
                                "korunan dizin özyinelemeli silme: {0}",
                                "protected directory recursive deletion: {0}",
                            )
                            .replace("{0}", &target.text),
                        ),
                    ),
                    TargetKind::Critical => push(
                        out,
                        ShellRisk::Destructive(
                            tr_en("kritik yol silme: {0}", "critical path deletion: {0}")
                                .replace("{0}", &target.text),
                        ),
                    ),
                    TargetKind::Protected => push(
                        out,
                        ShellRisk::Catastrophic(
                            tr_en(
                                "korunan yol silme: {0}",
                                "protected path deletion: {0}"
                            )
                            .replace("{0}", &target.text),
                        ),
                    ),
                    TargetKind::Dynamic if recursive => push(
                        out,
                        ShellRisk::Destructive(format!(
                            "variable recursive deletion: {}",
                            target.text
                        )),
                    ),
                    _ if recursive => push(
                        out,
                        ShellRisk::Destructive(
                            tr_en("özyinelemeli silme: {0}", "recursive deletion: {0}")
                                .replace("{0}", &target.text),
                        ),
                    ),
                    _ if force => push(
                        out,
                        ShellRisk::Destructive(
                            tr_en("zorla silme: {0}", "forced deletion: {0}")
                                .replace("{0}", &target.text),
                        ),
                    ),
                    _ => {}
                }
            }
        }
        "find" => {
            let mut deletes = false;
            for (pos, arg) in args.iter().enumerate() {
                if arg.text == "-delete" {
                    deletes = true;
                }
                if matches!(arg.text.as_str(), "-exec" | "-execdir")
                    && args
                        .get(pos + 1)
                        .map(|t| basename(&t.text) == "rm")
                        .unwrap_or(false)
                {
                    deletes = true;
                }
            }
            if deletes {
                let path_token = args
                    .iter()
                    .find(|a| !a.text.starts_with('-') && !a.text.starts_with('('));
                let kind = path_token
                    .map(|t| classify_target(policy, t))
                    .unwrap_or(TargetKind::Dynamic);
                match kind {
                    TargetKind::Critical | TargetKind::Protected => push(
                        out,
                        ShellRisk::Catastrophic(format!(
                            "korunan yolda find ile silme: {}",
                            path_token.map(|t| t.text.as_str()).unwrap_or(".")
                        )),
                    ),
                    _ => push(
                        out,
                        ShellRisk::Destructive("find ile dosya silme".to_string()),
                    ),
                }
            }
        }
        "truncate" | "shred" | "unlink" => {
            for target in args.iter().filter(|a| !a.text.starts_with('-')) {
                match classify_target(policy, target) {
                    TargetKind::Critical => push(
                        out,
                        ShellRisk::Catastrophic(format!("critical file typo: {}", target.text)),
                    ),
                    TargetKind::Protected => push(
                        out,
                        ShellRisk::Destructive(format!("protected file typo: {}", target.text)),
                    ),
                    _ => {}
                }
            }
        }
        "tee" => {
            for target in args.iter().filter(|a| !a.text.starts_with('-')) {
                match classify_target(policy, target) {
                    TargetKind::Critical => push(
                        out,
                        ShellRisk::Catastrophic(
                            tr_en("kritik dosyaya yazma: {0}", "critical file write: {0}")
                                .replace("{0}", &target.text),
                        ),
                    ),
                    TargetKind::Protected => push(
                        out,
                        ShellRisk::Destructive(
                            tr_en("korunan dosyaya yazma: {0}", "protected file write: {0}")
                                .replace("{0}", &target.text),
                        ),
                    ),
                    _ => {}
                }
            }
        }
        "sed" | "perl" => {
            let in_place = args.iter().any(|a| {
                a.text.starts_with("-i")
                    || a.text == "--in-place"
                    || a.text.starts_with("--in-place=")
            });
            if in_place {
                if let Some(target) = args.iter().rev().find(|a| !a.text.starts_with('-')) {
                    match classify_target(policy, target) {
                        TargetKind::Critical | TargetKind::Protected => push(
                            out,
                            ShellRisk::Destructive(format!(
                                "in-place edit on protected file: {}",
                                target.text
                            )),
                        ),
                        _ => {}
                    }
                }
            }
        }
        "dd" => {
            for arg in args {
                if let Some(target) = arg.text.strip_prefix("of=") {
                    let token = Tok {
                        text: target.to_string(),
                        dynamic: arg.dynamic,
                    };
                    match classify_target(policy, &token) {
                        TargetKind::Critical | TargetKind::Protected => push(
                            out,
                            ShellRisk::Catastrophic(
                                tr_en(
                                    "dd ile disk/dosya yazma: {0}",
                                    "dd disk/file write: {0}"
                                )
                                .replace("{0}", target),
                            ),
                        ),
                        _ => {}
                    }
                }
            }
        }
        "chmod" | "chown" | "chgrp" => {
            let recursive = args.iter().any(|a| {
                a.text == "--recursive"
                    || (a.text.starts_with('-')
                        && !a.text.starts_with("--")
                        && a.text.contains('R'))
            });
            if recursive {
                if let Some(target) = args.iter().rev().find(|a| !a.text.starts_with('-')) {
                    match classify_target(policy, target) {
                        TargetKind::Critical | TargetKind::Protected => push(
                            out,
                            ShellRisk::Destructive(format!(
                                "permission change on protected path: {}",
                                target.text
                            )),
                        ),
                        _ => {}
                    }
                }
            }
        }
        "mv" | "cp" | "install" | "ln" | "rsync" => {
            if args.iter().any(|a| a.text == "--delete") {
                push(out, ShellRisk::Destructive("rsync --delete".to_string()));
            }
            if cmd == "mv" {
                // The sources vanish from their place, so moving a protected one is a delete.
                let paths: Vec<&Tok> = args.iter().filter(|a| !a.text.starts_with('-')).collect();
                for src in paths.iter().take(paths.len().saturating_sub(1)) {
                    match classify_target(policy, src) {
                        TargetKind::Critical => push(
                            out,
                            ShellRisk::Catastrophic(format!("critical path move: {}", src.text)),
                        ),
                        TargetKind::Protected => push(
                            out,
                            ShellRisk::Destructive(format!("protected path move: {}", src.text)),
                        ),
                        _ => {}
                    }
                }
            }
            if let Some(dest) = args.iter().rev().find(|a| !a.text.starts_with('-')) {
                if matches!(
                    classify_target(policy, dest),
                    TargetKind::Critical | TargetKind::Protected
                ) {
                    push(
                        out,
                        ShellRisk::Destructive(format!(
                            "move/copy to protected path: {}",
                            dest.text
                        )),
                    );
                }
            }
        }
        "git" => {
            let sub = args.first().map(|a| a.text.as_str()).unwrap_or("");
            let flags: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
            if sub == "push"
                && flags
                    .iter()
                    .any(|f| *f == "-f" || *f == "--force" || *f == "--force-with-lease")
            {
                push(out, ShellRisk::Destructive("git push --force".to_string()));
            }
            if sub == "reset" && flags.contains(&"--hard") {
                push(out, ShellRisk::Destructive("git reset --hard".to_string()));
            }
            if sub == "clean"
                && flags.iter().any(|f| {
                    f.starts_with('-')
                        && !f.starts_with("--")
                        && (f.contains('f') || f.contains('d') || f.contains('x'))
                })
            {
                push(
                    out,
                    ShellRisk::Destructive("git clean (dosya silme)".to_string()),
                );
            }
        }
        "docker" => {
            let joined: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
            if joined.contains(&"down")
                || joined.contains(&"rm")
                || joined.contains(&"prune")
                || joined.contains(&"up")
            {
                push(
                    out,
                    ShellRisk::Destructive("docker compose/service change".to_string()),
                );
            }
        }
        "systemctl" => {
            if args
                .iter()
                .any(|a| matches!(a.text.as_str(), "restart" | "stop" | "disable" | "mask"))
            {
                push(
                    out,
                    ShellRisk::Destructive("systemctl service change".to_string()),
                );
            }
        }
        "launchctl" => {
            if args.iter().any(|a| {
                matches!(
                    a.text.as_str(),
                    "unload" | "bootout" | "remove" | "disable" | "kill"
                )
            }) {
                push(
                    out,
                    ShellRisk::Destructive("launchctl service change".to_string()),
                );
            }
        }
        "kill" | "pkill" | "killall" => {
            push(
                out,
                ShellRisk::Destructive("process termination".to_string()),
            );
        }
        "shutdown" | "reboot" | "halt" | "mkfs" | "fdisk" | "gpt" => {
            push(
                out,
                ShellRisk::Destructive(format!("sistem komutu: {}", cmd)),
            );
        }
        "diskutil" => {
            if args
                .iter()
                .any(|a| a.text.starts_with("erase") || a.text.starts_with("apfsDelete"))
            {
                push(
                    out,
                    ShellRisk::Catastrophic("diskutil ile disk silme".to_string()),
                );
            } else {
                push(out, ShellRisk::Destructive("diskutil".to_string()));
            }
        }
        "defaults" if args.iter().any(|a| a.text == "write" || a.text == "delete") => {
            push(
                out,
                ShellRisk::Destructive("macOS setting change".to_string()),
            );
        }
        _ => {}
    }
}

fn classify_target(policy: &PathPolicy, token: &Tok) -> TargetKind {
    let text = token.text.trim();
    if text.is_empty() {
        return TargetKind::Plain;
    }
    let resolved = policy.resolve(text);
    if is_critical_path(policy, &resolved) {
        return TargetKind::Critical;
    }
    if policy.is_protected(&resolved) {
        return TargetKind::Protected;
    }
    if is_secret_path(policy, &resolved) {
        return TargetKind::Protected;
    }
    if token.dynamic || text.contains('$') || text.contains('`') {
        return TargetKind::Dynamic;
    }
    TargetKind::Plain
}

fn is_critical_path(policy: &PathPolicy, path: &Path) -> bool {
    if path == Path::new("/") || path == policy.home {
        return true;
    }
    if policy.workspace_roots.iter().any(|root| path == root) {
        return true;
    }
    if EXACT_CRITICAL.iter().any(|root| path == Path::new(root)) {
        return true;
    }
    SYSTEM_ROOTS.iter().any(|root| {
        let root = Path::new(root);
        path == root || path.starts_with(root)
    })
}

/// `curl ... | sh`, `base64 -d | bash` and `$VAR args`: the real command is invisible to the checks.
fn indirect_exec_risk(segment: &[Tok], seg_idx: usize) -> Option<ShellRisk> {
    let first = segment.first()?;
    if is_env_assignment(&first.text) {
        return None;
    }
    if first.dynamic || first.text.contains('$') {
        return Some(ShellRisk::Destructive(format!(
            "dynamic command name: {}",
            first.text
        )));
    }
    let reads_stdin = segment.len() == 1 || (segment.len() == 2 && segment[1].text == "-s");
    if seg_idx > 0
        && reads_stdin
        && matches!(
            basename(&first.text),
            "sh" | "bash" | "zsh" | "dash" | "ksh"
        )
    {
        return Some(ShellRisk::Destructive(
            "pipe script injection into shell (curl | sh)".to_string(),
        ));
    }
    None
}

/// Script text run by `sh -c '...'`, `bash -lc "..."` or `eval ...`, so it gets the same checks.
fn nested_shell_script(cmd: &str, args: &[Tok]) -> Option<String> {
    if cmd == "eval" {
        let joined = args
            .iter()
            .map(|a| a.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        return (!joined.is_empty()).then_some(joined);
    }
    if !matches!(cmd, "sh" | "bash" | "zsh" | "dash" | "ksh") {
        return None;
    }
    let pos = args.iter().position(|a| {
        a.text.starts_with('-') && !a.text.starts_with("--") && a.text.contains('c')
    })?;
    args.get(pos + 1).map(|a| a.text.clone())
}

fn is_secret_only_path(policy: &PathPolicy, path: &Path) -> bool {
    SECRET_HOME.iter().any(|rel| {
        let base = policy.home.join(rel);
        path == base.as_path() || path.starts_with(&base)
    })
}

fn is_secret_path(policy: &PathPolicy, path: &Path) -> bool {
    SECRET_HOME.iter().chain(SENSITIVE_HOME.iter()).any(|rel| {
        let base = policy.home.join(rel);
        path == base.as_path() || path.starts_with(&base)
    })
}

fn is_temp_path(path: &Path) -> bool {
    let mut roots = vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/tmp"),
        PathBuf::from("/private/var/tmp"),
        PathBuf::from("/var/folders"),
        PathBuf::from("/private/var/folders"),
    ];
    if let Ok(tmp) = std::env::temp_dir().canonicalize() {
        roots.push(tmp);
    }
    under_any(path, &roots)
}

fn under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| path == root || path.starts_with(root))
}

fn looks_remote(raw: &str) -> bool {
    if let Some((prefix, rest)) = raw.split_once(':') {
        return !prefix.is_empty()
            && prefix.len() > 1
            && !prefix.contains('/')
            && (rest.starts_with('/') || rest.starts_with('~'));
    }
    false
}

fn expand_path(raw: &str, home: &Path, cwd: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.join(rest);
    }
    if raw == "$HOME" || raw == "${HOME}" {
        return home.to_path_buf();
    }
    if let Some(rest) = raw
        .strip_prefix("$HOME/")
        .or_else(|| raw.strip_prefix("${HOME}/"))
    {
        return home.join(rest);
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

pub fn canonicalize_soft(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if let Some(name) = current.file_name() {
            suffix.push(name.to_os_string());
        }
        match current.parent() {
            Some(parent) if parent != current => {
                if let Ok(canonical) = std::fs::canonicalize(parent) {
                    let mut out = canonical;
                    for part in suffix.iter().rev() {
                        out.push(part);
                    }
                    return normalize_lexical(&out);
                }
                current = parent.to_path_buf();
            }
            _ => break,
        }
    }
    normalize_lexical(path)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir => out.push("/"),
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::Normal(part) => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

fn push(out: &mut Option<ShellRisk>, risk: ShellRisk) {
    match out {
        Some(existing) if rank(existing) >= rank(&risk) => {}
        _ => *out = Some(risk),
    }
}

fn escalate(worst: &mut ShellRisk, risk: ShellRisk) {
    if rank(&risk) > rank(worst) {
        *worst = risk;
    }
}

fn rank(risk: &ShellRisk) -> u8 {
    match risk {
        ShellRisk::None => 0,
        ShellRisk::Destructive(_) => 1,
        ShellRisk::Catastrophic(_) => 2,
    }
}

#[allow(unused_assignments)]
fn tokenize(cmd: &str) -> Vec<Vec<Tok>> {
    let mut segments: Vec<Vec<Tok>> = vec![Vec::new()];
    let mut current = String::new();
    let mut dynamic = false;
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();

    macro_rules! flush {
        () => {{
            if in_word {
                segments.last_mut().unwrap().push(Tok {
                    text: std::mem::take(&mut current),
                    dynamic,
                });
                in_word = false;
                dynamic = false;
            }
        }};
    }
    macro_rules! split_segment {
        () => {{
            flush!();
            segments.push(Vec::new());
        }};
    }

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for c2 in chars.by_ref() {
                    if c2 == '\'' {
                        break;
                    }
                    current.push(c2);
                }
            }
            '"' => {
                in_word = true;
                while let Some(c2) = chars.next() {
                    match c2 {
                        '"' => break,
                        '\\' => {
                            if let Some(c3) = chars.next() {
                                current.push(c3);
                            }
                        }
                        '$' | '`' => {
                            dynamic = true;
                            current.push(c2);
                        }
                        _ => current.push(c2),
                    }
                }
            }
            '\\' => {
                if let Some(c2) = chars.next() {
                    in_word = true;
                    current.push(c2);
                }
            }
            ' ' | '\t' | '\r' => flush!(),
            ';' | '\n' => split_segment!(),
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
                split_segment!();
            }
            '|' => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                }
                split_segment!();
            }
            '(' | ')' | '{' | '}' => split_segment!(),
            '#' if !in_word => {
                for c2 in chars.by_ref() {
                    if c2 == '\n' {
                        break;
                    }
                }
                split_segment!();
            }
            '>' | '<' => {
                if c == '>'
                    && in_word
                    && !current.is_empty()
                    && current.chars().all(|ch| ch.is_ascii_digit())
                {
                    current.clear();
                    in_word = false;
                    dynamic = false;
                }
                flush!();
                let mut token = c.to_string();
                if c == '>' {
                    if chars.peek() == Some(&'>') {
                        chars.next();
                        token.push('>');
                    }
                    if chars.peek() == Some(&'&') {
                        chars.next();
                        token.push('&');
                        while let Some(digit) = chars.peek() {
                            if digit.is_ascii_digit() {
                                token.push(*digit);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                }
                segments.last_mut().unwrap().push(Tok {
                    text: token,
                    dynamic: false,
                });
            }
            '$' | '`' => {
                in_word = true;
                dynamic = true;
                current.push(c);
                // Keep `${NAME}` whole; `{`/`}` would otherwise split the command into segments.
                if c == '$' && chars.peek() == Some(&'{') {
                    for c2 in chars.by_ref() {
                        current.push(c2);
                        if c2 == '}' {
                            break;
                        }
                    }
                }
            }
            _ => {
                in_word = true;
                current.push(c);
            }
        }
    }
    flush!();
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PathPolicy {
        PathPolicy::for_test(
            "/Users/tester/proj",
            "/Users/tester/proj",
            vec!["/Users/tester/apps"],
        )
    }

    #[test]
    fn quoted_flags_are_detected() {
        let risk = assess_shell_command("rm \"-rf\" /Users/tester/apps/x", &policy());
        assert!(matches!(risk, ShellRisk::Catastrophic(_)), "{:?}", risk);
    }

    #[test]
    fn root_delete_is_catastrophic() {
        assert!(matches!(
            assess_shell_command("rm -rf /", &policy()),
            ShellRisk::Catastrophic(_)
        ));
    }

    #[test]
    fn home_delete_is_catastrophic() {
        assert!(matches!(
            assess_shell_command("rm -rf ~", &policy()),
            ShellRisk::Catastrophic(_)
        ));
    }

    #[test]
    fn workspace_root_delete_is_catastrophic() {
        assert!(matches!(
            assess_shell_command("rm -rf .", &policy()),
            ShellRisk::Catastrophic(_)
        ));
        assert!(matches!(
            assess_shell_command("rm -rf *", &policy()),
            ShellRisk::Catastrophic(_)
        ));
    }

    #[test]
    fn protected_path_delete_is_catastrophic() {
        assert!(matches!(
            assess_shell_command("rm -rf /Users/tester/apps/thing", &policy()),
            ShellRisk::Catastrophic(_)
        ));
    }

    #[test]
    fn subdirectory_delete_is_destructive() {
        assert!(matches!(
            assess_shell_command("rm -rf ./build", &policy()),
            ShellRisk::Destructive(_)
        ));
    }

    #[test]
    fn dynamic_delete_is_destructive() {
        assert!(matches!(
            assess_shell_command("rm -rf $DIR", &policy()),
            ShellRisk::Destructive(_)
        ));
    }

    #[test]
    fn find_delete_on_protected_is_catastrophic() {
        assert!(matches!(
            assess_shell_command("find /Users/tester/apps -delete", &policy()),
            ShellRisk::Catastrophic(_)
        ));
    }

    #[test]
    fn redirect_to_shell_rc_is_destructive() {
        assert!(matches!(
            assess_shell_command("echo evil >> ~/.zshrc", &policy()),
            ShellRisk::Destructive(_)
        ));
    }

    #[test]
    fn tee_to_shell_rc_is_destructive() {
        assert!(matches!(
            assess_shell_command("echo evil | tee ~/.zshrc", &policy()),
            ShellRisk::Destructive(_)
        ));
    }

    #[test]
    fn in_place_edit_on_protected_is_destructive() {
        assert!(matches!(
            assess_shell_command("sed -i '' 's/a/b/' ~/.zshrc", &policy()),
            ShellRisk::Destructive(_)
        ));
    }

    #[test]
    fn chained_command_is_scanned() {
        assert!(matches!(
            assess_shell_command("ls -la && rm -rf /", &policy()),
            ShellRisk::Catastrophic(_)
        ));
    }

    #[test]
    fn force_push_is_destructive() {
        assert!(matches!(
            assess_shell_command("git push --force origin main", &policy()),
            ShellRisk::Destructive(_)
        ));
    }

    #[test]
    fn sudo_is_destructive() {
        assert!(matches!(
            assess_shell_command("sudo ls", &policy()),
            ShellRisk::Destructive(_)
        ));
    }

    #[test]
    fn harmless_commands_are_clean() {
        assert_eq!(assess_shell_command("ls -la", &policy()), ShellRisk::None);
        assert_eq!(
            assess_shell_command("cargo test --all", &policy()),
            ShellRisk::None
        );
        assert_eq!(
            assess_shell_command("rg pattern src", &policy()),
            ShellRisk::None
        );
        assert_eq!(
            assess_shell_command("ls ~/.cargo/bin 2>/dev/null", &policy()),
            ShellRisk::None
        );
    }

    #[test]
    fn egress_classification_covers_the_ssrf_bypasses() {
        // Loopback spellings that used to slip past a `contains("127.0.0.1")` test.
        for url in [
            "http://127.0.0.1:8080/v1",
            "http://localhost/admin",
            "http://[::1]:11434/api/tags",
            "http://127.0.0.1.nip.io/",
            "http://lvh.me/",
            "http://engine.local/",
        ] {
            assert_eq!(classify_egress(url), Some(EgressTarget::Loopback), "{url}");
        }
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/admin",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://100.100.100.200/",
            "http://[fd00::1]/",
            "http://0.0.0.0/",
        ] {
            assert_eq!(classify_egress(url), Some(EgressTarget::Private), "{url}");
            assert!(egress_is_blocked(url), "{url} must be blocked");
        }
        for url in ["https://example.com/x", "https://api.github.com"] {
            assert_eq!(classify_egress(url), Some(EgressTarget::Public), "{url}");
            assert!(!egress_is_blocked(url), "{url}");
        }
        // A name that merely *starts* with "localhost" is remote; it is only dangerous when it
        // resolves to loopback, which the resolver now refuses (see `PublicOnlyResolver`).
        assert_eq!(
            classify_egress("http://localhost.attacker.net/"),
            Some(EgressTarget::Public)
        );
    }

    #[test]
    fn mapped_v6_and_internal_names_are_not_public() {
        // `[::ffff:127.0.0.1]` dials v4 loopback; `metadata.google.internal` resolves to
        // 169.254.169.254. Both used to classify as public because neither is a v4 literal and
        // the name is not one of the wildcard-DNS services.
        for url in [
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:169.254.169.254]/",
        ] {
            assert!(egress_is_blocked(url), "{url} must be blocked");
        }
        for url in [
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://router.lan/",
            "http://nas.home.arpa/",
            "http://build.corp/",
        ] {
            assert_eq!(classify_egress(url), Some(EgressTarget::Private), "{url}");
        }
    }

    #[test]
    fn numeric_and_hex_loopback_spellings_are_decimal_mapped() {
        // `http://2130706433/` and `http://0x7f000001/` both reach 127.0.0.1.
        for url in ["http://2130706433/", "http://0x7f000001/"] {
            assert_eq!(classify_egress(url), Some(EgressTarget::Loopback), "{url}");
        }
    }

    #[test]
    fn path_inside_workspace_is_allowed() {
        assert_eq!(policy().check("src/main.rs", true), PathRisk::Allow);
        assert_eq!(policy().check(".", false), PathRisk::Allow);
    }

    #[test]
    fn ssh_key_read_requires_approval() {
        let ssh_key = policy().home.join(".ssh/id_rsa");
        let risk = policy().check(&ssh_key.to_string_lossy(), false);
        assert!(
            matches!(
                risk,
                PathRisk::Ask {
                    even_sovereign: true,
                    ..
                }
            ),
            "{:?}",
            risk
        );
    }

    #[test]
    fn system_write_is_denied() {
        assert!(matches!(
            policy().check("/etc/hosts", true),
            PathRisk::Deny { .. }
        ));
    }

    #[test]
    fn outside_workspace_write_asks() {
        assert!(matches!(
            policy().check("/Users/tester/other/file.txt", true),
            PathRisk::Ask { .. }
        ));
    }

    #[test]
    fn secrets_are_protected_even_when_workspace_is_home() {
        let home_policy =
            PathPolicy::for_test("/Users/tester", "/Users/tester", vec!["/Users/tester/apps"]);
        let zshrc = home_policy.home.join(".zshrc");
        assert!(
            matches!(
                home_policy.check(&zshrc.to_string_lossy(), true),
                PathRisk::Ask { .. }
            ),
            "shell rc must still require approval when cwd is home"
        );
        let ssh = home_policy.home.join(".ssh/id_rsa");
        assert!(matches!(
            home_policy.check(&ssh.to_string_lossy(), false),
            PathRisk::Ask {
                even_sovereign: true,
                ..
            }
        ));
    }

    #[test]
    fn temp_write_is_allowed() {
        assert_eq!(policy().check("/tmp/spavn-test.txt", true), PathRisk::Allow);
    }

    #[test]
    fn nested_shell_is_inspected() {
        for c in [
            "bash -c 'rm -rf /Users/tester/apps/x'",
            "sh -c \"rm -rf /Users/tester/apps/x\"",
            "zsh -lc 'rm -rf /Users/tester/apps/x'",
            "eval rm -rf /Users/tester/apps/x",
        ] {
            assert!(
                matches!(
                    assess_shell_command(c, &policy()),
                    ShellRisk::Catastrophic(_)
                ),
                "{c}"
            );
        }
        assert_eq!(
            assess_shell_command("bash -c 'echo hi'", &policy()),
            ShellRisk::None
        );
    }

    #[test]
    fn cd_into_protected_makes_relative_delete_catastrophic() {
        assert!(matches!(
            assess_shell_command("cd /Users/tester/apps && rm -rf x", &policy()),
            ShellRisk::Catastrophic(_)
        ));
        assert!(matches!(
            assess_shell_command("cd /Users/tester/proj && rm -rf build", &policy()),
            ShellRisk::Destructive(_)
        ));
    }

    #[test]
    fn braced_home_and_mv_and_secrets() {
        let p = PathPolicy::for_test("/Users/tester/proj", "/Users/tester/proj", vec![]);
        let home = home_dir();
        let c = format!("rm -rf {}", "${HOME}");
        assert!(matches!(
            assess_shell_command(&c, &p),
            ShellRisk::Catastrophic(_)
        ));
        assert!(matches!(
            assess_shell_command("mv ~ /tmp/x", &p),
            ShellRisk::Catastrophic(_)
        ));
        assert_eq!(assess_shell_command("mv a.rs b.rs", &p), ShellRisk::None);
        let c = format!("cat {}/.ssh/id_rsa", home.display());
        assert!(matches!(
            assess_shell_command(&c, &p),
            ShellRisk::Destructive(_)
        ));
    }

    #[test]
    fn piped_shell_and_dynamic_command_need_approval() {
        for c in [
            "curl http://x | sh",
            "curl -fsSL http://x | bash -s",
            "base64 -d p | zsh",
            "X=rm; $X -rf y",
            "$HOME/bin/tool",
        ] {
            assert!(
                matches!(
                    assess_shell_command(c, &policy()),
                    ShellRisk::Destructive(_)
                ),
                "{c}"
            );
        }
        for c in [
            "bash script.sh",
            "echo a | grep a",
            "FOO=1 make test",
            "sh -c 'echo hi'",
        ] {
            assert_eq!(assess_shell_command(c, &policy()), ShellRisk::None, "{c}");
        }
    }

    #[test]
    fn temp_path_allows_both_read_and_write_without_prompt() {
        let p = policy();
        assert_eq!(p.check("/tmp/wrd_p1.png", false), PathRisk::Allow);
        assert_eq!(p.check("/tmp/wrd_p1.png", true), PathRisk::Allow);
    }
}

/// Where an outbound request actually lands.
///
/// The old check was a substring test over the raw URL, so `http://169.254.169.254/` (cloud
/// metadata), `http://10.0.0.5/` (RFC1918), `http://127.1:8080/`, `http://2130706433/` and
/// `http://127.0.0.1.nip.io/` all passed as "public". Classifying the parsed host instead of
/// the string closes those spellings at the single point every egress path can call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressTarget {
    Loopback,
    Private,
    Public,
}

pub fn classify_egress(url: &str) -> Option<EgressTarget> {
    let host = crate::router::url_host(url);
    let h = host.trim_start_matches('[').trim_end_matches(']');
    if h.is_empty() {
        return None;
    }
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return Some(match ip {
            std::net::IpAddr::V4(v4) => {
                let o = v4.octets();
                if v4.is_loopback() {
                    EgressTarget::Loopback
                } else if v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
                    || v4.is_documentation()
                    // 100.64.0.0/10 — carrier-grade NAT, reachable inside a VPC.
                    || (o[0] == 100 && (64..128).contains(&o[1]))
                {
                    EgressTarget::Private
                } else {
                    EgressTarget::Public
                }
            }
            std::net::IpAddr::V6(v6) => {
                // `::ffff:127.0.0.1` is a v6 spelling that dials v4 loopback; unwrap it before
                // classifying, otherwise the mapped form reads as public.
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return classify_egress(&format!("http://{}/", v4));
                }
                if v6.is_loopback() {
                    EgressTarget::Loopback
                } else if v6.is_unspecified()
                    // fc00::/7 unique-local; also catches the IPv4-mapped form's prefix.
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                {
                    EgressTarget::Private
                } else {
                    EgressTarget::Public
                }
            }
        });
    }
    // Decimal/hex spellings of an IPv4 address never reach `parse`, but they also only resolve
    // locally, so anything that is all digits-with-dots or a bare integer is treated as private.
    if h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") {
        return Some(EgressTarget::Loopback);
    }
    let numeric =
        h.starts_with("0x") || (!h.contains('.') && h.chars().all(|c| c.is_ascii_digit()));
    if numeric {
        // `2130706433` and `0x7f000001` are inet_aton spellings of 127.0.0.1; decode them so the
        // loopback branch above applies instead of lumping every numeric host into "private".
        let decoded = if let Some(hex) = h.strip_prefix("0x") {
            u32::from_str_radix(hex, 16).ok()
        } else {
            h.parse::<u32>().ok()
        };
        if let Some(raw) = decoded {
            let v4 = std::net::Ipv4Addr::from(raw);
            return classify_egress(&format!("http://{}/", v4));
        }
        return Some(EgressTarget::Private);
    }
    // `1.0x7f.0.1` / `127.1` style short forms keep a dot but still encode loopback.
    if h.split('.').count() < 4
        && h.split('.').all(|p| {
            p.chars().all(|c| c.is_ascii_digit())
                || (p.starts_with("0x") && p[2..].chars().all(|c| c.is_ascii_hexdigit()))
        })
        && h.contains('.')
    {
        return Some(EgressTarget::Private);
    }
    // Wildcard-DNS services encode the destination in the name. `127.0.0.1.nip.io` parses as a
    // hostname, so it used to be classified public and then dialled at loopback.
    for label_group in ["nip.io", "sslip.io", "xip.io"] {
        if let Some(prefix) = h.strip_suffix(label_group) {
            let embedded = prefix.trim_end_matches('.');
            if let Ok(ip) = embedded.parse::<std::net::IpAddr>() {
                return classify_egress(&format!("http://{}/", ip));
            }
        }
    }
    // These always answer with loopback, so no embedded address to read.
    if h == "lvh.me" || h.ends_with(".lvh.me") || h == "vcap.me" || h.ends_with(".vcap.me") {
        return Some(EgressTarget::Loopback);
    }
    // Internal-only naming. `metadata.google.internal` resolves to 169.254.169.254, and the
    // non-pinned fetch path never re-resolves a redirect hop, so a name that only exists inside
    // a private zone has to be refused on its spelling alone.
    const INTERNAL_SUFFIXES: [&str; 6] = [
        ".internal",
        ".intranet",
        ".localdomain",
        ".home.arpa",
        ".lan",
        ".corp",
    ];
    if INTERNAL_SUFFIXES.iter().any(|suffix| h.ends_with(suffix)) {
        return Some(EgressTarget::Private);
    }
    Some(EgressTarget::Public)
}

/// True when an egress to `url` must be refused without an explicit local-address opt-in.
pub fn egress_is_blocked(url: &str) -> bool {
    !matches!(classify_egress(url), Some(EgressTarget::Public))
}

/// True for an address a public-internet request must never reach.
///
/// Shared by the URL classifier and the DNS resolver, so a hostname cannot be classified by one
/// spelling and dialled by another.
pub fn blocked_addr(ip: std::net::IpAddr) -> bool {
    !matches!(
        classify_egress(&format!("http://{}/", ip)),
        Some(EgressTarget::Public)
    )
}
