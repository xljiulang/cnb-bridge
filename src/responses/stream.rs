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
    added: bool,
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
    next_call_key_to_add: usize,
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
            next_call_key_to_add: 0,
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
        events.extend(self.flush_ready_calls());
        for key in self.calls.keys().copied().collect::<Vec<_>>() {
            let (added, missing_name) = self
                .calls
                .get(&key)
                .map(|call| (call.added, call.display_name.is_empty()))
                .unwrap_or((true, true));
            if added || missing_name {
                continue;
            }
            if self.calls[&key].call_id.is_empty() {
                self.calls
                    .get_mut(&key)
                    .expect("call key came from self.calls")
                    .call_id = format!("call_{key}");
            }
            events.extend(self.add_call_events(key));
        }

        let incomplete = self.finish_reason.as_deref() == Some("length");
        if self.failure.is_none() && !incomplete {
            for call in self.calls.values() {
                if call.display_name.is_empty() {
                    self.failure = Some((
                        "upstream_tool_call_dropped".to_string(),
                        "upstream returned a tool call without a function name".to_string(),
                    ));
                    break;
                }
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
                    if !input.is_empty() {
                        events.push(sse_event(
                            "response.custom_tool_call_input.delta",
                            json!({
                                "type": "response.custom_tool_call_input.delta",
                                "item_id": call.item_id,
                                "output_index": call.output_index,
                                "delta": input
                            }),
                        ));
                    }
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
                    let (arguments, repaired) = normalize_arguments(&call.arguments);
                    let arguments = if incomplete && repaired {
                        call.arguments.clone()
                    } else {
                        arguments
                    };
                    events.push(sse_event(
                        "response.function_call_arguments.done",
                        json!({
                            "type": "response.function_call_arguments.done",
                            "item_id": call.item_id,
                            "output_index": call.output_index,
                            "arguments": arguments
                        }),
                    ));
                    let mut item = json!({
                        "id": call.item_id,
                        "type": "function_call",
                        "status": "completed",
                        "call_id": call.call_id,
                        "name": call.display_name,
                        "arguments": arguments
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

    fn resolve_tool_key(&self, call: &Value) -> usize {
        if let Some(index) = call.get("index").and_then(Value::as_u64) {
            return usize::try_from(index).unwrap_or(usize::MAX - 1);
        }

        let last_key = self.calls.keys().next_back().copied();
        let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return last_key.unwrap_or(0);
        };

        if let Some((key, _)) = self.calls.iter().find(|(_, state)| state.call_id == id) {
            return *key;
        }
        match last_key {
            Some(key) => key.checked_add(1).unwrap_or(key),
            None => 0,
        }
    }

    fn flush_ready_calls(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        loop {
            let key = self.next_call_key_to_add;
            let Some(state) = self.calls.get(&key) else {
                break;
            };
            if state.added {
                self.next_call_key_to_add = self.next_call_key_to_add.saturating_add(1);
                continue;
            }
            if state.call_id.is_empty() || state.display_name.is_empty() {
                break;
            }
            events.extend(self.add_call_events(key));
            self.next_call_key_to_add = self.next_call_key_to_add.saturating_add(1);
        }
        events
    }

    fn add_call_events(&mut self, key: usize) -> Vec<Bytes> {
        let Some(state) = self.calls.get(&key) else {
            return Vec::new();
        };
        if state.added || state.call_id.is_empty() || state.display_name.is_empty() {
            return Vec::new();
        }

        let raw_name = state.display_name.clone();
        let call_id = state.call_id.clone();
        let arguments = state.arguments.clone();
        let (display_name, namespace) = self.shape.resolve(&raw_name);
        let custom = self.shape.is_custom(&display_name);
        let output_index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        let item_id = gen_id(if custom { "ctc" } else { "fc" });

        let Some(state) = self.calls.get_mut(&key) else {
            return Vec::new();
        };
        state.added = true;
        state.display_name = display_name.clone();
        state.namespace = namespace.clone();
        state.custom = custom;
        state.output_index = output_index;
        state.item_id = item_id.clone();

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
            if let Some(namespace) = namespace {
                item["namespace"] = json!(namespace);
            }
            item
        };

        let mut events = vec![sse_event(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": item
            }),
        )];
        if !custom && !arguments.is_empty() {
            events.push(sse_event(
                "response.function_call_arguments.delta",
                json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": item_id,
                    "output_index": output_index,
                    "delta": arguments
                }),
            ));
        }
        events
    }

    fn tool_delta(&mut self, call: &Value) -> Vec<Bytes> {
        let tool_index = self.resolve_tool_key(call);
        let mut events = Vec::new();
        let was_added = self.calls.get(&tool_index).is_some_and(|track| track.added);
        let state = self.calls.entry(tool_index).or_insert_with(|| CallTrack {
            item_id: String::new(),
            call_id: String::new(),
            display_name: String::new(),
            namespace: None,
            custom: false,
            arguments: String::new(),
            output_index: 0,
            added: false,
        });
        if !state.added {
            if let Some(call_id) = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|call_id| !call_id.is_empty())
            {
                state.call_id = call_id.to_string();
            }
            if let Some(name) = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
            {
                state.display_name = name.to_string();
            }
        }
        let arguments = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        state.arguments.push_str(arguments);
        let item_id = state.item_id.clone();
        let output_index = state.output_index;
        let custom = state.custom;

        events.extend(self.flush_ready_calls());
        if was_added && !arguments.is_empty() && !custom {
            events.push(sse_event(
                "response.function_call_arguments.delta",
                json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": item_id,
                    "output_index": output_index,
                    "delta": arguments
                }),
            ));
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
