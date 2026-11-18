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
