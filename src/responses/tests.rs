use super::*;
use serde_json::json;

fn terminal_response(events: &[String], event_name: &str) -> Value {
    for event in events {
        if event.starts_with(&format!("event: {event_name}\n")) {
            let data = event
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap();
            return serde_json::from_str(data).unwrap();
        }
    }
    panic!("missing terminal event {event_name}");
}

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
    let mut stream = ResponsesStream::new("resp_1".to_string(), "deepseek-v4.1-flash".to_string());
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
fn completed_stream_includes_accumulated_output() {
    let mut stream = ResponsesStream::new("resp_output".to_string(), "model".to_string());
    let mut events = stream.push(&json!({
        "choices": [{
            "delta": {
                "reasoning_content": "think",
                "content": "answer",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": { "name": "lookup", "arguments": "{}" }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }));
    events.extend(stream.finish());

    let terminal = terminal_response(&events, "response.completed");
    let output = terminal["response"]["output"].as_array().unwrap();
    assert_eq!(output.len(), 3);
    assert_eq!(output[0]["type"], "reasoning");
    assert_eq!(output[1]["type"], "message");
    assert_eq!(output[1]["content"][0]["text"], "answer");
    assert_eq!(output[2]["type"], "function_call");
    assert_eq!(output[2]["call_id"], "call_1");
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
