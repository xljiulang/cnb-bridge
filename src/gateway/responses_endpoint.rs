use super::*;

pub(super) async fn handle_responses(
    State(state): State<GatewayState>,
    uri: Uri,
    mut headers: HeaderMap,
    body: Body,
) -> Response<Body> {
    let bytes = match read_body_limited(&headers, body).await {
        Ok(bytes) => bytes,
        Err(BodyReadError::TooLarge) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "request body is too large",
            )
        }
        Err(BodyReadError::Invalid(message)) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_body",
                format!("request body could not be read: {message}"),
            )
        }
    };
    let request: Value = match serde_json::from_slice(&bytes) {
        Ok(request @ Value::Object(_)) => request,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                "request body is not a valid JSON object",
            )
        }
    };
    let stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let options = model_catalog::summaries()
        .ok()
        .and_then(|models| {
            models
                .into_iter()
                .find(|candidate| candidate.id == model)
                .map(|candidate| {
                    TranslationOptions::new(
                        candidate.supports_file_input,
                        candidate.supports_audio_input,
                    )
                })
        })
        .unwrap_or_default();
    let prepared = match prepare_responses(&request, true, options) {
        Ok(prepared) => prepared,
        Err(error) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                error.code(),
                error.message().to_string(),
            )
        }
    };
    if !prepared.unsupported().is_empty() {
        state.logger.log(
            "warn",
            format!(
                "responses.compat.unsupported features={}",
                prepared.unsupported().join(",")
            ),
        );
    }
    if !prepared.ignored().is_empty() {
        state.logger.log(
            "warn",
            format!(
                "responses.compat.ignored count={} details={}",
                prepared.ignored().len(),
                prepared.ignored().join(",")
            ),
        );
    }
    let shape = prepared.shape().clone();
    let mut chat = prepared.chat;
    if chat.get("stream_options").is_none() {
        chat["stream_options"] = json!({ "include_usage": true });
    }
    let mut gate_report = normalize_chat(&mut chat, &headers);
    apply_headers(&mut headers, &mut gate_report);
    let exchange = loop {
        let encoded = match serde_json::to_vec(&chat) {
            Ok(encoded) => Bytes::from(encoded),
            Err(_) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "failed to encode translated request",
                )
            }
        };
        let send = send_chat_request(&state, &headers, &uri, encoded);
        let exchange = if stream {
            match send.await {
                Ok(exchange) => exchange,
                Err(response) => return response,
            }
        } else {
            match tokio::time::timeout(state.non_stream_timeout, send).await {
                Ok(Ok(exchange)) => exchange,
                Ok(Err(response)) => return response,
                Err(_) => {
                    return error_response(
                        StatusCode::GATEWAY_TIMEOUT,
                        "upstream_timeout",
                        "upstream request timed out",
                    )
                }
            }
        };
        if exchange.response.status().is_success() {
            break exchange;
        }
        let response_status = exchange.response.status();
        let status = status_from_reqwest(response_status);
        let retry_after = exchange
            .response
            .headers()
            .get(header::RETRY_AFTER)
            .cloned();
        let response_headers = exchange.response.headers().clone();
        let body = exchange.response.bytes().await.unwrap_or_default();
        let text = String::from_utf8_lossy(&body).into_owned();
        let lower = text.to_ascii_lowercase();
        let removable = if status == StatusCode::BAD_REQUEST {
            OPTIONAL_CHAT_FIELDS.iter().find_map(|(field, needles)| {
                chat.get(*field).is_some().then_some(()).filter(|_| {
                    needles
                        .iter()
                        .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
                })?;
                Some(*field)
            })
        } else {
            None
        };
        if let Some(field) = removable {
            state.logger.log(
                "warn",
                format!("responses.compat.retry_without field={field}"),
            );
            if let Some(object) = chat.as_object_mut() {
                object.remove(field);
            }
            chat["stream"] = Value::Bool(true);
            continue;
        }
        let trace = begin_upstream_trace_parts(
            &state,
            &UpstreamTraceParts {
                protocol: "responses",
                request_headers: &exchange.request_headers,
                request_body: exchange.request_body.clone(),
                request_target: &exchange.request_target,
                started: exchange.started,
                started_at: exchange.started_at,
                response_headers: &response_headers,
                response_status,
            },
        );
        let request_id = response_request_id(&trace);
        log_gate_report(&state, &request_id, &gate_report);
        if let Some(mut trace) = trace {
            trace.write_response_chunk(&body).ok();
            let elapsed = trace.elapsed();
            trace.finish(status.as_u16(), "failed", elapsed).ok();
        }
        return upstream_error_from_text(
            status,
            retry_after,
            text,
            &request_id,
            &gate_report,
            &state.logger,
        );
    };
    let mut trace = begin_upstream_trace(&state, "responses", &exchange);
    let request_id = response_request_id(&trace);
    log_gate_report(&state, &request_id, &gate_report);
    let status = status_from_reqwest(exchange.response.status());
    if stream && is_event_stream(exchange.response.headers()) {
        return responses_stream_response(exchange.response, model, shape, trace, &request_id);
    }
    let chat = if is_event_stream(exchange.response.headers()) {
        let aggregate = aggregate_chat_stream(exchange.response, trace, status, &model);
        match tokio::time::timeout(state.non_stream_timeout, aggregate).await {
            Ok(Ok(chat)) => chat,
            Ok(Err(error)) => {
                return error_response_with_details(
                    error.status,
                    error.code,
                    error.message,
                    error.error_type,
                    error.param,
                    error.retryable,
                    request_id,
                )
            }
            Err(_) => {
                return error_response_with_id(
                    StatusCode::GATEWAY_TIMEOUT,
                    "upstream_timeout",
                    "upstream response timed out",
                    request_id,
                )
            }
        }
    } else {
        let body = match tokio::time::timeout(
            state.non_stream_timeout,
            read_upstream_body_bounded(exchange.response, BODY_LIMIT),
        )
        .await
        {
            Ok(Ok(body)) => body,
            Ok(Err(UpstreamBodyError::Network(error))) => {
                if let Some(mut trace) = trace.take() {
                    let elapsed = trace.elapsed();
                    trace.finish(status.as_u16(), "stream_error", elapsed).ok();
                }
                return error_response_with_id(
                    StatusCode::BAD_GATEWAY,
                    "upstream_network_error",
                    format!("failed to read upstream response: {error}"),
                    request_id,
                );
            }
            Ok(Err(UpstreamBodyError::TooLarge)) => {
                return error_response_with_id(
                    StatusCode::BAD_GATEWAY,
                    "upstream_server_error",
                    "upstream response exceeds 64 MiB",
                    request_id,
                )
            }
            Err(_) => {
                return error_response_with_id(
                    StatusCode::GATEWAY_TIMEOUT,
                    "upstream_timeout",
                    "upstream response timed out",
                    request_id,
                )
            }
        };
        if let Some(mut trace) = trace {
            trace.write_response_chunk(&body).ok();
            let elapsed = trace.elapsed();
            trace.finish(status.as_u16(), "completed", elapsed).ok();
        }
        match serde_json::from_slice(&body) {
            Ok(chat) => chat,
            Err(error) => {
                return error_response_with_id(
                    StatusCode::BAD_GATEWAY,
                    "upstream_server_error",
                    format!("upstream response is not valid JSON: {error}"),
                    request_id,
                )
            }
        }
    };
    if chat
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| choices.len() > 1)
    {
        state
            .logger
            .log("warn", "responses.compat.multiple_choices kept=first");
    }
    match chat_to_responses(&chat, &model, &shape) {
        Ok(response) => with_request_id(Json(response).into_response(), &request_id),
        Err(error) => error_response_with_id(
            StatusCode::BAD_GATEWAY,
            error.code(),
            error.message().to_string(),
            request_id,
        ),
    }
}

pub(super) struct ResponsesStreamState {
    source: futures_util::stream::BoxStream<'static, Result<Bytes, reqwest::Error>>,
    translator: ResponsesStream,
    buffer: Vec<u8>,
    pending: std::collections::VecDeque<Result<Bytes, std::io::Error>>,
    saw_done: bool,
    done: bool,
    trace: Option<HttpTraceSession>,
    status: StatusCode,
}

pub(super) fn responses_buffer_too_large(current: usize, additional: usize) -> bool {
    current.saturating_add(additional) > BODY_LIMIT
}

impl Drop for ResponsesStreamState {
    fn drop(&mut self) {
        if let Some(mut trace) = self.trace.take() {
            let elapsed = trace.elapsed();
            trace
                .finish(self.status.as_u16(), "client_disconnected", elapsed)
                .ok();
        }
    }
}

pub(super) fn responses_stream_response(
    response: reqwest::Response,
    model: String,
    shape: ToolShape,
    trace: Option<HttpTraceSession>,
    request_id: &str,
) -> Response<Body> {
    let status = status_from_reqwest(response.status());
    let response_id = format!(
        "resp_{}_{}",
        chrono::Utc::now().timestamp_millis(),
        next_request_sequence()
    );
    let state = ResponsesStreamState {
        source: response.bytes_stream().boxed(),
        translator: ResponsesStream::with_shape(response_id, model, shape),
        buffer: Vec::new(),
        pending: std::collections::VecDeque::new(),
        saw_done: false,
        done: false,
        trace,
        status,
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(item) = state.pending.pop_front() {
                return Some((item, state));
            }
            if state.done {
                return None;
            }
            match state.source.next().await {
                Some(Ok(chunk)) => {
                    if responses_buffer_too_large(state.buffer.len(), chunk.len()) {
                        for event in state
                            .translator
                            .fail("upstream_server_error", "upstream SSE frame exceeds 64 MiB")
                        {
                            state.pending.push_back(Ok(event));
                        }
                        if let Some(mut trace) = state.trace.take() {
                            let elapsed = trace.elapsed();
                            trace.finish(state.status.as_u16(), "failed", elapsed).ok();
                        }
                        state.done = true;
                        continue;
                    }
                    if let Some(trace) = state.trace.as_mut() {
                        trace.write_response_chunk(&chunk).ok();
                    }
                    state.buffer.extend_from_slice(&chunk);
                    let mut consumed = 0;
                    'lines: while let Some(relative) = state.buffer[consumed..]
                        .iter()
                        .position(|byte| *byte == b'\n')
                    {
                        let position = consumed + relative;
                        let line = String::from_utf8_lossy(&state.buffer[consumed..=position]);
                        consumed = position + 1;
                        let Some(payload) = line.trim().strip_prefix("data:") else {
                            continue;
                        };
                        if payload.trim() == "[DONE]" {
                            state.saw_done = true;
                            for event in state.translator.finish() {
                                state.pending.push_back(Ok(event));
                            }
                            if let Some(mut trace) = state.trace.take() {
                                let elapsed = trace.elapsed();
                                trace
                                    .finish(state.status.as_u16(), "completed", elapsed)
                                    .ok();
                            }
                            state.done = true;
                            break;
                        }
                        match serde_json::from_str::<Value>(payload.trim()) {
                            Ok(chunk) => {
                                for event in state.translator.push(&chunk) {
                                    state.pending.push_back(Ok(event));
                                }
                            }
                            Err(error) => {
                                for event in state.translator.fail(
                                    "upstream_server_error",
                                    format!("无效 SSE JSON: {error}"),
                                ) {
                                    state.pending.push_back(Ok(event));
                                }
                                if let Some(mut trace) = state.trace.take() {
                                    let elapsed = trace.elapsed();
                                    trace.finish(state.status.as_u16(), "failed", elapsed).ok();
                                }
                                state.done = true;
                                break 'lines;
                            }
                        }
                    }
                    if consumed > 0 {
                        state.buffer.drain(..consumed);
                    }
                }
                Some(Err(error)) => {
                    if state.saw_done {
                        for event in state.translator.finish() {
                            state.pending.push_back(Ok(event));
                        }
                        if let Some(mut trace) = state.trace.take() {
                            let elapsed = trace.elapsed();
                            trace
                                .finish(state.status.as_u16(), "completed", elapsed)
                                .ok();
                        }
                        state.done = true;
                        continue;
                    }
                    for event in state
                        .translator
                        .fail("upstream_network_error", format!("读取上游流失败: {error}"))
                    {
                        state.pending.push_back(Ok(event));
                    }
                    if let Some(mut trace) = state.trace.take() {
                        let elapsed = trace.elapsed();
                        trace
                            .finish(state.status.as_u16(), "stream_error", elapsed)
                            .ok();
                    }
                    state.done = true;
                }
                None => {
                    let events = if state.saw_done {
                        state.translator.finish()
                    } else {
                        state
                            .translator
                            .fail("incomplete_stream", "上游流在 [DONE] 前结束")
                    };
                    for event in events {
                        state.pending.push_back(Ok(event));
                    }
                    if let Some(mut trace) = state.trace.take() {
                        let elapsed = trace.elapsed();
                        trace
                            .finish(
                                state.status.as_u16(),
                                if state.saw_done {
                                    "completed"
                                } else {
                                    "incomplete"
                                },
                                elapsed,
                            )
                            .ok();
                    }
                    state.done = true;
                }
            }
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    with_request_id(response, request_id)
}
