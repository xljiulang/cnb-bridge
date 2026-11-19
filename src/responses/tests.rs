use super::request::HANDLED_INPUT_TYPES;
use super::*;
use bytes::Bytes;
use serde_json::json;

fn terminal_response(events: &[Bytes], event_name: &str) -> Value {
    for event in events {
        let event = std::str::from_utf8(event).unwrap();
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
    assert_eq!(chat["max_tokens"], 123);
    assert!(chat.get("max_completion_tokens").is_none());
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
fn preserves_completion_metadata_and_incomplete_status() {
    let chat = json!({
        "id": "chatcmpl-meta",
        "created": 42,
        "system_fingerprint": "fp_1",
        "choices": [{
            "message": { "role": "assistant", "content": "partial" },
            "finish_reason": "length"
        }]
    });

    let response = chat_to_responses(&chat, "model", &ToolShape::default()).unwrap();

    assert_eq!(response["created_at"], 42);
    assert_eq!(response["status"], "incomplete");
    assert_eq!(
        response["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert_eq!(response["system_fingerprint"], "fp_1");
}

#[test]
fn reports_unsupported_responses_features_without_rejecting_them() {
    let request = json!({
        "previous_response_id": "resp_old",
        "text": { "format": { "type": "json_schema" } },
        "input": [{ "type": "mcp_call" }]
    });

    assert_eq!(
        unsupported_feature_names(&request),
        ["previous_response_id", "text.format", "input.items"]
    );
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

    let joined = String::from_utf8(events.concat()).unwrap();
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
fn removes_orphan_tool_outputs_before_translation() {
    let request = json!({
        "model": "model",
        "input": [
            { "type": "function_call_output", "call_id": "orphan", "output": "lost" },
            { "type": "message", "role": "user", "content": "continue" }
        ]
    });

    let chat = responses_to_chat(&request, false).unwrap();

    assert!(chat["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|message| message["role"] != "tool"));
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
    let joined = String::from_utf8(events.concat()).unwrap();
    assert!(joined.contains("event: response.failed"));
    assert!(!joined.contains("event: response.completed"));
    assert!(joined.contains("malformed_tool_call_arguments"));
}

#[test]
fn drops_empty_user_messages_after_compaction() {
    let request = json!({
        "model": "model",
        "input": [
            { "type": "message", "role": "user", "content": [] },
            { "type": "message", "role": "user", "content": "" },
            {
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "keep" }]
            }
        ]
    });

    let chat = responses_to_chat(&request, false).unwrap();

    let messages = chat["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "keep");
}

#[test]
fn keeps_image_only_user_message() {
    let request = json!({
        "model": "model",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_image", "image_url": "https://example.test/a.png" }]
        }]
    });

    let chat = responses_to_chat(&request, false).unwrap();

    let messages = chat["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"][0]["type"], "image_url");
}

#[test]
fn normalizes_tool_parameters_for_strict_upstreams() {
    let request = json!({
        "model": "model",
        "tools": [
            { "type": "function", "name": "no_parameters" },
            { "type": "function", "name": "null_parameters", "parameters": null },
            {
                "type": "function",
                "name": "array_root",
                "parameters": { "type": "array", "items": { "type": "string" } }
            },
            {
                "type": "function",
                "name": "bad_required",
                "parameters": { "type": "object", "properties": {}, "required": "name" }
            },
            {
                "type": "function",
                "name": "closed_nested",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "inner": { "type": "object", "additionalProperties": false }
                    }
                }
            }
        ]
    });

    let chat = responses_to_chat(&request, false).unwrap();
    let tools = chat["tools"].as_array().unwrap();

    let empty_schema = json!({ "type": "object", "properties": {} });
    assert_eq!(tools[0]["function"]["parameters"], empty_schema);
    assert_eq!(tools[1]["function"]["parameters"], empty_schema);
    assert_eq!(tools[2]["function"]["parameters"]["type"], "object");
    assert_eq!(
        tools[2]["function"]["parameters"]["items"]["type"],
        "string"
    );
    assert_eq!(tools[3]["function"]["parameters"]["required"], json!([]));
    assert_eq!(
        tools[4]["function"]["parameters"]["properties"]["inner"]["required"],
        json!([])
    );
}

#[test]
fn leaves_schema_default_values_untouched() {
    let request = json!({
        "model": "model",
        "tools": [{
            "type": "function",
            "name": "with_default",
            "parameters": {
                "type": "object",
                "properties": {
                    "y": {
                        "type": "object",
                        "default": { "additionalProperties": false, "required": "nope" }
                    }
                }
            }
        }]
    });

    let chat = responses_to_chat(&request, false).unwrap();
    let default = &chat["tools"][0]["function"]["parameters"]["properties"]["y"]["default"];

    assert_eq!(default["additionalProperties"], false);
    assert_eq!(default["required"], "nope");
}

#[test]
fn usage_defaults_to_zero_when_upstream_omits_it() {
    let zero = json!({
        "input_tokens": 0,
        "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens": 0,
        "total_tokens": 0,
        "output_tokens_details": { "reasoning_tokens": 0 }
    });

    assert_eq!(usage_to_responses(None), zero);
    assert_eq!(usage_to_responses(Some(&json!({}))), zero);
    assert_eq!(usage_to_responses(Some(&json!("oops"))), zero);
}

#[test]
fn usage_fills_missing_details_and_accepts_responses_names() {
    let usage = usage_to_responses(Some(&json!({
        "input_tokens": 7,
        "output_tokens": 4,
        "input_tokens_details": { "cached_tokens": 2 }
    })));

    assert_eq!(usage["input_tokens"], 7);
    assert_eq!(usage["output_tokens"], 4);
    assert_eq!(usage["total_tokens"], 11);
    assert_eq!(usage["input_tokens_details"]["cached_tokens"], 2);
    assert_eq!(usage["output_tokens_details"]["reasoning_tokens"], 0);
}

fn roles(messages: &[Value]) -> Vec<&str> {
    messages
        .iter()
        .filter_map(|message| message["role"].as_str())
        .collect()
}

fn large_image_data_url() -> String {
    format!("data:image/png;base64,{}", "A".repeat(9000))
}

#[test]
fn moves_tool_output_image_into_following_user_message() {
    let url = large_image_data_url();
    let request = json!({
        "model": "model",
        "input": [
            {
                "type": "function_call",
                "call_id": "call_1",
                "name": "view_image",
                "arguments": "{}"
            },
            {
                "type": "function_call_output",
                "call_id": "call_1",
                "output": {
                    "content": [
                        { "type": "input_text", "text": "screenshot" },
                        { "type": "input_image", "image_url": url }
                    ]
                }
            }
        ]
    });

    let chat = responses_to_chat(&request, false).unwrap();
    let messages = chat["messages"].as_array().unwrap();

    assert_eq!(roles(messages), ["assistant", "tool", "user"]);
    let tool = messages[1]["content"].as_str().unwrap();
    assert!(tool.contains("moved to the following user message"));
    assert!(!tool.contains("base64"));
    assert_eq!(messages[2]["content"][0]["type"], "text");
    assert_eq!(messages[2]["content"][1]["type"], "image_url");
    assert_eq!(messages[2]["content"][1]["image_url"]["url"], url);
}

#[test]
fn keeps_parallel_tool_results_contiguous_around_media() {
    let url = large_image_data_url();
    let request = json!({
        "model": "model",
        "input": [
            { "type": "function_call", "call_id": "c1", "name": "shot", "arguments": "{}" },
            { "type": "function_call", "call_id": "c2", "name": "read", "arguments": "{}" },
            {
                "type": "function_call_output",
                "call_id": "c1",
                "output": {
                    "content": [{ "type": "input_image", "image_url": url }]
                }
            },
            { "type": "function_call_output", "call_id": "c2", "output": "ok" }
        ]
    });

    let chat = responses_to_chat(&request, false).unwrap();
    let messages = chat["messages"].as_array().unwrap();

    assert_eq!(roles(messages), ["assistant", "tool", "tool", "user"]);
    assert_eq!(messages[1]["tool_call_id"], "c1");
    assert_eq!(messages[2]["tool_call_id"], "c2");
    assert!(messages[3]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("c1"));
}

#[test]
fn injects_missing_tool_placeholder_before_media_message() {
    let url = large_image_data_url();
    let request = json!({
        "model": "model",
        "input": [
            { "type": "function_call", "call_id": "c1", "name": "shot", "arguments": "{}" },
            { "type": "function_call", "call_id": "c2", "name": "read", "arguments": "{}" },
            {
                "type": "function_call_output",
                "call_id": "c1",
                "output": {
                    "content": [{ "type": "input_image", "image_url": url }]
                }
            }
        ]
    });

    let chat = responses_to_chat(&request, false).unwrap();
    let messages = chat["messages"].as_array().unwrap();

    assert_eq!(roles(messages), ["assistant", "tool", "tool", "user"]);
    assert_eq!(messages[1]["tool_call_id"], "c1");
    assert_eq!(messages[2]["tool_call_id"], "c2");
    assert_eq!(messages[2]["content"], MISSING_TOOL_OUTPUT);
    assert_eq!(messages[3]["content"][1]["type"], "image_url");
}

#[test]
fn extracts_nested_media_from_custom_tool_call_output() {
    let url = large_image_data_url();
    let output = json!({
        "content": [{ "type": "input_image", "image_url": url }]
    })
    .to_string();
    let request = json!({
        "model": "model",
        "input": [
            {
                "type": "custom_tool_call",
                "call_id": "call_9",
                "name": "apply_patch",
                "input": "*** Begin Patch"
            },
            { "type": "custom_tool_call_output", "call_id": "call_9", "output": output }
        ]
    });

    let chat = responses_to_chat(&request, false).unwrap();
    let messages = chat["messages"].as_array().unwrap();

    assert_eq!(roles(messages), ["assistant", "tool", "user"]);
    let tool = messages[1]["content"].as_str().unwrap();
    assert!(tool.contains("moved to the following user message"));
    assert!(!tool.contains("base64"));
    assert!(serde_json::from_str::<Value>(tool).is_ok());
    assert_eq!(messages[2]["content"][1]["image_url"]["url"], url);
}

#[test]
fn leaves_tool_output_without_media_unchanged() {
    let request = json!({
        "model": "model",
        "input": [
            { "type": "function_call", "call_id": "c1", "name": "read", "arguments": "{}" },
            { "type": "function_call_output", "call_id": "c1", "output": "ok" }
        ]
    });

    let chat = responses_to_chat(&request, false).unwrap();
    let messages = chat["messages"].as_array().unwrap();

    assert_eq!(roles(messages), ["assistant", "tool"]);
    assert_eq!(messages[1]["content"], "ok");
}

#[test]
fn keeps_first_declaration_when_flat_tool_names_collide() {
    // `ns` + `exec` and the bare function `ns_exec` flatten to the same Chat
    // name. The first declaration is what the upstream is told about, so the
    // losing one must not reach the tool list.
    let request = json!({
        "model": "model",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "hi" }]
        }],
        "tools": [{
            "type": "namespace",
            "name": "ns",
            "tools": [{
                "type": "function",
                "name": "exec",
                "description": "from namespace",
                "parameters": {
                    "type": "object",
                    "properties": { "cmd": { "type": "string" } }
                }
            }]
        }, {
            "type": "function",
            "name": "ns_exec",
            "description": "from top level",
            "parameters": { "type": "object", "properties": {} }
        }]
    });

    let chat = responses_to_chat(&request, false).unwrap();
    let tools = chat["tools"].as_array().unwrap();

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["function"]["name"], "ns_exec");
    assert_eq!(tools[0]["function"]["description"], "from namespace");
    assert!(tools[0]["function"]["parameters"]["properties"]["cmd"].is_object());
}

#[test]
fn reverse_maps_only_surviving_flat_tool_names() {
    let namespace_first = json!({
        "tools": [{
            "type": "namespace",
            "name": "ns",
            "tools": [{ "type": "function", "name": "exec" }]
        }, {
            "type": "function",
            "name": "ns_exec"
        }]
    });
    let shape = ToolShape::from_request(&namespace_first);
    assert_eq!(
        shape.resolve("ns_exec"),
        ("exec".to_string(), Some("ns".to_string()))
    );

    let function_first = json!({
        "tools": [{
            "type": "function",
            "name": "ns_exec"
        }, {
            "type": "namespace",
            "name": "ns",
            "tools": [{ "type": "function", "name": "exec" }]
        }]
    });
    let shape = ToolShape::from_request(&function_first);
    assert_eq!(shape.resolve("ns_exec"), ("ns_exec".to_string(), None));
}

#[test]
fn reports_duplicate_flat_tool_names() {
    let request = json!({
        "tools": [{
            "type": "function",
            "name": "ns_exec"
        }],
        "input": [{
            "type": "additional_tools",
            "role": "developer",
            "tools": [{
                "type": "namespace",
                "name": "ns",
                "tools": [{ "type": "function", "name": "exec" }]
            }]
        }]
    });

    let details = ignored_feature_details(&request);

    assert!(
        details.contains(&"tools.duplicate:ns_exec kept=top dropped=additional_tools".to_string()),
        "{details:?}"
    );
}

#[test]
fn reads_reasoning_from_reasoning_details() {
    assert_eq!(
        reasoning_text(&json!({ "reasoning_details": [{ "text": "think" }] })),
        Some("think")
    );
    assert_eq!(
        reasoning_text(&json!({ "reasoning_details": [{ "summary": "short" }] })),
        Some("short")
    );
    // The explicit field still wins when both are present.
    assert_eq!(
        reasoning_text(&json!({
            "reasoning_content": "primary",
            "reasoning_details": [{ "text": "secondary" }]
        })),
        Some("primary")
    );
}

#[test]
fn non_stream_message_reads_reasoning_details() {
    let chat = json!({
        "id": "chatcmpl-1",
        "choices": [{
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "done",
                "reasoning_details": [{ "text": "because" }]
            }
        }]
    });

    let response = chat_to_responses(&chat, "model", &ToolShape::default()).unwrap();

    assert_eq!(response["output"][0]["type"], "reasoning");
    assert_eq!(response["output"][0]["summary"][0]["text"], "because");
}

#[test]
fn reports_ignored_input_items_and_content_parts() {
    let request = json!({
        "input": [
            { "type": "web_search_call" },
            "not-an-object",
            { "no_type": true },
            {
                "type": "message",
                "role": "user",
                "content": [
                    { "type": "input_audio", "input_audio": { "data": "x" } },
                    { "type": "input_text", "text": "kept" }
                ]
            },
            { "type": "web_search_call" }
        ]
    });

    let details = ignored_feature_details(&request);

    assert_eq!(
        details,
        [
            "content.part:input_audio",
            "input.item:non_object",
            "input.item:none",
            "input.item:web_search_call"
        ]
    );
}

#[test]
fn reports_nothing_for_fully_translated_request() {
    let request = json!({
        "instructions": "be brief",
        "input": [
            { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
            { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "thinking" }] },
            { "type": "function_call", "call_id": "c1", "name": "read", "arguments": "{}" },
            { "type": "function_call_output", "call_id": "c1", "output": "ok" },
            {
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            }
        ],
        "tools": [{
            "type": "namespace",
            "name": "ns",
            "tools": [{ "type": "function", "name": "exec" }]
        }]
    });

    assert_eq!(ignored_feature_details(&request), Vec::<String>::new());
}

#[test]
fn handled_input_types_stay_in_sync() {
    for name in HANDLED_INPUT_TYPES {
        let request = json!({ "input": [{ "type": name, "role": "user", "content": [] }] });
        let details = ignored_feature_details(&request);

        assert!(
            !details
                .iter()
                .any(|detail| detail == &format!("input.item:{name}")),
            "handled type {name} is reported as dropped: {details:?}"
        );
    }
}
