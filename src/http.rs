use std::time::Duration;

/// Agent tuned for long-lived streaming (SSE) responses.
///
/// `timeout_per_read` restarts on every transport read, so a slow model never
/// trips a total-body budget, while a dead connection still fails within 90s.
pub fn agent_stream() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(300)))
        .timeout_per_read(Some(Duration::from_secs(90)))
        .http_status_as_error(false)
        .build();
    config.into()
}

fn agent_with_timeout(secs: u64) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(secs.max(1))))
        .timeout_connect(Some(Duration::from_secs(secs.max(1))))
        .http_status_as_error(false)
        .build();
    config.into()
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
    let agent = agent_with_timeout(timeout_secs);
    let mut request = agent.get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut response = request.call().map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    Ok((status, text))
}
