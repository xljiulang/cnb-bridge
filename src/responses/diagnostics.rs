//! Observability for Responses features the translation layer drops.
//!
//! `responses_to_chat` is a pure function with no logger, so a request can lose
//! items, content parts or whole tool declarations without anything appearing
//! in the log. The endpoint calls [`ignored_feature_details`] once per request
//! to record that degradation.

use super::request::is_handled_input_type;
use super::*;

/// Content part types `message_content` carries into the Chat message.
const HANDLED_CONTENT_PARTS: [&str; 3] = ["input_text", "output_text", "input_image"];

/// Lists, in a stable order, everything the translation layer accepted but
/// could not carry into the Chat request.
pub fn ignored_feature_details(request: &Value) -> Vec<String> {
    let mut details = Vec::new();
    collect_input_items(request, &mut details);
    collect_content_parts(request, &mut details);
    details.extend(collect_declared_tools(request).notes);
    details.sort();
    details.dedup();
    details
}

fn collect_input_items(request: &Value, details: &mut Vec<String>) {
    let Some(items) = request.get("input").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(object) = item.as_object() else {
            details.push("input.item:non_object".to_string());
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            Some(name) => {
                if !is_handled_input_type(name) {
                    details.push(format!("input.item:{name}"));
                }
            }
            // An item without a type is treated as a message only when it
            // carries a role, matching the fallback in `translate_input_items`.
            None if !object.contains_key("role") => details.push("input.item:none".to_string()),
            None => {}
        }
    }
}

fn collect_content_parts(request: &Value, details: &mut Vec<String>) {
    let Some(items) = request.get("input").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if part.get("text").and_then(Value::as_str).is_some() {
                continue;
            }
            match part.get("type").and_then(Value::as_str) {
                Some(name) if HANDLED_CONTENT_PARTS.contains(&name) => {}
                Some(name) => details.push(format!("content.part:{name}")),
                None => details.push("content.part:none".to_string()),
            }
        }
    }
}
