use super::*;

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

/// Lists accepted-but-unsupported Responses features for compatibility logging.
pub fn unsupported_feature_names(request: &Value) -> Vec<&'static str> {
    let mut features = Vec::new();
    if request.get("previous_response_id").is_some() {
        features.push("previous_response_id");
    }
    if request.pointer("/text/format").is_some() {
        features.push("text.format");
    }
    if request
        .get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some(
                        "local_shell_call"
                            | "local_shell_call_output"
                            | "computer_call"
                            | "computer_call_output"
                            | "mcp_call"
                            | "mcp_list_tools"
                    )
                )
            })
        })
    {
        features.push("input.items");
    }
    features
}

pub(super) fn translate_input_items(items: &[Value], messages: &mut Vec<Value>) {
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

pub(super) fn flush_pending(messages: &mut Vec<Value>, pending: &mut Option<Value>) {
    if let Some(message) = pending.take() {
        messages.push(message);
    }
}

pub(super) fn repair_tool_pairing(messages: &mut Vec<Value>) {
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
    let mut known_calls = HashSet::new();
    messages.retain(|message| {
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for id in calls
                .iter()
                .filter_map(|call| call.get("id").and_then(Value::as_str))
            {
                known_calls.insert(id.to_string());
            }
        }
        if message.get("role").and_then(Value::as_str) == Some("tool") {
            message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .is_some_and(|id| known_calls.contains(id))
        } else {
            true
        }
    });
}

pub(super) fn message_content(item: &Value) -> Value {
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

pub(super) fn reasoning_item_text(item: &Value) -> Option<String> {
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

pub(super) fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
