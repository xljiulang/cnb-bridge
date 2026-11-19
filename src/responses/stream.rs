use super::*;
use bytes::Bytes;
use std::io::Write as _;

#[derive(Clone)]
pub(super) struct TextTrack {
    id: String,
    index: usize,
    text: String,
}

#[derive(Clone)]
pub(super) struct CallTrack {
    item_id: String,
    call_id: String,
    display_name: String,
    namespace: Option<String>,
    custom: bool,
    arguments: String,
    output_index: usize,
}

/// Stateful Chat SSE chunk to Responses SSE translator.
pub struct ResponsesStream {
    response_id: String,
    model: String,
    shape: ToolShape,
    created: bool,
    next_index: usize,
    reasoning: Option<TextTrack>,
    text: Option<TextTrack>,
    calls: BTreeMap<usize, CallTrack>,
    output: Vec<Value>,
    finish_reason: Option<String>,
    usage: Option<Value>,
    failure: Option<(String, String)>,
    finished: bool,
}

impl ResponsesStream {
    /// Creates a translator for one downstream Responses request.
    pub fn new(response_id: String, model: String) -> Self {
        Self::with_shape(response_id, model, ToolShape::default())
    }

    /// Creates a translator with request-local tool metadata.
    pub fn with_shape(response_id: String, model: String, shape: ToolShape) -> Self {
        Self {
            response_id,
            model,
            shape,
            created: false,
            next_index: 0,
            reasoning: None,
            text: None,
            calls: BTreeMap::new(),
            output: Vec::new(),
            finish_reason: None,
            usage: None,
            failure: None,
            finished: false,
        }
    }

    /// Translates one Chat completion chunk into zero or more Responses SSE events.
    pub fn push(&mut self, chunk: &Value) -> Vec<Bytes> {
        if self.finished {
            return Vec::new();
        }
        let mut events = self.ensure_created();
        if let Some(error) = chunk.get("error").filter(|value| !value.is_null()) {
            self.failure = Some(("upstream_network_error".to_string(), error.to_string()));
            return events;
        }
        if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            self.usage = Some(usage.clone());
        }
        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            return events;
        };
        for choice in choices {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(reasoning) = reasoning_text(delta) {
                events.extend(self.reasoning_delta(reasoning));
            }
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                if !content.is_empty() {
                    events.extend(self.text_delta(content));
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    events.extend(self.tool_delta(call));
                }
            }
            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.is_empty() && *reason != "null")
            {
                self.finish_reason = Some(reason.to_string());
            }
        }
        events
    }

    /// Emits terminal events exactly once.
    pub fn finish(&mut self) -> Vec<Bytes> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = self.ensure_created();
        if let Some(track) = self.reasoning.take() {
            events.push(sse_event(
                "response.reasoning_summary_text.done",
                json!({
                    "type": "response.reasoning_summary_text.done",
                    "item_id": track.id,
                    "output_index": track.index,
                    "summary_index": 0,
                    "text": track.text
                }),
            ));
            events.push(sse_event(
                "response.reasoning_summary_part.done",
                json!({
                    "type": "response.reasoning_summary_part.done",
                    "item_id": track.id,
                    "output_index": track.index,
                    "summary_index": 0,
                    "part": { "type": "summary_text", "text": track.text }
                }),
            ));
            events.push(sse_event("response.output_item.done", {
                let item = json!({
                    "id": track.id,
                    "type": "reasoning",
                    "summary": [{ "type": "summary_text", "text": track.text }]
                });
                self.output.push(item.clone());
                json!({
                    "type": "response.output_item.done",
                    "output_index": track.index,
                    "item": item
                })
            }));
        }
        if let Some(track) = self.text.take() {
            events.push(sse_event(
                "response.output_text.done",
                json!({
                    "type": "response.output_text.done",
                    "item_id": track.id,
                    "output_index": track.index,
                    "content_index": 0,
                    "text": track.text,
                    "annotations": []
                }),
            ));
            events.push(sse_event(
                "response.content_part.done",
                json!({
                    "type": "response.content_part.done",
                    "item_id": track.id,
                    "output_index": track.index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": track.text,
                        "annotations": []
                    }
                }),
            ));
            events.push(sse_event("response.output_item.done", {
                let item = json!({
                    "id": track.id,
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": track.text,
                        "annotations": []
                    }]
                });
                self.output.push(item.clone());
                json!({
                    "type": "response.output_item.done",
                    "output_index": track.index,
                    "item": item
                })
            }));
        }
        if self.failure.is_none() {
            for call in self.calls.values() {
                if !call.custom && normalize_arguments(&call.arguments).1 {
                    self.failure = Some((
                        "malformed_tool_call_arguments".to_string(),
                        format!("tool call arguments for {} are invalid", call.display_name),
                    ));
                    break;
                }
            }
        }
        if self.failure.is_none() {
            for (_, call) in std::mem::take(&mut self.calls) {
                if call.custom {
                    let input = custom_tool_input(&call.arguments);
                    events.push(sse_event(
                        "response.custom_tool_call_input.done",
                        json!({
                            "type": "response.custom_tool_call_input.done",
                            "item_id": call.item_id,
                            "output_index": call.output_index,
                            "input": input
                        }),
                    ));
                    events.push(sse_event("response.output_item.done", {
                        let item = json!({
                            "id": call.item_id,
                            "type": "custom_tool_call",
                            "status": "completed",
                            "call_id": call.call_id,
                                "name": call.display_name,
                            "input": input
                        });
                        self.output.push(item.clone());
                        json!({
                            "type": "response.output_item.done",
                            "output_index": call.output_index,
                            "item": item
                        })
                    }));
                } else {
                    events.push(sse_event(
                        "response.function_call_arguments.done",
                        json!({
                            "type": "response.function_call_arguments.done",
                            "item_id": call.item_id,
                            "output_index": call.output_index,
                            "arguments": call.arguments
                        }),
                    ));
                    let mut item = json!({
                        "id": call.item_id,
                        "type": "function_call",
                        "status": "completed",
                        "call_id": call.call_id,
                        "name": call.display_name,
                        "arguments": call.arguments
                    });
                    if let Some(namespace) = call.namespace {
                        item["namespace"] = json!(namespace);
                    }
                    events.push(sse_event(
                        "response.output_item.done",
                        {
                            self.output.push(item.clone());
                            json!({ "type": "response.output_item.done", "output_index": call.output_index, "item": item })
                        },
                    ));
                }
            }
        }
        let terminal = if let Some((code, detail)) = self.failure.take() {
            sse_event(
                "response.failed",
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": self.response_id,
                        "object": "response",
                        "status": "failed",
                        "model": self.model,
                        "output": self.output,
                        "error": {
                            "code": code,
                            "message": detail,
                            "param": null,
                            "retryable": false
                        }
                    }
                }),
            )
        } else if self.finish_reason.as_deref() == Some("length") {
            sse_event(
                "response.incomplete",
                json!({
                    "type": "response.incomplete",
                    "response": {
                        "id": self.response_id,
                        "object": "response",
                        "status": "incomplete",
                        "model": self.model,
                        "output": self.output,
                        "error": null,
                        "incomplete_details": { "reason": "max_output_tokens" },
                        "usage": usage_to_responses(self.usage.as_ref())
                    }
                }),
            )
        } else {
            sse_event(
                "response.completed",
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": self.response_id,
                        "object": "response",
                        "status": "completed",
                        "model": self.model,
                        "output": self.output,
                        "error": null,
                        "usage": usage_to_responses(self.usage.as_ref())
                    }
                }),
            )
        };
        events.push(terminal);
        events
    }

    /// Marks the stream as failed and emits its terminal event.
    pub fn fail(&mut self, code: impl Into<String>, message: impl Into<String>) -> Vec<Bytes> {
        if self.finished {
            return Vec::new();
        }
        self.failure = Some((code.into(), message.into()));
        self.finish()
    }

    fn ensure_created(&mut self) -> Vec<Bytes> {
        if self.created {
            return Vec::new();
        }
        self.created = true;
        vec![sse_event(
            "response.created",
            json!({
                "type": "response.created",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": unix_now(),
                    "status": "in_progress",
                    "model": self.model,
                    "output": [],
                    "error": null
                }
            }),
        )]
    }

    fn reasoning_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let mut events = Vec::new();
        if self.reasoning.is_none() {
            let index = self.next_index;
            self.next_index += 1;
            let id = gen_id("rs");
            self.reasoning = Some(TextTrack {
                id: id.clone(),
                index,
                text: String::new(),
            });
            events.push(sse_event(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": index,
                    "item": { "id": id, "type": "reasoning", "summary": [] }
                }),
            ));
            events.push(sse_event(
                "response.reasoning_summary_part.added",
                json!({
                    "type": "response.reasoning_summary_part.added",
                    "item_id": id,
                    "output_index": index,
                    "summary_index": 0,
                    "part": { "type": "summary_text", "text": "" }
                }),
            ));
        }
        let track = self.reasoning.as_mut().unwrap();
        track.text.push_str(delta);
        events.push(sse_event(
            "response.reasoning_summary_text.delta",
            json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": track.id,
                "output_index": track.index,
                "summary_index": 0,
                "delta": delta
            }),
        ));
        events
    }

    fn text_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let mut events = Vec::new();
        if self.text.is_none() {
            let index = self.next_index;
            self.next_index += 1;
            let id = gen_id("msg");
            self.text = Some(TextTrack {
                id: id.clone(),
                index,
                text: String::new(),
            });
            events.push(sse_event(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": index,
                    "item": {
                        "id": id,
                        "type": "message",
                        "role": "assistant",
                        "status": "in_progress",
                        "content": []
                    }
                }),
            ));
            events.push(sse_event(
                "response.content_part.added",
                json!({
                    "type": "response.content_part.added",
                    "item_id": id,
                    "output_index": index,
                    "content_index": 0,
                    "part": { "type": "output_text", "text": "", "annotations": [] }
                }),
            ));
        }
        let track = self.text.as_mut().unwrap();
        track.text.push_str(delta);
        events.push(sse_event(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "item_id": track.id,
                "output_index": track.index,
                "content_index": 0,
                "delta": delta
            }),
        ));
        events
    }

    fn tool_delta(&mut self, call: &Value) -> Vec<Bytes> {
        let tool_index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let mut events = Vec::new();
        if !self.calls.contains_key(&tool_index) {
            let output_index = self.next_index;
            self.next_index += 1;
            let item_id = gen_id("fc");
            let call_id = call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let (display_name, namespace) = self.shape.resolve(&name);
            let custom = self.shape.is_custom(&display_name);
            let item = if custom {
                json!({
                    "id": item_id,
                    "type": "custom_tool_call",
                    "status": "in_progress",
                    "call_id": call_id,
                    "name": display_name,
                    "input": ""
                })
            } else {
                let mut item = json!({
                    "id": item_id,
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": call_id,
                    "name": display_name,
                    "arguments": ""
                });
                if let Some(namespace) = namespace.clone() {
                    item["namespace"] = json!(namespace);
                }
                item
            };
            self.calls.insert(
                tool_index,
                CallTrack {
                    item_id: item_id.clone(),
                    call_id,
                    display_name,
                    namespace,
                    custom,
                    arguments: String::new(),
                    output_index,
                },
            );
            events.push(sse_event(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": item
                }),
            ));
        }
        if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str) {
            if !arguments.is_empty() {
                if let Some(track) = self.calls.get_mut(&tool_index) {
                    track.arguments.push_str(arguments);
                    let event = if track.custom {
                        "response.custom_tool_call_input.delta"
                    } else {
                        "response.function_call_arguments.delta"
                    };
                    events.push(sse_event(
                        event,
                        json!({
                            "type": event,
                            "item_id": track.item_id,
                            "output_index": track.output_index,
                            "delta": arguments
                        }),
                    ));
                }
            }
        }
        events
    }
}

/// Extracts reasoning text from a Chat delta or message.
///
/// Upstreams disagree on the field: `reasoning_content` (DeepSeek, Kimi),
/// `reasoning` (OpenRouter) or `reasoning_details[].text` (aggregated
/// providers). Both the streaming translator and the non-streaming aggregator
/// use this so the two paths cannot drift apart.
pub(crate) fn reasoning_text(delta: &Value) -> Option<&str> {
    delta
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| {
            delta
                .get("reasoning")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        })
        .or_else(|| {
            delta
                .get("reasoning_details")
                .and_then(Value::as_array)
                .and_then(|details| {
                    details.iter().find_map(|detail| {
                        detail
                            .get("text")
                            .and_then(Value::as_str)
                            .or_else(|| detail.get("summary").and_then(Value::as_str))
                    })
                })
                .filter(|text| !text.is_empty())
        })
}

pub(super) fn sse_event(name: &str, data: Value) -> Bytes {
    let mut event = Vec::with_capacity(96);
    write!(event, "event: {name}\ndata: ").expect("writing to Vec cannot fail");
    serde_json::to_writer(&mut event, &data).expect("SSE event data is serializable");
    event.extend_from_slice(b"\n\n");
    Bytes::from(event)
}

pub(super) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(super) fn gen_id(prefix: &str) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    format!(
        "{prefix}_{}_{}",
        unix_now(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}
