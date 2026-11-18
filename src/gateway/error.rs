use super::*;

pub(super) async fn handle_not_found() -> Response<Body> {
    error_response(StatusCode::NOT_FOUND, "not_found", "endpoint not found")
}

pub(super) async fn handle_method_not_allowed() -> Response<Body> {
    error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "HTTP method not allowed",
    )
}

#[derive(Debug, Serialize)]
pub(super) struct ErrorEnvelope {
    error: ErrorDetail,
    request_id: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ErrorDetail {
    message: String,
    #[serde(rename = "type")]
    error_type: &'static str,
    code: String,
    param: Option<String>,
    retryable: bool,
}

pub(super) fn error_response(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
) -> Response<Body> {
    error_response_with_id(status, code, message, new_request_id())
}

pub(super) fn error_response_with_id(
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

pub(super) fn new_request_id() -> String {
    format!(
        "req_{:x}_{:x}",
        chrono::Utc::now().timestamp_millis(),
        next_request_sequence()
    )
}

pub(super) fn next_request_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

pub(super) fn error_contract(code: &str, status: StatusCode) -> (&'static str, bool) {
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

pub(super) fn with_request_id(mut response: Response<Body>, request_id: &str) -> Response<Body> {
    if !response.headers().contains_key(REQUEST_ID_HEADER) {
        if let Ok(value) = HeaderValue::from_str(request_id) {
            response.headers_mut().insert(REQUEST_ID_HEADER, value);
        }
    }
    response
}
