//! Persistent loopback HTTP gateway that activates CNB only when needed.

use crate::bridge::Bridge;
use crate::http_trace::{HttpTraceSession, HttpTraceStore};
use crate::logging::Logger;
use crate::model_catalog;
use crate::prompt_gate::{apply_headers, detect_gate_block, normalize_chat, GateReport};
use crate::responses::{
    chat_to_responses, ignored_feature_details_with_options, responses_to_chat_with_options,
    unsupported_feature_names, ResponsesStream, ToolShape, TranslationOptions,
};
use axum::body::{Body, Bytes};
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
/// Chat fields dropped when the upstream rejects them by name.
///
/// Only needles unique to the field belong here: matching on `json_schema`
/// would also fire for tool parameter errors and strip a field that was fine.
const OPTIONAL_CHAT_FIELDS: [(&str, &[&str]); 17] = [
    ("stream_options", &["stream_options", "include_usage"]),
    (
        "reasoning_effort",
        &["reasoning_effort", "reasoning effort"],
    ),
    ("parallel_tool_calls", &["parallel_tool_calls"]),
    ("prompt_cache_key", &["prompt_cache_key"]),
    ("service_tier", &["service_tier"]),
    ("tool_choice", &["tool_choice"]),
    ("max_tokens", &["max_tokens"]),
    ("response_format", &["response_format", "response format"]),
    ("frequency_penalty", &["frequency_penalty"]),
    ("logit_bias", &["logit_bias"]),
    ("logprobs", &["logprobs"]),
    ("metadata", &["metadata"]),
    ("n", &["parameter n", "field n", "\"n\""]),
    ("presence_penalty", &["presence_penalty"]),
    (
        "stop",
        &[
            "unsupported parameter: stop",
            "parameter stop",
            "field stop",
            "\"stop\"",
        ],
    ),
    ("top_logprobs", &["top_logprobs"]),
    ("user", &["parameter user", "field user", "\"user\""]),
];

#[derive(Debug)]
pub(super) enum BodyReadError {
    TooLarge,
    Invalid(String),
}

pub(super) async fn read_body_limited(
    headers: &HeaderMap,
    body: Body,
) -> Result<Bytes, BodyReadError> {
    if let Some(length) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    {
        if length > BODY_LIMIT {
            return Err(BodyReadError::TooLarge);
        }
    }
    let mut stream = body.into_data_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| BodyReadError::Invalid(error.to_string()))?;
        if body.len().saturating_add(chunk.len()) > BODY_LIMIT {
            return Err(BodyReadError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(body))
}

pub(super) fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
}

mod chat;
mod error;
mod handle;
mod models;
mod responses_endpoint;
mod router;
mod state;
mod trace;
mod upstream;

use chat::*;
use error::*;
use handle::*;
pub use handle::{GatewayConfig, GatewayHandle};
use models::*;
use responses_endpoint::*;
use router::*;
use state::*;
use trace::*;
use upstream::*;

#[cfg(test)]
mod tests;
