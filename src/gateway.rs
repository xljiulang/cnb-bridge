//! Persistent loopback HTTP gateway that activates CNB only when needed.

use crate::bridge::Bridge;
use crate::http_trace::{HttpTraceSession, HttpTraceStore};
use crate::logging::Logger;
use crate::model_catalog;
use crate::prompt_gate::{apply_headers, detect_gate_block, normalize_chat, GateReport};
use crate::responses::{chat_to_responses, responses_to_chat, ResponsesStream, ToolShape};
use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::watch;

const REQUEST_ID_HEADER: &str = "x-request-id";
const BODY_LIMIT: usize = 64 * 1024 * 1024;
const OPTIONAL_CHAT_FIELDS: [(&str, &[&str]); 6] = [
    ("stream_options", &["stream_options", "include_usage"]),
    (
        "reasoning_effort",
        &["reasoning_effort", "reasoning effort"],
    ),
    ("parallel_tool_calls", &["parallel_tool_calls"]),
    ("prompt_cache_key", &["prompt_cache_key"]),
    ("service_tier", &["service_tier"]),
    ("tool_choice", &["tool_choice"]),
];

/// Gateway timing parameters.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub port: u16,
    pub activation_timeout: Duration,
    pub idle_timeout: Duration,
}

impl GatewayConfig {
    /// Production gateway settings.
    pub fn new(port: u16) -> Self {
        Self {
            port,
            activation_timeout: Duration::from_secs(180),
            idle_timeout: Duration::from_secs(5 * 60),
        }
    }
}

fn should_close_idle_tunnel(
    active_connections: usize,
    idle_for: Duration,
    idle_timeout: Duration,
) -> bool {
    active_connections == 0 && idle_for >= idle_timeout
}

struct GatewayActivity {
    active_connections: AtomicUsize,
    last_activity: Mutex<Instant>,
}

impl GatewayActivity {
    fn new() -> Self {
        Self {
            active_connections: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
        }
    }

    fn enter(self: &Arc<Self>) -> ActivityGuard {
        self.active_connections.fetch_add(1, Ordering::SeqCst);
        self.touch();
        ActivityGuard {
            activity: Arc::clone(self),
        }
    }

    fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
    }

    fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::SeqCst)
    }

    fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .elapsed()
    }
}

struct ActivityGuard {
    activity: Arc<GatewayActivity>,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.activity
            .active_connections
            .fetch_sub(1, Ordering::SeqCst);
        self.activity.touch();
    }
}

#[derive(Clone)]
struct GatewayState {
    bridge: Arc<Bridge>,
    activity: Arc<GatewayActivity>,
    models: Arc<ModelResponses>,
    activation_timeout: Duration,
    client: reqwest::Client,
    upstream_override: Option<u16>,
    trace_store: HttpTraceStore,
    logger: Arc<Logger>,
}

struct ModelResponses {
    list: Bytes,
    details: BTreeMap<String, Bytes>,
}

impl ModelResponses {
    fn from_catalog(catalog: Value) -> Self {
        let models = catalog["models"]
            .as_array()
            .expect("validated model catalog has a models array");
        let list =
            serde_json::to_vec(&json!({ "object": "list", "data": models })).unwrap_or_default();
        let details = models
            .iter()
            .filter_map(|model| {
                let id = model["id"].as_str()?.to_string();
                let body = serde_json::to_vec(model).ok()?;
                Some((id, Bytes::from(body)))
            })
            .collect();
        Self {
            list: Bytes::from(list),
            details,
        }
    }
}

/// Handle owning one gateway listener and its monitoring task.
pub struct GatewayHandle {
    port: u16,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    activity: Arc<GatewayActivity>,
}

impl GatewayHandle {
    /// Binds the configured loopback port and starts serving requests.
    pub async fn start(bridge: Arc<Bridge>, config: GatewayConfig) -> Result<Self, String> {
        Self::start_with_upstream_internal(
            bridge,
            config,
            None,
            crate::paths::logs_dir().join("http-trace"),
        )
        .await
    }

    #[cfg(test)]
    async fn start_with_upstream(
        bridge: Arc<Bridge>,
        config: GatewayConfig,
        upstream_port: u16,
        trace_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        Self::start_with_upstream_internal(bridge, config, Some(upstream_port), trace_root).await
    }

    async fn start_with_upstream_internal(
        bridge: Arc<Bridge>,
        config: GatewayConfig,
        upstream_override: Option<u16>,
        trace_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", config.port))
            .await
            .map_err(|error| format!("绑定本地网关端口 {} 失败: {error}", config.port))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("读取本地网关端口失败: {error}"))?
            .port();
        let (shutdown, _) = watch::channel(false);
        let activity = Arc::new(GatewayActivity::new());
        let state = GatewayState {
            bridge: Arc::clone(&bridge),
            activity: Arc::clone(&activity),
            models: Arc::new(ModelResponses::from_catalog(model_catalog::catalog()?)),
            activation_timeout: config.activation_timeout,
            client: reqwest::Client::builder()
                .no_proxy()
                .build()
                .map_err(|error| format!("创建本地 HTTP 客户端失败: {error}"))?,
            upstream_override,
            trace_store: HttpTraceStore::new(trace_root),
            logger: bridge.logger_handle(),
        };
        let app = router(state);
        let mut shutdown_rx = shutdown.subscribe();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    while !*shutdown_rx.borrow() {
                        if shutdown_rx.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await;
        });

        let idle_bridge = Arc::downgrade(&bridge);
        let idle_activity = Arc::clone(&activity);
        let idle_timeout = config.idle_timeout;
        let mut idle_shutdown = shutdown.subscribe();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    changed = idle_shutdown.changed() => {
                        if changed.is_err() || *idle_shutdown.borrow() {
                            break;
                        }
                    }
                    _ = interval.tick() => {
                        if !should_close_idle_tunnel(
                            idle_activity.active_connections(),
                            idle_activity.idle_for(),
                            idle_timeout,
                        ) {
                            continue;
                        }
                        let Some(bridge) = idle_bridge.upgrade() else {
                            break;
                        };
                        if bridge.sleep_tunnel().await {
                            idle_activity.touch();
                        }
                    }
                }
            }
        });

        Ok(Self {
            port,
            shutdown,
            task,
            activity,
        })
    }

    /// Local port bound by this gateway.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Number of HTTP requests currently being served.
    pub fn active_connections(&self) -> usize {
        self.activity.active_connections()
    }

    /// Stops accepting new requests and ends the idle monitor.
    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

fn router(state: GatewayState) -> Router {
    Router::new()
        .route("/v1/healthz", get(handle_health))
        .route("/v1/models", get(handle_models))
        .route("/v1/models/{id}", get(handle_model))
        .route("/v1/chat/completions", post(handle_chat))
        .route("/v1/responses", post(handle_responses))
        .fallback(handle_not_found)
        .method_not_allowed_fallback(handle_method_not_allowed)
        .layer(middleware::from_fn(cors_middleware))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            activity_middleware,
        ))
        .with_state(state)
}

async fn cors_middleware(request: Request<Body>, next: Next) -> Response<Body> {
    if request.method() == Method::OPTIONS && is_known_path(request.uri().path()) {
        return with_cors(StatusCode::NO_CONTENT.into_response());
    }
    with_cors(next.run(request).await)
}

async fn activity_middleware(
    State(state): State<GatewayState>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    activity_response(next.run(request).await, state.activity.enter())
}

fn begin_upstream_trace(
    state: &GatewayState,
    protocol: &str,
    exchange: &UpstreamExchange,
) -> Option<HttpTraceSession> {
    let parts = UpstreamTraceParts {
        protocol,
        request_headers: &exchange.request_headers,
        request_body: exchange.request_body.clone(),
        request_target: &exchange.request_target,
        started: exchange.started,
        started_at: exchange.started_at,
        response_headers: exchange.response.headers(),
        response_status: exchange.response.status(),
    };
    begin_upstream_trace_parts(state, &parts)
}

struct UpstreamTraceParts<'a> {
    protocol: &'a str,
    request_headers: &'a HeaderMap,
    request_body: Bytes,
    request_target: &'a str,
    started: Instant,
    started_at: std::time::SystemTime,
    response_headers: &'a HeaderMap,
    response_status: reqwest::StatusCode,
}

fn begin_upstream_trace_parts(
    state: &GatewayState,
    parts: &UpstreamTraceParts<'_>,
) -> Option<HttpTraceSession> {
    let mut trace = state
        .trace_store
        .start_with_filter(
            parts.protocol,
            state.bridge.http_log_filter(),
            parts.started,
            parts.started_at,
        )
        .ok()?;
    trace
        .write_request_http(
            "POST",
            parts.request_target,
            parts.request_headers,
            parts.request_body.clone(),
        )
        .ok();
    trace
        .write_response_http(
            status_from_reqwest(parts.response_status),
            parts.response_headers,
        )
        .ok();
    Some(trace)
}

fn response_request_id(trace: &Option<HttpTraceSession>) -> String {
    trace
        .as_ref()
        .map_or_else(new_request_id, |trace| trace.id().to_string())
}

struct TracedUpstreamState {
    source: futures_util::stream::BoxStream<'static, Result<Bytes, reqwest::Error>>,
    trace: Option<HttpTraceSession>,
    status: StatusCode,
}

impl Drop for TracedUpstreamState {
    fn drop(&mut self) {
        if let Some(mut trace) = self.trace.take() {
            let elapsed = trace.elapsed();
            let _ = trace.finish(self.status.as_u16(), "client_disconnected", elapsed);
        }
    }
}

fn traced_upstream_response(
    response: reqwest::Response,
    trace: Option<HttpTraceSession>,
    request_id: &str,
) -> Response<Body> {
    let status = status_from_reqwest(response.status());
    let mut headers = HeaderMap::new();
    copy_response_headers(response.headers(), &mut headers);
    if !headers.contains_key(REQUEST_ID_HEADER) {
        if let Ok(value) = HeaderValue::from_str(request_id) {
            headers.insert(REQUEST_ID_HEADER, value);
        }
    }
    let state = TracedUpstreamState {
        source: response.bytes_stream().boxed(),
        trace,
        status,
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        match state.source.next().await {
            Some(Ok(bytes)) => {
                if let Some(trace) = state.trace.as_mut() {
                    trace.write_response_chunk(&bytes).ok();
                }
                Some((Ok::<Bytes, std::io::Error>(bytes), state))
            }
            Some(Err(error)) => {
                if let Some(mut trace) = state.trace.take() {
                    let elapsed = trace.elapsed();
                    let _ = trace.finish(state.status.as_u16(), "stream_error", elapsed);
                }
                Some((Err(std::io::Error::other(error)), state))
            }
            None => {
                if let Some(mut trace) = state.trace.take() {
                    let result = if state.status.is_success() {
                        "completed"
                    } else {
                        "failed"
                    };
                    let elapsed = trace.elapsed();
                    let _ = trace.finish(state.status.as_u16(), result, elapsed);
                }
                None
            }
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

struct ActivityResponseState {
    source: futures_util::stream::BoxStream<'static, Result<Bytes, axum::Error>>,
    guard: Option<ActivityGuard>,
}

impl Drop for ActivityResponseState {
    fn drop(&mut self) {
        self.guard.take();
    }
}

fn activity_response(response: Response<Body>, guard: ActivityGuard) -> Response<Body> {
    let response = with_request_id(response, &new_request_id());
    let (parts, body) = response.into_parts();
    let state = ActivityResponseState {
        source: body.into_data_stream().boxed(),
        guard: Some(guard),
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        match state.source.next().await {
            Some(Ok(bytes)) => Some((Ok::<Bytes, std::io::Error>(bytes), state)),
            Some(Err(error)) => {
                state.guard.take();
                Some((Err(std::io::Error::other(error)), state))
            }
            None => {
                state.guard.take();
                None
            }
        }
    });
    Response::from_parts(parts, Body::from_stream(stream))
}

fn with_cors(mut response: Response<Body>) -> Response<Body> {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,POST,OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
    response
}

fn is_known_path(path: &str) -> bool {
    matches!(
        path,
        "/v1/healthz" | "/v1/models" | "/v1/chat/completions" | "/v1/responses"
    ) || path
        .strip_prefix("/v1/models/")
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

async fn handle_health(State(_state): State<GatewayState>) -> Response<Body> {
    Json(json!({
        "status": "ok",
        "bridge": "local-gateway"
    }))
    .into_response()
}

async fn handle_models(State(state): State<GatewayState>) -> Response<Body> {
    json_bytes_response(state.models.list.clone())
}

async fn handle_model(State(state): State<GatewayState>, Path(id): Path<String>) -> Response<Body> {
    match state.models.details.get(&id) {
        Some(model) => json_bytes_response(model.clone()),
        None => error_response(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("The model '{id}' does not exist"),
        ),
    }
}

fn json_bytes_response(body: Bytes) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

async fn handle_chat(
    State(state): State<GatewayState>,
    uri: Uri,
    mut headers: HeaderMap,
    body: Body,
) -> Response<Body> {
    let bytes = match to_bytes(body, BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "request body is too large",
            )
        }
    };
    let mut prepared = prepare_chat_request(bytes, &headers);
    if prepared.valid_json_object {
        apply_headers(&mut headers, &mut prepared.gate_report);
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
    }
    let exchange = match send_chat_request(&state, &headers, &uri, prepared.body).await {
        Ok(exchange) => exchange,
        Err(response) => return response,
    };
    let trace = begin_upstream_trace(&state, "chat", &exchange);
    let request_id = response_request_id(&trace);
    log_gate_report(&state, &request_id, &prepared.gate_report);
    if !exchange.response.status().is_success() {
        return upstream_error_response(
            &state,
            exchange,
            trace,
            &request_id,
            &prepared.gate_report,
        )
        .await;
    }
    let content_type = exchange
        .response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !prepared.downstream_stream && content_type.starts_with("text/event-stream") {
        let status = status_from_reqwest(exchange.response.status());
        return match aggregate_chat_stream(
            exchange.response,
            trace,
            status,
            &prepared.requested_model,
        )
        .await
        {
            Ok(chat) => with_request_id(Json(chat).into_response(), &request_id),
            Err((status, code, message)) => {
                error_response_with_id(status, code, message, request_id)
            }
        };
    }
    traced_upstream_response(exchange.response, trace, &request_id)
}

struct PreparedChatRequest {
    body: Bytes,
    gate_report: GateReport,
    valid_json_object: bool,
    downstream_stream: bool,
    requested_model: String,
}

fn prepare_chat_request(bytes: Bytes, headers: &HeaderMap) -> PreparedChatRequest {
    let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(&bytes) else {
        return PreparedChatRequest {
            body: bytes,
            gate_report: GateReport::default(),
            valid_json_object: false,
            downstream_stream: false,
            requested_model: String::new(),
        };
    };
    let downstream_stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let requested_model = object
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    object.insert("stream".to_string(), Value::Bool(true));
    let mut value = Value::Object(object);
    let report = normalize_chat(&mut value, headers);
    let body = if !downstream_stream || report.changed() {
        Bytes::from(
            serde_json::to_vec(&value).expect("a parsed JSON object is always serializable"),
        )
    } else {
        bytes
    };
    PreparedChatRequest {
        body,
        gate_report: report,
        valid_json_object: true,
        downstream_stream,
        requested_model,
    }
}

async fn handle_responses(
    State(state): State<GatewayState>,
    uri: Uri,
    mut headers: HeaderMap,
    body: Body,
) -> Response<Body> {
    let bytes = match to_bytes(body, BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "request body is too large",
            )
        }
    };
    let request: Value = match serde_json::from_slice(&bytes) {
        Ok(request @ Value::Object(_)) => request,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                "request body is not a valid JSON object",
            )
        }
    };
    let stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let shape = ToolShape::from_request(&request);
    let mut chat = match responses_to_chat(&request, true) {
        Ok(chat) => chat,
        Err(message) => return error_response(StatusCode::BAD_REQUEST, "invalid_json", message),
    };
    if chat.get("stream_options").is_none() {
        chat["stream_options"] = json!({ "include_usage": true });
    }
    let mut gate_report = normalize_chat(&mut chat, &headers);
    apply_headers(&mut headers, &mut gate_report);
    let exchange = loop {
        let encoded = match serde_json::to_vec(&chat) {
            Ok(encoded) => Bytes::from(encoded),
            Err(_) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "failed to encode translated request",
                )
            }
        };
        let exchange = match send_chat_request(&state, &headers, &uri, encoded).await {
            Ok(exchange) => exchange,
            Err(response) => return response,
        };
        if exchange.response.status().is_success() {
            break exchange;
        }
        let response_status = exchange.response.status();
        let status = status_from_reqwest(response_status);
        let retry_after = exchange
            .response
            .headers()
            .get(header::RETRY_AFTER)
            .cloned();
        let response_headers = exchange.response.headers().clone();
        let body = exchange.response.bytes().await.unwrap_or_default();
        let text = String::from_utf8_lossy(&body).into_owned();
        let lower = text.to_ascii_lowercase();
        let removable = if status == StatusCode::BAD_REQUEST {
            OPTIONAL_CHAT_FIELDS.iter().find_map(|(field, needles)| {
                chat.get(*field).is_some().then_some(()).filter(|_| {
                    needles
                        .iter()
                        .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
                })?;
                Some(*field)
            })
        } else {
            None
        };
        if let Some(field) = removable {
            if let Some(object) = chat.as_object_mut() {
                object.remove(field);
            }
            continue;
        }
        let trace = begin_upstream_trace_parts(
            &state,
            &UpstreamTraceParts {
                protocol: "responses",
                request_headers: &exchange.request_headers,
                request_body: exchange.request_body.clone(),
                request_target: &exchange.request_target,
                started: exchange.started,
                started_at: exchange.started_at,
                response_headers: &response_headers,
                response_status,
            },
        );
        let request_id = response_request_id(&trace);
        log_gate_report(&state, &request_id, &gate_report);
        if let Some(mut trace) = trace {
            trace.write_response_chunk(&body).ok();
            let elapsed = trace.elapsed();
            trace.finish(status.as_u16(), "failed", elapsed).ok();
        }
        return upstream_error_from_text(
            status,
            retry_after,
            text,
            &request_id,
            &gate_report,
            &state.logger,
        );
    };
    let mut trace = begin_upstream_trace(&state, "responses", &exchange);
    let request_id = response_request_id(&trace);
    log_gate_report(&state, &request_id, &gate_report);
    let status = status_from_reqwest(exchange.response.status());
    let content_type = exchange
        .response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if stream && content_type.starts_with("text/event-stream") {
        return responses_stream_response(exchange.response, model, shape, trace, &request_id);
    }
    let chat = if content_type.starts_with("text/event-stream") {
        match aggregate_chat_stream(exchange.response, trace, status, &model).await {
            Ok(chat) => chat,
            Err((status, code, message)) => {
                return error_response_with_id(status, code, message, request_id)
            }
        }
    } else {
        let body = match exchange.response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                if let Some(mut trace) = trace.take() {
                    let elapsed = trace.elapsed();
                    trace.finish(status.as_u16(), "stream_error", elapsed).ok();
                }
                return error_response_with_id(
                    StatusCode::BAD_GATEWAY,
                    "upstream_network_error",
                    format!("failed to read upstream response: {error}"),
                    request_id,
                );
            }
        };
        if let Some(mut trace) = trace {
            trace.write_response_chunk(&body).ok();
            let elapsed = trace.elapsed();
            trace.finish(status.as_u16(), "completed", elapsed).ok();
        }
        match serde_json::from_slice(&body) {
            Ok(chat) => chat,
            Err(error) => {
                return error_response_with_id(
                    StatusCode::BAD_GATEWAY,
                    "upstream_server_error",
                    format!("upstream response is not valid JSON: {error}"),
                    request_id,
                )
            }
        }
    };
    match chat_to_responses(&chat, &model, &shape) {
        Ok(response) => with_request_id(Json(response).into_response(), &request_id),
        Err(message) => error_response_with_id(
            StatusCode::BAD_GATEWAY,
            "malformed_tool_call_arguments",
            message,
            request_id,
        ),
    }
}

async fn ensure_upstream_port(state: &GatewayState) -> Result<u16, Response<Body>> {
    if let Some(port) = state.upstream_override {
        return Ok(port);
    }
    let bridge = Arc::clone(&state.bridge);
    let activation = tokio::spawn(async move { bridge.ensure_active().await });
    match tokio::time::timeout(state.activation_timeout, activation).await {
        Ok(Ok(Ok(port))) => Ok(port),
        Ok(Ok(Err(error))) => Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "workspace_unavailable",
            error.to_string(),
        )),
        Ok(Err(error)) => Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "workspace_unavailable",
            format!("workspace activation task failed: {error}"),
        )),
        Err(_) => Err(error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "workspace_activation_timeout",
            "workspace activation timed out",
        )),
    }
}

struct UpstreamExchange {
    response: reqwest::Response,
    request_headers: HeaderMap,
    request_body: Bytes,
    request_target: String,
    started: Instant,
    started_at: std::time::SystemTime,
}

async fn send_chat_request(
    state: &GatewayState,
    headers: &HeaderMap,
    uri: &Uri,
    body: Bytes,
) -> Result<UpstreamExchange, Response<Body>> {
    let mut last_error = None;
    for attempt in 0..2 {
        let port = ensure_upstream_port(state).await?;
        let mut request_target = "/chat/completions".to_string();
        if let Some(query) = uri.query() {
            request_target.push('?');
            request_target.push_str(query);
        }
        let url = format!("http://127.0.0.1:{port}{request_target}");
        let request_headers = forwarded_request_headers(headers, port, body.len());
        let started = Instant::now();
        let started_at = std::time::SystemTime::now();
        let request = state
            .client
            .post(url)
            .headers(request_headers.clone())
            .body(body.clone());
        match request.send().await {
            Ok(response) => {
                return Ok(UpstreamExchange {
                    response,
                    request_headers,
                    request_body: body,
                    request_target,
                    started,
                    started_at,
                })
            }
            Err(error) => {
                last_error = Some(error.to_string());
                if attempt == 0 && state.upstream_override.is_none() {
                    state
                        .bridge
                        .invalidate_tunnel(port, format!("本地 HTTP 网关连接 agent 失败: {error}"))
                        .await;
                }
            }
        }
    }
    Err(error_response(
        StatusCode::BAD_GATEWAY,
        "upstream_network_error",
        last_error.unwrap_or_else(|| "could not reach upstream".to_string()),
    ))
}

fn forwarded_request_headers(headers: &HeaderMap, port: u16, content_length: usize) -> HeaderMap {
    let mut forwarded = HeaderMap::new();
    for (name, value) in headers.iter() {
        if !is_request_hop_header(name.as_str()) {
            forwarded.append(name, value.clone());
        }
    }
    if let Ok(value) = HeaderValue::from_str(&content_length.to_string()) {
        forwarded.insert(header::CONTENT_LENGTH, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("127.0.0.1:{port}")) {
        forwarded.insert(header::HOST, value);
    }
    forwarded
}

async fn upstream_error_response(
    state: &GatewayState,
    exchange: UpstreamExchange,
    mut trace: Option<HttpTraceSession>,
    request_id: &str,
    gate_report: &GateReport,
) -> Response<Body> {
    let status = status_from_reqwest(exchange.response.status());
    let retry_after = exchange
        .response
        .headers()
        .get(header::RETRY_AFTER)
        .cloned();
    let body = exchange.response.bytes().await.unwrap_or_default();
    if let Some(trace) = trace.as_mut() {
        trace.write_response_chunk(&body).ok();
        let elapsed = trace.elapsed();
        trace.finish(status.as_u16(), "failed", elapsed).ok();
    }
    upstream_error_from_text(
        status,
        retry_after,
        String::from_utf8_lossy(&body).into_owned(),
        request_id,
        gate_report,
        &state.logger,
    )
}

fn upstream_error_from_text(
    status: StatusCode,
    retry_after: Option<HeaderValue>,
    text: String,
    request_id: &str,
    gate_report: &GateReport,
    logger: &Logger,
) -> Response<Body> {
    if let Some(block) = detect_gate_block(&text) {
        logger.log(
            "warn",
            format!(
                "prompt_gate.miss request_id={request_id};upstream_request_id={};{}",
                block.upstream_request_id.as_deref().unwrap_or("-"),
                gate_report.summary()
            ),
        );
        return error_response_with_id(
            status,
            "prompt_gate_blocked",
            block.message,
            request_id.to_string(),
        );
    }
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| {
            if text.is_empty() {
                format!("upstream returned HTTP {}", status.as_u16())
            } else {
                text
            }
        });
    let code = match status.as_u16() {
        400 => "upstream_invalid_request",
        401 => "upstream_unauthorized",
        403 => "upstream_forbidden",
        408 => "upstream_timeout",
        429 => "upstream_rate_limited",
        500..=599 => "upstream_server_error",
        _ => "upstream_invalid_request",
    };
    let mut response = error_response_with_id(status, code, message, request_id.to_string());
    if let Some(retry_after) = retry_after {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, retry_after);
    }
    response
}

fn log_gate_report(state: &GatewayState, request_id: &str, report: &GateReport) {
    if !report.changed() && !report.user_agent_overridden() {
        return;
    }
    state.logger.log(
        "info",
        format!(
            "prompt_gate.applied request_id={request_id};profiles={};{}",
            report.profile_order().join(","),
            report.summary()
        ),
    );
}

fn status_from_reqwest(status: reqwest::StatusCode) -> StatusCode {
    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
}

fn is_request_hop_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "content-length"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn copy_response_headers(source: &reqwest::header::HeaderMap, destination: &mut HeaderMap) {
    for (name, value) in source.iter() {
        if matches!(
            name.as_str(),
            "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        ) {
            continue;
        }
        if let Ok(value) = HeaderValue::from_bytes(value.as_bytes()) {
            destination.append(name, value);
        }
    }
}

#[derive(Default)]
struct AggregateToolCall {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct AggregateChoice {
    content: String,
    reasoning_content: String,
    has_content: bool,
    tool_calls: BTreeMap<usize, AggregateToolCall>,
    finish_reason: Option<String>,
}

#[derive(Default)]
struct ChatCompletionAccumulator {
    id: Option<String>,
    created: Option<u64>,
    model: Option<String>,
    system_fingerprint: Option<Value>,
    usage: Option<Value>,
    choices: BTreeMap<usize, AggregateChoice>,
    saw_done: bool,
    saw_finish: bool,
}

impl ChatCompletionAccumulator {
    fn push_data(&mut self, payload: &str) -> Result<(), String> {
        if payload == "[DONE]" {
            self.saw_done = true;
            return Ok(());
        }
        let chunk: Value =
            serde_json::from_str(payload).map_err(|error| format!("无效 SSE JSON: {error}"))?;
        if let Some(error) = chunk.get("error").filter(|value| !value.is_null()) {
            return Err(format!("上游流内错误: {error}"));
        }
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            self.id = Some(id.to_string());
        }
        if let Some(created) = chunk.get("created").and_then(Value::as_u64) {
            self.created = Some(created);
        }
        if let Some(model) = chunk.get("model").and_then(Value::as_str) {
            self.model = Some(model.to_string());
        }
        if let Some(fingerprint) = chunk
            .get("system_fingerprint")
            .filter(|value| !value.is_null())
        {
            self.system_fingerprint = Some(fingerprint.clone());
        }
        if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
            self.usage = Some(usage.clone());
        }
        for choice in chunk
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let state = self.choices.entry(index).or_default();
            let delta = choice
                .get("delta")
                .or_else(|| choice.get("message"))
                .unwrap_or(&Value::Null);
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                state.has_content = true;
                state.content.push_str(content);
            }
            if let Some(reasoning) = aggregate_reasoning_text(delta) {
                state.reasoning_content.push_str(reasoning);
            }
            for tool in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let tool_index = tool.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let tool_state = state.tool_calls.entry(tool_index).or_default();
                if let Some(id) = tool.get("id").and_then(Value::as_str) {
                    tool_state.id = id.to_string();
                }
                if let Some(kind) = tool.get("type").and_then(Value::as_str) {
                    tool_state.kind = kind.to_string();
                }
                if let Some(name) = tool.pointer("/function/name").and_then(Value::as_str) {
                    tool_state.name.push_str(name);
                }
                if let Some(arguments) = tool.pointer("/function/arguments").and_then(Value::as_str)
                {
                    tool_state.arguments.push_str(arguments);
                }
            }
            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .filter(|reason| !reason.is_empty())
            {
                state.finish_reason = Some(reason.to_string());
                self.saw_finish = true;
            }
        }
        Ok(())
    }

    fn finish(self, requested_model: &str) -> Result<Value, String> {
        if !self.saw_done && !self.saw_finish {
            return Err("上游 SSE 缺少完成分片或 [DONE]".to_string());
        }
        let choices = self
            .choices
            .into_iter()
            .map(|(index, choice)| {
                let mut message = json!({
                    "role": "assistant",
                    "content": if choice.has_content {
                        Value::String(choice.content)
                    } else {
                        Value::Null
                    }
                });
                if !choice.reasoning_content.is_empty() {
                    message["reasoning_content"] = Value::String(choice.reasoning_content);
                }
                if !choice.tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(
                        choice
                            .tool_calls
                            .into_values()
                            .map(|tool| {
                                json!({
                                    "id": tool.id,
                                    "type": if tool.kind.is_empty() { "function" } else { tool.kind.as_str() },
                                    "function": {
                                        "name": tool.name,
                                        "arguments": tool.arguments
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                json!({
                    "index": index,
                    "message": message,
                    "finish_reason": choice.finish_reason
                })
            })
            .collect::<Vec<_>>();
        let mut completion = json!({
            "id": self.id.unwrap_or_else(|| format!("chatcmpl_{}", new_request_id())),
            "object": "chat.completion",
            "created": self.created.unwrap_or_else(|| chrono::Utc::now().timestamp() as u64),
            "model": self.model.unwrap_or_else(|| requested_model.to_string()),
            "choices": choices
        });
        if let Some(fingerprint) = self.system_fingerprint {
            completion["system_fingerprint"] = fingerprint;
        }
        if let Some(usage) = self.usage {
            completion["usage"] = usage;
        }
        Ok(completion)
    }
}

fn aggregate_reasoning_text(delta: &Value) -> Option<&str> {
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
}

fn aggregate_response_too_large(current: usize, additional: usize) -> bool {
    current.saturating_add(additional) > BODY_LIMIT
}

async fn aggregate_chat_stream(
    response: reqwest::Response,
    mut trace: Option<HttpTraceSession>,
    status: StatusCode,
    requested_model: &str,
) -> Result<Value, (StatusCode, &'static str, String)> {
    let mut chunks = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut total = 0usize;
    let mut accumulator = ChatCompletionAccumulator::default();
    while let Some(chunk) = chunks.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                if let Some(mut trace) = trace.take() {
                    let elapsed = trace.elapsed();
                    trace.finish(status.as_u16(), "stream_error", elapsed).ok();
                }
                return Err((
                    StatusCode::BAD_GATEWAY,
                    "upstream_network_error",
                    format!("failed to read upstream stream: {error}"),
                ));
            }
        };
        if aggregate_response_too_large(total, chunk.len()) {
            if let Some(mut trace) = trace.take() {
                let elapsed = trace.elapsed();
                trace.finish(status.as_u16(), "failed", elapsed).ok();
            }
            return Err((
                StatusCode::BAD_GATEWAY,
                "upstream_server_error",
                "aggregated upstream response exceeds 64 MiB".to_string(),
            ));
        }
        total += chunk.len();
        if let Some(trace) = trace.as_mut() {
            trace.write_response_chunk(&chunk).ok();
        }
        buffer.extend_from_slice(&chunk);
        let mut consumed = 0;
        while let Some(relative) = buffer[consumed..].iter().position(|byte| *byte == b'\n') {
            let position = consumed + relative;
            if let Err(error) =
                process_aggregate_line(&buffer[consumed..=position], &mut accumulator)
            {
                if let Some(mut trace) = trace.take() {
                    let elapsed = trace.elapsed();
                    trace.finish(status.as_u16(), "failed", elapsed).ok();
                }
                return Err(error);
            }
            consumed = position + 1;
            if accumulator.saw_done {
                break;
            }
        }
        if consumed > 0 {
            buffer.drain(..consumed);
        }
        if accumulator.saw_done {
            break;
        }
    }
    if !buffer.is_empty() && !accumulator.saw_done {
        if let Err(error) = process_aggregate_line(&buffer, &mut accumulator) {
            if let Some(mut trace) = trace.take() {
                let elapsed = trace.elapsed();
                trace.finish(status.as_u16(), "failed", elapsed).ok();
            }
            return Err(error);
        }
    }
    let chat = accumulator.finish(requested_model).map_err(|message| {
        (
            StatusCode::BAD_GATEWAY,
            "upstream_server_error",
            format!("failed to aggregate upstream chat stream: {message}"),
        )
    })?;
    if let Some(mut trace) = trace.take() {
        let elapsed = trace.elapsed();
        trace.finish(status.as_u16(), "completed", elapsed).ok();
    }
    Ok(chat)
}

fn process_aggregate_line(
    line: &[u8],
    accumulator: &mut ChatCompletionAccumulator,
) -> Result<(), (StatusCode, &'static str, String)> {
    let line = String::from_utf8_lossy(line);
    let Some(payload) = line.trim_end().strip_prefix("data:") else {
        return Ok(());
    };
    accumulator
        .push_data(payload.trim())
        .map_err(|message| (StatusCode::BAD_GATEWAY, "upstream_server_error", message))
}

struct ResponsesStreamState {
    source: futures_util::stream::BoxStream<'static, Result<Bytes, reqwest::Error>>,
    translator: ResponsesStream,
    buffer: Vec<u8>,
    pending: std::collections::VecDeque<Result<Bytes, std::io::Error>>,
    done: bool,
    trace: Option<HttpTraceSession>,
    status: StatusCode,
}

impl Drop for ResponsesStreamState {
    fn drop(&mut self) {
        if let Some(mut trace) = self.trace.take() {
            let elapsed = trace.elapsed();
            trace
                .finish(self.status.as_u16(), "client_disconnected", elapsed)
                .ok();
        }
    }
}

fn responses_stream_response(
    response: reqwest::Response,
    model: String,
    shape: ToolShape,
    trace: Option<HttpTraceSession>,
    request_id: &str,
) -> Response<Body> {
    let status = status_from_reqwest(response.status());
    let response_id = format!(
        "resp_{}_{}",
        chrono::Utc::now().timestamp_millis(),
        next_request_sequence()
    );
    let state = ResponsesStreamState {
        source: response.bytes_stream().boxed(),
        translator: ResponsesStream::with_shape(response_id, model, shape),
        buffer: Vec::new(),
        pending: std::collections::VecDeque::new(),
        done: false,
        trace,
        status,
    };
    let stream = futures_util::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(item) = state.pending.pop_front() {
                return Some((item, state));
            }
            if state.done {
                return None;
            }
            match state.source.next().await {
                Some(Ok(chunk)) => {
                    if let Some(trace) = state.trace.as_mut() {
                        trace.write_response_chunk(&chunk).ok();
                    }
                    state.buffer.extend_from_slice(&chunk);
                    let mut consumed = 0;
                    while let Some(relative) = state.buffer[consumed..]
                        .iter()
                        .position(|byte| *byte == b'\n')
                    {
                        let position = consumed + relative;
                        let line = String::from_utf8_lossy(&state.buffer[consumed..=position]);
                        consumed = position + 1;
                        let Some(payload) = line.trim().strip_prefix("data:") else {
                            continue;
                        };
                        if payload.trim() == "[DONE]" {
                            for event in state.translator.finish() {
                                state.pending.push_back(Ok(Bytes::from(event)));
                            }
                            if let Some(mut trace) = state.trace.take() {
                                let elapsed = trace.elapsed();
                                trace
                                    .finish(state.status.as_u16(), "completed", elapsed)
                                    .ok();
                            }
                            state.done = true;
                            break;
                        }
                        if let Ok(chunk) = serde_json::from_str::<Value>(payload.trim()) {
                            for event in state.translator.push(&chunk) {
                                state.pending.push_back(Ok(Bytes::from(event)));
                            }
                        }
                    }
                    if consumed > 0 {
                        state.buffer.drain(..consumed);
                    }
                }
                Some(Err(error)) => {
                    if let Some(mut trace) = state.trace.take() {
                        let elapsed = trace.elapsed();
                        trace
                            .finish(state.status.as_u16(), "stream_error", elapsed)
                            .ok();
                    }
                    state
                        .pending
                        .push_back(Err(std::io::Error::other(error.to_string())));
                    state.done = true;
                }
                None => {
                    for event in state.translator.finish() {
                        state.pending.push_back(Ok(Bytes::from(event)));
                    }
                    if let Some(mut trace) = state.trace.take() {
                        let elapsed = trace.elapsed();
                        trace
                            .finish(state.status.as_u16(), "completed", elapsed)
                            .ok();
                    }
                    state.done = true;
                }
            }
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    with_request_id(response, request_id)
}

async fn handle_not_found() -> Response<Body> {
    error_response(StatusCode::NOT_FOUND, "not_found", "endpoint not found")
}

async fn handle_method_not_allowed() -> Response<Body> {
    error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "HTTP method not allowed",
    )
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorDetail,
    request_id: String,
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
    message: String,
    #[serde(rename = "type")]
    error_type: &'static str,
    code: String,
    param: Option<String>,
    retryable: bool,
}

fn error_response(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
) -> Response<Body> {
    error_response_with_id(status, code, message, new_request_id())
}

fn error_response_with_id(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
    request_id: String,
) -> Response<Body> {
    let code = code.into();
    let (error_type, retryable) = error_contract(&code, status);
    let payload = ErrorEnvelope {
        error: ErrorDetail {
            message: message.into(),
            error_type,
            code,
            param: None,
            retryable,
        },
        request_id: request_id.clone(),
    };
    let mut response = (status, Json(payload)).into_response();
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

fn new_request_id() -> String {
    format!(
        "req_{:x}_{:x}",
        chrono::Utc::now().timestamp_millis(),
        next_request_sequence()
    )
}

fn next_request_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

fn error_contract(code: &str, status: StatusCode) -> (&'static str, bool) {
    match code {
        "upstream_unauthorized" => ("authentication_error", false),
        "upstream_forbidden" => ("permission_error", false),
        "upstream_rate_limited" => ("rate_limit_error", true),
        "upstream_timeout"
        | "upstream_network_error"
        | "upstream_server_error"
        | "internal_error" => ("server_error", true),
        _ if status.is_server_error() => ("server_error", true),
        _ => ("invalid_request_error", false),
    }
}

fn with_request_id(mut response: Response<Body>, request_id: &str) -> Response<Body> {
    if !response.headers().contains_key(REQUEST_ID_HEADER) {
        if let Ok(value) = HeaderValue::from_str(request_id) {
            response.headers_mut().insert(REQUEST_ID_HEADER, value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
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
        let (responses_request, responses_response) =
            store.record_paths(&responses_record.id).unwrap();
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
            let prepared =
                prepare_chat_request(Bytes::from(request.to_string()), &HeaderMap::new());
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
            .push_data(
                r#"{"choices":[{"index":0,"delta":{"content":"half"},"finish_reason":null}]}"#,
            )
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
        let (network_temp, _bridge, network_gateway) =
            test_gateway_with_upstream(unused_port).await;
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

    async fn test_gateway_with_upstream(
        upstream_port: u16,
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
            },
            upstream_port,
            temp.path().join("http-trace"),
        )
        .await
        .unwrap();
        (temp, bridge, gateway)
    }
}
