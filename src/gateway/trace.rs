use super::*;

pub(super) fn begin_upstream_trace(
    state: &GatewayState,
    protocol: &str,
    exchange: &UpstreamExchange,
) -> Option<HttpTraceSession> {
    let parts = UpstreamTraceParts {
        protocol,
        request_headers: &exchange.request_headers,
        request_body: exchange.request_body.clone(),
        request_target: &exchange.request_target,
        started: exchange.started,
        started_at: exchange.started_at,
        response_headers: exchange.response.headers(),
        response_status: exchange.response.status(),
    };
    begin_upstream_trace_parts(state, &parts)
}

pub(super) struct UpstreamTraceParts<'a> {
    pub(super) protocol: &'a str,
    pub(super) request_headers: &'a HeaderMap,
    pub(super) request_body: Bytes,
    pub(super) request_target: &'a str,
    pub(super) started: Instant,
    pub(super) started_at: std::time::SystemTime,
    pub(super) response_headers: &'a HeaderMap,
    pub(super) response_status: reqwest::StatusCode,
}

pub(super) fn begin_upstream_trace_parts(
    state: &GatewayState,
    parts: &UpstreamTraceParts<'_>,
) -> Option<HttpTraceSession> {
    let mut trace = state
        .trace_store
        .start_with_filter(
            parts.protocol,
            state.bridge.http_log_filter(),
            parts.started,
            parts.started_at,
        )
        .ok()?;
    trace
        .write_request_http(
            "POST",
            parts.request_target,
            parts.request_headers,
            parts.request_body.clone(),
        )
        .ok();
    trace
        .write_response_http(
            status_from_reqwest(parts.response_status),
            parts.response_headers,
        )
        .ok();
    Some(trace)
}

pub(super) fn response_request_id(trace: &Option<HttpTraceSession>) -> String {
    trace
        .as_ref()
        .map_or_else(new_request_id, |trace| trace.id().to_string())
}

pub(super) struct TracedUpstreamState {
    source: futures_util::stream::BoxStream<'static, Result<Bytes, reqwest::Error>>,
    trace: Option<HttpTraceSession>,
    status: StatusCode,
}

impl Drop for TracedUpstreamState {
    fn drop(&mut self) {
        if let Some(mut trace) = self.trace.take() {
            let elapsed = trace.elapsed();
            let _ = trace.finish(self.status.as_u16(), "client_disconnected", elapsed);
        }
    }
}

pub(super) fn traced_upstream_response(
    response: reqwest::Response,
    trace: Option<HttpTraceSession>,
    request_id: &str,
) -> Response<Body> {
    let status = status_from_reqwest(response.status());
    let mut headers = HeaderMap::new();
    copy_response_headers(response.headers(), &mut headers);
    if !headers.contains_key(REQUEST_ID_HEADER) {
        if let Ok(value) = HeaderValue::from_str(request_id) {
            headers.insert(REQUEST_ID_HEADER, value);
        }
    }
    let state = TracedUpstreamState {
        source: response.bytes_stream().boxed(),
        trace,
        status,
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        match state.source.next().await {
            Some(Ok(bytes)) => {
                if let Some(trace) = state.trace.as_mut() {
                    trace.write_response_chunk(&bytes).ok();
                }
                Some((Ok::<Bytes, std::io::Error>(bytes), state))
            }
            Some(Err(error)) => {
                if let Some(mut trace) = state.trace.take() {
                    let elapsed = trace.elapsed();
                    let _ = trace.finish(state.status.as_u16(), "stream_error", elapsed);
                }
                Some((Err(std::io::Error::other(error)), state))
            }
            None => {
                if let Some(mut trace) = state.trace.take() {
                    let result = if state.status.is_success() {
                        "completed"
                    } else {
                        "failed"
                    };
                    let elapsed = trace.elapsed();
                    let _ = trace.finish(state.status.as_u16(), result, elapsed);
                }
                None
            }
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

pub(super) struct ActivityResponseState {
    source: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>,
    guard: Option<ActivityGuard>,
}

impl Drop for ActivityResponseState {
    fn drop(&mut self) {
        self.guard.take();
    }
}

pub(super) fn activity_response(response: Response<Body>, guard: ActivityGuard) -> Response<Body> {
    let response = with_request_id(response, &new_request_id());
    let (parts, body) = response.into_parts();
    let state = ActivityResponseState {
        source: body.into_data_stream().boxed(),
        guard: Some(guard),
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        match state.source.next().await {
            Some(Ok(bytes)) => Some((Ok::<Bytes, std::io::Error>(bytes), state)),
            Some(Err(error)) => {
                state.guard.take();
                Some((Err(std::io::Error::other(error)), state))
            }
            None => {
                state.guard.take();
                None
            }
        }
    });
    Response::from_parts(parts, Body::from_stream(stream))
}
