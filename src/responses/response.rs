use super::*;

/// Translates a non-streaming Chat completion into a Responses object.
pub fn chat_to_responses(chat: &Value, model: &str, shape: &ToolShape) -> Result<Value, String> {
    let chat_id = chat
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl-local");
    let response_id = format!("resp_{}", chat_id.trim_start_matches("chatcmpl-"));
    let mut output = Vec::new();
    let mut finish_reason = None;
    if let Some(choice) = chat
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    {
        let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
        finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(reasoning) = reasoning_text(&message) {
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
    let status = if finish_reason.as_deref() == Some("length") {
        "incomplete"
    } else {
        "completed"
    };
    let mut response = json!({
        "id": response_id,
        "object": "response",
        "created_at": chat.get("created").and_then(Value::as_u64).unwrap_or_else(unix_now),
        "status": status,
        "model": model,
        "output": output,
        "error": null,
        "usage": usage_to_responses(chat.get("usage"))
    });
    if status == "incomplete" {
        response["incomplete_details"] = json!({ "reason": "max_output_tokens" });
    }
    if let Some(fingerprint) = chat
        .get("system_fingerprint")
        .filter(|value| !value.is_null())
    {
        response["system_fingerprint"] = fingerprint.clone();
    }
    Ok(response)
}

pub(super) fn translate_tool_call(call: &Value, shape: &ToolShape) -> Result<Value, String> {
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

pub(super) fn custom_tool_input(arguments: &str) -> String {
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
///
/// Responses clients read every counter unconditionally, so missing upstream
/// usage is reported as zeros instead of an empty object. Chat-native field
/// names win over Responses-style ones, and non-numeric values are treated as
/// absent rather than forwarded.
pub fn usage_to_responses(usage: Option<&Value>) -> Value {
    let Some(usage) = usage.and_then(Value::as_object) else {
        return zero_usage();
    };
    let input_tokens = first_number(usage, &["prompt_tokens", "input_tokens"]).unwrap_or(0);
    let output_tokens = first_number(usage, &["completion_tokens", "output_tokens"]).unwrap_or(0);
    let total_tokens =
        first_number(usage, &["total_tokens"]).unwrap_or(input_tokens + output_tokens);
    let cached_tokens = nested_number(usage, "prompt_tokens_details", "cached_tokens")
        .or_else(|| nested_number(usage, "input_tokens_details", "cached_tokens"))
        .unwrap_or(0);
    let reasoning_tokens = nested_number(usage, "completion_tokens_details", "reasoning_tokens")
        .or_else(|| nested_number(usage, "output_tokens_details", "reasoning_tokens"))
        .unwrap_or(0);
    json!({
        "input_tokens": input_tokens,
        "input_tokens_details": { "cached_tokens": cached_tokens },
        "output_tokens": output_tokens,
        "total_tokens": total_tokens,
        "output_tokens_details": { "reasoning_tokens": reasoning_tokens }
    })
}

fn zero_usage() -> Value {
    json!({
        "input_tokens": 0,
        "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens": 0,
        "total_tokens": 0,
        "output_tokens_details": { "reasoning_tokens": 0 }
    })
}

fn first_number(usage: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| usage.get(*key).and_then(Value::as_u64))
}

fn nested_number(usage: &serde_json::Map<String, Value>, outer: &str, inner: &str) -> Option<u64> {
    usage
        .get(outer)
        .and_then(|details| details.get(inner))
        .and_then(Value::as_u64)
}
