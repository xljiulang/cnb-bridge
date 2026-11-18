use super::*;

const UPSTREAM_ERROR_BODY_LIMIT: usize = 64 * 1024;

pub(super) enum UpstreamBodyError {
    TooLarge,
    Network(reqwest::Error),
}

pub(super) async fn read_upstream_body_bounded(
    response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, UpstreamBodyError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(UpstreamBodyError::TooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(UpstreamBodyError::Network)?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(UpstreamBodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

pub(super) async fn read_upstream_body_prefix(
    response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, reqwest::Error> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = limit.saturating_sub(body.len());
        let take = chunk.len().min(remaining);
        body.extend_from_slice(&chunk[..take]);
        if body.len() == limit {
            break;
        }
    }
    Ok(Bytes::from(body))
}

pub(super) enum ActivationWaitError {
    Timeout,
    Join(tokio::task::JoinError),
}

pub(super) async fn await_activation_task(
    mut task: tokio::task::JoinHandle<Result<u16, crate::bridge::BridgeError>>,
    timeout: Duration,
) -> Result<Result<u16, crate::bridge::BridgeError>, ActivationWaitError> {
    match tokio::time::timeout(timeout, &mut task).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => Err(ActivationWaitError::Join(error)),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(ActivationWaitError::Timeout)
        }
    }
}

pub(super) async fn ensure_upstream_port(state: &GatewayState) -> Result<u16, Response<Body>> {
    if let Some(port) = state.upstream_override {
        return Ok(port);
    }
    let bridge = Arc::clone(&state.bridge);
    let activation = tokio::spawn(async move { bridge.ensure_active().await });
    match await_activation_task(activation, state.activation_timeout).await {
        Ok(Ok(port)) => Ok(port),
        Ok(Err(error)) => Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "workspace_unavailable",
            error.to_string(),
        )),
        Err(ActivationWaitError::Join(error)) => Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "workspace_unavailable",
            format!("workspace activation task failed: {error}"),
        )),
        Err(ActivationWaitError::Timeout) => Err(error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "workspace_activation_timeout",
            "workspace activation timed out",
        )),
    }
}

pub(super) struct UpstreamExchange {
    pub(super) response: reqwest::Response,
    pub(super) request_headers: HeaderMap,
    pub(super) request_body: Bytes,
    pub(super) request_target: String,
    pub(super) started: Instant,
    pub(super) started_at: std::time::SystemTime,
}

pub(super) async fn send_chat_request(
    state: &GatewayState,
    headers: &HeaderMap,
    uri: &Uri,
    body: Bytes,
) -> Result<UpstreamExchange, Response<Body>> {
    let mut last_error = None;
    for attempt in 0..2 {
        let port = ensure_upstream_port(state).await?;
        let mut request_target = "/chat/completions".to_string();
        if let Some(query) = uri.query() {
            request_target.push('?');
            request_target.push_str(query);
        }
        let url = format!("http://127.0.0.1:{port}{request_target}");
        let request_headers = forwarded_request_headers(headers, port, body.len());
        let started = Instant::now();
        let started_at = std::time::SystemTime::now();
        let request = state
            .client
            .post(url)
            .headers(request_headers.clone())
            .body(body.clone());
        match request.send().await {
            Ok(response) => {
                return Ok(UpstreamExchange {
                    response,
                    request_headers,
                    request_body: body,
                    request_target,
                    started,
                    started_at,
                })
            }
            Err(error) => {
                let detail = crate::net::format_error_chain(&error);
                last_error = Some(detail.clone());
                if attempt == 0 && state.upstream_override.is_none() {
                    state
                        .bridge
                        .invalidate_tunnel(
                            port,
                            format!("本地 Gateway 到 Agent 连接失败，正在重建隧道: {detail}"),
                        )
                        .await;
                }
            }
        }
    }
    Err(error_response(
        StatusCode::BAD_GATEWAY,
        "upstream_network_error",
        last_error.unwrap_or_else(|| "could not reach upstream".to_string()),
    ))
}

pub(super) fn forwarded_request_headers(
    headers: &HeaderMap,
    port: u16,
    content_length: usize,
) -> HeaderMap {
    let mut forwarded = HeaderMap::new();
    for (name, value) in headers.iter() {
        if name != header::ACCEPT_ENCODING && !is_request_hop_header(name.as_str()) {
            forwarded.append(name, value.clone());
        }
    }
    if let Ok(value) = HeaderValue::from_str(&content_length.to_string()) {
        forwarded.insert(header::CONTENT_LENGTH, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("127.0.0.1:{port}")) {
        forwarded.insert(header::HOST, value);
    }
    forwarded.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    forwarded
}

pub(super) async fn upstream_error_response(
    state: &GatewayState,
    exchange: UpstreamExchange,
    mut trace: Option<HttpTraceSession>,
    request_id: &str,
    gate_report: &GateReport,
) -> Response<Body> {
    let status = status_from_reqwest(exchange.response.status());
    let retry_after = exchange
        .response
        .headers()
        .get(header::RETRY_AFTER)
        .cloned();
    let body = match tokio::time::timeout(
        state.non_stream_timeout,
        read_upstream_body_prefix(exchange.response, UPSTREAM_ERROR_BODY_LIMIT),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(error)) => {
            return error_response_with_id(
                StatusCode::BAD_GATEWAY,
                "upstream_network_error",
                format!("failed to read upstream error response: {error}"),
                request_id.to_string(),
            )
        }
        Err(_) => {
            return error_response_with_id(
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                "upstream error response timed out",
                request_id.to_string(),
            )
        }
    };
    if let Some(trace) = trace.as_mut() {
        trace.write_response_chunk(&body).ok();
        let elapsed = trace.elapsed();
        trace.finish(status.as_u16(), "failed", elapsed).ok();
    }
    upstream_error_from_text(
        status,
        retry_after,
        String::from_utf8_lossy(&body).into_owned(),
        request_id,
        gate_report,
        &state.logger,
    )
}

pub(super) fn upstream_error_from_text(
    status: StatusCode,
    retry_after: Option<HeaderValue>,
    text: String,
    request_id: &str,
    gate_report: &GateReport,
    logger: &Logger,
) -> Response<Body> {
    if let Some(block) = detect_gate_block(&text) {
        logger.log(
            "warn",
            format!(
                "prompt_gate.miss request_id={request_id};upstream_request_id={};{}",
                block.upstream_request_id.as_deref().unwrap_or("-"),
                gate_report.summary()
            ),
        );
        return error_response_with_id(
            status,
            "prompt_gate_blocked",
            block.message,
            request_id.to_string(),
        );
    }
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| {
            if text.is_empty() {
                format!("upstream returned HTTP {}", status.as_u16())
            } else {
                text
            }
        });
    let code = match status.as_u16() {
        400 => "upstream_invalid_request",
        401 => "upstream_unauthorized",
        403 => "upstream_forbidden",
        408 => "upstream_timeout",
        429 => "upstream_rate_limited",
        500..=599 => "upstream_server_error",
        _ => "upstream_invalid_request",
    };
    let mut response = error_response_with_id(status, code, message, request_id.to_string());
    if let Some(retry_after) = retry_after {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, retry_after);
    }
    response
}

pub(super) fn log_gate_report(state: &GatewayState, request_id: &str, report: &GateReport) {
    if !report.changed() && !report.user_agent_overridden() {
        return;
    }
    state.logger.log(
        "info",
        format!(
            "prompt_gate.applied request_id={request_id};profiles={};{}",
            report.profile_order().join(","),
            report.summary()
        ),
    );
}

pub(super) fn status_from_reqwest(status: reqwest::StatusCode) -> StatusCode {
    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
}

pub(super) fn is_request_hop_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "content-length"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

pub(super) fn copy_response_headers(
    source: &reqwest::header::HeaderMap,
    destination: &mut HeaderMap,
) {
    for (name, value) in source.iter() {
        if matches!(
            name.as_str(),
            "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        ) {
            continue;
        }
        if let Ok(value) = HeaderValue::from_bytes(value.as_bytes()) {
            destination.append(name, value);
        }
    }
}
