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

async fn read_http_request(stream: &mut TcpStream) -> bool {
    let mut buffered = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        let read = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return false,
            Ok(read) => read,
        };
        buffered.extend_from_slice(&chunk[..read]);
        let Some(header_end) = buffered.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&buffered[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or_default();
        let request_length = header_end + 4 + content_length;
        while buffered.len() < request_length {
            let read = match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return false,
                Ok(read) => read,
            };
            buffered.extend_from_slice(&chunk[..read]);
        }
        buffered.drain(..request_length);
        return true;
    }
}

#[tokio::test]
async fn upstream_client_does_not_reuse_a_half_dead_keep_alive_connection() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let first_connection_requests = Arc::new(AtomicUsize::new(0));
    let second_connection_requests = Arc::new(AtomicUsize::new(0));
    let connection_count = Arc::new(AtomicUsize::new(0));
    let first_for_server = Arc::clone(&first_connection_requests);
    let second_for_server = Arc::clone(&second_connection_requests);
    let count_for_server = Arc::clone(&connection_count);
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else {
                        break;
                    };
                    let connection_index =
                        count_for_server.fetch_add(1, Ordering::SeqCst);
                    let first_requests = Arc::clone(&first_for_server);
                    let second_requests = Arc::clone(&second_for_server);
                    connections.spawn(async move {
                        loop {
                            if !read_http_request(&mut stream).await {
                                return;
                            }
                            let request_index = if connection_index == 0 {
                                first_requests.fetch_add(1, Ordering::SeqCst)
                            } else {
                                second_requests.fetch_add(1, Ordering::SeqCst)
                            };
                            if connection_index == 0 && request_index > 0 {
                                std::future::pending::<()>().await;
                            }
                            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: text/plain\r\nConnection: keep-alive\r\n\r\nok";
                            if stream.write_all(response).await.is_err() {
                                return;
                            }
                        }
                    });
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    });

    let config = GatewayConfig {
        port: 0,
        activation_timeout: Duration::from_secs(1),
        idle_timeout: Duration::from_secs(60),
        upstream_connect_timeout: Duration::from_secs(1),
        upstream_read_timeout: Duration::from_millis(100),
        non_stream_timeout: Duration::from_secs(1),
    };
    let client = gateway_upstream_client(&config).unwrap();
    let url = format!("http://{address}/chat/completions");

    let first = client.post(&url).body("{}").send().await.unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    assert_eq!(first.text().await.unwrap(), "ok");

    let second = client
        .post(&url)
        .body("{}")
        .send()
        .await
        .expect("second request should use a fresh connection");
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    assert_eq!(second.text().await.unwrap(), "ok");

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(connection_count.load(Ordering::SeqCst), 2);
    assert_eq!(first_connection_requests.load(Ordering::SeqCst), 1);
    assert_eq!(second_connection_requests.load(Ordering::SeqCst), 1);

    let _ = shutdown.send(true);
    server.await.unwrap();
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
fn only_stops_after_five_idle_minutes_without_active_connections() {
    let timeout = Duration::from_secs(5 * 60);
    assert!(!should_disconnect_idle_tunnel(
        1,
        Duration::from_secs(301),
        timeout
    ));
    assert!(!should_disconnect_idle_tunnel(
        0,
        Duration::from_secs(299),
        timeout
    ));
    assert!(should_disconnect_idle_tunnel(0, timeout, timeout));
}

#[tokio::test]
async fn activity_tracks_requests_and_last_use() {
    let activity = Arc::new(GatewayActivity::new());
    let first = activity.enter().await;
    let second = activity.enter().await;
    assert_eq!(activity.active_connections(), 2);
    drop(first);
    assert_eq!(activity.active_connections(), 1);
    drop(second);
    assert_eq!(activity.active_connections(), 0);
    assert!(activity.idle_for() < Duration::from_secs(1));
}

#[tokio::test]
async fn idle_claim_and_new_requests_are_serialized() {
    let activity = Arc::new(GatewayActivity::new());
    let active = activity.enter().await;
    assert!(activity.try_claim_idle_disconnect(Duration::ZERO).is_none());
    drop(active);

    let idle_claim = activity
        .try_claim_idle_disconnect(Duration::ZERO)
        .expect("idle activity should allow disconnect");
    let pending = tokio::spawn({
        let activity = Arc::clone(&activity);
        async move { activity.enter().await }
    });
    tokio::task::yield_now().await;
    assert!(!pending.is_finished());
    assert_eq!(activity.active_connections(), 1);

    drop(idle_claim);
    let guard = pending.await.unwrap();
    assert_eq!(activity.active_connections(), 1);
    drop(guard);
    assert_eq!(activity.active_connections(), 0);
}

#[tokio::test]
async fn cancelled_request_waiting_for_idle_claim_releases_activity() {
    let activity = Arc::new(GatewayActivity::new());
    let idle_claim = activity
        .try_claim_idle_disconnect(Duration::ZERO)
        .expect("idle activity should allow disconnect");
    let pending = tokio::spawn({
        let activity = Arc::clone(&activity);
        async move { activity.enter().await }
    });
    tokio::task::yield_now().await;
    assert_eq!(activity.active_connections(), 1);

    pending.abort();
    let _ = pending.await;
    assert_eq!(activity.active_connections(), 0);
    drop(idle_claim);
}

#[tokio::test]
async fn activity_guard_is_released_when_request_future_is_cancelled() {
    let activity = Arc::new(GatewayActivity::new());
    let activity_for_request = Arc::clone(&activity);
    let request = tokio::spawn(async move { activity_for_request.enter().await });
    tokio::task::yield_now().await;
    assert_eq!(activity.active_connections(), 1);

    request.abort();
    let _ = request.await;
    assert_eq!(activity.active_connections(), 0);
}

#[tokio::test]
async fn response_body_holds_activity_until_consumed() {
    let activity = Arc::new(GatewayActivity::new());
    let guard = activity.enter().await;
    let response = activity_response((StatusCode::OK, "body").into_response(), guard);
    assert_eq!(activity.active_connections(), 1);

    let body = axum::body::to_bytes(response.into_body(), 16)
        .await
        .unwrap();
    assert_eq!(body, "body");
    tokio::task::yield_now().await;
    assert_eq!(activity.active_connections(), 0);
}

#[tokio::test]
async fn request_stays_active_while_handler_waits_for_upstream_headers() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let started_for_upstream = Arc::clone(&started);
    let release_for_upstream = Arc::clone(&release);
    let upstream = Router::new().route(
        "/chat/completions",
        post(move || {
            let started = Arc::clone(&started_for_upstream);
            let release = Arc::clone(&release_for_upstream);
            async move {
                started.notify_one();
                release.notified().await;
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
    let (_temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;
    let request = tokio::spawn({
        let url = format!("http://127.0.0.1:{}/v1/responses", gateway.port());
        async move {
            reqwest::Client::new()
                .post(url)
                .json(&json!({
                    "model": "model",
                    "input": "hello",
                    "stream": false
                }))
                .send()
                .await
                .unwrap()
        }
    });

    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .expect("upstream request did not start");
    assert_eq!(gateway.active_connections(), 1);

    release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(3), request)
        .await
        .expect("response did not complete")
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["status"],
        "completed"
    );
    tokio::task::yield_now().await;
    assert_eq!(gateway.active_connections(), 0);

    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn stop_interrupts_blocked_idle_disconnect() {
    let probe = IdleMonitorProbe::default();
    let (_temp_probe, _bridge_probe, gateway) = {
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
        let gateway = GatewayHandle::start_with_upstream_internal_impl(
            Arc::clone(&bridge),
            GatewayConfig {
                port: 0,
                activation_timeout: Duration::from_secs(1),
                idle_timeout: Duration::ZERO,
                upstream_connect_timeout: Duration::from_secs(5),
                upstream_read_timeout: Duration::from_secs(60),
                non_stream_timeout: Duration::from_secs(120),
            },
            None,
            temp.path().join("http-trace"),
            probe.clone(),
        )
        .await
        .unwrap();
        (temp, bridge, gateway)
    };
    tokio::time::timeout(Duration::from_secs(3), probe.disconnect_started.notified())
        .await
        .expect("idle monitor should reach disconnect");

    tokio::time::timeout(Duration::from_secs(1), gateway.stop())
        .await
        .expect("gateway stop should not wait for a blocked disconnect");
    assert!(futures_util::FutureExt::now_or_never(probe.disconnect_finished.notified()).is_none());
}

#[tokio::test]
async fn timed_out_activation_task_is_aborted_and_awaited() {
    let completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let completed_for_task = Arc::clone(&completed);
    let task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        completed_for_task.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok::<u16, crate::bridge::BridgeError>(11233)
    });

    let result = await_activation_task(task, Duration::from_millis(10)).await;

    assert!(result.is_err());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!completed.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn request_body_precheck_rejects_large_content_length_before_reading() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&(BODY_LIMIT + 1).to_string()).unwrap(),
    );

    let error = read_body_limited(&headers, Body::empty())
        .await
        .unwrap_err();

    assert!(matches!(error, BodyReadError::TooLarge));
}

#[tokio::test]
async fn request_body_read_error_is_not_reported_as_payload_too_large() {
    let body = Body::from_stream(futures_util::stream::once(async {
        Err::<Bytes, std::io::Error>(std::io::Error::other("broken upload"))
    }));

    let error = read_body_limited(&HeaderMap::new(), body)
        .await
        .unwrap_err();

    assert!(matches!(error, BodyReadError::Invalid(message) if message.contains("broken upload")));
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
async fn removed_health_route_is_not_found() {
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

    assert!(response.starts_with("HTTP/1.1 404 Not Found"));
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
        let request_body = request.to_string();
        let prepared = openai_compat::chat::prepare_chat_request(request_body.as_bytes());
        let rewritten = prepared.rewritten_body();
        let upstream: Value =
            serde_json::from_slice(rewritten.as_deref().unwrap_or(request_body.as_bytes()))
                .unwrap();

        assert_eq!(upstream["stream"], true);
        assert_eq!(
            prepared.request().downstream_stream(),
            expected_downstream_stream
        );
        assert_eq!(prepared.request().requested_model(), "hy4-flash");
        assert!(prepared.request().valid_json_object());
    }
}

#[test]
fn chat_preparation_reuses_unchanged_streaming_body() {
    let original = Bytes::from_static(
        br#"{"model":"hy4-flash","messages":[{"role":"user","content":"hello"}],"stream":true}"#,
    );
    let original_pointer = original.as_ptr();

    let prepared = openai_compat::chat::prepare_chat_request(&original);

    assert!(prepared.rewritten_body().is_none());
    assert_eq!(original.as_ptr(), original_pointer);
    assert!(prepared.request().downstream_stream());
    assert!(!prepared.body_changed());
}

#[test]
fn non_stream_chat_preserves_unchanged_json_bytes() {
    let original = Bytes::from_static(
        br#"{"model":"hy4-flash","temperature":1.00,"messages":[{"role":"user","content":"hello"}],"stream":false}"#,
    );

    let prepared = openai_compat::chat::prepare_chat_request(&original);
    let prepared_body = prepared
        .rewritten_body()
        .expect("stream:false should be rewritten");
    let body = std::str::from_utf8(&prepared_body).unwrap();

    assert!(body.contains("\"temperature\":1.00"));
    assert!(body.contains("\"stream\":true"));
    assert!(!body.contains("\"stream\":false"));
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
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
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

#[tokio::test]
async fn chat_nonstream_preserves_request_id_for_in_stream_errors() {
    let upstream = Router::new().route(
        "/chat/completions",
        post(|| async {
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                r#"data: {"error":{"message":"context length exceeded","type":"invalid_request_error","code":"context_length_exceeded","param":"messages"}}

"#,
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

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let header_request_id = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["request_id"], header_request_id);
    assert_eq!(payload["error"]["code"], "context_length_exceeded");
    assert_eq!(payload["error"]["type"], "invalid_request_error");
    assert_eq!(payload["error"]["param"], "messages");
    assert!(payload["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("context length exceeded")));

    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn chat_nonstream_done_survives_late_transport_error() {
    let done = std::sync::Arc::new(Bytes::from_static(
        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    ));
    let upstream_done = std::sync::Arc::clone(&done);
    let upstream = Router::new().route(
        "/chat/completions",
        post(move || {
            let upstream_done = std::sync::Arc::clone(&upstream_done);
            async move {
                let first = futures_util::stream::once(async move {
                    Ok::<Bytes, std::io::Error>((*upstream_done).clone())
                });
                let second = futures_util::stream::once(async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Err(std::io::Error::other("late reset after done"))
                });
                let mut response = Response::new(Body::from_stream(first.chain(second)));
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                response
            }
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
    assert_eq!(payload["choices"][0]["message"]["content"], "ok");
    assert_eq!(payload["choices"][0]["finish_reason"], "stop");

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
fn responses_stream_buffer_enforces_the_size_limit() {
    assert!(!responses_buffer_too_large(BODY_LIMIT - 1, 1));
    assert!(responses_buffer_too_large(BODY_LIMIT, 1));
    assert!(responses_buffer_too_large(usize::MAX, 1));
}

#[test]
fn event_stream_content_type_is_case_insensitive_and_parameterized() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("Text/Event-Stream; charset=utf-8"),
    );
    assert!(is_event_stream(&headers));

    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    assert!(!is_event_stream(&headers));
}

#[test]
fn chat_completion_aggregation_rejects_in_stream_errors() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    let error = accumulator
        .push_data(r#"{"error":{"message":"upstream exploded"}}"#)
        .unwrap_err();

    assert_eq!(error.message, "upstream exploded");
    assert_eq!(error.code, "upstream_error");
}

#[test]
fn chat_completion_aggregation_normalizes_nested_and_flat_stream_errors() {
    let cases = [
        (
            r#"{"detail":"detail failure"}"#,
            "detail failure",
            "upstream_error",
        ),
        (
            r#"{"message":"flat failure"}"#,
            "flat failure",
            "upstream_error",
        ),
        (
            r#"{"base_resp":{"status_code":1001,"status_msg":"base failure"}}"#,
            "base failure",
            "1001",
        ),
        (
            r#"{"error":{"message":"context length exceeded","type":"invalid_request_error","code":"context_length_exceeded","param":"messages"}}"#,
            "context length exceeded",
            "context_length_exceeded",
        ),
    ];

    for (payload, message, code) in cases {
        let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
        let error = accumulator.push_data(payload).unwrap_err();

        assert_eq!(error.message, message);
        assert_eq!(error.code, code);
    }
}

#[test]
fn chat_completion_aggregation_accepts_usage_only_terminal_chunks() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"content":"complete"},"finish_reason":"stop"}]}"#,
        )
        .unwrap();
    accumulator
        .push_data(r#"{"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}"#)
        .unwrap();
    accumulator.push_data("[DONE]").unwrap();

    let completion = accumulator.finish("model").unwrap();

    assert_eq!(completion["choices"][0]["message"]["content"], "complete");
    assert_eq!(completion["usage"]["total_tokens"], 3);
}

#[test]
fn chat_completion_aggregation_rejects_truncated_streams() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(r#"{"choices":[{"index":0,"delta":{"content":"half"},"finish_reason":null}]}"#)
        .unwrap();

    let error = accumulator.finish("hy4-flash").unwrap_err();

    assert!(error.message.contains("缺少完成分片"));
}

#[test]
fn chat_completion_aggregation_rejects_invalid_sse_json() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    let error =
        openai_compat::chat::process_sse_line(&mut accumulator, b"data: not-json\n").unwrap_err();

    assert_eq!(error.code, "upstream_server_error");
}

#[test]
fn chat_completion_aggregation_keeps_parallel_calls_separate_without_index() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"id":"chatcmpl-parallel","created":42,"model":"upstream-model","choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_a","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}]},"finish_reason":null}]}"#,
        )
        .unwrap();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_b","function":{"name":"write_file","arguments":"{\"path\":\"b\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();

    let completion = accumulator.finish("requested-model").unwrap();
    let calls = completion["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();

    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "call_a");
    assert_eq!(calls[0]["function"]["name"], "read_file");
    assert_eq!(calls[1]["id"], "call_b");
    assert_eq!(calls[1]["function"]["name"], "write_file");
    assert_eq!(completion["model"], "upstream-model");
    assert_eq!(completion["created"], 42);
}

#[test]
fn chat_completion_aggregation_does_not_duplicate_repeated_tool_headers() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
        )
        .unwrap();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"","function":{"name":"read_file","arguments":"\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();

    let completion = accumulator.finish("model").unwrap();
    let call = &completion["choices"][0]["message"]["tool_calls"][0];

    assert_eq!(call["id"], "call_a");
    assert_eq!(call["function"]["name"], "read_file");
    assert_eq!(call["function"]["arguments"], "{\"path\":\"a.txt\"}");
}

#[test]
fn chat_completion_aggregation_reuses_a_known_id_when_its_index_changes() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
        )
        .unwrap();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":3,"id":"call_a","function":{"arguments":"\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();

    let completion = accumulator.finish("model").unwrap();
    let calls = completion["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();

    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_a");
    assert_eq!(calls[0]["function"]["arguments"], "{\"path\":\"a.txt\"}");
}

#[test]
fn chat_completion_aggregation_treats_a_new_id_at_the_same_index_as_new_call() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{}"}}]},"finish_reason":null}]}"#,
        )
        .unwrap();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_b","function":{"name":"write_file","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();

    let completion = accumulator.finish("model").unwrap();
    let calls = completion["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();

    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "call_a");
    assert_eq!(calls[1]["id"], "call_b");
}

#[test]
fn chat_completion_aggregation_keeps_sparse_parallel_ids_separate_in_one_frame() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{}"}},{"index":3,"id":"call_b","function":{"name":"write_file","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();

    let completion = accumulator.finish("model").unwrap();
    let calls = completion["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();

    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "call_a");
    assert_eq!(calls[1]["id"], "call_b");
}

#[test]
fn chat_completion_aggregation_preserves_content_parts_and_refusal() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"content":[{"type":"text","text":"partial"},{"type":"refusal","refusal":"blocked"}]},"finish_reason":"stop"}]}"#,
        )
        .unwrap();

    let completion = accumulator.finish("model").unwrap();
    let content = completion["choices"][0]["message"]["content"]
        .as_array()
        .unwrap();

    assert_eq!(content[0], json!({"type": "text", "text": "partial"}));
    assert_eq!(content[1], json!({"type": "refusal", "refusal": "blocked"}));
}

#[test]
fn chat_completion_aggregation_preserves_raw_function_arguments() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_raw","type":"function","function":{"name":"lookup","arguments":"{ \"b\": 2,"}}]},"finish_reason":null}]}"#,
        )
        .unwrap();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\n  \"a\": \"x\" }"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();

    let completion = accumulator.finish("model").unwrap();

    assert_eq!(
        completion["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{ \"b\": 2,\n  \"a\": \"x\" }"
    );
}

#[test]
fn chat_completion_aggregation_keeps_the_first_finish_reason() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();
    accumulator
        .push_data(r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#)
        .unwrap();

    let completion = accumulator.finish("model").unwrap();

    assert_eq!(completion["choices"][0]["finish_reason"], "tool_calls");
}

#[test]
fn chat_completion_aggregation_rejects_done_without_any_output() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator.push_data("[DONE]").unwrap();

    let error = accumulator.finish("model").unwrap_err();

    assert!(error.message.contains("没有有效输出"));
}

#[test]
fn chat_completion_aggregation_rejects_output_without_finish_reason() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}"#,
        )
        .unwrap();
    accumulator.push_data("[DONE]").unwrap();

    let error = accumulator.finish("model").unwrap_err();

    assert!(error.message.contains("缺少 finish_reason"));
}

#[test]
fn chat_completion_aggregation_preserves_length_and_content_filter_terminals() {
    for finish_reason in ["length", "content_filter"] {
        let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
        accumulator
            .push_data(&format!(
                r#"{{"choices":[{{"index":0,"delta":{{"content":"partial"}},"finish_reason":"{finish_reason}"}}]}}"#
            ))
            .unwrap();

        let completion = accumulator.finish("model").unwrap();

        assert_eq!(completion["choices"][0]["finish_reason"], finish_reason);
    }
}

#[test]
fn chat_completion_aggregation_marks_valid_tool_output_as_tool_calls() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"stop"}]}"#,
        )
        .unwrap();

    let completion = accumulator.finish("model").unwrap();

    assert_eq!(completion["choices"][0]["finish_reason"], "tool_calls");
}

#[test]
fn chat_completion_aggregation_rejects_tool_terminal_without_a_tool_call() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();

    let error = accumulator.finish("model").unwrap_err();

    assert_eq!(error.code, "incomplete_stream");
    assert!(error.message.contains("tool_calls"));
}

#[test]
fn chat_completion_aggregation_rejects_tool_calls_without_names() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();

    let error = accumulator.finish("model").unwrap_err();

    assert!(error.message.contains("缺少函数名"));
}

#[test]
fn chat_completion_aggregation_rejects_malformed_tool_arguments() {
    let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
    accumulator
        .push_data(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"lookup","arguments":"{\"path\":"}}]},"finish_reason":"tool_calls"}]}"#,
        )
        .unwrap();

    let error = accumulator.finish("model").unwrap_err();

    assert!(error.message.contains("参数不是合法 JSON"));
}

#[test]
fn chat_completion_aggregation_normalizes_missing_or_whitespace_tool_arguments() {
    for arguments in [None, Some(""), Some(" \n\t ")] {
        let mut accumulator = openai_compat::chat::ChatCompletionAccumulator::default();
        let function = match arguments {
            Some(arguments) => json!({"name": "lookup", "arguments": arguments}),
            None => json!({"name": "lookup"}),
        };
        accumulator
            .push_data(
                &json!({
                    "choices": [{
                        "index": 0,
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "call_a",
                                "function": function
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                })
                .to_string(),
            )
            .unwrap();

        let completion = accumulator.finish("model").unwrap();

        assert_eq!(
            completion["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{}"
        );
    }
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
async fn responses_apply_patch_round_trip_preserves_definition_and_raw_input() {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_upstream = Arc::clone(&captured);
    let upstream = Router::new().route(
        "/chat/completions",
        post(move |body: Bytes| {
            let captured = Arc::clone(&captured_for_upstream);
            async move {
                *captured.lock().unwrap() = Some(serde_json::from_slice(&body).unwrap());
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    r#"data: {"id":"chatcmpl_patch","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_patch","function":{"name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n"}}]}}]}

data: {"id":"chatcmpl_patch","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"*** End Patch\"}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

"#,
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
    let (_temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": "edit the file",
            "stream": true,
            "tools": [{
                "type": "custom",
                "name": "apply_patch",
                "description": "Apply a patch to files.",
                "format": {
                    "type": "grammar",
                    "syntax": "lark",
                    "definition": "start: begin_patch hunk+ end_patch"
                }
            }],
            "tool_choice": {
                "type": "custom",
                "name": "apply_patch"
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let text = response.text().await.unwrap();
    assert_eq!(text.matches("\"delta\":\"*** Begin Patch").count(), 1);
    assert!(text.contains("*** Begin Patch\\n*** End Patch"));
    assert!(text.contains("event: response.custom_tool_call_input.done"));
    assert!(text.contains("\"type\":\"custom_tool_call\""));

    let forwarded = captured.lock().unwrap().clone().unwrap();
    assert_eq!(forwarded["tool_choice"]["function"]["name"], "apply_patch");
    let description = forwarded["tools"][0]["function"]["description"]
        .as_str()
        .unwrap();
    assert!(description.contains("start: begin_patch hunk+ end_patch"));

    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn non_stream_responses_apply_patch_unwraps_custom_input() {
    let (_temp, gateway, upstream_task) = gateway_with_static_responses_stream(
        r#"data: {"id":"chatcmpl_patch","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_patch","function":{"name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n*** End Patch\"}"}}]}}]}

data: {"id":"chatcmpl_patch","choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}

data: [DONE]

"#,
    )
    .await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": "edit the file",
            "stream": false,
            "tools": [{
                "type": "custom",
                "name": "apply_patch"
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: Value = response.json().await.unwrap();

    assert_eq!(payload["output"][0]["type"], "custom_tool_call");
    assert_eq!(
        payload["output"][0]["input"],
        "*** Begin Patch\n*** End Patch"
    );
    assert_eq!(payload["usage"]["input_tokens"], 2);
    assert_eq!(payload["usage"]["output_tokens"], 3);

    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn non_stream_responses_preserve_raw_function_arguments() {
    let (_temp, gateway, upstream_task) = gateway_with_static_responses_stream(
        r#"data: {"id":"chatcmpl_raw","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_raw","type":"function","function":{"name":"lookup","arguments":"{ \"b\": 2,"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl_raw","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\n  \"a\": \"x\" }"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

"#,
    )
    .await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": "look it up",
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: Value = response.json().await.unwrap();

    assert_eq!(
        payload["output"][0]["arguments"],
        "{ \"b\": 2,\n  \"a\": \"x\" }"
    );

    gateway.stop().await;
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
async fn responses_stream_ignores_frames_after_done_in_the_same_chunk() {
    let (_temp, gateway, upstream_task) = gateway_with_static_responses_stream(
        "data: {\"choices\":[{\"delta\":{\"content\":\"visible\"},\"finish_reason\":\"stop\"}]}\n\n\
         data: [DONE]\n\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"late\"},\"finish_reason\":\"stop\"}]}\n\n\
         data: {\"error\":{\"message\":\"late failure\"}}\n\n\
         data: [DONE]\n\n",
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

    assert_eq!(text.matches("event: response.completed").count(), 1);
    assert!(!text.contains("event: response.failed"));
    assert!(!text.contains("\"delta\":\"late\""));
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn responses_stream_done_survives_late_transport_error() {
    let done_chunk = std::sync::Arc::new(Bytes::from_static(
        b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    ));
    let late_error = std::sync::Arc::new("late reset after done");
    let handler_done = std::sync::Arc::clone(&done_chunk);
    let handler_error = std::sync::Arc::clone(&late_error);
    let upstream = Router::new().route(
        "/chat/completions",
        post(move || {
            let done_chunk = std::sync::Arc::clone(&handler_done);
            let late_error = std::sync::Arc::clone(&handler_error);
            async move {
                let first = futures_util::stream::once(async move {
                    Ok::<Bytes, std::io::Error>((*done_chunk).clone())
                });
                let second = futures_util::stream::once(async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Err(std::io::Error::other(*late_error.as_ref()))
                });
                let body = Body::from_stream(first.chain(second));
                let mut response = Response::new(body);
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                response
            }
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
    let text = response.text().await.unwrap();

    assert_eq!(text.matches("event: response.completed").count(), 1);
    assert!(!text.contains("event: response.failed"));
    assert!(!text.contains("late reset"));
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn responses_duplicate_argument_history_is_repaired_before_upstream() {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_upstream = Arc::clone(&captured);
    let upstream = Router::new().route(
        "/chat/completions",
        post(move |body: Bytes| {
            let captured = Arc::clone(&captured_for_upstream);
            async move {
                *captured.lock().unwrap() = Some(serde_json::from_slice(&body).unwrap());
                (
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
    let (_temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_dup",
                    "name": "lookup",
                    "arguments": "{\"time\":null,\"nested\":{\"x\":1,\"x\":2},\"time\":\"later\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_dup",
                    "output": "ok"
                },
                { "type": "message", "role": "user", "content": "continue" }
            ],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let _ = response.text().await.unwrap();

    let forwarded = captured.lock().unwrap().clone().unwrap();
    let arguments = forwarded["messages"][0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap();
    let parsed: Value = serde_json::from_str(arguments).unwrap();
    assert_eq!(parsed["time"], "later");
    assert_eq!(parsed["nested"]["x"], 2);

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
    assert_eq!(
        options
            .headers()
            .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
            .unwrap(),
        "x-request-id, retry-after"
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
    let prepared = openai_compat::chat::prepare_chat_request(&body);
    assert!(prepared.rewritten_body().is_none());
    assert!(!prepared.request().valid_json_object());
    assert!(!prepared.request().downstream_stream());
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
    assert!(logs.contains("rules=compat/developer-role:1"));
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

#[tokio::test]
async fn non_stream_responses_reports_reasoning_details() {
    let (_temp, gateway, upstream_task) = gateway_with_static_responses_stream(
        "data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"text\":\"because\"}]},\"finish_reason\":null}]}\n\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\n\
         data: [DONE]\n\n",
    )
    .await;

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
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let payload: Value = response.json().await.unwrap();

    assert_eq!(payload["output"][0]["type"], "reasoning");
    assert_eq!(payload["output"][0]["summary"][0]["text"], "because");
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn responses_stream_omits_whitespace_only_reasoning() {
    let (_temp, gateway, upstream_task) = gateway_with_static_responses_stream(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\" \"},\"finish_reason\":null}]}\n\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\n\
         data: [DONE]\n\n",
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
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let text = response.text().await.unwrap();

    assert!(!text.contains("response.reasoning_summary_part.added"));
    assert!(!text.contains("response.reasoning_summary_text.delta"));
    assert!(text.contains("event: response.output_text.delta"));
    assert!(text.contains("event: response.completed"));
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn responses_stream_keeps_reasoning_per_tool_turn() {
    let (_temp, gateway, upstream_task) = gateway_with_static_responses_stream(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"first\"},\"finish_reason\":null}]}\n\n\
         data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n\
         data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"second\"},\"finish_reason\":null}]}\n\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\n\
         data: [DONE]\n\n",
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
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let text = response.text().await.unwrap();

    assert_eq!(
        text.matches("event: response.reasoning_summary_part.added")
            .count(),
        2
    );
    assert!(text.contains("\"text\":\"first\""));
    assert!(text.contains("\"text\":\"second\""));
    assert!(text.contains("event: response.completed"));
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn responses_logs_ignored_input_features() {
    let upstream = Router::new().route(
        "/chat/completions",
        post(|| async {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
                .into_response()
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let upstream_port = listener.local_addr().unwrap().port();
    let upstream_task = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let (_temp, bridge, gateway) = test_gateway_with_upstream(upstream_port).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": [{ "type": "web_search_call" }],
            "tools": [{ "type": "web_search" }],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let logged = bridge
        .logger_handle()
        .recent(20)
        .iter()
        .map(|entry| entry.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        logged.contains("responses.compat.ignored"),
        "missing ignored log: {logged}"
    );
    assert!(
        logged.contains("input.item_degraded:web_search_call"),
        "missing detail: {logged}"
    );
    assert!(
        logged.contains("tools.ignored:web_search"),
        "missing tool detail: {logged}"
    );
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn response_format_is_dropped_on_upstream_400() {
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
                    assert!(value.get("response_format").is_some());
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        r#"{"error":{"message":"unsupported parameter: response_format"}}"#,
                    )
                        .into_response();
                }
                assert!(value.get("response_format").is_none());
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
    let (_temp, _bridge, gateway) = test_gateway_with_upstream(upstream_port).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/v1/responses", gateway.port()))
        .json(&json!({
            "model": "model",
            "input": "hello",
            "stream": false,
            "text": { "format": { "type": "json_object" } }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    gateway.stop().await;
    upstream_task.abort();
}

#[tokio::test]
async fn rejected_extended_chat_field_is_removed_and_retried_once() {
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
                    assert!(value.get("stop").is_some());
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": {"message": "unsupported parameter: stop"}})),
                    )
                        .into_response()
                } else {
                    assert!(value.get("stop").is_none());
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                    )
                        .into_response()
                }
            }
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
            "stream": false,
            "stop": ["DONE"]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    gateway.stop().await;
    upstream_task.abort();
}
