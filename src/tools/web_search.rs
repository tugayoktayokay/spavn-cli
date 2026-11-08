//! Built-in web search: an ordered provider chain with automatic fallback.
//!
//! The default chain leads with public, keyless search APIs (Parallel's and Exa's
//! public MCP endpoints, Firecrawl's keyless search), then a configured SearXNG, then the HTML
//! scrapers (DuckDuckGo, Yahoo, Brave's page). Tavily and the Brave Search API join the front
//! when their key env var is set; `EXA_API_KEY` / `FIRECRAWL_API_KEY` switch those providers to
//! their authenticated APIs. Providers run in order; the first one that returns results answers,
//! and every earlier failure is reported alongside those results so a fallback never passes
//! silently for the preferred engine. `webSearchProviders` in the user config replaces the
//! default order, and an empty list turns built-in search off.

use serde_json::{json, Value};

/// Desktop Chrome identity the HTML endpoints serve their no-JS result pages to.
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";
const HTML_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const DEFAULT_LIMIT: usize = 8;
const MAX_LIMIT: usize = 20;
const SNIPPET_CHARS: usize = 320;
const TIMEOUT_SECS: u64 = 20;
const PARALLEL_MCP_URL: &str = "https://search.parallel.ai/mcp";
const EXA_MCP_URL: &str = "https://mcp.exa.ai/mcp?tools=web_search_exa";
const FIRECRAWL_SEARCH_URL: &str = "https://api.firecrawl.dev/v2/search";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    Tavily,
    Brave,
    Parallel,
    Exa,
    Firecrawl,
    Searxng,
    DuckDuckGo,
    Yahoo,
    BraveWeb,
}

/// Default chain: keyed APIs the user paid for, then the keyless APIs (Parallel, Exa,
/// Firecrawl), then a self-hosted SearXNG, then the HTML scrapers.
pub(crate) const DEFAULT_CHAIN: [Provider; 9] = [
    Provider::Tavily,
    Provider::Brave,
    Provider::Parallel,
    Provider::Exa,
    Provider::Firecrawl,
    Provider::Searxng,
    Provider::DuckDuckGo,
    Provider::Yahoo,
    Provider::BraveWeb,
];

impl Provider {
    pub(crate) fn id(self) -> &'static str {
        match self {
            Provider::Tavily => "tavily",
            Provider::Brave => "brave",
            Provider::Parallel => "parallel",
            Provider::Exa => "exa",
            Provider::Firecrawl => "firecrawl",
            Provider::Searxng => "searxng",
            Provider::DuckDuckGo => "duckduckgo",
            Provider::Yahoo => "yahoo",
            Provider::BraveWeb => "brave-web",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Provider::Tavily => "Tavily",
            Provider::Brave => "Brave Search API",
            Provider::Parallel => "Parallel",
            Provider::Exa => "Exa",
            Provider::Firecrawl => "Firecrawl",
            Provider::Searxng => "SearXNG",
            Provider::DuckDuckGo => "DuckDuckGo",
            Provider::Yahoo => "Yahoo",
            Provider::BraveWeb => "Brave Search (web)",
        }
    }

    pub(crate) fn from_id(raw: &str) -> Option<Provider> {
        let id = raw.trim().to_ascii_lowercase();
        DEFAULT_CHAIN
            .into_iter()
            .find(|p| p.id() == id)
            .or(match id.as_str() {
                "ddg" => Some(Provider::DuckDuckGo),
                "brave-api" => Some(Provider::Brave),
                _ => None,
            })
    }

    /// What has to be configured before this provider can run; `None` for keyless engines.
    fn requirement(self) -> Option<&'static str> {
        match self {
            Provider::Tavily => Some("TAVILY_API_KEY"),
            Provider::Brave => Some("BRAVE_API_KEY"),
            Provider::Searxng => Some("searxngUrl / SPAVN_SEARXNG_URL"),
            Provider::Parallel
            | Provider::Exa
            | Provider::Firecrawl
            | Provider::DuckDuckGo
            | Provider::Yahoo
            | Provider::BraveWeb => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recency {
    Day,
    Week,
    Month,
    Year,
}

impl Recency {
    pub(crate) fn parse(raw: &str) -> Option<Recency> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "day" | "d" | "24h" => Some(Recency::Day),
            "week" | "w" => Some(Recency::Week),
            "month" | "m" => Some(Recency::Month),
            "year" | "y" => Some(Recency::Year),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Recency::Day => "day",
            Recency::Week => "week",
            Recency::Month => "month",
            Recency::Year => "year",
        }
    }

    fn letter(self) -> &'static str {
        &self.word()[..1]
    }

    fn days(self) -> u64 {
        match self {
            Recency::Day => 1,
            Recency::Week => 7,
            Recency::Month => 31,
            Recency::Year => 366,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    pub date: Option<String>,
}

pub(crate) struct Query<'a> {
    pub text: &'a str,
    pub limit: usize,
    pub recency: Option<Recency>,
}

pub(crate) fn clamp_limit(requested: Option<u64>) -> usize {
    requested
        .map(|n| (n as usize).clamp(1, MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT)
}

/// Where the chain stands before any request is made.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub runnable: Vec<Provider>,
    /// Explicitly selected providers that cannot run, or unknown names, in config order.
    pub skipped: Vec<String>,
}

/// Resolves the run order. The default chain drops unconfigured providers silently; an explicit
/// list reports them, since the operator asked for those engines by name.
pub(crate) fn plan_chain(
    configured: Option<&[String]>,
    available: impl Fn(Provider) -> bool,
) -> Plan {
    let mut plan = Plan {
        runnable: Vec::new(),
        skipped: Vec::new(),
    };
    let Some(names) = configured else {
        plan.runnable = DEFAULT_CHAIN
            .into_iter()
            .filter(|p| available(*p))
            .collect();
        return plan;
    };
    for name in names {
        match Provider::from_id(name) {
            None => plan
                .skipped
                .push(format!("{}: unknown provider", name.trim())),
            Some(p) if plan.runnable.contains(&p) => {}
            Some(p) if available(p) => plan.runnable.push(p),
            Some(p) => plan.skipped.push(format!(
                "{}: not configured (set {})",
                p.id(),
                p.requirement().unwrap_or("?")
            )),
        }
    }
    plan
}

#[derive(Debug)]
pub(crate) struct ChainOutcome {
    pub provider: Provider,
    pub hits: Vec<Hit>,
    pub failures: Vec<String>,
}

/// Runs providers in order until one returns at least one hit. An empty result counts as a
/// failure so the next engine gets a chance; `Err` carries every failure in attempt order.
pub(crate) fn run_chain(
    providers: &[Provider],
    mut run: impl FnMut(Provider) -> Result<Vec<Hit>, String>,
) -> Result<ChainOutcome, Vec<String>> {
    let mut failures = Vec::new();
    for &provider in providers {
        match run(provider) {
            Ok(hits) if !hits.is_empty() => {
                return Ok(ChainOutcome {
                    provider,
                    hits,
                    failures,
                })
            }
            Ok(_) => failures.push(format!("{}: no results", provider.id())),
            Err(e) => failures.push(format!("{}: {}", provider.id(), e)),
        }
    }
    Err(failures)
}

fn env_key(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn searxng_base() -> Option<String> {
    crate::config::load_user_config()
        .searxng_url
        .or_else(|| std::env::var("SPAVN_SEARXNG_URL").ok())
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
}

fn is_available(provider: Provider) -> bool {
    match provider {
        Provider::Tavily => env_key("TAVILY_API_KEY").is_some(),
        Provider::Brave => env_key("BRAVE_API_KEY").is_some(),
        Provider::Searxng => searxng_base().is_some(),
        Provider::Parallel
        | Provider::Exa
        | Provider::Firecrawl
        | Provider::DuckDuckGo
        | Provider::Yahoo
        | Provider::BraveWeb => true,
    }
}

/// Entry point for `web op=search` when no MCP backend answered.
pub(crate) fn search(query: &Query) -> String {
    let configured = crate::config::load_user_config().web_search_providers;
    let plan = plan_chain(configured.as_deref(), is_available);
    if plan.runnable.is_empty() {
        let mut msg = String::from(
            "Web search error: built-in search is off (`webSearchProviders` selects no runnable \
provider).",
        );
        if !plan.skipped.is_empty() {
            msg.push_str(&format!(" {}.", plan.skipped.join("; ")));
        }
        msg.push_str(&format!(
            " Known providers: {}.",
            DEFAULT_CHAIN.map(Provider::id).join(", ")
        ));
        return msg;
    }
    match run_chain(&plan.runnable, |p| run_provider(p, query)) {
        Ok(mut outcome) => {
            let mut notes = plan.skipped;
            notes.append(&mut outcome.failures);
            format_hits(query.text, outcome.provider, &outcome.hits, &notes)
        }
        Err(mut failures) => {
            let mut all = plan.skipped;
            all.append(&mut failures);
            format!(
                "Web search failed — every provider failed: {}",
                all.join("; ")
            )
        }
    }
}

fn run_provider(provider: Provider, q: &Query) -> Result<Vec<Hit>, String> {
    let hits = match provider {
        Provider::Tavily => search_tavily(q),
        Provider::Brave => search_brave_api(q),
        Provider::Parallel => search_parallel(q),
        Provider::Exa => search_exa(q),
        Provider::Firecrawl => search_firecrawl(q),
        Provider::Searxng => search_searxng(q),
        Provider::DuckDuckGo => search_duckduckgo(q),
        Provider::Yahoo => search_yahoo(q),
        Provider::BraveWeb => search_brave_web(q),
    }?;
    Ok(dedup(hits, q.limit))
}

fn dedup(hits: Vec<Hit>, limit: usize) -> Vec<Hit> {
    let mut seen = std::collections::HashSet::new();
    hits.into_iter()
        .filter(|h| seen.insert(h.url.trim_end_matches('/').to_string()))
        .take(limit)
        .collect()
}

pub(crate) fn format_hits(
    query: &str,
    provider: Provider,
    hits: &[Hit],
    notes: &[String],
) -> String {
    let mut out = format!("Web search ({}): {}\n", provider.label(), query);
    for (i, hit) in hits.iter().enumerate() {
        out.push_str(&format!("\n{}. {}\n   {}\n", i + 1, hit.title, hit.url));
        if let Some(date) = &hit.date {
            out.push_str(&format!("   published: {}\n", date));
        }
        if let Some(snippet) = &hit.snippet {
            out.push_str(&format!("   {}\n", truncate_chars(snippet, SNIPPET_CHARS)));
        }
    }
    if !notes.is_empty() {
        out.push_str(&format!(
            "\n[fallback] {} answered after: {}\n",
            provider.id(),
            notes.join("; ")
        ));
    }
    out
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

fn http_error(status: u16, body: &str) -> String {
    let hint = match status {
        401 | 403 => " (authorization failed — check the API key)",
        429 => " (rate limited)",
        _ => "",
    };
    // Only short plain-text or JSON bodies explain anything; HTML/JS challenge pages are noise.
    let body = body.trim();
    let detail = if body.starts_with('{') || (body.len() < 400 && !body.contains('<')) {
        format!(" {}", truncate_chars(&html_text(body), 160))
    } else {
        String::new()
    };
    format!("HTTP {}{}{}", status, hint, detail)
}

// ---------------------------------------------------------------------------------------------
// Keyed APIs
// ---------------------------------------------------------------------------------------------

fn search_tavily(q: &Query) -> Result<Vec<Hit>, String> {
    let key = env_key("TAVILY_API_KEY").ok_or("TAVILY_API_KEY is not set")?;
    let mut body = json!({ "query": q.text, "max_results": q.limit, "search_depth": "basic" });
    if let Some(r) = q.recency {
        body["time_range"] = json!(r.word());
    }
    let auth = format!("Bearer {}", key);
    let (status, raw) = crate::http::post_json(
        "https://api.tavily.com/search",
        &body.to_string(),
        &[("Authorization", auth.as_str())],
        TIMEOUT_SECS,
    )?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &raw));
    }
    let parsed: Value = serde_json::from_str(&raw).map_err(|e| format!("bad JSON: {e}"))?;
    Ok(json_hits(
        parsed.get("results"),
        "content",
        &["published_date"],
    ))
}

fn search_brave_api(q: &Query) -> Result<Vec<Hit>, String> {
    let key = env_key("BRAVE_API_KEY").ok_or("BRAVE_API_KEY is not set")?;
    let mut url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        form_encode(q.text),
        q.limit
    );
    if let Some(r) = q.recency {
        url.push_str(&format!("&freshness=p{}", r.letter()));
    }
    let (status, raw) = crate::http::get_text_public(
        &url,
        &[
            ("Accept", "application/json"),
            ("X-Subscription-Token", key.as_str()),
        ],
        TIMEOUT_SECS,
    )?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &raw));
    }
    let parsed: Value = serde_json::from_str(&raw).map_err(|e| format!("bad JSON: {e}"))?;
    Ok(json_hits(
        parsed.get("web").and_then(|w| w.get("results")),
        "description",
        &["page_age", "age"],
    ))
}

fn search_exa(q: &Query) -> Result<Vec<Hit>, String> {
    match env_key("EXA_API_KEY") {
        Some(key) => search_exa_api(q, &key),
        None => search_exa_mcp(q),
    }
}

fn search_exa_api(q: &Query, key: &str) -> Result<Vec<Hit>, String> {
    let mut body = json!({
        "query": q.text,
        "numResults": q.limit,
        "type": "auto",
        "contents": { "text": { "maxCharacters": SNIPPET_CHARS } }
    });
    if let Some(r) = q.recency {
        body["startPublishedDate"] = json!(recency_start(r));
    }
    let (status, raw) = crate::http::post_json(
        "https://api.exa.ai/search",
        &body.to_string(),
        &[("x-api-key", key)],
        TIMEOUT_SECS,
    )?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &raw));
    }
    let parsed: Value = serde_json::from_str(&raw).map_err(|e| format!("bad JSON: {e}"))?;
    Ok(json_hits(parsed.get("results"), "text", &["publishedDate"]))
}

/// Exa's unauthenticated public MCP, used when no `EXA_API_KEY` is set.
fn search_exa_mcp(q: &Query) -> Result<Vec<Hit>, String> {
    let mut query = q.text.to_string();
    if let Some(r) = q.recency {
        query.push_str(&format!(" after:{}", recency_start(r)));
    }
    let body = json!({
        "jsonrpc": "2.0",
        "id": "1",
        "method": "tools/call",
        "params": { "name": "web_search_exa", "arguments": { "query": query, "numResults": q.limit } }
    });
    let raw = mcp_post(EXA_MCP_URL, &body, &[("x-exa-source", "spavn")])?;
    let (structured, text) = mcp_tool_result(&raw)?;
    if let Some(payload) = structured {
        let hits = json_hits(payload.get("results"), "text", &["publishedDate"]);
        if !hits.is_empty() {
            return Ok(hits);
        }
    }
    Ok(parse_exa_text(&text))
}

/// Exa MCP's text rendering: entries of `Title:` / `URL:` / `Published:` / `Author:` lines
/// followed by a `Highlights:` (or `Text:` / `Summary:`) body, separated by `---`.
pub(crate) fn parse_exa_text(text: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let body = text.strip_prefix("Title: ").unwrap_or(text);
    for entry in body.split("\nTitle: ") {
        let mut lines = entry.lines();
        let title = lines.next().unwrap_or("").trim().to_string();
        let mut url = None;
        let mut date = None;
        let mut snippet_lines = Vec::new();
        let mut in_body = false;
        for line in lines {
            if in_body {
                if line.trim() != "---" {
                    snippet_lines.push(line);
                }
                continue;
            }
            if let Some(v) = line.strip_prefix("URL: ") {
                url = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Published: ") {
                date = Some(v.trim().to_string()).filter(|d| d != "N/A" && !d.is_empty());
            } else if ["Highlights:", "Text:", "Summary:"]
                .iter()
                .any(|k| line.starts_with(k))
            {
                in_body = true;
                let rest = line.split_once(':').map(|(_, r)| r).unwrap_or("");
                snippet_lines.push(rest);
            }
        }
        let Some(url) = url.filter(|u| u.starts_with("http")) else {
            continue;
        };
        let snippet = snippet_lines
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        hits.push(Hit {
            title: if title.is_empty() { url.clone() } else { title },
            url,
            snippet: Some(snippet).filter(|s| !s.is_empty()),
            date,
        });
    }
    hits
}

/// Parallel's public MCP `web_search`, keyless.
fn search_parallel(q: &Query) -> Result<Vec<Hit>, String> {
    let mut query = q.text.to_string();
    if let Some(r) = q.recency {
        query.push_str(&format!(" after:{}", recency_start(r)));
    }
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "web_search",
            "arguments": { "objective": q.text, "search_queries": [query] }
        }
    });
    let raw = mcp_post(PARALLEL_MCP_URL, &body, &[])?;
    let (structured, text) = mcp_tool_result(&raw)?;
    let payload = match structured {
        Some(v) => v,
        None => serde_json::from_str(text.trim())
            .map_err(|_| "unexpected response shape (no JSON payload)".to_string())?,
    };
    Ok(parse_parallel(&payload))
}

/// `{"results":[{"url","title","publish_date","excerpts":[…]}]}`; excerpts become the snippet.
pub(crate) fn parse_parallel(payload: &Value) -> Vec<Hit> {
    let Some(items) = payload.get("results").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let url = item.get("url")?.as_str()?.trim().to_string();
            if !url.starts_with("http") {
                return None;
            }
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(&url)
                .to_string();
            let snippet = item
                .get("excerpts")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" … ")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|s| !s.is_empty());
            let date = item
                .get("publish_date")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|d| !d.is_empty());
            Some(Hit {
                title,
                url,
                snippet,
                date,
            })
        })
        .collect()
}

/// Firecrawl's SERP-backed search; keyless unless `FIRECRAWL_API_KEY` is set.
fn search_firecrawl(q: &Query) -> Result<Vec<Hit>, String> {
    let mut body = json!({ "query": q.text, "limit": q.limit, "sources": [{ "type": "web" }] });
    if let Some(r) = q.recency {
        body["tbs"] = json!(format!("qdr:{}", r.letter()));
    }
    let auth = env_key("FIRECRAWL_API_KEY").map(|k| format!("Bearer {}", k));
    let headers: Vec<(&str, &str)> = auth.iter().map(|a| ("Authorization", a.as_str())).collect();
    let (status, raw) = crate::http::post_json(
        FIRECRAWL_SEARCH_URL,
        &body.to_string(),
        &headers,
        TIMEOUT_SECS,
    )?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &raw));
    }
    let parsed: Value = serde_json::from_str(&raw).map_err(|e| format!("bad JSON: {e}"))?;
    if parsed.get("success").and_then(Value::as_bool) == Some(false) {
        let msg = parsed
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        return Err(truncate_chars(msg, 200));
    }
    let results = parsed
        .get("data")
        .and_then(|d| if d.is_array() { Some(d) } else { d.get("web") });
    Ok(json_hits(results, "description", &[]))
}

/// POSTs one JSON-RPC message to a streamable-HTTP MCP endpoint and returns the raw body.
fn mcp_post(url: &str, body: &Value, extra: &[(&str, &str)]) -> Result<String, String> {
    let ua = format!("spavn/{}", env!("CARGO_PKG_VERSION"));
    let mut headers = vec![
        ("Accept", "application/json, text/event-stream"),
        ("User-Agent", ua.as_str()),
    ];
    headers.extend_from_slice(extra);
    let (status, raw) = crate::http::post_json(url, &body.to_string(), &headers, TIMEOUT_SECS)?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &raw));
    }
    Ok(raw)
}

/// Structured payload and joined text of an MCP `tools/call` reply, sent as plain JSON or as
/// SSE `data:` lines. JSON-RPC errors and `isError` results become `Err`.
pub(crate) fn mcp_tool_result(raw: &str) -> Result<(Option<Value>, String), String> {
    let trimmed = raw.trim();
    let messages: Vec<Value> = if trimmed.starts_with('{') {
        serde_json::from_str(trimmed).ok().into_iter().collect()
    } else {
        trimmed
            .lines()
            .filter_map(|l| l.trim().strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str(d.trim()).ok())
            .collect()
    };
    for msg in messages {
        if let Some(err) = msg.get("error") {
            let detail = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Err(format!("MCP error: {}", truncate_chars(detail, 200)));
        }
        let Some(result) = msg.get("result") else {
            continue;
        };
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let detail = if text.trim().is_empty() {
                "tool returned an error"
            } else {
                text.trim()
            };
            return Err(truncate_chars(detail, 200));
        }
        let structured = result
            .get("structuredContent")
            .filter(|v| !v.is_null())
            .cloned();
        return Ok((structured, text));
    }
    Err("unexpected MCP response".to_string())
}

/// First day of a recency window, `YYYY-MM-DD` (UTC).
fn recency_start(r: Recency) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso_date(now.saturating_sub(r.days() * 86_400))
}

fn search_searxng(q: &Query) -> Result<Vec<Hit>, String> {
    let base = searxng_base().ok_or("searxngUrl is not set")?;
    let mut url = format!("{}/search?q={}&format=json", base, form_encode(q.text));
    if let Some(r) = q.recency {
        url.push_str(&format!("&time_range={}", r.word()));
    }
    // The operator named this instance, so a LAN or loopback SearXNG is allowed here.
    let (status, raw) =
        crate::http::get_text(&url, &[("Accept", "application/json")], TIMEOUT_SECS)?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &raw));
    }
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|_| "response is not JSON (enable `json` in the instance's search.formats)")?;
    Ok(json_hits(
        parsed.get("results"),
        "content",
        &["publishedDate"],
    ))
}

/// Shared shape of every JSON API here: an array of objects with `title`, `url`, a snippet
/// field and an optional date field.
fn json_hits(results: Option<&Value>, snippet_key: &str, date_keys: &[&str]) -> Vec<Hit> {
    let Some(items) = results.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let url = item.get("url")?.as_str()?.trim().to_string();
            if !url.starts_with("http") {
                return None;
            }
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .map(html_text)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| url.clone());
            let snippet = item
                .get(snippet_key)
                .and_then(Value::as_str)
                .map(html_text)
                .filter(|s| !s.is_empty());
            let date = date_keys
                .iter()
                .find_map(|k| item.get(*k).and_then(Value::as_str))
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty());
            Some(Hit {
                title,
                url,
                snippet,
                date,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Credential-free scrapers
// ---------------------------------------------------------------------------------------------

fn search_duckduckgo(q: &Query) -> Result<Vec<Hit>, String> {
    let mut form = format!("q={}&kl=us-en&b=", form_encode(q.text));
    if let Some(r) = q.recency {
        form.push_str(&format!("&df={}", r.letter()));
    }
    let (status, html) = crate::http::post_form_public(
        "https://html.duckduckgo.com/html/",
        &form,
        &[
            ("User-Agent", BROWSER_UA),
            ("Accept", HTML_ACCEPT),
            ("Accept-Language", "en-US,en;q=0.9"),
            ("Referer", "https://html.duckduckgo.com/"),
        ],
        TIMEOUT_SECS,
    )?;
    if html.contains("anomaly-modal") || html.contains("anomaly.js") {
        return Err("bot challenge (anomaly page)".to_string());
    }
    if !(200..300).contains(&status) {
        return Err(http_error(status, &html));
    }
    Ok(parse_duckduckgo(&html))
}

/// Result rows on DDG's no-JS page: `<a class="result__a" href="//duckduckgo.com/l/?uddg=…">`
/// for the title, and a sibling `result__snippet` element for the preview.
pub(crate) fn parse_duckduckgo(html: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for block in blocks_after(html, "class=\"result__a\"") {
        let Some(tag_start) = block.rfind_open_tag else {
            continue;
        };
        let anchor = &html[tag_start..];
        let Some((open, inner)) = element(anchor, "a") else {
            continue;
        };
        let Some(url) = attr(open, "href").and_then(|h| unwrap_ddg_url(&h)) else {
            continue;
        };
        if url.contains("duckduckgo.com/y.js") {
            continue; // sponsored placement
        }
        let title = html_text(inner);
        if title.is_empty() {
            continue;
        }
        let snippet = find_classed(block.body, "result__snippet")
            .map(|(_, inner)| html_text(inner))
            .filter(|s| !s.is_empty());
        hits.push(Hit {
            title,
            url,
            snippet,
            date: None,
        });
    }
    hits
}

fn unwrap_ddg_url(href: &str) -> Option<String> {
    let href = decode_entities(href);
    if let Some(pos) = href.find("uddg=") {
        let encoded = href[pos + 5..].split('&').next().unwrap_or("");
        let url = percent_decode(encoded);
        return url.starts_with("http").then_some(url);
    }
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("https://{}", rest));
    }
    href.starts_with("http").then_some(href)
}

fn search_yahoo(q: &Query) -> Result<Vec<Hit>, String> {
    let mut url = format!(
        "https://search.yahoo.com/search?p={}&n={}",
        form_encode(q.text),
        q.limit.max(10)
    );
    // Yahoo's time filter knows day/week/month only; a year window is left unfiltered.
    if let Some(r) = q.recency.filter(|r| *r != Recency::Year) {
        url.push_str(&format!("&btf={}", r.letter()));
    }
    let (status, html) = crate::http::get_text_public(
        &url,
        &[
            ("User-Agent", BROWSER_UA),
            ("Accept", HTML_ACCEPT),
            ("Accept-Language", "en-US,en;q=0.9"),
        ],
        TIMEOUT_SECS,
    )?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &html));
    }
    Ok(parse_yahoo(&html))
}

/// Organic rows carry the `algo-sr` class; the title link goes through
/// `r.search.yahoo.com/…/RU=<percent-encoded target>/…`, the heading is the `title` h3 and the
/// snippet the `compText` div. Yahoo's own properties (ads, shopping) are dropped.
pub(crate) fn parse_yahoo(html: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for block in blocks_after(html, "algo-sr") {
        let body = block.body;
        let Some(url) = yahoo_target(body) else {
            continue;
        };
        let Some(title) = find_classed(body, "title")
            .map(|(_, inner)| html_text(inner))
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        let snippet = find_classed(body, "compText")
            .map(|(_, inner)| html_text(inner))
            .filter(|s| !s.is_empty());
        hits.push(Hit {
            title,
            url,
            snippet,
            date: None,
        });
    }
    hits
}

fn yahoo_target(body: &str) -> Option<String> {
    let mut rest = body;
    while let Some(pos) = rest.find("/RU=") {
        let encoded = rest[pos + 4..].split('/').next().unwrap_or("");
        let url = percent_decode(encoded);
        let host = url.split('/').nth(2).unwrap_or("");
        if url.starts_with("http") && !host.ends_with("yahoo.com") {
            return Some(url);
        }
        rest = &rest[pos + 4..];
    }
    None
}

fn search_brave_web(q: &Query) -> Result<Vec<Hit>, String> {
    let mut url = format!(
        "https://search.brave.com/search?q={}&source=web",
        form_encode(q.text)
    );
    if let Some(r) = q.recency {
        url.push_str(&format!("&tf=p{}", r.letter()));
    }
    let (status, html) = crate::http::get_text_public(
        &url,
        &[
            ("User-Agent", BROWSER_UA),
            ("Accept", HTML_ACCEPT),
            ("Accept-Language", "en-US,en;q=0.9"),
        ],
        TIMEOUT_SECS,
    )?;
    if !(200..300).contains(&status) {
        return Err(http_error(status, &html));
    }
    let hits = parse_brave_web(&html);
    if hits.is_empty() && html.to_ascii_lowercase().contains("captcha") {
        return Err("bot challenge (captcha page)".to_string());
    }
    Ok(hits)
}

/// Organic rows on Brave's server-rendered page carry `data-type="web"`; the first absolute
/// link is the target, a `title` div holds the heading and a `content` div the snippet.
pub(crate) fn parse_brave_web(html: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for block in blocks_after(html, "data-type=\"web\"") {
        let body = block.body;
        let Some(url) = first_external_href(body, "search.brave.com") else {
            continue;
        };
        let title = find_classed(body, "title")
            .map(|(open, inner)| attr(open, "title").unwrap_or_else(|| html_text(inner)))
            .map(|t| decode_entities(&t).trim().to_string())
            .filter(|t| !t.is_empty());
        let Some(title) = title else {
            continue;
        };
        let snippet = find_classed(body, "content")
            .map(|(_, inner)| html_text(inner))
            .filter(|s| !s.is_empty());
        hits.push(Hit {
            title,
            url,
            snippet,
            date: None,
        });
    }
    hits
}

fn first_external_href(body: &str, own_host: &str) -> Option<String> {
    let mut rest = body;
    while let Some(pos) = rest.find("<a ") {
        let tail = &rest[pos..];
        let end = tail.find('>')?;
        if let Some(href) = attr(&tail[..=end], "href") {
            let href = decode_entities(&href);
            if href.starts_with("http") && !href.contains(own_host) {
                return Some(href);
            }
        }
        rest = &tail[end..];
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Minimal HTML helpers (no regex/HTML crates by design)
// ---------------------------------------------------------------------------------------------

struct Block<'a> {
    /// Text from the marker up to the next marker (or end of document).
    body: &'a str,
    /// Byte offset in the document of the `<` opening the tag that contains the marker.
    rfind_open_tag: Option<usize>,
}

/// Splits a document at every occurrence of `marker`.
fn blocks_after<'a>(html: &'a str, marker: &str) -> Vec<Block<'a>> {
    let starts: Vec<usize> = html.match_indices(marker).map(|(i, _)| i).collect();
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let end = starts.get(n + 1).copied().unwrap_or(html.len());
            Block {
                body: &html[start..end],
                rfind_open_tag: html[..start].rfind('<'),
            }
        })
        .collect()
}

/// Opening tag and inner HTML of the element starting at `html` (which begins with `<tag`).
/// Inner HTML runs to the first matching close tag; nesting of the same tag is not tracked,
/// which holds for the leaf elements parsed here.
fn element<'a>(html: &'a str, tag: &str) -> Option<(&'a str, &'a str)> {
    let open_end = html.find('>')?;
    let open = &html[..=open_end];
    let rest = &html[open_end + 1..];
    let close = format!("</{}", tag);
    let inner_end = rest.find(&close).unwrap_or(rest.len());
    Some((open, &rest[..inner_end]))
}

/// First element whose `class` attribute contains `class` as a whole token.
fn find_classed<'a>(html: &'a str, class: &str) -> Option<(&'a str, &'a str)> {
    let mut offset = 0;
    while let Some(pos) = html[offset..].find("class=\"") {
        let attr_start = offset + pos;
        let value_start = attr_start + 7;
        let value_end = value_start + html[value_start..].find('"')?;
        let has_token = html[value_start..value_end]
            .split_whitespace()
            .any(|t| t == class);
        if has_token {
            let tag_start = html[..attr_start].rfind('<')?;
            let tag_name: String = html[tag_start + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if !tag_name.is_empty() {
                return element(&html[tag_start..], &tag_name);
            }
        }
        offset = value_end;
    }
    None
}

/// Value of a double-quoted attribute inside one opening tag.
fn attr(open_tag: &str, name: &str) -> Option<String> {
    let needle = format!(" {}=\"", name);
    let start = open_tag.find(&needle)? + needle.len();
    let end = start + open_tag[start..].find('"')?;
    Some(open_tag[start..end].to_string())
}

/// Visible text of an HTML fragment: tags dropped, entities decoded, whitespace collapsed.
pub(crate) fn html_text(fragment: &str) -> String {
    const INLINE: [&str; 10] = [
        "a", "b", "strong", "em", "i", "span", "mark", "code", "u", "small",
    ];
    let mut text = String::with_capacity(fragment.len());
    let mut tag = String::new();
    let mut in_tag = false;
    for c in fragment.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect::<String>()
                    .to_ascii_lowercase();
                // Inline markup sits inside words (`<b>ureq</b>’s`); block tags separate text.
                if !INLINE.contains(&name.as_str()) {
                    text.push(' ');
                }
            }
            _ if in_tag => tag.push(c),
            _ => text.push(c),
        }
    }
    decode_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let Some(semi) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "rsquo" => Some('’'),
            "lsquo" => Some('‘'),
            "rdquo" => Some('”'),
            "ldquo" => Some('“'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "middot" => Some('·'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Percent-decoding for URL components; `+` stays literal because these are URLs, not forms.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `application/x-www-form-urlencoded` encoding of one value.
pub(crate) fn form_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// `YYYY-MM-DD` for a Unix timestamp (UTC), via Howard Hinnant's civil-from-days.
fn iso_date(unix_secs: u64) -> String {
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(url: &str) -> Hit {
        Hit {
            title: url.to_string(),
            url: url.to_string(),
            snippet: None,
            date: None,
        }
    }

    #[test]
    fn default_chain_runs_only_configured_providers_in_priority_order() {
        let plan = plan_chain(None, |p| {
            matches!(
                p,
                Provider::Parallel | Provider::Exa | Provider::DuckDuckGo | Provider::BraveWeb
            )
        });
        assert_eq!(
            plan.runnable,
            vec![
                Provider::Parallel,
                Provider::Exa,
                Provider::DuckDuckGo,
                Provider::BraveWeb
            ]
        );
        assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
    }

    #[test]
    fn explicit_chain_keeps_order_and_reports_what_cannot_run() {
        let names: Vec<String> = ["brave-web", "tavily", "bogus", "ddg", "brave-web"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let plan = plan_chain(Some(&names), |p| p != Provider::Tavily);
        assert_eq!(
            plan.runnable,
            vec![Provider::BraveWeb, Provider::DuckDuckGo]
        );
        assert_eq!(plan.skipped.len(), 2, "{:?}", plan.skipped);
        assert!(
            plan.skipped[0].contains("TAVILY_API_KEY"),
            "{:?}",
            plan.skipped
        );
        assert!(plan.skipped[1].contains("bogus"), "{:?}", plan.skipped);
        // An empty list is the documented off switch.
        assert!(plan_chain(Some(&[]), |_| true).runnable.is_empty());
    }

    #[test]
    fn mcp_replies_parse_from_json_or_sse_and_surface_errors() {
        let sse = "event: message\ndata: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]},\"jsonrpc\":\"2.0\",\"id\":\"1\"}\n\n";
        assert_eq!(mcp_tool_result(sse).unwrap(), (None, "hello".to_string()));
        let structured = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[],"structuredContent":{"results":[]}}}"#;
        assert_eq!(
            mcp_tool_result(structured).unwrap().0,
            Some(json!({ "results": [] }))
        );
        let tool_err =
            r#"{"result":{"isError":true,"content":[{"type":"text","text":"rate limit"}]}}"#;
        assert_eq!(mcp_tool_result(tool_err).unwrap_err(), "rate limit");
        let rpc_err = r#"{"error":{"code":-32602,"message":"bad args"}}"#;
        assert!(mcp_tool_result(rpc_err).unwrap_err().contains("bad args"));
        assert!(mcp_tool_result("<html>").is_err());
    }

    #[test]
    fn exa_text_entries_become_hits() {
        let text = "Title: freyavoice/Freya-TTS · Hugging Face\nURL: https://huggingface.co/freyavoice/Freya-TTS\nPublished: N/A\nAuthor: N/A\nHighlights:\n# FreyaTTS-small\n\nTurkish   TTS.\n\n---\n\nTitle: Second\nURL: https://b.example/\nPublished: 2026-05-01T00:00:00.000Z\nAuthor: x\nText: body";
        let hits = parse_exa_text(text);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].title, "freyavoice/Freya-TTS · Hugging Face");
        assert_eq!(hits[0].url, "https://huggingface.co/freyavoice/Freya-TTS");
        assert_eq!(hits[0].date, None);
        assert_eq!(
            hits[0].snippet.as_deref(),
            Some("# FreyaTTS-small Turkish TTS.")
        );
        assert_eq!(hits[1].date.as_deref(), Some("2026-05-01T00:00:00.000Z"));
        assert_eq!(hits[1].snippet.as_deref(), Some("body"));
    }

    #[test]
    fn parallel_results_join_excerpts() {
        let payload = json!({ "search_id": "s", "results": [
            { "url": "https://crates.io/crates/ureq/3.5.1", "title": "ureq - crates.io", "publish_date": null,
              "excerpts": ["ureq v3.5.1\n\nSimple, safe", "HTTP client"] },
            { "url": "ftp://nope", "title": "x" }
        ]});
        let hits = parse_parallel(&payload);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].snippet.as_deref(),
            Some("ureq v3.5.1 Simple, safe … HTTP client")
        );
        assert_eq!(hits[0].date, None);
    }

    #[test]
    fn chain_falls_through_errors_and_empty_results_and_keeps_every_failure() {
        let mut calls = Vec::new();
        let outcome = run_chain(
            &[Provider::Tavily, Provider::DuckDuckGo, Provider::BraveWeb],
            |p| {
                calls.push(p);
                match p {
                    Provider::Tavily => Err("HTTP 401".to_string()),
                    Provider::DuckDuckGo => Ok(Vec::new()),
                    _ => Ok(vec![hit("https://a.example/")]),
                }
            },
        )
        .unwrap();
        assert_eq!(outcome.provider, Provider::BraveWeb);
        assert_eq!(
            outcome.failures,
            vec!["tavily: HTTP 401", "duckduckgo: no results"]
        );
        assert_eq!(calls.len(), 3);
        let text = format_hits("q", outcome.provider, &outcome.hits, &outcome.failures);
        assert!(
            text.contains("[fallback] brave-web answered after: tavily: HTTP 401"),
            "{text}"
        );

        let all_failed = run_chain(&[Provider::DuckDuckGo], |_| Err("timeout".into())).unwrap_err();
        assert_eq!(all_failed, vec!["duckduckgo: timeout"]);
    }

    #[test]
    fn chain_stops_at_the_first_provider_with_results() {
        let mut calls = 0;
        let outcome = run_chain(&[Provider::DuckDuckGo, Provider::BraveWeb], |_| {
            calls += 1;
            Ok(vec![hit("https://first.example/")])
        })
        .unwrap();
        assert_eq!((outcome.provider, calls), (Provider::DuckDuckGo, 1));
        assert!(!format_hits("q", outcome.provider, &outcome.hits, &[]).contains("[fallback]"));
    }

    #[test]
    fn duckduckgo_rows_unwrap_redirects_and_skip_ads() {
        let html = r#"<div class="result results_links"><h2 class="result__title">
<a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fureq%3Fa%3D1%26b%3D2&amp;rut=abc"><b>ureq</b> &amp; Rust</a></h2>
<a class="result__snippet" href="x">Simple, <b>safe</b> HTTP client&#x27;s docs</a></div>
<div class="result result--ad"><a class="result__a" href="https://duckduckgo.com/y.js?ad=1">Ad</a></div>
<div class="result"><a class="result__a" href="https://crates.io/crates/ureq">crates.io: ureq</a></div>"#;
        let hits = parse_duckduckgo(html);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].url, "https://docs.rs/ureq?a=1&b=2");
        assert_eq!(hits[0].title, "ureq & Rust");
        assert_eq!(
            hits[0].snippet.as_deref(),
            Some("Simple, safe HTTP client's docs")
        );
        assert_eq!(hits[1].url, "https://crates.io/crates/ureq");
        assert_eq!(hits[1].snippet, None);
    }

    #[test]
    fn brave_rows_use_first_external_link_title_and_content() {
        let html = r#"<div class="snippet svelte-x" data-pos="0" data-type="web"><div class="result-content">
<a href="https://search.brave.com/goggles">x</a><a href="https://crates.io/crates/ureq" class="l1">
<div class="title search-snippet-title svelte-y" title="ureq - crates.io &amp; more">ureq</div></a>
<div class="generic-snippet"><div class="content desktop-default-regular">March 21, 2026 - <strong>Ureq</strong> is pure Rust.</div></div></div>
<div class="snippet" data-type="web"><a href="https://docs.rs/ureq"><div class="title">ureq - Rust</div></a></div>"#;
        let hits = parse_brave_web(html);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].url, "https://crates.io/crates/ureq");
        assert_eq!(hits[0].title, "ureq - crates.io & more");
        assert_eq!(
            hits[0].snippet.as_deref(),
            Some("March 21, 2026 - Ureq is pure Rust.")
        );
        assert_eq!(hits[1].title, "ureq - Rust");
    }

    #[test]
    fn yahoo_rows_decode_redirect_targets_and_drop_yahoo_properties() {
        let html = r#"<li><div class="dd fst algo algo-sr relsrch Sr"><div class="compTitle options-toggle">
<a class="d-ib" href="https://r.search.yahoo.com/_ylt=A;_ylu=Y/RV=2/RE=1/RO=10/RU=https%3a%2f%2flib.rs%2fcrates%2fureq%3fq%3da%2bb/RK=2/RS=x-">
<h3 class="title fc-2015C2-imp"><span class="d-b">Ureq &mdash; Rust HTTP client</span></h3></a></div>
<div class="compText aAbs"><p class="fc-dustygray"> <b>ureq</b>&rsquo;s API </p></div></div></li>
<li><div class="dd algo algo-sr Sr"><a href="https://r.search.yahoo.com/RU=https%3a%2f%2fshopping.yahoo.com%2fx/RK=2"><h3 class="title">Shop</h3></a></div></li>"#;
        let hits = parse_yahoo(html);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].url, "https://lib.rs/crates/ureq?q=a+b");
        assert_eq!(hits[0].title, "Ureq — Rust HTTP client");
        assert_eq!(hits[0].snippet.as_deref(), Some("ureq’s API"));
    }

    #[test]
    fn api_results_map_title_url_snippet_and_date() {
        let body: Value = serde_json::from_str(
            r#"{"web":{"results":[
                {"title":"A <strong>b</strong>","url":"https://a.example/","description":"d","page_age":"2026-01-02T00:00:00"},
                {"title":"no url"},
                {"title":"","url":"https://c.example/"}]}}"#,
        )
        .unwrap();
        let hits = json_hits(
            body.get("web").and_then(|w| w.get("results")),
            "description",
            &["page_age", "age"],
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "A b");
        assert_eq!(hits[0].date.as_deref(), Some("2026-01-02T00:00:00"));
        assert_eq!(hits[1].title, "https://c.example/");
    }

    #[test]
    fn encoding_helpers_round_trip() {
        assert_eq!(form_encode("çay & ürün?"), "%C3%A7ay+%26+%C3%BCr%C3%BCn%3F");
        assert_eq!(percent_decode("%C3%A7ay+%26%2"), "çay+&%2");
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_772_582_400), "2026-03-04");
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(500)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
    }
}
