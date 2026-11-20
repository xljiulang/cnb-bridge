use super::*;

pub(super) async fn handle_chat(
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
    let mut prepared = openai_compat::chat::prepare_chat_request(&bytes);
    let mut gate_report = GateReport::default();
    if prepared.request.valid_json_object {
        {
            let chat = prepared
                .value
                .as_mut()
                .expect("valid JSON objects are returned by the compatibility layer");
            gate_report = normalize_chat(chat, &headers);
            if gate_report.changed() {
                let body = openai_compat::chat::encode_chat(chat);
                prepared.set_body(body);
            }
        }
        gate_report.record_developer_roles(prepared.role_changes().developer_to_system);
        apply_headers(&mut headers, &mut gate_report);
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
    }
    let upstream_body = prepared.rewritten_body().unwrap_or(bytes);
    let send = send_chat_request(&state, &headers, &uri, upstream_body);
    let exchange = if prepared.request.downstream_stream {
        match send.await {
            Ok(exchange) => exchange,
            Err(response) => return response,
        }
    } else {
        match tokio::time::timeout(state.non_stream_timeout, send).await {
            Ok(Ok(exchange)) => exchange,
            Ok(Err(response)) => return response,
            Err(_) => {
                return error_response_with_id(
                    StatusCode::GATEWAY_TIMEOUT,
                    "upstream_timeout",
                    "upstream request timed out",
                    new_request_id(),
                )
            }
        }
    };
    let trace = begin_upstream_trace(&state, "chat", &exchange);
    let request_id = response_request_id(&trace);
    log_gate_report(&state, &request_id, &gate_report);
    if !exchange.response.status().is_success() {
        return upstream_error_response(&state, exchange, trace, &request_id, &gate_report).await;
    }
    if !prepared.request.downstream_stream && is_event_stream(exchange.response.headers()) {
        let status = status_from_reqwest(exchange.response.status());
        let aggregate = aggregate_chat_stream(
            exchange.response,
            trace,
            status,
            &prepared.request.requested_model,
        );
        return match tokio::time::timeout(state.non_stream_timeout, aggregate).await {
            Ok(Ok(chat)) => with_request_id(Json(chat).into_response(), &request_id),
            Ok(Err(error)) => error_response_with_details(
                error.status,
                error.code,
                error.message,
                error.error_type,
                error.param,
                error.retryable,
                request_id,
            ),
            Err(_) => error_response_with_id(
                StatusCode::GATEWAY_TIMEOUT,
                "upstream_timeout",
                "upstream response timed out",
                request_id,
            ),
        };
    }
    traced_upstream_response(exchange.response, trace, &request_id)
}

pub(super) struct AggregateHttpError {
    pub(super) status: StatusCode,
    pub(super) code: String,
    pub(super) message: String,
    pub(super) error_type: String,
    pub(super) param: Option<String>,
    pub(super) retryable: bool,
}

impl From<openai_compat::ChatError> for AggregateHttpError {
    fn from(error: openai_compat::ChatError) -> Self {
        let status = match error.code.as_str() {
            "context_length_exceeded" => StatusCode::BAD_REQUEST,
            "upstream_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::BAD_GATEWAY,
        };
        let code = match error.code.as_str() {
            "context_length_exceeded" => "context_length_exceeded".to_string(),
            "upstream_rate_limited" => "upstream_rate_limited".to_string(),
            _ => "upstream_server_error".to_string(),
        };
        let (fallback_type, retryable) = error_contract(&code, status);
        let error_type = if error.error_type == "upstream_error" {
            fallback_type.to_string()
        } else {
            error.error_type
        };
        AggregateHttpError {
            status,
            code,
            message: error.message,
            error_type,
            param: error.param,
            retryable,
        }
    }
}

pub(super) fn aggregate_response_too_large(current: usize, additional: usize) -> bool {
    current.saturating_add(additional) > BODY_LIMIT
}

pub(super) async fn aggregate_chat_stream(
    response: reqwest::Response,
    mut trace: Option<HttpTraceSession>,
    status: StatusCode,
    requested_model: &str,
) -> Result<Value, AggregateHttpError> {
    let mut chunks = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut total = 0usize;
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    while let Some(chunk) = chunks.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                if let Some(mut trace) = trace.take() {
                    let elapsed = trace.elapsed();
                    trace.finish(status.as_u16(), "stream_error", elapsed).ok();
                }
                return Err(AggregateHttpError {
                    status: StatusCode::BAD_GATEWAY,
                    code: "upstream_network_error".to_string(),
                    message: format!("failed to read upstream stream: {error}"),
                    error_type: "server_error".to_string(),
                    param: None,
                    retryable: true,
                });
            }
        };
        if aggregate_response_too_large(total, chunk.len()) {
            if let Some(mut trace) = trace.take() {
                let elapsed = trace.elapsed();
                trace.finish(status.as_u16(), "failed", elapsed).ok();
            }
            return Err(AggregateHttpError {
                status: StatusCode::BAD_GATEWAY,
                code: "upstream_server_error".to_string(),
                message: "aggregated upstream response exceeds 64 MiB".to_string(),
                error_type: "server_error".to_string(),
                param: None,
                retryable: true,
            });
        }
        total += chunk.len();
        if let Some(trace) = trace.as_mut() {
            trace.write_response_chunk(&chunk).ok();
        }
        buffer.extend_from_slice(&chunk);
        let mut consumed = 0;
        while let Some(relative) = buffer[consumed..].iter().position(|byte| *byte == b'\n') {
            let position = consumed + relative;
            if let Err(error) = openai_compat::chat::process_sse_line(
                &mut accumulator,
                &buffer[consumed..=position],
            ) {
                if let Some(mut trace) = trace.take() {
                    let elapsed = trace.elapsed();
                    trace.finish(status.as_u16(), "failed", elapsed).ok();
                }
                return Err(error.into());
            }
            consumed = position + 1;
            if accumulator.saw_done() {
                break;
            }
        }
        if consumed > 0 {
            buffer.drain(..consumed);
        }
        if accumulator.saw_done() {
            break;
        }
    }
    if !buffer.is_empty() && !accumulator.saw_done() {
        if let Err(error) = openai_compat::chat::process_sse_line(&mut accumulator, &buffer) {
            if let Some(mut trace) = trace.take() {
                let elapsed = trace.elapsed();
                trace.finish(status.as_u16(), "failed", elapsed).ok();
            }
            return Err(error.into());
        }
    }
    let chat = accumulator.finish(requested_model).map_err(|message| {
        let mut error: AggregateHttpError = message.into();
        error.message = format!(
            "failed to aggregate upstream chat stream: {}",
            error.message
        );
        error
    })?;
    if let Some(mut trace) = trace.take() {
        let elapsed = trace.elapsed();
        trace.finish(status.as_u16(), "completed", elapsed).ok();
    }
    Ok(chat)
}
