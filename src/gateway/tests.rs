use super::*;
use crate::account::AccountStore;
use crate::logging::Logger;
use crate::settings::SettingsStore;
use crate::ssh_key::SshKeyStore;
use axum::routing::post;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const HARNESS_USER_AGENT: &str = concat!(
    "deepseek-harness/0.2.2-rc.1 ",
    "(+https://github.com/deepseek-ai/deepseek-harness)"
);

#[test]
fn gateway_config_uses_request_driven_defaults() {
    let config = GatewayConfig::new(11233);
    assert_eq!(config.activation_timeout, Duration::from_secs(180));
    assert_eq!(config.idle_timeout, Duration::from_secs(5 * 60));
    assert_eq!(config.upstream_connect_timeout, Duration::from_secs(5));
    assert_eq!(config.upstream_read_timeout, Duration::from_secs(60));
    assert_eq!(config.non_stream_timeout, Duration::from_secs(120));
}

#[test]
fn forwarded_headers_force_identity_content_encoding() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("gzip, br"),
    );

    let forwarded = forwarded_request_headers(&headers, 11233, 42);

    assert_eq!(forwarded.get(header::ACCEPT_ENCODING).unwrap(), "identity");
    assert_eq!(forwarded.get_all(header::ACCEPT_ENCODING).iter().count(), 1);
}

#[test]
fn only_closes_after_five_idle_minutes_without_active_connections() {
    let timeout = Duration::from_secs(5 * 60);
    assert!(!should_close_idle_tunnel(
        1,
        Duration::from_secs(301),
        timeout
    ));
    assert!(!should_close_idle_tunnel(
        0,
        Duration::from_secs(299),
        timeout
    ));
    assert!(should_close_idle_tunnel(0, timeout, timeout));
}

#[tokio::test]
async fn activity_tracks_requests_and_last_use() {
    let activity = Arc::new(GatewayActivity::new());
    let first = activity.enter();
    let second = activity.enter();
    assert_eq!(activity.active_connections(), 2);
    drop(first);
    assert_eq!(activity.active_connections(), 1);
    drop(second);
    assert_eq!(activity.active_connections(), 0);
    assert!(activity.idle_for() < Duration::from_secs(1));
}

async fn test_gateway() -> (tempfile::TempDir, Arc<Bridge>, GatewayHandle) {
    let temp = tempfile::tempdir().unwrap();
    let account = crate::account::AccountManager::with_store(AccountStore::new(
        temp.path().join("account.bin"),
    ));
    let bridge = Arc::new(Bridge::new(
        account,
        {
            let store = SettingsStore::new(temp.path().join("settings.json"));
            store
                .save(&crate::settings::Settings {
                    http_log_filter: crate::settings::HttpLogFilter::All,
                    ..crate::settings::Settings::default()
                })
                .unwrap();
            store
        },
        SshKeyStore::new(
            temp.path().join("id_ed25519"),
            temp.path().join("id_ed25519.pub"),
        ),
        Arc::new(Logger::new(32)),
    ));
    let gateway = GatewayHandle::start_with_upstream_internal(
        Arc::clone(&bridge),
        GatewayConfig {
            port: 0,
            activation_timeout: Duration::from_secs(1),
            idle_timeout: Duration::from_secs(60),
            upstream_connect_timeout: Duration::from_secs(5),
            upstream_read_timeout: Duration::from_secs(60),
            non_stream_timeout: Duration::from_secs(120),
        },
        None,
        temp.path().join("http-trace"),
    )
    .await
    .unwrap();
    (temp, bridge, gateway)
}

#[tokio::test]
async fn health_request_is_answered_locally_without_activation() {
    let (_temp, bridge, gateway) = test_gateway().await;
    let mut stream = TcpStream::connect(("127.0.0.1", gateway.port()))
        .await
        .unwrap();
    stream
        .write_all(b"GET /v1/healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();

    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains("local-gateway"));
    assert_eq!(
        bridge.snapshot().await.state,
        crate::bridge::BridgeState::SignedOut
    );
    gateway.stop().await;
}

#[tokio::test]
async fn models_are_served_locally_from_the_embedded_catalog() {
    let (_temp, bridge, gateway) = test_gateway().await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", gateway.port());
    let list = client
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), reqwest::StatusCode::OK);
    let payload: serde_json::Value = list.json().await.unwrap();
    assert_eq!(payload["object"], "list");
    assert_eq!(payload["data"].as_array().unwrap().len(), 1);
    assert_eq!(payload["data"][0]["id"], "deepseek-v4.1-flash");
    for model in payload["data"].as_array().unwrap() {
        assert_eq!(model["context_window"], 1_048_576);
        assert_eq!(model["max_context_window"], 1_048_576);
        assert_eq!(model["input_modalities"], json!(["text", "image"]));
        assert_eq!(model["output_modalities"], json!(["text"]));
        assert_eq!(model["supports_images"], true);
        assert_eq!(model["supports_tools"], true);
        assert_eq!(model["supports_streaming"], true);
        assert_eq!(model["capabilities"]["image_input"], true);
    }

    let detail = client
        .get(format!("{base}/v1/models/deepseek-v4.1-flash"))
        .send()
        .await
        .unwrap();
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    assert_eq!(
        detail.json::<serde_json::Value>().await.unwrap()["id"],
        "deepseek-v4.1-flash"
    );
    let deepseek_flash = client
        .get(format!("{base}/v1/models/deepseek-flash"))
        .send()
        .await
        .unwrap();
    assert_eq!(deepseek_flash.status(), reqwest::StatusCode::NOT_FOUND);
    let missing = client
        .get(format!("{base}/v1/models/other"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        bridge.snapshot().await.state,
        crate::bridge::BridgeState::SignedOut
    );
    gateway.stop().await;
}

#[tokio::test]
async fn chat_and_responses_are_routed_through_the_local_http_proxy() {
    let captured = Arc::new(Mutex::new(None::<(String, Value)>));
    let captured_for_upstream = Arc::clone(&captured);
    let upstream = Router::new().route(
            "/chat/completions",
            post(move |uri: axum::http::Uri, body: Bytes| {
                let captured = Arc::clone(&captured_for_upstream);
                async move {
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    *captured.lock().unwrap() =
                        Some((uri.query().unwrap_or_default().to_string(), value.clone()));
                    if value["messages"]
                        .as_array()
                        .is_some_and(|messages| messages.iter().any(|message| {
                            message.get("content").and_then(Value::as_str) == Some("hello")
                        }))
                    {
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"chatcmpl-1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2,\"total_tokens\":5}}\n\ndata: [DONE]\n\n",
                        )
                            .into_response()
                    } else {
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"choices\":[{\"delta\":{\"content\":\"chat-ok\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
                        )
                            .into_response()
                    }
                }
            }),
        );
    let upstream_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = upstream_listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(upstream_listener, upstream).await.unwrap();
    });

    let (temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", gateway.port());

    let chat = client
        .post(format!("{base}/v1/chat/completions?trace=1"))
        .header(header::AUTHORIZATION, "Bearer client")
        .json(&json!({
            "model": "another-model",
            "messages": [{ "role": "user", "content": "chat" }],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(chat.status(), reqwest::StatusCode::OK);
    assert!(chat.text().await.unwrap().contains("chat-ok"));
    let (query, body) = captured.lock().unwrap().clone().unwrap();
    assert_eq!(query, "trace=1");
    assert_eq!(body["model"], "another-model");

    let response = client
        .post(format!("{base}/v1/responses"))
        .header(header::USER_AGENT, HARNESS_USER_AGENT)
        .json(&json!({
            "model": "deepseek-v4.1-flash",
            "instructions": concat!(
                "DeepSeek Harness and Codex. ",
                "Tokens prefixed with @ are paths the user explicitly referenced. ",
                "Relative paths resolve from the workspace root; ",
                "absolute paths identify files or directories on the host."
            ),
            "input": "hello",
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["object"], "response");
    assert_eq!(payload["output"][0]["content"][0]["text"], "hello");
    assert_eq!(payload["usage"]["input_tokens"], 3);
    let (_, translated) = captured.lock().unwrap().clone().unwrap();
    assert_eq!(translated["stream"], true);
    assert_eq!(
        translated["messages"][0]["content"],
        concat!(
            "Harness of DeepSeek and Codex. ",
            "Paths beginning with @ refer to resources the user explicitly named. ",
            "Relative paths are resolved from the workspace root; ",
            "absolute paths refer to host files or directories."
        )
    );
    assert_eq!(translated["messages"][1]["content"], "hello");

    gateway.stop().await;
    let store = HttpTraceStore::new(temp.path().join("http-trace"));
    let summaries = store.list(10).unwrap();
    assert_eq!(summaries.len(), 2);
    let chat_record = summaries
        .iter()
        .find(|summary| summary.id.ends_with("_chat"))
        .unwrap();
    assert_eq!(chat_record.protocol, "chat");
    let (chat_request, chat_response) = store.record_paths(&chat_record.id).unwrap();
    assert!(std::fs::read_to_string(&chat_request)
        .unwrap()
        .starts_with("POST /chat/completions?trace=1 HTTP/1.1"));
    assert!(std::fs::read_to_string(&chat_request)
        .unwrap()
        .contains("authorization: Bearer client"));
    assert!(std::fs::read_to_string(&chat_request)
        .unwrap()
        .contains("another-model"));
    let chat_http = std::fs::read_to_string(chat_response).unwrap();
    assert!(chat_http.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(chat_http.contains("content-type: text/event-stream"));
    assert!(chat_http.contains("chat-ok"));

    let responses_record = summaries
        .iter()
        .find(|summary| summary.id.ends_with("_responses"))
        .unwrap();
    assert_eq!(responses_record.protocol, "responses->chat");
    let (responses_request, responses_response) = store.record_paths(&responses_record.id).unwrap();
    assert!(std::fs::read_to_string(&responses_request)
        .unwrap()
        .starts_with("POST /chat/completions HTTP/1.1"));
    assert!(std::fs::read_to_string(&responses_request)
        .unwrap()
        .contains("\"stream\":true"));
    let responses_http = std::fs::read_to_string(responses_response).unwrap();
    assert!(responses_http.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(responses_http.contains("content-type: text/event-stream"));
    assert!(responses_http.contains("hello"));
    assert!(!responses_http.contains("\"object\":\"response\""));
    upstream_task.abort();
}

#[tokio::test]
async fn chat_nonstream_aggregates_forced_upstream_stream() {
    let captured = Arc::new(Mutex::new(None::<(HeaderMap, Value)>));
    let captured_for_upstream = Arc::clone(&captured);
    let upstream = Router::new().route(
            "/chat/completions",
            post(move |headers: HeaderMap, body: Bytes| {
                let captured = Arc::clone(&captured_for_upstream);
                async move {
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    *captured.lock().unwrap() = Some((headers, value));
                    (
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        concat!(
                            "data: {\"id\":\"chatcmpl-upstream\",\"model\":\"deepseek-v4.1-flash\",",
                            "\"created\":123,\"system_fingerprint\":\"fp-upstream\",",
                            "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",",
                            "\"content\":\"\",\"reasoning_content\":\"think \"},\"finish_reason\":null}]}\n\n",
                            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\",",
                            "\"reasoning_content\":\"done\"},\"finish_reason\":null}]}\n\n",
                            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,",
                            "\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",",
                            "\"arguments\":\"{\\\"q\\\":\"}}]},\"finish_reason\":null}]}\n\n",
                            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,",
                            "\"function\":{\"arguments\":\"\\\"x\\\"}\"}}]},\"finish_reason\":\"tool_calls\"},",
                            "{\"index\":1,\"delta\":{\"content\":\"second\"},\"finish_reason\":\"stop\"}],",
                            "\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":3,\"total_tokens\":7}}\n\n",
                            "data: [DONE]\n\n"
                        ),
                    )
                        .into_response()
                }
            }),
        );
    let upstream_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = upstream_listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(upstream_listener, upstream).await.unwrap();
    });

    let (temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            gateway.port()
        ))
        .json(&json!({
            "model": "hy4-flash",
            "messages": [{ "role": "user", "content": "hello" }],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert!(response.headers().contains_key(REQUEST_ID_HEADER));
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["object"], "chat.completion");
    assert_eq!(payload["id"], "chatcmpl-upstream");
    assert_eq!(payload["model"], "deepseek-v4.1-flash");
    assert_eq!(payload["created"], 123);
    assert_eq!(payload["system_fingerprint"], "fp-upstream");
    assert_eq!(payload["choices"][0]["index"], 0);
    assert_eq!(payload["choices"][0]["message"]["content"], "OK");
    assert_eq!(
        payload["choices"][0]["message"]["reasoning_content"],
        "think done"
    );
    assert_eq!(
        payload["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"q\":\"x\"}"
    );
    assert_eq!(payload["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(payload["choices"][1]["message"]["content"], "second");
    assert_eq!(payload["choices"][1]["finish_reason"], "stop");
    assert_eq!(payload["usage"]["total_tokens"], 7);

    let (headers, upstream_body) = captured.lock().unwrap().clone().unwrap();
    assert_eq!(upstream_body["stream"], true);
    assert_eq!(
        headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    gateway.stop().await;
    let store = HttpTraceStore::new(temp.path().join("http-trace"));
    let summary = store.list(1).unwrap().remove(0);
    let (request_path, response_path) = store.record_paths(&summary.id).unwrap();
    assert!(std::fs::read_to_string(request_path)
        .unwrap()
        .contains("\"stream\":true"));
    assert!(std::fs::read_to_string(response_path)
        .unwrap()
        .contains("content-type: text/event-stream"));
    upstream_task.abort();
}

#[test]
fn chat_preparation_forces_upstream_stream_and_keeps_client_intent() {
    for (stream, expected_downstream_stream) in
        [(None, false), (Some(false), false), (Some(true), true)]
    {
        let mut request = json!({
            "model": "hy4-flash",
            "messages": [{ "role": "user", "content": "hello" }]
        });
        if let Some(stream) = stream {
            request["stream"] = json!(stream);
        }
        let prepared = prepare_chat_request(Bytes::from(request.to_string()), &HeaderMap::new());
        let upstream: Value = serde_json::from_slice(&prepared.body).unwrap();

        assert_eq!(upstream["stream"], true);
        assert_eq!(prepared.downstream_stream, expected_downstream_stream);
        assert_eq!(prepared.requested_model, "hy4-flash");
        assert!(prepared.valid_json_object);
    }
}

#[test]
fn chat_preparation_reuses_unchanged_streaming_body() {
    let original = Bytes::from_static(
        br#"{"model":"hy4-flash","messages":[{"role":"user","content":"hello"}],"stream":true}"#,
    );
    let original_pointer = original.as_ptr();

    let prepared = prepare_chat_request(original, &HeaderMap::new());

    assert_eq!(prepared.body.as_ptr(), original_pointer);
    assert!(prepared.downstream_stream);
    assert!(!prepared.gate_report.changed());
}

#[tokio::test]
async fn chat_streaming_response_remains_sse() {
    let upstream = Router::new().route(
        "/chat/completions",
        post(|| async {
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                concat!(
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},",
                    "\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
                .into_response()
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (_temp, _bridge, gateway) = test_gateway_with_upstream(port).await;
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            gateway.port()
        ))
        .json(&json!({
            "model": "hy4-flash",
            "messages": [{ "role": "user", "content": "hello" }],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let text = response.text().await.unwrap();
    assert!(text.contains("data: {"));
    assert!(text.contains("data: [DONE]"));

    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn chat_nonstream_passes_through_unexpected_upstream_json() {
    let upstream = Router::new().route(
        "/chat/completions",
        post(|| async {
            Json(json!({
                "id": "chatcmpl-json",
                "object": "chat.completion",
                "model": "deepseek-v4.1-flash",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "json" },
                    "finish_reason": "stop"
                }]
            }))
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (_temp, _bridge, gateway) = test_gateway_with_upstream(port).await;
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            gateway.port()
        ))
        .json(&json!({
            "model": "hy4-flash",
            "messages": [{ "role": "user", "content": "hello" }],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["id"], "chatcmpl-json");
    assert_eq!(payload["choices"][0]["message"]["content"], "json");

    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn chat_nonstream_returns_an_error_for_in_stream_failures() {
    let upstream = Router::new().route(
        "/chat/completions",
        post(|| async {
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                concat!(
                    "data: {\"error\":{\"message\":\"upstream exploded\"}}\n\n",
                    "data: [DONE]\n\n"
                ),
            )
                .into_response()
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (_temp, _bridge, gateway) = test_gateway_with_upstream(port).await;
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            gateway.port()
        ))
        .json(&json!({
            "model": "hy4-flash",
            "messages": [{ "role": "user", "content": "hello" }],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "upstream_server_error");
    assert!(payload["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("upstream exploded")));

    gateway.stop().await;
    upstream_task.abort();
}

#[test]
fn chat_completion_aggregation_enforces_the_size_limit() {
    assert!(!aggregate_response_too_large(BODY_LIMIT - 1, 1));
    assert!(aggregate_response_too_large(BODY_LIMIT, 1));
    assert!(aggregate_response_too_large(usize::MAX, 1));
}

#[test]
fn chat_completion_aggregation_rejects_in_stream_errors() {
    let mut accumulator = ChatCompletionAccumulator::default();
    let error = accumulator
        .push_data(r#"{"error":{"message":"upstream exploded"}}"#)
        .unwrap_err();

    assert!(error.contains("上游流内错误"));
    assert!(error.contains("upstream exploded"));
}

#[test]
fn chat_completion_aggregation_rejects_truncated_streams() {
    let mut accumulator = ChatCompletionAccumulator::default();
    accumulator
        .push_data(r#"{"choices":[{"index":0,"delta":{"content":"half"},"finish_reason":null}]}"#)
        .unwrap();

    let error = accumulator.finish("hy4-flash").unwrap_err();

    assert!(error.contains("缺少完成分片"));
}

#[test]
fn chat_completion_aggregation_rejects_invalid_sse_json() {
    let mut accumulator = ChatCompletionAccumulator::default();
    let error = process_aggregate_line(b"data: not-json\n", &mut accumulator).unwrap_err();

    assert_eq!(error.0, StatusCode::BAD_GATEWAY);
    assert_eq!(error.1, "upstream_server_error");
}

#[tokio::test]
async fn responses_stream_is_translated_and_logged_without_buffering() {
    let upstream = Router::new().route(
            "/chat/completions",
            post(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "data: {\"choices\":[{\"delta\":{\"content\":\"a\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"b\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\ndata: [DONE]\n\n",
                )
                    .into_response()
            }),
        );
    let upstream_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = upstream_listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(upstream_listener, upstream).await.unwrap();
    });
    let (temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", gateway.port());
    let response = client
        .post(format!("{base}/v1/responses"))
        .json(&json!({
            "model": "deepseek-v4.1-flash",
            "input": "stream please",
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let text = response.text().await.unwrap();
    assert!(text.contains("event: response.output_text.delta"));
    assert!(text.contains("\"delta\":\"a\""));
    assert!(text.contains("\"delta\":\"b\""));
    assert!(text.contains("event: response.completed"));
    assert!(text.contains("\"input_tokens\":1"));
    gateway.stop().await;

    let store = HttpTraceStore::new(temp.path().join("http-trace"));
    let summary = store.list(1).unwrap().remove(0);
    assert!(summary.stream);
    assert_eq!(summary.protocol, "responses->chat");
    let (_, response_path) = store.record_paths(&summary.id).unwrap();
    let logged = std::fs::read_to_string(response_path).unwrap();
    assert!(logged.contains("data: {\"choices\""));
    assert!(logged.contains("\"delta\":{\"content\":\"a\"}"));
    assert!(!logged.contains("event: response.completed"));
    upstream_task.abort();
}

#[tokio::test]
async fn responses_stream_reports_invalid_upstream_frames_as_failed() {
    let (_temp, gateway, upstream_task) =
        gateway_with_static_responses_stream("data: not-json\n\n").await;
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": "hello",
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    let text = response.text().await.unwrap();
    assert!(text.contains("event: response.failed"));
    assert!(text.contains("upstream_server_error"));
    assert!(!text.contains("event: response.completed"));
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn responses_stream_reports_eof_without_done_as_failed() {
    let (_temp, gateway, upstream_task) = gateway_with_static_responses_stream(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
    )
    .await;
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": "hello",
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    let text = response.text().await.unwrap();
    assert!(text.contains("\"delta\":\"partial\""));
    assert!(text.contains("event: response.failed"));
    assert!(text.contains("incomplete_stream"));
    assert!(!text.contains("event: response.completed"));
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn non_stream_chat_returns_timeout_when_aggregation_exceeds_budget() {
    let upstream = Router::new().route(
        "/chat/completions",
        post(|| async {
            let body = Body::from_stream(futures_util::stream::once(async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"late\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                ))
            }));
            let mut response = Response::new(body);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            response
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (_temp, _bridge, gateway) =
        test_gateway_with_upstream_timeout(upstream_port, Duration::from_millis(20)).await;

    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            gateway.port()
        ))
        .json(&json!({
            "model": "model",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::GATEWAY_TIMEOUT);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "upstream_timeout");
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn non_stream_responses_returns_timeout_when_upstream_headers_are_late() {
    let upstream = Router::new().route(
        "/chat/completions",
        post(|| async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Json(json!({
                "id": "chatcmpl-late",
                "choices": [{
                    "message": { "role": "assistant", "content": "late" },
                    "finish_reason": "stop"
                }]
            }))
            .into_response()
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (_temp, _bridge, gateway) =
        test_gateway_with_upstream_timeout(upstream_port, Duration::from_millis(20)).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": "hello",
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::GATEWAY_TIMEOUT);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "upstream_timeout");
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn responses_stream_reports_upstream_read_errors_as_failed() {
    let upstream = Router::new().route(
        "/chat/completions",
        post(|| async {
            let body = Body::from_stream(futures_util::stream::iter([
                Ok::<Bytes, std::io::Error>(Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"before\"},\"finish_reason\":null}]}\n\n",
                )),
                Err(std::io::Error::other("stream exploded")),
            ]));
            let mut response = Response::new(body);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            response
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (_temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": "hello",
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    if response.status() == reqwest::StatusCode::OK {
        let text = response.text().await.unwrap();
        assert!(text.contains("event: response.failed"));
        assert!(text.contains("upstream_network_error"));
        assert!(text.contains("stream exploded"));
    } else {
        assert_eq!(response.status(), reqwest::StatusCode::BAD_GATEWAY);
        let payload: Value = response.json().await.unwrap();
        assert_eq!(payload["error"]["code"], "upstream_network_error");
        assert!(payload["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()));
    }
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn routes_expose_cors_method_errors_and_not_found_contract() {
    let (temp, _bridge, gateway) = test_gateway().await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", gateway.port());

    let options = client
        .request(reqwest::Method::OPTIONS, format!("{base}/v1/responses"))
        .send()
        .await
        .unwrap();
    assert_eq!(options.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        options
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .unwrap(),
        "*"
    );

    let wrong_method = client
        .post(format!("{base}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        wrong_method.status(),
        reqwest::StatusCode::METHOD_NOT_ALLOWED
    );
    let missing = client.get(format!("{base}/missing")).send().await.unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(missing.headers().contains_key(REQUEST_ID_HEADER));
    gateway.stop().await;
    assert!(HttpTraceStore::new(temp.path().join("http-trace"))
        .list(10)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn removes_all_routes_without_the_v1_prefix() {
    let (temp, _bridge, gateway) = test_gateway().await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", gateway.port());

    let root_requests = [
        client.get(format!("{base}/healthz")).send(),
        client
            .request(reqwest::Method::OPTIONS, format!("{base}/healthz"))
            .send(),
        client.get(format!("{base}/models")).send(),
        client
            .get(format!("{base}/models/deepseek-v4.1-flash"))
            .send(),
        client
            .post(format!("{base}/chat/completions"))
            .json(&json!({ "messages": [] }))
            .send(),
        client
            .post(format!("{base}/responses"))
            .json(&json!({ "input": "hello" }))
            .send(),
    ];
    for response in root_requests {
        let response = response.await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        assert!(response.headers().contains_key(REQUEST_ID_HEADER));
    }

    gateway.stop().await;
    assert!(HttpTraceStore::new(temp.path().join("http-trace"))
        .list(10)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn translated_requests_drop_explicitly_rejected_optional_fields() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_upstream = Arc::clone(&calls);
    let upstream = Router::new().route(
            "/chat/completions",
            post(move |body: Bytes| {
                let calls = Arc::clone(&calls_for_upstream);
                async move {
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    let attempt = calls.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        assert!(value.get("stream_options").is_some());
                        return (
                            StatusCode::BAD_REQUEST,
                            [(header::CONTENT_TYPE, "application/json")],
                            r#"{"error":{"message":"unsupported stream_options"}}"#,
                        )
                            .into_response();
                    }
                    assert!(value.get("stream_options").is_none());
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                    )
                        .into_response()
                }
            }),
        );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "deepseek-v4.1-flash",
            "input": "hello",
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    gateway.stop().await;
    let store = HttpTraceStore::new(temp.path().join("http-trace"));
    let summary = store.list(1).unwrap().remove(0);
    let (request_path, response_path) = store.record_paths(&summary.id).unwrap();
    let request = std::fs::read_to_string(request_path).unwrap();
    assert!(!request.contains("stream_options"));
    let response = std::fs::read_to_string(response_path).unwrap();
    assert!(response.contains("\"content\":\"ok\""));
    upstream_task.abort();
}

#[tokio::test]
async fn chat_trace_records_rewritten_upstream_request_and_response() {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_upstream = Arc::clone(&captured);
    let upstream = Router::new().route(
        "/chat/completions",
        post(move |body: Bytes| {
            let captured = Arc::clone(&captured_for_upstream);
            async move {
                *captured.lock().unwrap() = Some(serde_json::from_slice(&body).unwrap());
                Json(json!({ "choices": [] })).into_response()
            }
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            gateway.port()
        ))
        .header(header::USER_AGENT, HARNESS_USER_AGENT)
        .json(&json!({
            "model": "deepseek-v4.1-flash",
            "messages": [{
                "role": "system",
                "content": "Codex via DeepSeek Harness"
            }, {
                "role": "developer",
                "content": [{ "type": "text", "text": "CODEX" }]
            }, {
                "role": "user",
                "content": "Codex stays"
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let forwarded = captured.lock().unwrap().clone().unwrap();
    assert_eq!(
        forwarded["messages"][0]["content"],
        "Codex via Harness of DeepSeek"
    );
    assert_eq!(forwarded["messages"][1]["content"][0]["text"], "CODEX");
    assert_eq!(forwarded["messages"][2]["content"], "Codex stays");
    gateway.stop().await;

    let store = HttpTraceStore::new(temp.path().join("http-trace"));
    let summary = store.list(1).unwrap().remove(0);
    let (request_path, response_path) = store.record_paths(&summary.id).unwrap();
    let request = std::fs::read_to_string(request_path).unwrap();
    assert!(request.contains("Codex via Harness of DeepSeek"));
    assert!(!request.contains("Codex via DeepSeek Harness"));
    let response = std::fs::read_to_string(response_path).unwrap();
    assert!(response.contains("\"choices\":[]"));
    upstream_task.abort();
}

#[tokio::test]
async fn non_harness_user_agent_skips_profile_on_chat_and_responses() {
    let captured = Arc::new(Mutex::new(Vec::<(HeaderMap, Value)>::new()));
    let captured_for_upstream = Arc::clone(&captured);
    let upstream = Router::new().route(
        "/chat/completions",
        post(move |headers: HeaderMap, body: Bytes| {
            let captured = Arc::clone(&captured_for_upstream);
            async move {
                captured
                    .lock()
                    .unwrap()
                    .push((headers, serde_json::from_slice(&body).unwrap()));
                Json(json!({
                    "id": "chatcmpl-1",
                    "model": "deepseek-v4.1-flash",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": { "role": "assistant", "content": "ok" },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 3,
                        "completion_tokens": 1,
                        "total_tokens": 4
                    }
                }))
                .into_response()
            }
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (_temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", gateway.port());
    let instructions = concat!(
        "DeepSeek Harness. ",
        "Tokens prefixed with @ are paths the user explicitly referenced. ",
        "Relative paths resolve from the workspace root; ",
        "absolute paths identify files or directories on the host."
    );

    let chat = client
        .post(format!("{base}/v1/chat/completions"))
        .header(header::USER_AGENT, "codex_vscode/0.146.0")
        .json(&json!({
            "model": "deepseek-v4.1-flash",
            "messages": [{ "role": "system", "content": instructions }],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(chat.status(), reqwest::StatusCode::OK);

    let responses = client
        .post(format!("{base}/v1/responses"))
        .header(header::USER_AGENT, "not-deepseek-harness/1.0")
        .json(&json!({
            "model": "deepseek-v4.1-flash",
            "instructions": instructions,
            "input": "hello",
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(responses.status(), reqwest::StatusCode::OK);

    {
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 2);
        for (headers, body) in captured.iter() {
            assert_eq!(
                headers.get(header::USER_AGENT).unwrap(),
                crate::prompt_gate::WORKBUDDY_USER_AGENT
            );
            assert_eq!(body["messages"][0]["content"], instructions);
        }
    }

    gateway.stop().await;
    upstream_task.abort();
}

#[test]
fn non_json_chat_bodies_are_not_reencoded() {
    let body = Bytes::from_static(b"Codex is not JSON");
    let headers = HeaderMap::new();
    let prepared = prepare_chat_request(body.clone(), &headers);
    assert_eq!(prepared.body, body);
    assert!(!prepared.gate_report.changed());
    assert!(!prepared.valid_json_object);
    assert!(!prepared.downstream_stream);
}

#[tokio::test]
async fn developer_role_is_forwarded_as_system() {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_upstream = Arc::clone(&captured);
    let upstream = Router::new().route(
        "/chat/completions",
        post(move |body: Bytes| {
            let captured = Arc::clone(&captured_for_upstream);
            async move {
                *captured.lock().unwrap() = Some(serde_json::from_slice(&body).unwrap());
                Json(json!({ "choices": [] })).into_response()
            }
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (_temp, bridge, gateway) = test_gateway_with_upstream(upstream_port).await;

    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            gateway.port()
        ))
        .json(&json!({
            "model": "deepseek-v4.1-flash",
            "messages": [{
                "role": "developer",
                "content": "Create a concise title for an AI coding-assistant session."
            }, {
                "role": "user",
                "content": "你好"
            }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let forwarded = captured.lock().unwrap().clone().unwrap();
    assert_eq!(forwarded["messages"][0]["role"], "system");
    assert_eq!(forwarded["messages"][1]["role"], "user");
    gateway.stop().await;

    let logs = bridge
        .logger_handle()
        .recent(20)
        .into_iter()
        .map(|entry| entry.message)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(logs.contains("rules=gate/developer-role:1"));
    upstream_task.abort();
}

#[tokio::test]
async fn upstream_errors_are_recorded_as_raw_http_messages() {
    let upstream = Router::new().route(
        "/chat/completions",
        post(|| async {
            (
                StatusCode::TOO_MANY_REQUESTS,
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::RETRY_AFTER, "17"),
                ],
                r#"{"error":{"message":"busy"}}"#,
            )
                .into_response()
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (temp, _bridge, gateway) = test_gateway_with_upstream(port).await;
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            gateway.port()
        ))
        .json(&json!({
            "model": "deepseek-v4.1-flash",
            "messages": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    gateway.stop().await;
    let store = HttpTraceStore::new(temp.path().join("http-trace"));
    let summary = store.list(1).unwrap().remove(0);
    assert_eq!(summary.status, 429);
    let (_, response_path) = store.record_paths(&summary.id).unwrap();
    let response_http = std::fs::read_to_string(response_path).unwrap();
    assert!(response_http.contains("retry-after: 17"));
    assert!(response_http.contains(r#"{"error":{"message":"busy"}}"#));
    upstream_task.abort();
}

#[tokio::test]
async fn prompt_gate_applies_profiles_fixed_ua_and_reports_11128_misses() {
    let captured = Arc::new(Mutex::new(None::<(HeaderMap, Value)>));
    let calls = Arc::new(AtomicUsize::new(0));
    let captured_for_upstream = Arc::clone(&captured);
    let calls_for_upstream = Arc::clone(&calls);
    let upstream = Router::new().route(
            "/chat/completions",
            post(move |headers: HeaderMap, body: Bytes| {
                let captured = Arc::clone(&captured_for_upstream);
                let calls = Arc::clone(&calls_for_upstream);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    *captured.lock().unwrap() =
                        Some((headers, serde_json::from_slice(&body).unwrap()));
                    (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        r#"{"code":11128,"msg":"Illegal API invocation from an unapproved channel","requestId":"upstream-gate-1"}"#,
                    )
                        .into_response()
                }
            }),
        );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (temp, bridge, gateway) = test_gateway_with_upstream(port).await;
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            gateway.port()
        ))
        .header(header::USER_AGENT, HARNESS_USER_AGENT)
        .json(&json!({
            "model": "deepseek-v4.1-flash",
            "messages": [{
                "role": "system",
                "content": concat!(
                    "DeepSeek Harness and Codex. ",
                    "Tokens prefixed with @ are paths the user explicitly referenced. ",
                    "Relative paths resolve from the workspace root; ",
                    "absolute paths identify files or directories on the host."
                )
            }, {
                "role": "user",
                "content": "Codex stays"
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "prompt_gate_blocked");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let (headers, body) = captured.lock().unwrap().clone().unwrap();
    assert_eq!(
        headers.get(header::USER_AGENT).unwrap(),
        crate::prompt_gate::WORKBUDDY_USER_AGENT
    );
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["messages"][0]["content"],
        concat!(
            "Harness of DeepSeek and Codex. ",
            "Paths beginning with @ refer to resources the user explicitly named. ",
            "Relative paths are resolved from the workspace root; ",
            "absolute paths refer to host files or directories."
        )
    );
    assert_eq!(body["messages"][1]["content"], "Codex stays");
    gateway.stop().await;

    let store = HttpTraceStore::new(temp.path().join("http-trace"));
    let summary = store.list(1).unwrap().remove(0);
    let (request_path, _) = store.record_paths(&summary.id).unwrap();
    let request = std::fs::read_to_string(request_path).unwrap();
    assert!(request.contains("user-agent: WorkBuddy/1.0.0"));
    let logs = bridge
        .logger_handle()
        .recent(20)
        .into_iter()
        .map(|entry| entry.message)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(logs.contains("prompt_gate.applied"));
    assert!(logs.contains("harness/path-instructions:1"));
    assert!(logs.contains("prompt_gate.miss"));
    assert!(logs.contains("upstream-gate-1"));
    assert!(!logs.contains("Tokens prefixed with @"));
    upstream_task.abort();
}

#[tokio::test]
async fn pre_upstream_failures_do_not_create_traces() {
    let (activation_temp, _bridge, activation_gateway) = test_gateway().await;
    let activation_response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            activation_gateway.port()
        ))
        .json(&json!({ "messages": [] }))
        .send()
        .await
        .unwrap();
    assert!(activation_response.status().is_server_error());
    activation_gateway.stop().await;
    assert!(
        HttpTraceStore::new(activation_temp.path().join("http-trace"))
            .list(10)
            .unwrap()
            .is_empty()
    );

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let unused_port = listener.local_addr().unwrap().port();
    drop(listener);
    let (network_temp, _bridge, network_gateway) = test_gateway_with_upstream(unused_port).await;
    let network_response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            network_gateway.port()
        ))
        .json(&json!({ "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(network_response.status(), reqwest::StatusCode::BAD_GATEWAY);
    network_gateway.stop().await;
    assert!(HttpTraceStore::new(network_temp.path().join("http-trace"))
        .list(10)
        .unwrap()
        .is_empty());
}

async fn gateway_with_static_responses_stream(
    body: &'static str,
) -> (
    tempfile::TempDir,
    GatewayHandle,
    tokio::task::JoinHandle<()>,
) {
    let upstream = Router::new().route(
        "/chat/completions",
        post(move || async move {
            ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (temp, _bridge, gateway) = test_gateway_with_upstream(port).await;
    (temp, gateway, upstream_task)
}

async fn test_gateway_with_upstream(
    upstream_port: u16,
) -> (tempfile::TempDir, Arc<Bridge>, GatewayHandle) {
    test_gateway_with_upstream_timeout(upstream_port, Duration::from_secs(120)).await
}

async fn test_gateway_with_upstream_timeout(
    upstream_port: u16,
    non_stream_timeout: Duration,
) -> (tempfile::TempDir, Arc<Bridge>, GatewayHandle) {
    let temp = tempfile::tempdir().unwrap();
    let account = crate::account::AccountManager::with_store(AccountStore::new(
        temp.path().join("account.bin"),
    ));
    let bridge = Arc::new(Bridge::new(
        account,
        {
            let store = SettingsStore::new(temp.path().join("settings.json"));
            store
                .save(&crate::settings::Settings {
                    http_log_filter: crate::settings::HttpLogFilter::All,
                    ..crate::settings::Settings::default()
                })
                .unwrap();
            store
        },
        SshKeyStore::new(
            temp.path().join("id_ed25519"),
            temp.path().join("id_ed25519.pub"),
        ),
        Arc::new(Logger::new(32)),
    ));
    let gateway = GatewayHandle::start_with_upstream(
        Arc::clone(&bridge),
        GatewayConfig {
            port: 0,
            activation_timeout: Duration::from_secs(1),
            idle_timeout: Duration::from_secs(60),
            upstream_connect_timeout: Duration::from_secs(5),
            upstream_read_timeout: Duration::from_secs(60),
            non_stream_timeout,
        },
        upstream_port,
        temp.path().join("http-trace"),
    )
    .await
    .unwrap();
    (temp, bridge, gateway)
}
