use crate::config::{home_dir, load_user_config};
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
                    reason: "uzak host üzerinde yazma".to_string(),
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
                    reason: format!("hassas kullanıcı dosyası ({}), onay gerekli", rel),
                    even_sovereign: false,
                };
            }
        }

        if under_any(&path, &self.workspace_roots) {
            return PathRisk::Allow;
        }

        if write {
            if is_temp_path(&path) {
                return PathRisk::Allow;
            }
            for sys in SYSTEM_WRITE_DENY {
                let root = PathBuf::from(sys);
                if path == root || path.starts_with(&root) {
                    return PathRisk::Deny {
                        reason: format!("sistem dizini ({}) yazma korumalı", sys),
                    };
                }
            }
            if path.parent().is_none() {
                return PathRisk::Deny {
                    reason: "kök dizin yazma korumalı".to_string(),
                };
            }
        }

        PathRisk::Ask {
            reason: "çalışma alanı dışında".to_string(),
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
    for segment in &tokenize(cmd) {
        if let Some(risk) = analyze_segment(segment, policy) {
            escalate(&mut worst, risk);
        }
        for idx in 0..segment.len() {
            let is_redirect = matches!(segment[idx].text.as_str(), ">" | ">>" | "&>");
            if is_redirect {
                if let Some(target) = segment.get(idx + 1) {
                    let mut out: Option<ShellRisk> = None;
                    match classify_target(policy, target) {
                        TargetKind::Critical => push(
                            &mut out,
                            ShellRisk::Catastrophic(format!(
                                "kritik dosyaya yönlendirme: {}",
                                target.text
                            )),
                        ),
                        TargetKind::Protected => push(
                            &mut out,
                            ShellRisk::Destructive(format!(
                                "korunan dosyaya yönlendirme: {}",
                                target.text
                            )),
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
    }
    if saw_sudo {
        push(
            &mut out,
            ShellRisk::Destructive("sudo ile yetki yükseltme".to_string()),
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
                            "kök/çalışma alanı özyinelemeli silme: {}",
                            target.text
                        )),
                    ),
                    TargetKind::Protected if recursive => push(
                        out,
                        ShellRisk::Catastrophic(format!(
                            "korunan dizin özyinelemeli silme: {}",
                            target.text
                        )),
                    ),
                    TargetKind::Critical => push(
                        out,
                        ShellRisk::Destructive(format!("kritik yol silme: {}", target.text)),
                    ),
                    TargetKind::Protected => push(
                        out,
                        ShellRisk::Destructive(format!("korunan yol silme: {}", target.text)),
                    ),
                    TargetKind::Dynamic if recursive => push(
                        out,
                        ShellRisk::Destructive(format!(
                            "değişkenli özyinelemeli silme: {}",
                            target.text
                        )),
                    ),
                    _ if recursive => push(
                        out,
                        ShellRisk::Destructive(format!("özyinelemeli silme: {}", target.text)),
                    ),
                    _ if force => push(
                        out,
                        ShellRisk::Destructive(format!("zorla silme: {}", target.text)),
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
                        ShellRisk::Catastrophic(format!("kritik dosya imhası: {}", target.text)),
                    ),
                    TargetKind::Protected => push(
                        out,
                        ShellRisk::Destructive(format!("korunan dosya imhası: {}", target.text)),
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
                        ShellRisk::Catastrophic(format!("kritik dosyaya yazma: {}", target.text)),
                    ),
                    TargetKind::Protected => push(
                        out,
                        ShellRisk::Destructive(format!("korunan dosyaya yazma: {}", target.text)),
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
                                "korunan dosyada yerinde düzenleme: {}",
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
                            ShellRisk::Catastrophic(format!("dd ile disk/dosya yazma: {}", target)),
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
                                "korunan yolda izin değişikliği: {}",
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
            if let Some(dest) = args.iter().rev().find(|a| !a.text.starts_with('-')) {
                if matches!(
                    classify_target(policy, dest),
                    TargetKind::Critical | TargetKind::Protected
                ) {
                    push(
                        out,
                        ShellRisk::Destructive(format!(
                            "korunan yola taşıma/kopyalama: {}",
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
                    ShellRisk::Destructive("docker compose/servis değişikliği".to_string()),
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
                    ShellRisk::Destructive("systemctl servis değişikliği".to_string()),
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
                    ShellRisk::Destructive("launchctl servis değişikliği".to_string()),
                );
            }
        }
        "kill" | "pkill" | "killall" => {
            push(out, ShellRisk::Destructive("süreç sonlandırma".to_string()));
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
                ShellRisk::Destructive("macOS ayar değişikliği".to_string()),
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
    if raw == "$HOME" {
        return home.to_path_buf();
    }
    if let Some(rest) = raw.strip_prefix("$HOME/") {
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
}
