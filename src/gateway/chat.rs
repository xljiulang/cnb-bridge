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
    let mut prepared = prepare_chat_request(bytes, &headers);
    if prepared.valid_json_object {
        apply_headers(&mut headers, &mut prepared.gate_report);
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
    }
    let send = send_chat_request(&state, &headers, &uri, prepared.body);
    let exchange = if prepared.downstream_stream {
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
    log_gate_report(&state, &request_id, &prepared.gate_report);
    if !exchange.response.status().is_success() {
        return upstream_error_response(
            &state,
            exchange,
            trace,
            &request_id,
            &prepared.gate_report,
        )
        .await;
    }
    if !prepared.downstream_stream && is_event_stream(exchange.response.headers()) {
        let status = status_from_reqwest(exchange.response.status());
        let aggregate =
            aggregate_chat_stream(exchange.response, trace, status, &prepared.requested_model);
        return match tokio::time::timeout(state.non_stream_timeout, aggregate).await {
            Ok(Ok(chat)) => with_request_id(Json(chat).into_response(), &request_id),
            Ok(Err((status, code, message))) => {
                error_response_with_id(status, code, message, request_id)
            }
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

pub(super) struct PreparedChatRequest {
    pub(super) body: Bytes,
    pub(super) gate_report: GateReport,
    pub(super) valid_json_object: bool,
    pub(super) downstream_stream: bool,
    pub(super) requested_model: String,
}

pub(super) fn prepare_chat_request(bytes: Bytes, headers: &HeaderMap) -> PreparedChatRequest {
    let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(&bytes) else {
        return PreparedChatRequest {
            body: bytes,
            gate_report: GateReport::default(),
            valid_json_object: false,
            downstream_stream: false,
            requested_model: String::new(),
        };
    };
    let downstream_stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let requested_model = object
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    object.insert("stream".to_string(), Value::Bool(true));
    let mut value = Value::Object(object);
    let report = normalize_chat(&mut value, headers);
    let body = if report.changed() {
        Bytes::from(
            serde_json::to_vec(&value).expect("a parsed JSON object is always serializable"),
        )
    } else {
        force_top_level_stream_true(&bytes)
    };
    PreparedChatRequest {
        body,
        gate_report: report,
        valid_json_object: true,
        downstream_stream,
        requested_model,
    }
}

fn force_top_level_stream_true(bytes: &Bytes) -> Bytes {
    let Some(object_start) = bytes.iter().position(|byte| !byte.is_ascii_whitespace()) else {
        return bytes.clone();
    };
    if bytes[object_start] != b'{' {
        return bytes.clone();
    }
    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut index = object_start + 1;
    let mut closing = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' => {
                if depth == 1 {
                    let key_start = index + 1;
                    let mut key_end = key_start;
                    let mut key_escaped = false;
                    while key_end < bytes.len() {
                        if key_escaped {
                            key_escaped = false;
                        } else if bytes[key_end] == b'\\' {
                            key_escaped = true;
                        } else if bytes[key_end] == b'"' {
                            break;
                        }
                        key_end += 1;
                    }
                    if key_end < bytes.len() && &bytes[key_start..key_end] == b"stream" {
                        let mut value_start = key_end + 1;
                        while value_start < bytes.len()
                            && (bytes[value_start].is_ascii_whitespace()
                                || bytes[value_start] == b':')
                        {
                            value_start += 1;
                        }
                        let mut value_end = value_start;
                        while value_end < bytes.len() && !matches!(bytes[value_end], b',' | b'}') {
                            value_end += 1;
                        }
                        while value_end > value_start && bytes[value_end - 1].is_ascii_whitespace()
                        {
                            value_end -= 1;
                        }
                        return match &bytes[value_start..value_end] {
                            b"true" => bytes.clone(),
                            b"false" => {
                                let mut replaced =
                                    Vec::with_capacity(bytes.len().saturating_sub(4));
                                replaced.extend_from_slice(&bytes[..value_start]);
                                replaced.extend_from_slice(b"true");
                                replaced.extend_from_slice(&bytes[value_end..]);
                                Bytes::from(replaced)
                            }
                            _ => bytes.clone(),
                        };
                    }
                }
                in_string = true;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    closing = Some(index);
                    break;
                }
            }
            _ => {}
        }
        index += 1;
    }
    let Some(closing) = closing else {
        return bytes.clone();
    };
    let previous = bytes[..closing]
        .iter()
        .rfind(|byte| !byte.is_ascii_whitespace())
        .copied();
    let mut inserted = Vec::with_capacity(bytes.len() + 15);
    inserted.extend_from_slice(&bytes[..closing]);
    if !matches!(previous, Some(b'{') | Some(b',')) {
        inserted.push(b',');
    }
    inserted.extend_from_slice(b"\"stream\":true");
    inserted.extend_from_slice(&bytes[closing..]);
    Bytes::from(inserted)
}

#[derive(Default)]
pub(super) struct AggregateToolCall {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
pub(super) struct AggregateChoice {
    content: String,
    reasoning_content: String,
    has_content: bool,
    tool_calls: BTreeMap<usize, AggregateToolCall>,
    finish_reason: Option<String>,
}

#[derive(Default)]
pub(super) struct ChatCompletionAccumulator {
    id: Option<String>,
    created: Option<u64>,
    model: Option<String>,
    system_fingerprint: Option<Value>,
    usage: Option<Value>,
    choices: BTreeMap<usize, AggregateChoice>,
    saw_done: bool,
    saw_finish: bool,
}

impl ChatCompletionAccumulator {
    pub(super) fn push_data(&mut self, payload: &str) -> Result<(), String> {
        if payload == "[DONE]" {
            self.saw_done = true;
            return Ok(());
        }
        let chunk: Value =
            serde_json::from_str(payload).map_err(|error| format!("无效 SSE JSON: {error}"))?;
        if let Some(error) = chunk.get("error").filter(|value| !value.is_null()) {
            return Err(format!("上游流内错误: {error}"));
        }
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            self.id = Some(id.to_string());
        }
        if let Some(created) = chunk.get("created").and_then(Value::as_u64) {
            self.created = Some(created);
        }
        if let Some(model) = chunk.get("model").and_then(Value::as_str) {
            self.model = Some(model.to_string());
        }
        if let Some(fingerprint) = chunk
            .get("system_fingerprint")
            .filter(|value| !value.is_null())
        {
            self.system_fingerprint = Some(fingerprint.clone());
        }
        if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            self.usage = Some(usage.clone());
        }
        for choice in chunk
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let state = self.choices.entry(index).or_default();
            let delta = choice
                .get("delta")
                .or_else(|| choice.get("message"))
                .unwrap_or(&Value::Null);
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                state.has_content = true;
                state.content.push_str(content);
            }
            if let Some(reasoning) = aggregate_reasoning_text(delta) {
                state.reasoning_content.push_str(reasoning);
            }
            for tool in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let tool_index = tool.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let tool_state = state.tool_calls.entry(tool_index).or_default();
                if let Some(id) = tool.get("id").and_then(Value::as_str) {
                    tool_state.id = id.to_string();
                }
                if let Some(kind) = tool.get("type").and_then(Value::as_str) {
                    tool_state.kind = kind.to_string();
                }
                if let Some(name) = tool.pointer("/function/name").and_then(Value::as_str) {
                    tool_state.name.push_str(name);
                }
                if let Some(arguments) = tool.pointer("/function/arguments").and_then(Value::as_str)
                {
                    tool_state.arguments.push_str(arguments);
                }
            }
            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.is_empty())
            {
                state.finish_reason = Some(reason.to_string());
                self.saw_finish = true;
            }
        }
        Ok(())
    }

    pub(super) fn finish(self, requested_model: &str) -> Result<Value, String> {
        if !self.saw_done && !self.saw_finish {
            return Err("上游 SSE 缺少完成分片或 [DONE]".to_string());
        }
        let choices = self
            .choices
            .into_iter()
            .map(|(index, choice)| {
                let mut message = json!({
                    "role": "assistant",
                    "content": if choice.has_content {
                        Value::String(choice.content)
                    } else {
                        Value::Null
                    }
                });
                if !choice.reasoning_content.is_empty() {
                    message["reasoning_content"] = Value::String(choice.reasoning_content);
                }
                if !choice.tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(
                        choice
                            .tool_calls
                            .into_values()
                            .map(|tool| {
                                json!({
                                    "id": tool.id,
                                    "type": if tool.kind.is_empty() { "function" } else { tool.kind.as_str() },
                                    "function": {
                                        "name": tool.name,
                                        "arguments": tool.arguments
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                json!({
                    "index": index,
                    "message": message,
                    "finish_reason": choice.finish_reason
                })
            })
            .collect::<Vec<_>>();
        let mut completion = json!({
            "id": self.id.unwrap_or_else(|| format!("chatcmpl_{}", new_request_id())),
            "object": "chat.completion",
            "created": self.created.unwrap_or_else(|| chrono::Utc::now().timestamp() as u64),
            "model": self.model.unwrap_or_else(|| requested_model.to_string()),
            "choices": choices
        });
        if let Some(fingerprint) = self.system_fingerprint {
            completion["system_fingerprint"] = fingerprint;
        }
        if let Some(usage) = self.usage {
            completion["usage"] = usage;
        }
        Ok(completion)
    }
}

pub(super) fn aggregate_reasoning_text(delta: &Value) -> Option<&str> {
    crate::responses::reasoning_text(delta)
}

pub(super) fn aggregate_response_too_large(current: usize, additional: usize) -> bool {
    current.saturating_add(additional) > BODY_LIMIT
}

pub(super) async fn aggregate_chat_stream(
    response: reqwest::Response,
    mut trace: Option<HttpTraceSession>,
    status: StatusCode,
    requested_model: &str,
) -> Result<Value, (StatusCode, &'static str, String)> {
    let mut chunks = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut total = 0usize;
    let mut accumulator = ChatCompletionAccumulator::default();
    while let Some(chunk) = chunks.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                if let Some(mut trace) = trace.take() {
                    let elapsed = trace.elapsed();
                    trace.finish(status.as_u16(), "stream_error", elapsed).ok();
                }
                return Err((
                    StatusCode::BAD_GATEWAY,
                    "upstream_network_error",
                    format!("failed to read upstream stream: {error}"),
                ));
            }
        };
        if aggregate_response_too_large(total, chunk.len()) {
            if let Some(mut trace) = trace.take() {
                let elapsed = trace.elapsed();
                trace.finish(status.as_u16(), "failed", elapsed).ok();
            }
            return Err((
                StatusCode::BAD_GATEWAY,
                "upstream_server_error",
                "aggregated upstream response exceeds 64 MiB".to_string(),
            ));
        }
        total += chunk.len();
        if let Some(trace) = trace.as_mut() {
            trace.write_response_chunk(&chunk).ok();
        }
        buffer.extend_from_slice(&chunk);
        let mut consumed = 0;
        while let Some(relative) = buffer[consumed..].iter().position(|byte| *byte == b'\n') {
            let position = consumed + relative;
            if let Err(error) =
                process_aggregate_line(&buffer[consumed..=position], &mut accumulator)
            {
                if let Some(mut trace) = trace.take() {
                    let elapsed = trace.elapsed();
                    trace.finish(status.as_u16(), "failed", elapsed).ok();
                }
                return Err(error);
            }
            consumed = position + 1;
            if accumulator.saw_done {
                break;
            }
        }
        if consumed > 0 {
            buffer.drain(..consumed);
        }
        if accumulator.saw_done {
            break;
        }
    }
    if !buffer.is_empty() && !accumulator.saw_done {
        if let Err(error) = process_aggregate_line(&buffer, &mut accumulator) {
            if let Some(mut trace) = trace.take() {
                let elapsed = trace.elapsed();
                trace.finish(status.as_u16(), "failed", elapsed).ok();
            }
            return Err(error);
        }
    }
    let chat = accumulator.finish(requested_model).map_err(|message| {
        (
            StatusCode::BAD_GATEWAY,
            "upstream_server_error",
            format!("failed to aggregate upstream chat stream: {message}"),
        )
    })?;
    if let Some(mut trace) = trace.take() {
        let elapsed = trace.elapsed();
        trace.finish(status.as_u16(), "completed", elapsed).ok();
    }
    Ok(chat)
}

pub(super) fn process_aggregate_line(
    line: &[u8],
    accumulator: &mut ChatCompletionAccumulator,
) -> Result<(), (StatusCode, &'static str, String)> {
    let line = String::from_utf8_lossy(line);
    let Some(payload) = line.trim_end().strip_prefix("data:") else {
        return Ok(());
    };
    accumulator
        .push_data(payload.trim())
        .map_err(|message| (StatusCode::BAD_GATEWAY, "upstream_server_error", message))
}
