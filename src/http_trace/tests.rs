use super::format::next_trace_id;
use super::session::{ABNORMAL_HEAD_BYTES, ABNORMAL_TAIL_BYTES, ABNORMAL_TRUNCATION_MARKER};
use super::summary::{read_http_prefix, HTTP_PREFIX_READ_BYTES};
use super::*;
use crate::settings::HttpLogFilter;
use axum::body::Bytes;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use std::fs::{self, File};
use std::io::Write;
use std::time::SystemTime;

#[test]
fn records_flat_raw_http_pair_and_summary() {
    let dir = tempfile::tempdir().unwrap();
    let store = HttpTraceStore::new(dir.path());
    let mut headers = HeaderMap::new();
    headers.append(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer secret"),
    );
    headers.append("x-custom", HeaderValue::from_static("one"));
    headers.append("x-custom", HeaderValue::from_static("two"));
    let mut trace = store.start("responses").unwrap();
    trace
        .write_request_http(
            "POST",
            "/chat/completions?trace=1",
            &headers,
            Bytes::from_static(br#"{"input":"hello"}"#),
        )
        .unwrap();
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response_headers.insert("x-request-id", HeaderValue::from_static("upstream-123"));
    trace
        .write_response_http(StatusCode::OK, &response_headers)
        .unwrap();
    trace.write_response_chunk(br#"{"ok":true}"#).unwrap();
    trace
        .finish(200, "completed", std::time::Duration::from_millis(12))
        .unwrap();

    let (request_path, response_path) = store.record_paths(trace.id()).unwrap();
    assert_eq!(request_path.parent(), Some(dir.path()));
    assert_eq!(response_path.parent(), Some(dir.path()));
    assert_eq!(
        fs::read(&request_path).unwrap(),
        b"POST /chat/completions?trace=1 HTTP/1.1\r\nauthorization: Bearer secret\r\nx-custom: one\r\nx-custom: two\r\n\r\n{\"input\":\"hello\"}"
    );
    assert_eq!(
        fs::read(&response_path).unwrap(),
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-request-id: upstream-123\r\n\r\n{\"ok\":true}"
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);

    let summaries = store.list(10).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].method, "POST");
    assert_eq!(summaries[0].path, "/chat/completions");
    assert_eq!(summaries[0].status, 200);
    assert_eq!(summaries[0].protocol, "responses->chat");
    assert!(!summaries[0].stream);
    assert_eq!(summaries[0].request_bytes, 17);
    assert_eq!(summaries[0].response_bytes, 11);
    assert_eq!(summaries[0].result, "completed");
}

#[test]
fn truncates_both_flat_files_at_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = HttpTraceStore::with_limits(
        dir.path(),
        HttpTraceLimits {
            file_bytes: 100,
            dir_bytes: 1024,
            keep_days: 7,
        },
    );
    let mut trace = store.start("chat").unwrap();
    trace
        .write_request_http(
            "POST",
            "/chat/completions",
            &HeaderMap::new(),
            Bytes::from(vec![b'x'; 200]),
        )
        .unwrap();
    trace
        .write_response_http(StatusCode::OK, &HeaderMap::new())
        .unwrap();
    trace.write_response_chunk(&[b'y'; 200]).unwrap();
    trace
        .finish(200, "completed", std::time::Duration::from_millis(1))
        .unwrap();
    let (request_path, response_path) = store.record_paths(trace.id()).unwrap();
    assert_eq!(fs::metadata(request_path).unwrap().len(), 100);
    assert_eq!(fs::metadata(response_path).unwrap().len(), 100);
    let summary = store.list(1).unwrap().remove(0);
    assert!(summary.request_truncated);
    assert!(summary.response_truncated);
}

#[test]
fn cleanup_removes_flat_pairs_and_supports_legacy_meta() {
    let dir = tempfile::tempdir().unwrap();
    let store = HttpTraceStore::with_limits(
        dir.path(),
        HttpTraceLimits {
            file_bytes: 1024,
            dir_bytes: 1,
            keep_days: 7,
        },
    );
    let mut old = store.start("chat").unwrap();
    old.write_request_http(
        "POST",
        "/chat/completions",
        &HeaderMap::new(),
        Bytes::from_static(b"old"),
    )
    .unwrap();
    old.write_response_http(StatusCode::OK, &HeaderMap::new())
        .unwrap();
    old.write_response_chunk(b"old").unwrap();
    old.finish(200, "completed", std::time::Duration::from_millis(1))
        .unwrap();
    let old_id = old.id().to_string();
    let mut recent = store.start("chat").unwrap();
    recent
        .write_request_http(
            "POST",
            "/chat/completions",
            &HeaderMap::new(),
            Bytes::from_static(b"recent"),
        )
        .unwrap();
    recent
        .write_response_http(StatusCode::OK, &HeaderMap::new())
        .unwrap();
    recent.write_response_chunk(b"recent").unwrap();
    recent
        .finish(200, "completed", std::time::Duration::from_millis(1))
        .unwrap();
    store.cleanup_now().unwrap();
    let summaries = store.list(10).unwrap();
    assert_eq!(summaries.len(), 1);
    let (old_request, old_response) = store.record_paths(&old_id).unwrap();
    assert!(!old_request.exists());
    assert!(!old_response.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);

    let legacy = dir.path().join("trace_legacy");
    fs::create_dir_all(&legacy).unwrap();
    let legacy_summary = HttpTraceSummary {
        id: "trace_legacy".to_string(),
        started_at: "2026-11-16T20:00:00+08:00".to_string(),
        method: "POST".to_string(),
        path: "/responses".to_string(),
        status: 200,
        elapsed_ms: 12,
        protocol: "responses->chat".to_string(),
        stream: true,
        request_bytes: 10,
        response_bytes: 20,
        request_truncated: false,
        response_truncated: false,
        result: "completed".to_string(),
    };
    fs::write(
        legacy.join("meta.json"),
        serde_json::to_vec(&legacy_summary).unwrap(),
    )
    .unwrap();
    assert!(store
        .list(10)
        .unwrap()
        .iter()
        .any(|summary| summary.id == "trace_legacy"));
}

#[test]
fn abnormal_filter_discards_successful_completed_exchange() {
    let dir = tempfile::tempdir().unwrap();
    let store = HttpTraceStore::new(dir.path());
    let mut trace = store
        .start_with_filter(
            "chat",
            HttpLogFilter::Abnormal,
            std::time::Instant::now(),
            SystemTime::now(),
        )
        .unwrap();
    trace
        .write_request_http(
            "POST",
            "/chat/completions",
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .unwrap();
    trace
        .write_response_http(StatusCode::OK, &HeaderMap::new())
        .unwrap();
    trace
        .finish(200, "completed", std::time::Duration::from_millis(1))
        .unwrap();

    assert!(store.list(10).unwrap().is_empty());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn abnormal_filter_keeps_errors_and_interrupted_responses() {
    let dir = tempfile::tempdir().unwrap();
    let store = HttpTraceStore::new(dir.path());
    for (protocol, status, result) in [
        ("chat", 503, "failed"),
        ("responses", 200, "stream_error"),
        ("responses", 200, "client_disconnected"),
    ] {
        let mut trace = store
            .start_with_filter(
                protocol,
                HttpLogFilter::Abnormal,
                std::time::Instant::now(),
                SystemTime::now(),
            )
            .unwrap();
        trace
            .write_request_http(
                "POST",
                "/chat/completions",
                &HeaderMap::new(),
                Bytes::from_static(b"{}"),
            )
            .unwrap();
        trace
            .write_response_http(StatusCode::from_u16(status).unwrap(), &HeaderMap::new())
            .unwrap();
        trace
            .finish(status, result, std::time::Duration::from_millis(1))
            .unwrap();
    }

    assert_eq!(store.list(10).unwrap().len(), 3);
}

#[test]
fn abnormal_filter_discards_redirects_without_failure() {
    let dir = tempfile::tempdir().unwrap();
    let store = HttpTraceStore::new(dir.path());
    let mut trace = store
        .start_with_filter(
            "chat",
            HttpLogFilter::Abnormal,
            std::time::Instant::now(),
            SystemTime::now(),
        )
        .unwrap();
    trace
        .write_request_http(
            "POST",
            "/chat/completions",
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .unwrap();
    trace
        .write_response_http(StatusCode::TEMPORARY_REDIRECT, &HeaderMap::new())
        .unwrap();
    trace
        .finish(307, "completed", std::time::Duration::from_millis(1))
        .unwrap();

    assert!(store.list(10).unwrap().is_empty());
}

#[test]
fn all_filter_keeps_successful_completed_exchange() {
    let dir = tempfile::tempdir().unwrap();
    let store = HttpTraceStore::new(dir.path());
    let mut trace = store
        .start_with_filter(
            "chat",
            HttpLogFilter::All,
            std::time::Instant::now(),
            SystemTime::now(),
        )
        .unwrap();
    trace
        .write_request_http(
            "POST",
            "/chat/completions",
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .unwrap();
    trace
        .write_response_http(StatusCode::OK, &HeaderMap::new())
        .unwrap();
    trace
        .finish(200, "completed", std::time::Duration::from_millis(1))
        .unwrap();

    assert_eq!(store.list(10).unwrap().len(), 1);
}

#[test]
fn abnormal_stream_keeps_bounded_head_and_tail_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = HttpTraceStore::new(dir.path());
    let mut trace = store
        .start_with_filter(
            "chat",
            HttpLogFilter::Abnormal,
            std::time::Instant::now(),
            SystemTime::now(),
        )
        .unwrap();
    trace
        .write_request_http(
            "POST",
            "/chat/completions",
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    trace.write_response_http(StatusCode::OK, &headers).unwrap();
    let mut body = vec![b'H'; ABNORMAL_HEAD_BYTES];
    body.extend(std::iter::repeat_n(b'M', 1024));
    body.extend(std::iter::repeat_n(b'T', ABNORMAL_TAIL_BYTES));
    trace.write_response_chunk(&body).unwrap();
    trace
        .finish(200, "stream_error", std::time::Duration::from_millis(5))
        .unwrap();

    let summary = store.list(1).unwrap().remove(0);
    assert_eq!(summary.response_bytes, body.len() as u64);
    assert!(summary.response_truncated);
    assert_eq!(summary.result, "stream_error");
    let (_, response_path) = store.record_paths(&summary.id).unwrap();
    let response = fs::read(response_path).unwrap();
    assert!(response
        .windows(ABNORMAL_TRUNCATION_MARKER.len())
        .any(|window| window == ABNORMAL_TRUNCATION_MARKER));
    assert!(response.ends_with(&vec![b'T'; ABNORMAL_TAIL_BYTES]));
}

#[test]
fn legacy_summary_reads_only_a_bounded_header_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let id = next_trace_id("chat", SystemTime::now());
    let request_path = dir.path().join(format!("{id}.request.http"));
    fs::write(
        &request_path,
        b"POST /chat/completions HTTP/1.1\r\ncontent-type: application/json\r\n\r\n",
    )
    .unwrap();
    let response_path = dir.path().join(format!("{id}.response.http"));
    let mut response = File::create(&response_path).unwrap();
    let response_header = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
    response.write_all(response_header).unwrap();
    response
        .write_all(&vec![b'x'; HTTP_PREFIX_READ_BYTES * 2])
        .unwrap();
    drop(response);

    let prefix = read_http_prefix(&request_path).unwrap();
    assert!(prefix.len() < HTTP_PREFIX_READ_BYTES);
    assert_eq!(
        read_http_prefix(&response_path).unwrap().len(),
        HTTP_PREFIX_READ_BYTES
    );
    let store = HttpTraceStore::new(dir.path());
    let summary = store.list(1).unwrap().remove(0);
    assert_eq!(summary.status, 200);
    assert!(summary.stream);
    assert_eq!(summary.response_bytes, (HTTP_PREFIX_READ_BYTES * 2) as u64);
}
