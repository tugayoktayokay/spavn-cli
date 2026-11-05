use std::time::Duration;

/// Agent tuned for long-lived streaming (SSE) responses.
///
/// `timeout_per_read` restarts on every transport read, so a slow model never
/// trips a total-body budget, while a dead connection still fails within 90s.
pub fn agent_stream() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(8)))
        .timeout_recv_response(Some(Duration::from_secs(90)))
        .timeout_per_read(Some(Duration::from_secs(45)))
        .http_status_as_error(false)
        .build();
    config.into()
}

/// Follows redirects by hand so every hop is classified before it is dialled.
///
/// ureq's automatic redirect following (default: 10 hops) validated nothing but the URL the
/// caller typed, so `evil.example` could answer `302 → http://169.254.169.254/…` and the
/// metadata service was reached on the second hop. Returns the final response.
fn call_with_manual_redirects(
    agent: &ureq::Agent,
    mut url: String,
    headers: &[(&str, &str)],
    follow: bool,
) -> Result<(u16, String), String> {
    const MAX_HOPS: usize = 5;
    let mut hops = 0usize;
    // A caller that deliberately targets loopback (local model discovery) still gets its
    // answer, but a redirect there is never followed: that is the pivot this guard exists for.
    let follow = follow && !crate::guard::egress_is_blocked(&url);
    loop {
        let mut request = agent.get(&url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request.call().map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        if follow && matches!(status, 301 | 302 | 303 | 307 | 308) {
            let Some(next) = location else {
                let text = response.body_mut().read_to_string().unwrap_or_default();
                return Ok((status, text));
            };
            hops += 1;
            if hops > MAX_HOPS {
                return Err(format!("redirect limit exceeded (>{MAX_HOPS})"));
            }
            if crate::guard::egress_is_blocked(&next) {
                return Err(format!(
                    "redirect to a non-public address refused: {}",
                    next
                ));
            }
            url = next;
            continue;
        }

        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        return Ok((status, text));
    }
}

/// Resolver that refuses to hand back an address the egress policy forbids.
///
/// Classifying the *hostname* is not enough: `127.0.0.1.nip.io` is a public-looking name that
/// resolves to loopback, and a rebinding attacker can answer differently on the second lookup.
/// Resolving here means the address that was checked is the address that gets dialled.
#[derive(Debug, Default)]
struct PublicOnlyResolver {
    inner: ureq::unversioned::resolver::DefaultResolver,
}

impl ureq::unversioned::resolver::Resolver for PublicOnlyResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let addrs = self.inner.resolve(uri, config, timeout)?;
        for addr in addrs.iter() {
            if crate::guard::blocked_addr(addr.ip()) {
                return Err(ureq::Error::Other(
                    format!(
                        "egress policy: {} resolves to a non-public address ({})",
                        uri.authority().map(|a| a.host()).unwrap_or("?"),
                        addr.ip()
                    )
                    .into(),
                ));
            }
        }
        Ok(addrs)
    }
}

fn agent_with_timeout(secs: u64) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(secs.max(1))))
        .timeout_connect(Some(Duration::from_secs(secs.max(1))))
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    config.into()
}

/// Same timeouts, but DNS is pinned to public addresses only.
fn agent_public_only(secs: u64) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(secs.max(1))))
        .timeout_connect(Some(Duration::from_secs(secs.max(1))))
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        PublicOnlyResolver::default(),
    )
}

pub fn post_json(
    url: &str,
    body: &str,
    headers: &[(&str, &str)],
    timeout_secs: u64,
) -> Result<(u16, String), String> {
    let agent = agent_with_timeout(timeout_secs);
    let mut request = agent.post(url).header("Content-Type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut response = request.send(body).map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok((status, text))
}

pub fn get_text(
    url: &str,
    headers: &[(&str, &str)],
    timeout_secs: u64,
) -> Result<(u16, String), String> {
    get_text_following(url, headers, timeout_secs, true)
}

/// Same as [`get_text`] with an explicit redirect policy.
pub fn get_text_following(
    url: &str,
    headers: &[(&str, &str)],
    timeout_secs: u64,
    follow: bool,
) -> Result<(u16, String), String> {
    let agent = agent_with_timeout(timeout_secs);
    call_with_manual_redirects(&agent, url.to_string(), headers, follow)
}

/// Unattended fetch: every hop and every resolved address must be public.
///
/// This is the variant `web` uses. It additionally pins DNS through
/// [`PublicOnlyResolver`], so a name that resolves to loopback (or rebinds between the check and
/// the dial) is refused at connect time rather than trusted on the strength of its spelling.
pub fn get_text_public(
    url: &str,
    headers: &[(&str, &str)],
    timeout_secs: u64,
) -> Result<(u16, String), String> {
    let agent = agent_public_only(timeout_secs);
    call_with_manual_redirects(&agent, url.to_string(), headers, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn an_unattended_fetch_never_reaches_loopback() {
        // The URL classifier alone was not enough: `127.0.0.1.nip.io` is a public-looking name,
        // so the resolver is the guard that actually holds. Pinning it here proves the refusal
        // happens at connect time, before any byte is spoken.
        let (port, _rx) = serve_once("HTTP/1.1 200 OK", r#"{"ok":true}"#);
        let url = format!("http://127.0.0.1:{}/v1/models", port);
        let err = get_text_public(&url, &[], 5).expect_err("loopback entry must be refused");
        assert!(err.to_lowercase().contains("egress policy"), "{err}");
    }

    #[test]
    fn a_redirect_hop_into_a_private_address_is_classified_as_blocked() {
        // `call_with_manual_redirects` asks `guard::egress_is_blocked` about every `Location`
        // before dialling it. These are the pivots that used to slip through ureq's automatic
        // follower, so each one must classify as blocked on its own.
        for target in [
            "http://169.254.169.254/latest/meta-data/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://0.0.0.0/",
            "http://localhost/",
            "http://something.local/",
        ] {
            assert!(
                crate::guard::egress_is_blocked(target),
                "redirect pivot not blocked: {target}"
            );
        }
        assert!(!crate::guard::egress_is_blocked(
            "https://api.anthropic.com/v1/messages"
        ));
    }

    /// Serves one canned HTTP/1.1 response on an ephemeral loopback port and returns
    /// `(port, captured_request)` where the captured request is filled in by the thread.
    fn serve_once(
        status_line: &'static str,
        body: &'static str,
    ) -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("local addr").port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
            let mut captured = String::new();
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        captured.push_str(&line);
                        if line == "\r\n" || line == "\n" {
                            break;
                        }
                    }
                }
            }
            let mut stream = stream;
            let _ = stream.write_all(
                format!(
                    "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status_line,
                    body.len(),
                    body
                )
                .as_bytes(),
            );
            let _ = stream.flush();
            let _ = tx.send(captured);
        });
        (port, rx)
    }

    #[test]
    fn post_json_returns_status_and_body_and_forwards_headers() {
        let (port, captured) = serve_once("HTTP/1.1 201 Created", r#"{"ok":true}"#);
        let url = format!("http://127.0.0.1:{}/v1/chat", port);
        let (status, body) = post_json(&url, r#"{"a":1}"#, &[("x-api-key", "sk-test")], 5)
            .expect("post_json should succeed against the loopback stub");
        assert_eq!(status, 201);
        assert_eq!(body, r#"{"ok":true}"#);

        let request = captured
            .recv_timeout(Duration::from_secs(5))
            .expect("captured request");
        let lower = request.to_lowercase();
        assert!(
            lower.contains("post /v1/chat"),
            "unexpected request line: {request}"
        );
        assert!(
            lower.contains("x-api-key: sk-test"),
            "custom header missing: {request}"
        );
        assert!(
            lower.contains("content-type: application/json"),
            "content-type missing: {request}"
        );
    }

    #[test]
    fn get_text_returns_error_status_without_failing() {
        let (port, _captured) = serve_once("HTTP/1.1 429 Too Many Requests", "rate limited");
        let url = format!("http://127.0.0.1:{}/models", port);
        let (status, body) =
            get_text(&url, &[], 5).expect("http_status_as_error(false) keeps error bodies");
        assert_eq!(status, 429);
        assert_eq!(body, "rate limited");
    }
}
