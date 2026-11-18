use super::*;

pub(super) fn router(state: GatewayState) -> Router {
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

pub(super) async fn cors_middleware(request: Request<Body>, next: Next) -> Response<Body> {
    if request.method() == Method::OPTIONS && is_known_path(request.uri().path()) {
        return with_cors(StatusCode::NO_CONTENT.into_response());
    }
    with_cors(next.run(request).await)
}

pub(super) async fn activity_middleware(
    State(state): State<GatewayState>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    activity_response(next.run(request).await, state.activity.enter())
}

pub(super) fn with_cors(mut response: Response<Body>) -> Response<Body> {
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

pub(super) fn is_known_path(path: &str) -> bool {
    matches!(
        path,
        "/v1/healthz" | "/v1/models" | "/v1/chat/completions" | "/v1/responses"
    ) || path
        .strip_prefix("/v1/models/")
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

pub(super) async fn handle_health(State(_state): State<GatewayState>) -> Response<Body> {
    Json(json!({
        "status": "ok",
        "bridge": "local-gateway"
    }))
    .into_response()
}
