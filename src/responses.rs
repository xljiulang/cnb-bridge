//! Responses API to Chat Completions compatibility translation.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MISSING_TOOL_OUTPUT: &str =
    "Tool output was missing from conversation history and has been replaced by the proxy.";

/// Tool declarations and their Responses-only shapes for one request.
#[derive(Default, Clone)]
pub struct ToolShape {
    namespaces: HashMap<String, (String, String)>,
    custom: HashSet<String>,
}

impl ToolShape {
    /// Builds the request-local tool shape from Responses declarations.
    pub fn from_request(request: &Value) -> Self {
        let mut shape = Self::default();
        let Some(tools) = request.get("tools").and_then(Value::as_array) else {
            return shape;
        };
        for tool in tools {
            match tool.get("type").and_then(Value::as_str) {
                Some("namespace") => {
                    let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(children) = tool.get("tools").and_then(Value::as_array) else {
                        continue;
                    };
                    for child in children {
                        let Some(name) = child.get("name").and_then(Value::as_str) else {
                            continue;
                        };
                        shape.namespaces.insert(
                            flat_namespace_name(namespace, name),
                            (namespace.to_string(), name.to_string()),
                        );
                    }
                }
                Some("custom") => {
                    if let Some(name) = tool.get("name").and_then(Value::as_str) {
                        shape.custom.insert(name.to_string());
                    }
                }
                _ => {}
            }
        }
        shape
    }

    fn resolve(&self, name: &str) -> (String, Option<String>) {
        match self.namespaces.get(name) {
            Some((namespace, tool)) => (tool.clone(), Some(namespace.clone())),
            None => (name.to_string(), None),
        }
    }

    fn is_custom(&self, name: &str) -> bool {
        self.custom.contains(name)
    }
}

/// Translates a Responses request into a Chat Completions request.
pub fn responses_to_chat(request: &Value, want_stream: bool) -> Result<Value, String> {
    if !request.is_object() {
        return Err("request body must be a JSON object".to_string());
    }
    let mut messages = Vec::new();
    if let Some(instructions) = request
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        messages.push(json!({ "role": "system", "content": instructions }));
    }

    match request.get("input") {
        Some(Value::String(text)) if !text.is_empty() => {
            messages.push(json!({ "role": "user", "content": text }));
        }
        Some(Value::Array(items)) => translate_input_items(items, &mut messages),
        _ => {}
    }
    repair_tool_pairing(&mut messages);

    let mut chat = json!({
        "model": request.get("model").cloned().unwrap_or(Value::Null),
        "messages": messages,
        "stream": want_stream
    });
    if let Some(tools) = request.get("tools").and_then(Value::as_array) {
        let translated = translate_tools(tools);
        if !translated.is_empty() {
            chat["tools"] = Value::Array(translated);
        }
    }
    for (destination, source) in [
        ("max_completion_tokens", "max_output_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("seed", "seed"),
    ] {
        if let Some(value) = request.get(source).filter(|value| !value.is_null()) {
            chat[destination] = value.clone();
        }
    }
    if let Some(effort) = request
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        chat["reasoning_effort"] = json!(effort);
    }
    for field in ["parallel_tool_calls", "prompt_cache_key", "service_tier"] {
        if let Some(value) = request.get(field).filter(|value| !value.is_null()) {
            chat[field] = value.clone();
        }
    }
    if let Some(choice) = translate_tool_choice(request.get("tool_choice")) {
        chat["tool_choice"] = choice;
    }
    Ok(chat)
}

fn translate_input_items(items: &[Value], messages: &mut Vec<Value>) {
    let mut pending: Option<Value> = None;
    let mut reasoning: Option<String> = None;
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let item_type = object
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| object.contains_key("role").then_some("message"));
        match item_type {
            Some("message") => {
                flush_pending(messages, &mut pending);
                let role = object.get("role").and_then(Value::as_str).unwrap_or("user");
                let role = if role == "developer" { "system" } else { role };
                if role != "assistant" {
                    reasoning = None;
                }
                let mut message = json!({
                    "role": role,
                    "content": message_content(item)
                });
                if role == "assistant" {
                    if let Some(text) = reasoning.take() {
                        message["reasoning_content"] = json!(text);
                    }
                }
                messages.push(message);
            }
            Some("reasoning") => {
                reasoning = reasoning_item_text(item);
            }
            Some("function_call" | "custom_tool_call") => {
                let raw_name = object.get("name").and_then(Value::as_str).unwrap_or("");
                let name = object
                    .get("namespace")
                    .and_then(Value::as_str)
                    .filter(|namespace| !namespace.is_empty())
                    .map(|namespace| flat_namespace_name(namespace, raw_name))
                    .unwrap_or_else(|| raw_name.to_string());
                let call_id = object
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let arguments = if item_type == Some("custom_tool_call") {
                    json!({
                        "input": object
                            .get("input")
                            .map(value_to_text)
                            .unwrap_or_default()
                    })
                    .to_string()
                } else {
                    normalize_arguments(
                        object
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    )
                    .0
                };
                let call = json!({
                    "id": call_id,
                    "type": "function",
                    "function": { "name": name, "arguments": arguments }
                });
                if let Some(message) = pending.as_mut() {
                    if let Some(calls) = message["tool_calls"].as_array_mut() {
                        calls.push(call);
                    }
                } else {
                    let mut message = json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [call]
                    });
                    if let Some(text) = reasoning.take() {
                        message["reasoning_content"] = json!(text);
                    }
                    pending = Some(message);
                }
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                flush_pending(messages, &mut pending);
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": object
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    "content": value_to_text(object.get("output").unwrap_or(&Value::Null))
                }));
            }
            _ => {}
        }
    }
    flush_pending(messages, &mut pending);
}

fn flush_pending(messages: &mut Vec<Value>, pending: &mut Option<Value>) {
    if let Some(message) = pending.take() {
        messages.push(message);
    }
}

fn translate_tools(tools: &[Value]) -> Vec<Value> {
    let mut translated = Vec::new();
    let mut seen = HashSet::new();
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("namespace") => {
                let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let Some(children) = tool.get("tools").and_then(Value::as_array) else {
                    continue;
                };
                for child in children {
                    let Some(name) = child.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let flat = flat_namespace_name(namespace, name);
                    if seen.insert(flat.clone()) {
                        translated.push(function_tool(&flat, child));
                    }
                }
            }
            Some("custom") => {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if seen.insert(name.to_string()) {
                    let description = custom_tool_description(
                        tool.get("description")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        name,
                    );
                    translated.push(json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": description,
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "input": {
                                        "type": "string",
                                        "description": "The complete free-form tool input."
                                    }
                                },
                                "required": ["input"]
                            }
                        }
                    }));
                }
            }
            _ => {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if seen.insert(name.to_string()) {
                    translated.push(function_tool(name, tool));
                }
            }
        }
    }
    translated
}

fn function_tool(name: &str, source: &Value) -> Value {
    let mut function = json!({ "name": name });
    if let Some(description) = source.get("description") {
        function["description"] = description.clone();
    }
    if let Some(parameters) = source.get("parameters") {
        function["parameters"] = parameters.clone();
    }
    json!({ "type": "function", "function": function })
}

fn custom_tool_description(raw: &str, name: &str) -> String {
    let hint =
        "This is a function call. Put the complete free-form tool input in the JSON \"input\" field.";
    if raw.trim().is_empty() {
        format!("Run the {name} tool.\n\n{hint}")
    } else {
        format!("{}\n\n{hint}", raw.trim())
    }
}

fn translate_tool_choice(choice: Option<&Value>) -> Option<Value> {
    let choice = choice.filter(|value| !value.is_null())?;
    match choice {
        Value::String(value) => Some(json!(value)),
        Value::Object(object) if object.get("type").and_then(Value::as_str) == Some("function") => {
            let name = object.get("name").and_then(Value::as_str)?;
            Some(json!({ "type": "function", "function": { "name": name } }))
        }
        _ => None,
    }
}

fn flat_namespace_name(namespace: &str, name: &str) -> String {
    format!("{namespace}_{name}")
}

fn normalize_arguments(raw: &str) -> (String, bool) {
    let raw = raw.trim();
    if raw.is_empty() {
        return ("{}".to_string(), false);
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(_)) => (raw.to_string(), false),
        _ => ("{}".to_string(), true),
    }
}

fn repair_tool_pairing(messages: &mut Vec<Value>) {
    let mut index = 0;
    while index < messages.len() {
        let ids: Vec<String> = messages[index]
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|call| call.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        if ids.is_empty() {
            index += 1;
            continue;
        }
        let mut next = index + 1;
        let mut answered = HashSet::new();
        while next < messages.len()
            && messages[next].get("role").and_then(Value::as_str) == Some("tool")
        {
            if let Some(id) = messages[next].get("tool_call_id").and_then(Value::as_str) {
                answered.insert(id.to_string());
            }
            next += 1;
        }
        let missing: Vec<String> = ids
            .into_iter()
            .filter(|id| !answered.contains(id))
            .collect();
        for (offset, id) in missing.iter().enumerate() {
            messages.insert(
                next + offset,
                json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": MISSING_TOOL_OUTPUT
                }),
            );
        }
        index = next + missing.len();
    }
}

fn message_content(item: &Value) -> Value {
    let Some(parts) = item.get("content").and_then(Value::as_array) else {
        return item
            .get("content")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new()));
    };
    if !parts
        .iter()
        .any(|part| part.get("type").and_then(Value::as_str) == Some("input_image"))
    {
        return Value::String(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        );
    }
    let content: Vec<Value> = parts
        .iter()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text") => part
                .get("text")
                .and_then(Value::as_str)
                .map(|text| json!({ "type": "text", "text": text })),
            Some("input_image") => part.get("image_url").and_then(Value::as_str).map(|url| {
                let mut image = json!({ "url": url });
                if let Some(detail) = part
                    .get("detail")
                    .and_then(Value::as_str)
                    .filter(|detail| matches!(*detail, "auto" | "low" | "high"))
                {
                    image["detail"] = json!(detail);
                }
                json!({ "type": "image_url", "image_url": image })
            }),
            _ => None,
        })
        .collect();
    Value::Array(content)
}

fn reasoning_item_text(item: &Value) -> Option<String> {
    let collect = |key: &str| {
        let text = item
            .get(key)
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        (!text.is_empty()).then_some(text)
    };
    collect("summary").or_else(|| collect("content"))
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Translates a non-streaming Chat completion into a Responses object.
pub fn chat_to_responses(chat: &Value, model: &str, shape: &ToolShape) -> Result<Value, String> {
    let chat_id = chat
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl-local");
    let response_id = format!("resp_{}", chat_id.trim_start_matches("chatcmpl-"));
    let mut output = Vec::new();
    if let Some(choice) = chat
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    {
        let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
        if let Some(reasoning) = message
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            output.push(json!({
                "id": gen_id("rs"),
                "type": "reasoning",
                "summary": [{ "type": "summary_text", "text": reasoning }]
            }));
        }
        let content = message.get("content").and_then(Value::as_str).unwrap_or("");
        if !content.is_empty() {
            output.push(json!({
                "id": gen_id("msg"),
                "type": "message",
                "role": message
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("assistant"),
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": content,
                    "annotations": []
                }]
            }));
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                output.push(translate_tool_call(call, shape)?);
            }
        }
    }
    Ok(json!({
        "id": response_id,
        "object": "response",
        "created_at": unix_now(),
        "status": "completed",
        "model": model,
        "output": output,
        "error": null,
        "usage": usage_to_responses(chat.get("usage"))
    }))
}

fn translate_tool_call(call: &Value, shape: &ToolShape) -> Result<Value, String> {
    let call_id = call
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let name = call
        .pointer("/function/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let arguments = call
        .pointer("/function/arguments")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (name, namespace) = shape.resolve(name);
    if shape.is_custom(&name) || shape.custom.contains(&name) {
        return Ok(json!({
            "id": gen_id("ctc"),
            "type": "custom_tool_call",
            "status": "completed",
            "call_id": call_id,
            "name": name,
            "input": custom_tool_input(arguments)
        }));
    }
    let (arguments, repaired) = normalize_arguments(arguments);
    if repaired {
        return Err(format!(
            "tool call arguments for {name} are not a JSON object"
        ));
    }
    let mut item = json!({
        "id": gen_id("fc"),
        "type": "function_call",
        "status": "completed",
        "call_id": call_id,
        "name": name,
        "arguments": arguments
    });
    if let Some(namespace) = namespace {
        item["namespace"] = json!(namespace);
    }
    Ok(item)
}

fn custom_tool_input(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments.trim())
        .ok()
        .and_then(|value| {
            value
                .get("input")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| arguments.to_string())
}

/// Maps Chat token usage fields to Responses usage fields.
pub fn usage_to_responses(usage: Option<&Value>) -> Value {
    let Some(usage) = usage else {
        return json!({});
    };
    let mut output = json!({});
    for (destination, source) in [
        ("input_tokens", "prompt_tokens"),
        ("output_tokens", "completion_tokens"),
        ("total_tokens", "total_tokens"),
    ] {
        if let Some(value) = usage.get(source) {
            output[destination] = value.clone();
        }
    }
    let mut input_details = json!({});
    if let Some(value) = usage.pointer("/prompt_tokens_details/cached_tokens") {
        input_details["cached_tokens"] = value.clone();
    }
    if input_details
        .as_object()
        .is_some_and(|details| !details.is_empty())
    {
        output["input_tokens_details"] = input_details;
    }
    let mut output_details = json!({});
    if let Some(value) = usage.pointer("/completion_tokens_details/reasoning_tokens") {
        output_details["reasoning_tokens"] = value.clone();
    }
    if output_details
        .as_object()
        .is_some_and(|details| !details.is_empty())
    {
        output["output_tokens_details"] = output_details;
    }
    output
}

#[derive(Clone)]
struct TextTrack {
    id: String,
    index: usize,
    text: String,
}

#[derive(Clone)]
struct CallTrack {
    item_id: String,
    call_id: String,
    name: String,
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
            finish_reason: None,
            usage: None,
            failure: None,
            finished: false,
        }
    }

    /// Translates one Chat completion chunk into zero or more Responses SSE events.
    pub fn push(&mut self, chunk: &Value) -> Vec<String> {
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
    pub fn finish(&mut self) -> Vec<String> {
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
            events.push(sse_event(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": track.index,
                    "item": {
                        "id": track.id,
                        "type": "reasoning",
                        "summary": [{ "type": "summary_text", "text": track.text }]
                    }
                }),
            ));
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
            events.push(sse_event(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": track.index,
                    "item": {
                        "id": track.id,
                        "type": "message",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{
                            "type": "output_text",
                            "text": track.text,
                            "annotations": []
                        }]
                    }
                }),
            ));
        }
        if self.failure.is_none() {
            for call in self.calls.values() {
                let (name, _) = self.shape.resolve(&call.name);
                if !self.shape.is_custom(&name) && normalize_arguments(&call.arguments).1 {
                    self.failure = Some((
                        "malformed_tool_call_arguments".to_string(),
                        format!("tool call arguments for {name} are invalid"),
                    ));
                    break;
                }
            }
        }
        if self.failure.is_none() {
            for (_, call) in std::mem::take(&mut self.calls) {
                let (name, namespace) = self.shape.resolve(&call.name);
                let custom = self.shape.is_custom(&name);
                if custom {
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
                    events.push(sse_event(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": call.output_index,
                            "item": {
                                "id": call.item_id,
                                "type": "custom_tool_call",
                                "status": "completed",
                                "call_id": call.call_id,
                                "name": name,
                                "input": input
                            }
                        }),
                    ));
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
                        "name": name,
                        "arguments": call.arguments
                    });
                    if let Some(namespace) = namespace {
                        item["namespace"] = json!(namespace);
                    }
                    events.push(sse_event(
                        "response.output_item.done",
                        json!({ "type": "response.output_item.done", "output_index": call.output_index, "item": item }),
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
                        "output": [],
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
                        "output": [],
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
                        "output": [],
                        "error": null,
                        "usage": usage_to_responses(self.usage.as_ref())
                    }
                }),
            )
        };
        events.push(terminal);
        events
    }

    fn ensure_created(&mut self) -> Vec<String> {
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

    fn reasoning_delta(&mut self, delta: &str) -> Vec<String> {
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

    fn text_delta(&mut self, delta: &str) -> Vec<String> {
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

    fn tool_delta(&mut self, call: &Value) -> Vec<String> {
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
            let item = if self.shape.is_custom(&display_name) {
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
            self.calls.insert(
                tool_index,
                CallTrack {
                    item_id: item_id.clone(),
                    call_id,
                    name,
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
                    let custom = self.shape.is_custom(&track.name);
                    let event = if custom {
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

fn reasoning_text(delta: &Value) -> Option<&str> {
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

fn sse_event(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn gen_id(prefix: &str) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    format!(
        "{prefix}_{}_{}",
        unix_now(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn translates_instructions_messages_images_and_tools() {
        let request = json!({
            "model": "deepseek-v4.1-flash",
            "instructions": "Be concise",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "look" },
                    { "type": "input_image", "image_url": "https://example.test/a.png", "detail": "high" }
                ]
            }],
            "tools": [{
                "type": "namespace",
                "name": "codexui",
                "tools": [{
                    "type": "function",
                    "name": "get_usage",
                    "description": "usage",
                    "parameters": { "type": "object", "properties": {} }
                }]
            }, {
                "type": "custom",
                "name": "apply_patch",
                "description": "FREEFORM patch tool"
            }],
            "max_output_tokens": 123,
            "stream": true
        });

        let chat = responses_to_chat(&request, true).unwrap();
        assert_eq!(chat["messages"][0]["role"], "system");
        assert_eq!(chat["messages"][0]["content"], "Be concise");
        assert_eq!(chat["messages"][1]["content"][1]["type"], "image_url");
        assert_eq!(
            chat["messages"][1]["content"][1]["image_url"]["detail"],
            "high"
        );
        assert_eq!(chat["tools"][0]["function"]["name"], "codexui_get_usage");
        assert_eq!(chat["tools"][1]["function"]["name"], "apply_patch");
        assert_eq!(chat["max_completion_tokens"], 123);
        assert_eq!(chat["stream"], true);
    }

    #[test]
    fn restores_namespace_and_custom_tools_in_response() {
        let request = json!({
            "tools": [{
                "type": "namespace",
                "name": "codexui",
                "tools": [{ "type": "function", "name": "get_usage" }]
            }, {
                "type": "custom",
                "name": "apply_patch"
            }]
        });
        let shape = ToolShape::from_request(&request);
        let chat = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "done",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "codexui_get_usage",
                            "arguments": "{}"
                        }
                    }, {
                        "id": "call_2",
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "arguments": "{\"input\":\"*** Begin Patch\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });

        let response = chat_to_responses(&chat, "deepseek-v4.1-flash", &shape).unwrap();
        assert_eq!(response["object"], "response");
        assert_eq!(response["output"][0]["content"][0]["text"], "done");
        assert_eq!(response["output"][1]["type"], "function_call");
        assert_eq!(response["output"][1]["name"], "get_usage");
        assert_eq!(response["output"][1]["namespace"], "codexui");
        assert_eq!(response["output"][2]["type"], "custom_tool_call");
        assert_eq!(response["output"][2]["input"], "*** Begin Patch");
        assert_eq!(response["usage"]["input_tokens"], 10);
        assert_eq!(response["usage"]["output_tokens"], 5);
    }

    #[test]
    fn streams_text_and_terminal_events_in_order() {
        let mut stream =
            ResponsesStream::new("resp_1".to_string(), "deepseek-v4.1-flash".to_string());
        let mut events = stream.push(&json!({
            "choices": [{
                "delta": { "content": "hello" },
                "finish_reason": null
            }]
        }));
        events.extend(stream.push(&json!({
            "choices": [{
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5 }
        })));
        events.extend(stream.finish());

        let joined = events.concat();
        let names: Vec<&str> = joined
            .lines()
            .filter_map(|line| line.strip_prefix("event: "))
            .collect();
        assert_eq!(
            names,
            vec![
                "response.created",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        assert!(joined.contains("\"input_tokens\":3"));
        assert!(joined.contains("\"output_tokens\":2"));
    }

    #[test]
    fn repairs_missing_tool_outputs_in_history() {
        let request = json!({
            "model": "deepseek-v4.1-flash",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "lookup",
                    "arguments": "{\"q\":1}"
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": "continue"
                }
            ],
            "stream": false
        });
        let chat = responses_to_chat(&request, false).unwrap();
        assert_eq!(chat["messages"][0]["role"], "assistant");
        assert_eq!(chat["messages"][1]["role"], "tool");
        assert_eq!(chat["messages"][1]["tool_call_id"], "call_1");
        assert_eq!(chat["messages"][2]["content"], "continue");
    }

    #[test]
    fn malformed_stream_tool_arguments_fail_the_response() {
        let mut stream = ResponsesStream::new("resp_bad".to_string(), "model".to_string());
        let mut events = stream.push(&json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_bad",
                        "function": { "name": "lookup", "arguments": "{" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }));
        events.extend(stream.finish());
        let joined = events.concat();
        assert!(joined.contains("event: response.failed"));
        assert!(!joined.contains("event: response.completed"));
        assert!(joined.contains("malformed_tool_call_arguments"));
    }
}
