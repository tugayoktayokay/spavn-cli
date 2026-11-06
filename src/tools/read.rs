use super::*;

pub(crate) fn is_structural_outline_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let indent = line.len().saturating_sub(trimmed.len());
    if indent > 12
        || trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
    {
        return false;
    }
    const PREFIXES: &[&str] = &[
        "pub fn ",
        "pub async fn ",
        "pub const fn ",
        "pub unsafe fn ",
        "pub(crate) fn ",
        "pub(crate) async fn ",
        "pub(crate) const fn ",
        "pub(super) fn ",
        "fn ",
        "async fn ",
        "const fn ",
        "unsafe fn ",
        "pub struct ",
        "pub(crate) struct ",
        "struct ",
        "pub enum ",
        "pub(crate) enum ",
        "enum ",
        "pub trait ",
        "pub(crate) trait ",
        "trait ",
        "impl ",
        "impl<",
        "pub type ",
        "pub(crate) type ",
        "pub mod ",
        "mod ",
        "def ",
        "async def ",
        "class ",
        "func ",
        "export function ",
        "export async function ",
        "export default function ",
        "export class ",
        "export interface ",
        "export type ",
        "export const ",
        "function ",
        "interface ",
    ];
    if PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
        return true;
    }
    // React / TS / JS arrow functions & hooks (`const handleSend = async () => {`, `const Screen: FC =`)
    if (trimmed.starts_with("const ") || trimmed.starts_with("let "))
        && (trimmed.contains("=>")
            || trimmed.contains("= async")
            || trimmed.contains(": FC")
            || trimmed.contains("React.memo(")
            || trimmed.contains(" forwardRef("))
    {
        return true;
    }
    // C# / Java / Kotlin / Swift class & method declarations
    const OO_MODIFIERS: &[&str] = &[
        "public ",
        "private ",
        "protected ",
        "internal ",
        "override ",
        "@objc ",
    ];
    if OO_MODIFIERS.iter().any(|m| trimmed.starts_with(m))
        && (trimmed.contains('(') || trimmed.contains("class ") || trimmed.contains("interface "))
        && !trimmed.ends_with(';')
    {
        return true;
    }
    false
}

/// One `N-M` (or bare `N`) span from a `path:N-M,K-L` selector.
pub(crate) type LineSpan = (usize, usize);

/// Parses `N-M`, `N`, `L12-L40`, or a comma-separated list of those.
///
/// Returns `None` when `suffix` is not purely line ranges, so callers can fall back to
/// treating the whole argument as a path (e.g. `~/x:y.rs` on a remote host).
pub(crate) fn parse_line_spans(suffix: &str) -> Option<Vec<LineSpan>> {
    let mut spans = Vec::new();
    for part in suffix.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let part = part.strip_prefix('L').unwrap_or(part);
        if let Some((s_str, e_str)) = part.split_once('-') {
            let e_str = e_str.strip_prefix('L').unwrap_or(e_str);
            let s = s_str.trim().parse::<usize>().ok()?;
            let e = e_str.trim().parse::<usize>().ok()?;
            spans.push((s, e));
        } else {
            let n = part.parse::<usize>().ok()?;
            spans.push((n, n));
        }
    }
    if spans.is_empty() {
        None
    } else {
        Some(spans)
    }
}

/// Parses the `:ranges` tail of a read selector into merged, ordered spans.
///
/// A bare `N` expands to `N..N+120` (rg-style peek) *only* when it is the sole span; inside a
/// multi-range list it means exactly line `N`, so `path:5,90` does not silently swallow 120
/// lines twice.
pub(crate) fn expand_line_spans(spans: Vec<LineSpan>) -> Vec<LineSpan> {
    let single = spans.len() == 1;
    let mut out: Vec<LineSpan> = spans
        .into_iter()
        .map(|(s, e)| {
            if single && s == e {
                (s, s + 120)
            } else {
                (s.min(e), s.max(e))
            }
        })
        .collect();
    out.sort_by_key(|(s, _)| *s);
    let mut merged: Vec<LineSpan> = Vec::with_capacity(out.len());
    for (s, e) in out {
        match merged.last_mut() {
            // Overlapping or touching spans read as one continuous block.
            Some((_, prev_e)) if s <= prev_e.saturating_add(1) => {
                *prev_e = (*prev_e).max(e);
            }
            _ => merged.push((s, e)),
        }
    }
    merged
}

pub(crate) fn split_inline_raw_query(raw: &str) -> (&str, Option<String>) {
    if Path::new(raw).exists() || expand_tilde(raw).exists() {
        return (raw, None);
    }
    if let Some((base, query_part)) = raw.split_once("?q=") {
        if !base.is_empty() && !query_part.trim().is_empty() {
            return (base, Some(query_part.trim().to_string()));
        }
    }
    (raw, None)
}

pub(crate) fn normalize_json_pointer(q: &str) -> String {
    let trimmed = q.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == "/" {
        return String::new();
    }
    if trimmed.starts_with('/') {
        return trimmed.to_string();
    }
    let stripped = trimmed.trim_start_matches('.');
    format!(
        "/{}",
        stripped
            .replace('[', "/")
            .replace(']', "")
            .split('.')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    )
}

/// Splits `path:-N` tail selector (last `N` lines of a file) when the literal path does not exist.
pub(crate) fn split_inline_tail_lines(raw: &str) -> Option<(&str, usize)> {
    if Path::new(raw).exists() || expand_tilde(raw).exists() {
        return None;
    }
    let (base, suffix) = raw.rsplit_once(":-")?;
    if base.is_empty() || base.ends_with(':') {
        return None;
    }
    let n = suffix.trim().parse::<usize>().ok()?;
    if n == 0 {
        return None;
    }
    Some((base, n.min(250)))
}

/// Splits `path:ranges` into the path and the merged, ordered spans.
///
/// `read` accepts the shape people already type at `sed -n`: several disjoint regions of one
/// file in a single call (`src/x.rs:1-12,640-665`). Without it every multi-region look costs
/// an extra `bash sed` round trip — measured at 53% of all file inspection.
pub(crate) fn split_inline_line_spans(raw: &str) -> (&str, Option<Vec<LineSpan>>) {
    if Path::new(raw).exists() || expand_tilde(raw).exists() {
        return (raw, None);
    }
    if let Some((base, suffix)) = raw.rsplit_once(':') {
        let is_bare_ssh_host = !base.contains(':') && split_remote_prefix(raw).is_some();
        if !base.is_empty() && !base.ends_with(':') && !is_bare_ssh_host {
            if let Some(spans) = parse_line_spans(suffix) {
                return (base, Some(expand_line_spans(spans)));
            }
            // rg-style `path:line:col`: the base itself ends with `:line`.
            if let Some((inner_base, line_str)) = base.rsplit_once(':') {
                if !inner_base.is_empty() && !inner_base.ends_with(':') {
                    if let Ok(line_no) = line_str.parse::<usize>() {
                        return (inner_base, Some(vec![(line_no, line_no + 120)]));
                    }
                }
            }
        }
    }
    (raw, None)
}

/// Renders several disjoint regions of `content` in one block, the way `sed -n 'a,b;c,d'`
/// does — but with absolute line numbers and an explicit gap marker per jump.
pub(crate) fn format_spans(content: &str, spans: &[LineSpan]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    if total == 0 {
        return "[EOF STOP: File is empty (0 lines).]\n".to_string();
    }
    let budget: usize = 250;
    let mut out = String::new();
    let mut printed = 0usize;
    let mut shown_any = false;
    for &(s, e) in spans {
        let s = s.max(1);
        if s > total {
            out.push_str(&format!(
                "... [requested {}-{} is past EOF ({} lines)] ...\n",
                s, e, total
            ));
            continue;
        }
        let e = e.min(total);
        let take = (e - s + 1).min(budget.saturating_sub(printed).max(1));
        if shown_any {
            out.push_str(&format!("... [jump to line {}] ...\n", s));
        }
        for (i, line) in lines[(s - 1)..(s - 1 + take)].iter().enumerate() {
            out.push_str(&format!("{:>4}: {}\n", s + i, line));
        }
        printed += take;
        shown_any = true;
        if take < e - s + 1 {
            out.push_str(&format!(
                "... [{} lines elided; {} line budget reached] ...\n",
                (e - s + 1) - take,
                budget
            ));
        }
    }
    if !shown_any {
        return format!(
            "(File has {} lines; none of the requested ranges exist)\n",
            total
        );
    }
    out.push_str(&format!("... [EOF: line {}/{}]\n", total, total));
    out
}

pub(crate) fn format_lines(
    content: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let s = start_line.unwrap_or(1).max(1);
    if s > total {
        return format!(
            "[EOF STOP: File only has {} lines total (1..{}). Requested start_line={} is past EOF — STOP calling read on this file!]",
            total, total, s
        );
    }
    let default_end = if start_line.is_some() {
        (s + 199).min(total)
    } else {
        total
    };
    let e = end_line.unwrap_or(default_end).min(s + 249).min(total);
    if s > e {
        return format!(
            "(File has {} lines; requested range {}..{} is empty)",
            total, s, e
        );
    }

    let slice = &lines[(s - 1)..e];
    if total > 240 && start_line.is_none() && end_line.is_none() {
        let mut out = String::new();
        for (i, line) in lines.iter().take(100).enumerate() {
            out.push_str(&format!("{:>4}: {}\n", i + 1, line));
        }
        let mid_end = total.saturating_sub(60);
        out.push_str(&format!(
            "\n... [SPAVN Structural Outline of lines 101..{} ({} total lines) — pass start_line/end_line or path:N-M to read exact function body] ...\n",
            mid_end, total
        ));
        let mut collected_syms: Vec<(usize, &str)> = Vec::new();
        for (idx, line) in lines.iter().enumerate().take(mid_end).skip(100) {
            if is_structural_outline_line(line) {
                collected_syms.push((idx + 1, line));
            }
        }
        let max_syms = 120usize;
        if collected_syms.len() <= max_syms {
            for (line_no, line) in collected_syms {
                out.push_str(&format!("{:>4}: {}\n", line_no, line));
            }
        } else {
            let total_syms = collected_syms.len();
            for (i, (line_no, line)) in collected_syms.iter().enumerate() {
                if i < 90 || i + 25 >= total_syms {
                    out.push_str(&format!("{:>4}: {}\n", line_no, line));
                } else if i == 90 {
                    out.push_str(&format!(
                        "  .. [{} middle symbols elided] ..\n",
                        total_syms.saturating_sub(115)
                    ));
                }
            }
        }
        out.push_str("... [End of Structural Outline; showing final 60 lines] ...\n\n");
        for (i, line) in lines.iter().enumerate().skip(mid_end) {
            out.push_str(&format!("{:>4}: {}\n", i + 1, line));
        }
        out.push_str(&format!("... [EOF: line {}/{}]\n", total, total));
        return out;
    }

    let mut out = String::new();
    for (i, line) in slice.iter().enumerate() {
        out.push_str(&format!("{:>4}: {}\n", s + i, line));
    }
    if e == total {
        out.push_str(&format!("... [EOF: line {}/{}]\n", total, total));
    } else if total > e + 20 {
        let tail_start = total.saturating_sub(15).max(e);
        out.push_str(&format!(
            "... [Showing lines {}..{} of {} total; Structural Outline of remaining lines {}..{} below — jump directly with start_line/end_line] ...\n",
            s,
            e,
            total,
            e + 1,
            tail_start
        ));
        let mut shown_syms = 0usize;
        for (idx, line) in lines.iter().enumerate().take(tail_start).skip(e) {
            if is_structural_outline_line(line) {
                out.push_str(&format!("{:>4}: {}\n", idx + 1, line));
                shown_syms += 1;
                if shown_syms >= 45 {
                    out.push_str("  .. [remaining symbols elided] ..\n");
                    break;
                }
            }
        }
        if s == 1 {
            out.push_str(&format!(
                "... [Final lines {}..{}] ...\n",
                tail_start + 1,
                total
            ));
            for (idx, line) in lines.iter().enumerate().skip(tail_start) {
                out.push_str(&format!("{:>4}: {}\n", idx + 1, line));
            }
        }
        out.push_str(&format!("... [EOF: line {}/{}]\n", total, total));
    } else {
        out.push_str(&format!(
            "... [Showing lines {}..{} of {} total]\n",
            s, e, total
        ));
    }
    out
}

pub(crate) fn get_str_alias<'a>(args: &'a Value, keys: &[&str]) -> &'a str {
    for k in keys {
        if let Some(s) = args.get(*k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s;
            }
        }
    }
    ""
}

/// Guards context against accidental reads of compiled JS bundles (`dist/app.bundle.js`),
/// source maps, or minified artifacts that blow up the context window and derail investigation.
pub(crate) fn check_compiled_artifact_shield(raw_path: &str, content: &str) -> Option<String> {
    let norm = raw_path.replace('\\', "/");
    let lower = norm.to_ascii_lowercase();
    let is_bundle_path = lower.ends_with(".bundle")
        || lower.ends_with(".bundle.js")
        || lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.ends_with(".js.map")
        || lower.ends_with(".hbc")
        || ((lower.contains("/dist/") || lower.contains("/build/"))
            && (lower.ends_with(".js") || lower.ends_with(".cjs") || lower.ends_with(".mjs")));
    let total_lines = content.lines().count();
    let max_line_chars = content.lines().take(50).map(|l| l.len()).max().unwrap_or(0);
    let is_minified_giant =
        content.len() > 200_000 && (total_lines > 8_000 || max_line_chars > 2_500);
    if is_bundle_path && (content.len() > 60_000 || total_lines > 2_000 || max_line_chars > 1_500)
        || is_minified_giant
    {
        let kb = content.len() / 1024;
        return Some(format!(
            "[SPAVN Artifact Shield: `{}` is a compiled/minified artifact ({} lines, {} KB). Reading compiled bundles wastes context and hides the real bug — inspect the source files under `src/` instead.]",
            raw_path, total_lines, kb
        ));
    }
    None
}

pub(crate) fn eval_json_pointer_view(content: &str, pointer: &str) -> Result<String, String> {
    let parsed: Value =
        serde_json::from_str(content).map_err(|e| format!("JSON parse error: {}", e))?;
    if pointer.is_empty() {
        return serde_json::to_string_pretty(&parsed).map_err(|e| e.to_string());
    }
    if let Some(target) = parsed.pointer(pointer) {
        return serde_json::to_string_pretty(target).map_err(|e| e.to_string());
    }
    let parent_ptr = pointer.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    let available = parsed
        .pointer(parent_ptr)
        .and_then(|v| v.as_object())
        .map(|obj| {
            let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).take(25).collect();
            keys.sort_unstable();
            format!(
                " (available keys at '{}': {})",
                if parent_ptr.is_empty() {
                    "/"
                } else {
                    parent_ptr
                },
                keys.join(", ")
            )
        })
        .unwrap_or_default();
    Err(format!("JSON pointer '{}' not found{}", pointer, available))
}

pub(crate) fn exec_read(ctx: &mut ToolContext, args: &Value) -> String {
    let raw_input_path = get_str_alias(args, &["path", "file_path", "file", "filepath"]);
    let (path_no_query, inline_raw_q) = split_inline_raw_query(raw_input_path);
    let explicit_raw_q = get_str_alias(args, &["q", "json_pointer", "pointer"]);
    let raw_query = if !explicit_raw_q.is_empty() {
        Some(explicit_raw_q.trim().to_string())
    } else {
        inline_raw_q
    };
    let json_query = raw_query.as_deref().map(normalize_json_pointer);
    let (raw_path, tail_count, inline_spans) =
        if let Some((base, n)) = split_inline_tail_lines(path_no_query) {
            (base, Some(n), None)
        } else {
            let (base, spans) = split_inline_line_spans(path_no_query);
            (base, None, spans)
        };
    let (inline_start, inline_end) = match inline_spans.as_deref() {
        // A single span stays on the plain `start_line`/`end_line` path so the existing
        // head+outline heuristics (and the re-read dedup key) keep working unchanged.
        Some([(s, e)]) => (Some(*s), Some(*e)),
        _ => (None, None),
    };
    if let Err(blocked) = ctx.enforce_path(raw_path, false, "read") {
        return blocked;
    }
    let start_line = args
        .get("start_line")
        .or_else(|| args.get("offset"))
        .or_else(|| args.get("start"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .or(inline_start);
    let limit_lines = args
        .get("limit")
        .or_else(|| args.get("num_lines"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let end_line = args
        .get("end_line")
        .or_else(|| args.get("end"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .or_else(|| limit_lines.map(|lim| start_line.unwrap_or(1).max(1) + lim.saturating_sub(1)))
        .or(inline_end);
    if let Some((ssh_host, remote_path)) = split_remote_prefix(raw_path) {
        let quoted = shell_quote_remote_path(remote_path);
        let remote_cmd = format!(
            "if [ -d {q} ]; then ls -1p {q} | head -n 200; else cat -- {q}; fi",
            q = quoted
        );
        let out = Command::new("ssh")
            .args(["-o", "ConnectTimeout=5", &ssh_host, &remote_cmd])
            .output();
        return match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                let (eff_s, eff_e) = if let Some(n) = tail_count {
                    let total = text.lines().count().max(1);
                    (
                        Some(total.saturating_sub(n.saturating_sub(1)).max(1)),
                        Some(total),
                    )
                } else {
                    (start_line, end_line)
                };
                format_lines(&text, eff_s, eff_e)
            }
            Ok(o) => format!(
                "Remote read error ({}): {}",
                ssh_host,
                String::from_utf8_lossy(&o.stderr)
            ),
            Err(e) => format!("SSH error: {}", e),
        };
    }

    let path = ctx.resolve_local_path(raw_path);
    if path.is_dir() {
        let mut entries = Vec::new();
        if let Ok(rd) = fs::read_dir(&path) {
            for entry in rd.flatten().take(200) {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                entries.push(if is_dir { format!("{}/", name) } else { name });
            }
        }
        entries.sort();
        return entries.join("\n");
    }

    let key = path.to_string_lossy().to_string();
    if let Some(ext) = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
    {
        let mime_opt = match ext.as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "webp" => Some("image/webp"),
            "gif" => Some("image/gif"),
            _ => None,
        };
        if let Some(mime) = mime_opt {
            return match fs::read(&path) {
                Ok(bytes) => {
                    if bytes.len() > 15 * 1024 * 1024 {
                        format!(
                            "Read error ({}): image exceeds 15MB limit ({} bytes)",
                            path.display(),
                            bytes.len()
                        )
                    } else {
                        let kb = (bytes.len() as f64 / 1024.0).max(0.1);
                        let fname = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| raw_path.to_string());
                        let b64 = crate::router::base64_encode_bytes(&bytes);
                        let drag_img = crate::router::DraggedImage {
                            raw_span: raw_path.to_string(),
                            path: key.clone(),
                            filename: fname.clone(),
                            mime,
                            size_bytes: bytes.len(),
                            base64: b64,
                        };
                        crate::hud::render_inline_image_preview(&drag_img);
                        crate::router::register_read_image(&key, drag_img);
                        format!(
                            "[SPAVN Vision Image Loaded: {} ({}, {:.1} KB) — multimodal image attached to your visual context]",
                            key, mime, kb
                        )
                    }
                }
                Err(e) => {
                    let hint = suggest_similar_path(raw_path);
                    format!("Read error ({}): {}{}", path.display(), e, hint)
                }
            };
        }
        if matches!(ext.as_str(), "sqlite" | "sqlite3" | "db") {
            let q = raw_query
                .as_deref()
                .unwrap_or("")
                .trim()
                .trim_start_matches('/');
            let sql_cmd = if q.is_empty() {
                "SELECT name, sql FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name;".to_string()
            } else if q.to_ascii_uppercase().starts_with("SELECT ") || q.starts_with('.') {
                q.to_string()
            } else {
                let safe_tbl: String = q
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                format!(
                    "PRAGMA table_info({t}); SELECT * FROM {t} LIMIT 40;",
                    t = safe_tbl
                )
            };
            if let Ok(out) = Command::new("sqlite3")
                .args(["-header", "-box", path.to_string_lossy().as_ref(), &sql_cmd])
                .output()
            {
                if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    return format_lines(&text, start_line, end_line);
                }
            }
        }
        if ext == "pdf" {
            let py_extract = "import sys\ntry:\n    import pypdf\n    r = pypdf.PdfReader(sys.argv[1])\n    for i, p in enumerate(r.pages):\n        print(f'--- Page {i+1} ---')\n        print(p.extract_text() or '')\nexcept Exception as e:\n    sys.exit(1)\n";
            if let Ok(out) = Command::new("pdftotext")
                .args([path.to_string_lossy().as_ref(), "-"])
                .output()
                .and_then(|o| {
                    if o.status.success() {
                        Ok(o)
                    } else {
                        Err(std::io::Error::other("pdftotext failed"))
                    }
                })
                .or_else(|_| {
                    Command::new("python3")
                        .args(["-c", py_extract, path.to_string_lossy().as_ref()])
                        .output()
                })
            {
                if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    return format_lines(&text, start_line, end_line);
                }
            }
        }
    }
    if json_query.is_none() && tail_count.is_none() {
        if let Ok(mtime) = fs::metadata(&path).and_then(|m| m.modified()) {
            if let Some(&(prev_mtime, prev_s, prev_e, prev_step)) = ctx.read_ranges.get(&key) {
                if prev_mtime == mtime
                    && prev_s == start_line
                    && prev_e == end_line
                    && ctx.tool_step_counter.saturating_sub(prev_step) <= 5
                {
                    return format!(
                        "[SPAVN Read Dedup: {} is unchanged on disk since your read {} step(s) ago. Its exact content is already in your context — refer to that earlier read result instead of re-reading.]",
                        raw_path,
                        ctx.tool_step_counter.saturating_sub(prev_step).max(1)
                    );
                }
            }
        }
    }

    match fs::read_to_string(&path) {
        Ok(content) => {
            if let Some(ref q) = json_query {
                ctx.record_file_mtime(raw_path);
                return match eval_json_pointer_view(&content, q) {
                    Ok(rendered) => format_lines(&rendered, start_line, end_line),
                    Err(err_msg) => format!("Read JSON query error ({}): {}", raw_path, err_msg),
                };
            }
            if let Some(shield_msg) = check_compiled_artifact_shield(raw_path, &content) {
                return shield_msg;
            }
            ctx.record_file_mtime(raw_path);
            let (eff_s, eff_e) = if let Some(n) = tail_count {
                let total = content.lines().count().max(1);
                (
                    Some(total.saturating_sub(n.saturating_sub(1)).max(1)),
                    Some(total),
                )
            } else {
                (start_line, end_line)
            };
            if let Ok(mtime) = fs::metadata(&path).and_then(|m| m.modified()) {
                ctx.read_ranges
                    .insert(key, (mtime, eff_s, eff_e, ctx.tool_step_counter));
            }
            match inline_spans.as_deref() {
                Some(spans) if spans.len() > 1 => format_spans(&content, spans),
                _ => format_lines(&content, eff_s, eff_e),
            }
        }
        Err(e) => {
            let hint = suggest_similar_path(raw_path);
            format!("Read error ({}): {}{}", path.display(), e, hint)
        }
    }
}
