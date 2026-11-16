//! Raw upstream HTTP request and response traces.

use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Local, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Per-record and whole-directory retention limits.
#[derive(Debug, Clone, Copy)]
pub struct HttpTraceLimits {
    pub file_bytes: u64,
    pub dir_bytes: u64,
    pub keep_days: i64,
}

impl Default for HttpTraceLimits {
    fn default() -> Self {
        Self {
            file_bytes: 64 * 1024 * 1024,
            dir_bytes: 1024 * 1024 * 1024,
            keep_days: 7,
        }
    }
}

/// UI-facing summary for one raw upstream exchange.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HttpTraceSummary {
    pub id: String,
    pub started_at: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub elapsed_ms: u64,
    pub protocol: String,
    pub stream: bool,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub request_truncated: bool,
    pub response_truncated: bool,
    pub result: String,
}

/// Store for flat request/response file pairs and legacy record directories.
#[derive(Debug, Clone)]
pub struct HttpTraceStore {
    root: PathBuf,
    limits: HttpTraceLimits,
    last_cleanup_ms: Arc<AtomicU64>,
}

impl HttpTraceStore {
    /// Creates a store with production limits.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_limits(root, HttpTraceLimits::default())
    }

    /// Creates a store with explicit limits.
    pub fn with_limits(root: impl Into<PathBuf>, limits: HttpTraceLimits) -> Self {
        Self {
            root: root.into(),
            limits,
            last_cleanup_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Root directory containing flat HTTP pair files.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates the trace root.
    pub fn create_root(&self) -> Result<(), String> {
        fs::create_dir_all(&self.root)
            .map_err(|error| format!("创建 HTTP 日志目录 {} 失败: {error}", self.root.display()))
    }

    /// Starts one trace pair with the current time.
    pub fn start(&self, protocol: &str) -> Result<HttpTraceSession, String> {
        self.start_at(protocol, std::time::Instant::now(), SystemTime::now())
    }

    /// Starts one trace pair using the upstream attempt start time.
    pub fn start_at(
        &self,
        protocol: &str,
        started: std::time::Instant,
        started_at: SystemTime,
    ) -> Result<HttpTraceSession, String> {
        self.create_root()?;
        let _ = self.cleanup_if_due();
        let id = next_trace_id(protocol, started_at);
        let (request_path, response_path) = self.record_paths(&id)?;
        Ok(HttpTraceSession {
            id,
            request_path,
            response_path,
            started,
            response_started: false,
            finished: false,
            limits: self.limits,
        })
    }

    /// Returns the request and response paths for a validated trace id.
    pub fn record_paths(&self, id: &str) -> Result<(PathBuf, PathBuf), String> {
        if !valid_trace_id(id) {
            return Err("HTTP 日志标识无效".to_string());
        }
        Ok((
            self.root.join(format!("{id}.request.http")),
            self.root.join(format!("{id}.response.http")),
        ))
    }

    /// Lists recent flat pairs and legacy records, newest first.
    pub fn list(&self, limit: usize) -> Result<Vec<HttpTraceSummary>, String> {
        let _ = self.cleanup_if_due();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(Vec::new());
        };
        let mut summaries = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() && valid_trace_id(&name) {
                if let Some(summary) = legacy_summary(&path) {
                    summaries.push(summary);
                }
                continue;
            }
            let Some(id) = name.strip_suffix(".request.http") else {
                continue;
            };
            if let Some(summary) = self.flat_summary(id) {
                summaries.push(summary);
            }
        }
        summaries.sort_by(|left, right| right.id.cmp(&left.id));
        summaries.truncate(limit);
        Ok(summaries)
    }

    /// Applies retention immediately.
    pub fn cleanup(&self) -> Result<(), String> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(());
        };
        let mut groups: BTreeMap<String, RecordGroup> = BTreeMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let (id, is_new_file) = if path.is_dir() {
                (name.clone(), false)
            } else if let Some(id) = name
                .strip_suffix(".request.http")
                .or_else(|| name.strip_suffix(".response.http"))
            {
                (id.to_string(), true)
            } else {
                continue;
            };
            if !valid_trace_id(&id) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            let size = if is_new_file {
                entry.metadata().map(|metadata| metadata.len()).unwrap_or(0)
            } else {
                directory_size(&path)
            };
            let group = groups.entry(id.clone()).or_insert_with(|| RecordGroup {
                timestamp_ms: trace_start_ms(&id).unwrap_or(0),
                modified,
                size: 0,
                paths: Vec::new(),
            });
            group.modified = group.modified.max(modified);
            group.size = group.size.saturating_add(size);
            group.paths.push(path);
        }

        let cutoff =
            Utc::now().timestamp_millis() - self.limits.keep_days.max(0).saturating_mul(86_400_000);
        groups.retain(|_, group| {
            if group.timestamp_ms < cutoff {
                let _ = remove_group(group);
                false
            } else {
                true
            }
        });
        let mut ordered: Vec<_> = groups.into_iter().collect();
        ordered.sort_by_key(|(_, group)| (group.timestamp_ms, group.modified));
        let mut total = ordered.iter().map(|(_, group)| group.size).sum::<u64>();
        let removable = ordered.len().saturating_sub(1);
        for (_, group) in ordered.into_iter().take(removable) {
            if total <= self.limits.dir_bytes {
                break;
            }
            if remove_group(&group).is_ok() {
                total = total.saturating_sub(group.size);
            }
        }
        Ok(())
    }

    /// Applies retention immediately for tests and maintenance.
    pub fn cleanup_now(&self) -> Result<(), String> {
        self.cleanup()
    }

    fn flat_summary(&self, id: &str) -> Option<HttpTraceSummary> {
        let (request_path, response_path) = self.record_paths(id).ok()?;
        let request = fs::read(&request_path).ok()?;
        let parsed_request = parse_http_message(&request)?;
        let method = parsed_request
            .start_line
            .split_whitespace()
            .next()?
            .to_string();
        let target = parsed_request.start_line.split_whitespace().nth(1)?;
        let path = target.split('?').next().unwrap_or(target).to_string();

        let response = fs::read(&response_path).ok();
        let parsed_response = response.as_deref().and_then(parse_http_message);
        let status = parsed_response
            .as_ref()
            .and_then(|message| message.start_line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(0);
        let stream = parsed_response.as_ref().is_some_and(|message| {
            message.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("content-type")
                    && value.to_ascii_lowercase().contains("text/event-stream")
            })
        });
        let started_ms = trace_start_ms(id)?;
        let started = Local.timestamp_millis_opt(started_ms).single()?;
        let modified_ms = fs::metadata(&response_path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(started_ms);
        let result = if parsed_response.is_none() {
            "incomplete"
        } else if (200..300).contains(&status) {
            "completed"
        } else {
            "failed"
        };
        Some(HttpTraceSummary {
            id: id.to_string(),
            started_at: started.to_rfc3339_opts(SecondsFormat::Millis, true),
            method,
            path,
            status,
            elapsed_ms: modified_ms.saturating_sub(started_ms).max(0) as u64,
            protocol: protocol_from_id(id),
            stream,
            request_bytes: parsed_request.body_bytes,
            response_bytes: parsed_response
                .as_ref()
                .map_or(0, |message| message.body_bytes),
            request_truncated: fs::metadata(&request_path)
                .map(|metadata| metadata.len() >= self.limits.file_bytes)
                .unwrap_or(false),
            response_truncated: fs::metadata(&response_path)
                .map(|metadata| metadata.len() >= self.limits.file_bytes)
                .unwrap_or(false),
            result: result.to_string(),
        })
    }

    fn cleanup_if_due(&self) -> Result<(), String> {
        let now = Utc::now().timestamp_millis().max(0) as u64;
        let last = self.last_cleanup_ms.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < 60_000 {
            return Ok(());
        }
        self.last_cleanup_ms.store(now, Ordering::Relaxed);
        self.cleanup()
    }
}

struct RecordGroup {
    timestamp_ms: i64,
    modified: SystemTime,
    size: u64,
    paths: Vec<PathBuf>,
}

fn remove_group(group: &RecordGroup) -> std::io::Result<()> {
    for path in &group.paths {
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// One in-progress flat request/response pair.
pub struct HttpTraceSession {
    id: String,
    request_path: PathBuf,
    response_path: PathBuf,
    started: std::time::Instant,
    response_started: bool,
    finished: bool,
    limits: HttpTraceLimits,
}

impl HttpTraceSession {
    /// Record identifier used as the shared filename prefix.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Elapsed time since the upstream attempt started.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Writes an upstream request line, headers, and body into one file.
    pub fn write_request_http(
        &mut self,
        method: &str,
        target: &str,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<(), String> {
        let start_line = format!("{method} {target} HTTP/1.1");
        write_http_prefix_and_body(
            &self.request_path,
            &format_headers(&start_line, headers),
            body,
            self.limits.file_bytes,
        )
    }

    /// Writes an upstream response status line and headers.
    pub fn write_response_http(
        &mut self,
        status: StatusCode,
        headers: &HeaderMap,
    ) -> Result<(), String> {
        let reason = status.canonical_reason().unwrap_or("");
        let start_line = format!("HTTP/1.1 {} {reason}", status.as_u16());
        let (_, truncated) = write_capped(
            &self.response_path,
            format_headers(&start_line, headers).as_bytes(),
            self.limits.file_bytes,
        )?;
        self.response_started = true;
        if truncated {
            return Err("响应头超过 HTTP 日志文件上限".to_string());
        }
        Ok(())
    }

    /// Appends raw upstream response body bytes.
    pub fn write_response_chunk(&mut self, body: &[u8]) -> Result<(), String> {
        let current = fs::metadata(&self.response_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let remaining = self.limits.file_bytes.saturating_sub(current);
        if remaining == 0 {
            return Ok(());
        }
        let take = body.len().min(remaining as usize);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.response_path)
            .map_err(|error| {
                format!(
                    "写入 HTTP 响应文件 {} 失败: {error}",
                    self.response_path.display()
                )
            })?;
        file.write_all(&body[..take]).map_err(|error| {
            format!(
                "写入 HTTP 响应文件 {} 失败: {error}",
                self.response_path.display()
            )
        })
    }

    /// Marks the trace complete. File contents are fully derived at list time.
    pub fn finish(
        &mut self,
        status: u16,
        _result: impl Into<String>,
        _elapsed: Duration,
    ) -> Result<(), String> {
        if self.finished {
            return Ok(());
        }
        if !self.response_started {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            self.write_response_http(code, &HeaderMap::new())?;
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for HttpTraceSession {
    fn drop(&mut self) {
        self.finished = true;
    }
}

fn parse_http_message(bytes: &[u8]) -> Option<ParsedHttpMessage> {
    let separator = bytes.windows(4).position(|window| window == b"\r\n\r\n")?;
    let headers = String::from_utf8_lossy(&bytes[..separator]);
    let mut lines = headers.split("\r\n");
    let start_line = lines.next()?.to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();
    Some(ParsedHttpMessage {
        start_line,
        headers,
        body_bytes: (bytes.len() - separator - 4) as u64,
    })
}

struct ParsedHttpMessage {
    start_line: String,
    headers: Vec<(String, String)>,
    body_bytes: u64,
}

fn legacy_summary(path: &Path) -> Option<HttpTraceSummary> {
    let bytes = fs::read(path.join("meta.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn format_headers(start_line: &str, headers: &HeaderMap) -> String {
    let mut lines = headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect::<Vec<_>>();
    lines.sort();
    let mut output = format!("{start_line}\r\n");
    for (name, value) in lines {
        output.push_str(&name);
        output.push_str(": ");
        output.push_str(&value);
        output.push_str("\r\n");
    }
    output.push_str("\r\n");
    output
}

fn write_http_prefix_and_body(
    path: &Path,
    prefix: &str,
    body: &[u8],
    limit: u64,
) -> Result<(), String> {
    let mut file = File::create(path)
        .map_err(|error| format!("创建 HTTP 报文文件 {} 失败: {error}", path.display()))?;
    let prefix = prefix.as_bytes();
    let prefix_take = prefix.len().min(limit as usize);
    file.write_all(&prefix[..prefix_take])
        .map_err(|error| format!("写入 HTTP 报文头 {} 失败: {error}", path.display()))?;
    let remaining = limit.saturating_sub(prefix_take as u64);
    file.write_all(&body[..body.len().min(remaining as usize)])
        .map_err(|error| format!("写入 HTTP 报文正文 {} 失败: {error}", path.display()))
}

fn write_capped(path: &Path, body: &[u8], limit: u64) -> Result<(u64, bool), String> {
    let take = body.len().min(limit as usize);
    let mut file = File::create(path)
        .map_err(|error| format!("创建 HTTP 报文文件 {} 失败: {error}", path.display()))?;
    file.write_all(&body[..take])
        .map_err(|error| format!("写入 HTTP 报文文件 {} 失败: {error}", path.display()))?;
    Ok((take as u64, take < body.len()))
}

fn directory_size(path: &Path) -> u64 {
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .map(
            |metadata| {
                if metadata.is_dir() {
                    0
                } else {
                    metadata.len()
                }
            },
        )
        .sum()
}

fn valid_trace_id(id: &str) -> bool {
    id.starts_with("trace_")
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn trace_start_ms(id: &str) -> Option<i64> {
    let rest = id.strip_prefix("trace_")?;
    let stamp = rest.split('_').next()?;
    if let Ok(value) = stamp.parse::<i64>() {
        return Some(value);
    }
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S%3f")
        .ok()
        .and_then(|naive| Local.from_local_datetime(&naive).single())
        .map(|value| value.timestamp_millis())
}

fn protocol_from_id(id: &str) -> String {
    match id.rsplit('_').next().unwrap_or("chat") {
        "responses" => "responses->chat".to_string(),
        _ => "chat".to_string(),
    }
}

fn next_trace_id(protocol: &str, started_at: SystemTime) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let started = DateTime::<Local>::from(started_at);
    format!(
        "trace_{}_{}_{}",
        started.format("%Y%m%d-%H%M%S%3f"),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
        protocol
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, HeaderMap, HeaderValue};
    use std::fs;

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
                br#"{"input":"hello"}"#,
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
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);

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
            .write_request_http("POST", "/chat/completions", &HeaderMap::new(), &[b'x'; 200])
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
        old.write_request_http("POST", "/chat/completions", &HeaderMap::new(), b"old")
            .unwrap();
        old.write_response_http(StatusCode::OK, &HeaderMap::new())
            .unwrap();
        old.write_response_chunk(b"old").unwrap();
        old.finish(200, "completed", std::time::Duration::from_millis(1))
            .unwrap();
        let old_id = old.id().to_string();
        let mut recent = store.start("chat").unwrap();
        recent
            .write_request_http("POST", "/chat/completions", &HeaderMap::new(), b"recent")
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
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);

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
}
