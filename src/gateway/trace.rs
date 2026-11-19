use super::*;

const TERMINAL_SCAN_CARRY_BYTES: usize = 256;
const FINISH_REASON_KEY: &[u8] = b"\"finish_reason\":\"";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceStreamResult {
    Completed,
    Incomplete,
}

impl TraceStreamResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Default)]
struct SseCompletionTracker {
    carry: Vec<u8>,
    saw_done: bool,
    saw_finish_reason: bool,
    incomplete: bool,
}

impl SseCompletionTracker {
    fn push(&mut self, chunk: &[u8]) {
        let mut scan = Vec::with_capacity(self.carry.len() + chunk.len());
        scan.extend_from_slice(&self.carry);
        scan.extend_from_slice(chunk);

        if scan
            .windows(b"[DONE]".len())
            .any(|window| window == b"[DONE]")
        {
            self.saw_done = true;
        }
        let mut remaining = scan.as_slice();
        while let Some(position) = remaining
            .windows(FINISH_REASON_KEY.len())
            .position(|window| window == FINISH_REASON_KEY)
        {
            let value_start = position + FINISH_REASON_KEY.len();
            let Some(value_len) = remaining[value_start..]
                .iter()
                .position(|byte| *byte == b'"')
            else {
                break;
            };
            let reason = &remaining[value_start..value_start + value_len];
            if !reason.is_empty() && reason != b"null" {
                self.saw_finish_reason = true;
                if matches!(reason, b"length" | b"content_filter") {
                    self.incomplete = true;
                }
            }
            remaining = &remaining[value_start + value_len..];
        }

        let keep = scan.len().min(TERMINAL_SCAN_CARRY_BYTES);
        self.carry.clear();
        self.carry.extend_from_slice(&scan[scan.len() - keep..]);
    }

    fn result(&self) -> Option<TraceStreamResult> {
        if self.incomplete {
            Some(TraceStreamResult::Incomplete)
        } else if self.saw_done || self.saw_finish_reason {
            Some(TraceStreamResult::Completed)
        } else {
            None
        }
    }
}

fn client_disconnect_result(
    status: StatusCode,
    is_sse: bool,
    tracker: &SseCompletionTracker,
) -> &'static str {
    if !status.is_success() {
        "failed"
    } else if is_sse {
        tracker
            .result()
            .map(TraceStreamResult::as_str)
            .unwrap_or("client_disconnected")
    } else {
        "client_disconnected"
    }
}

fn upstream_eof_result(
    status: StatusCode,
    is_sse: bool,
    tracker: &SseCompletionTracker,
) -> &'static str {
    if !status.is_success() {
        "failed"
    } else if is_sse {
        tracker
            .result()
            .map(TraceStreamResult::as_str)
            .unwrap_or("incomplete")
    } else {
        "completed"
    }
}

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
    is_sse: bool,
    completion: SseCompletionTracker,
}

impl Drop for TracedUpstreamState {
    fn drop(&mut self) {
        if let Some(mut trace) = self.trace.take() {
            let elapsed = trace.elapsed();
            let result = client_disconnect_result(self.status, self.is_sse, &self.completion);
            let _ = trace.finish(self.status.as_u16(), result, elapsed);
        }
    }
}

pub(super) fn traced_upstream_response(
    response: reqwest::Response,
    trace: Option<HttpTraceSession>,
    request_id: &str,
) -> Response<Body> {
    let status = status_from_reqwest(response.status());
    let is_sse = is_event_stream(response.headers());
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
        is_sse,
        completion: SseCompletionTracker::default(),
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        match state.source.next().await {
            Some(Ok(bytes)) => {
                if state.is_sse {
                    state.completion.push(&bytes);
                }
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
                    let result = upstream_eof_result(state.status, state.is_sse, &state.completion);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_done_split_across_chunks_as_completed() {
        let mut tracker = SseCompletionTracker::default();
        tracker.push(b"data: [DO");
        tracker.push(b"NE]\n\n");

        assert_eq!(tracker.result(), Some(TraceStreamResult::Completed));
    }

    #[test]
    fn detects_finish_reason_without_done() {
        let mut tracker = SseCompletionTracker::default();
        tracker.push(br#"data: {"choices":[{"finish_reason":"stop"}]}"#);

        assert_eq!(tracker.result(), Some(TraceStreamResult::Completed));
    }

    #[test]
    fn detects_finish_reason_split_across_chunks() {
        let mut tracker = SseCompletionTracker::default();
        tracker.push(br#"data: {"choices":[{"finish_"#);
        tracker.push(br#"reason":"tool_calls"}]}"#);

        assert_eq!(tracker.result(), Some(TraceStreamResult::Completed));
    }

    #[test]
    fn keeps_abnormal_finish_reasons_incomplete() {
        for reason in ["length", "content_filter"] {
            let mut tracker = SseCompletionTracker::default();
            tracker.push(
                format!(r#"data: {{"choices":[{{"finish_reason":"{reason}"}}]}}"#).as_bytes(),
            );

            assert_eq!(tracker.result(), Some(TraceStreamResult::Incomplete));
        }
    }

    #[test]
    fn treats_disconnect_without_terminal_marker_as_client_disconnected() {
        let tracker = SseCompletionTracker::default();

        assert_eq!(tracker.result(), None);
        assert_eq!(
            client_disconnect_result(StatusCode::OK, true, &tracker),
            "client_disconnected"
        );
    }

    #[test]
    fn successful_drop_after_terminal_marker_is_completed() {
        let mut tracker = SseCompletionTracker::default();
        tracker.push(b"data: [DONE]\n\n");

        assert_eq!(
            client_disconnect_result(StatusCode::OK, true, &tracker),
            "completed"
        );
        assert_eq!(
            client_disconnect_result(StatusCode::BAD_GATEWAY, true, &tracker),
            "failed"
        );
    }

    #[test]
    fn sse_eof_without_terminal_marker_is_incomplete() {
        let tracker = SseCompletionTracker::default();

        assert_eq!(
            upstream_eof_result(StatusCode::OK, true, &tracker),
            "incomplete"
        );
    }
}
