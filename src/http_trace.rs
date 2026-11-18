//! Raw upstream HTTP request and response traces.

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Local, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::settings::HttpLogFilter;

const HTTP_PREFIX_READ_BYTES: usize = 64 * 1024;
const ABNORMAL_HEAD_BYTES: usize = 4 * 1024 * 1024;
const ABNORMAL_TAIL_BYTES: usize = 4 * 1024 * 1024;
const ABNORMAL_TRUNCATION_MARKER: &[u8] = b"\r\n\r\n[trace truncated]\r\n\r\n";

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
        self.start_with_filter(
            protocol,
            HttpLogFilter::All,
            std::time::Instant::now(),
            SystemTime::now(),
        )
    }

    /// Starts one trace pair using the upstream attempt start time.
    pub fn start_at(
        &self,
        protocol: &str,
        started: std::time::Instant,
        started_at: SystemTime,
    ) -> Result<HttpTraceSession, String> {
        self.start_with_filter(protocol, HttpLogFilter::All, started, started_at)
    }

    /// Starts one trace pair using the configured abnormal/all policy.
    pub fn start_with_filter(
        &self,
        protocol: &str,
        filter: HttpLogFilter,
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
            started_at,
            response_started: false,
            finished: false,
            limits: self.limits,
            filter,
            request: None,
            request_method: "POST".to_string(),
            request_target: "/".to_string(),
            request_body_bytes: 0,
            request_file_bytes: 0,
            response_prefix: String::new(),
            response_writer: None,
            response_file_bytes: 0,
            response_body_bytes: 0,
            response_truncated: false,
            response_head: Vec::new(),
            response_tail: VecDeque::new(),
            response_stream: false,
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
                .or_else(|| name.strip_suffix(".trace.json"))
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
        if let Ok(bytes) = fs::read(summary_path(&response_path, id)) {
            if let Ok(summary) = serde_json::from_slice(&bytes) {
                return Some(summary);
            }
        }
        let request_len = fs::metadata(&request_path).ok()?.len();
        let request = read_http_prefix(&request_path)?;
        let parsed_request = parse_http_message(&request, request_len)?;
        let method = parsed_request
            .start_line
            .split_whitespace()
            .next()?
            .to_string();
        let target = parsed_request.start_line.split_whitespace().nth(1)?;
        let path = target.split('?').next().unwrap_or(target).to_string();

        let response_len = fs::metadata(&response_path)
            .ok()
            .map(|metadata| metadata.len());
        let response = read_http_prefix(&response_path);
        let parsed_response = response
            .as_deref()
            .zip(response_len)
            .and_then(|(bytes, total)| parse_http_message(bytes, total));
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

struct CapturedRequest {
    method: String,
    target: String,
    headers: HeaderMap,
    body: Bytes,
}

/// One in-progress flat request/response pair.
pub struct HttpTraceSession {
    id: String,
    request_path: PathBuf,
    response_path: PathBuf,
    started: std::time::Instant,
    started_at: SystemTime,
    response_started: bool,
    finished: bool,
    limits: HttpTraceLimits,
    filter: HttpLogFilter,
    request: Option<CapturedRequest>,
    request_method: String,
    request_target: String,
    request_body_bytes: u64,
    request_file_bytes: u64,
    response_prefix: String,
    response_writer: Option<BufWriter<File>>,
    response_file_bytes: u64,
    response_body_bytes: u64,
    response_truncated: bool,
    response_head: Vec<u8>,
    response_tail: VecDeque<u8>,
    response_stream: bool,
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
    pub fn write_request_http<B: Into<Bytes>>(
        &mut self,
        method: &str,
        target: &str,
        headers: &HeaderMap,
        body: B,
    ) -> Result<(), String> {
        let body = body.into();
        self.request_method = method.to_string();
        self.request_target = target.to_string();
        self.request_body_bytes = body.len() as u64;
        let start_line = format!("{method} {target} HTTP/1.1");
        let prefix = format_headers(&start_line, headers);
        self.request_file_bytes = prefix.len() as u64 + body.len() as u64;
        if self.filter == HttpLogFilter::Abnormal {
            self.request = Some(CapturedRequest {
                method: method.to_string(),
                target: target.to_string(),
                headers: headers.clone(),
                body,
            });
            Ok(())
        } else {
            write_http_prefix_and_body(
                &self.request_path,
                &prefix,
                body.as_ref(),
                self.limits.file_bytes,
            )
        }
    }

    /// Writes an upstream response status line and headers.
    pub fn write_response_http(
        &mut self,
        status: StatusCode,
        headers: &HeaderMap,
    ) -> Result<(), String> {
        let reason = status.canonical_reason().unwrap_or("");
        let start_line = format!("HTTP/1.1 {} {reason}", status.as_u16());
        self.response_prefix = format_headers(&start_line, headers);
        self.response_stream = headers.get("content-type").is_some_and(|value| {
            String::from_utf8_lossy(value.as_bytes())
                .to_ascii_lowercase()
                .contains("text/event-stream")
        });
        self.response_started = true;
        if self.filter == HttpLogFilter::All {
            let take = self
                .response_prefix
                .len()
                .min(self.limits.file_bytes as usize);
            if take < self.response_prefix.len() {
                return Err("响应头超过 HTTP 日志文件上限".to_string());
            }
            let mut writer = BufWriter::with_capacity(
                32 * 1024,
                File::create(&self.response_path).map_err(|error| {
                    format!(
                        "创建 HTTP 响应文件 {} 失败: {error}",
                        self.response_path.display()
                    )
                })?,
            );
            writer
                .write_all(&self.response_prefix.as_bytes()[..take])
                .map_err(|error| {
                    format!(
                        "写入 HTTP 响应文件 {} 失败: {error}",
                        self.response_path.display()
                    )
                })?;
            self.response_file_bytes = take as u64;
            self.response_writer = Some(writer);
        }
        Ok(())
    }

    /// Appends raw upstream response body bytes.
    pub fn write_response_chunk(&mut self, body: &[u8]) -> Result<(), String> {
        self.response_body_bytes = self.response_body_bytes.saturating_add(body.len() as u64);
        if let Some(writer) = self.response_writer.as_mut() {
            let remaining = self
                .limits
                .file_bytes
                .saturating_sub(self.response_file_bytes);
            let take = body.len().min(remaining as usize);
            if take < body.len() {
                self.response_truncated = true;
            }
            if take > 0 {
                writer.write_all(&body[..take]).map_err(|error| {
                    format!(
                        "写入 HTTP 响应文件 {} 失败: {error}",
                        self.response_path.display()
                    )
                })?;
                self.response_file_bytes += take as u64;
            }
            return Ok(());
        }
        self.capture_abnormal_response(body);
        Ok(())
    }

    /// Marks the trace complete and persists only abnormal exchanges.
    pub fn finish(
        &mut self,
        status: u16,
        result: impl Into<String>,
        elapsed: Duration,
    ) -> Result<(), String> {
        if self.finished {
            return Ok(());
        }
        let result = result.into();
        if !self.response_started {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            self.write_response_http(code, &HeaderMap::new())?;
        }
        let interrupted = matches!(
            result.as_str(),
            "stream_error" | "client_disconnected" | "incomplete"
        );
        let abnormal = status >= 400 || interrupted;
        if self.filter == HttpLogFilter::Abnormal && !abnormal {
            self.finished = true;
            return Ok(());
        }
        if self.filter == HttpLogFilter::Abnormal {
            self.write_abnormal_trace()?;
        } else {
            if let Some(writer) = self.response_writer.as_mut() {
                writer.flush().map_err(|error| {
                    format!(
                        "刷新 HTTP 响应文件 {} 失败: {error}",
                        self.response_path.display()
                    )
                })?;
            }
            self.response_writer = None;
        }
        self.write_summary(status, &result, elapsed)?;
        self.finished = true;
        Ok(())
    }

    fn capture_abnormal_response(&mut self, body: &[u8]) {
        let head_remaining = ABNORMAL_HEAD_BYTES.saturating_sub(self.response_head.len());
        let head_take = body.len().min(head_remaining);
        self.response_head.extend_from_slice(&body[..head_take]);
        let tail = &body[head_take..];
        if tail.is_empty() {
            return;
        }
        self.response_tail.extend(tail.iter().copied());
        while self.response_tail.len() > ABNORMAL_TAIL_BYTES {
            self.response_tail.pop_front();
        }
        if self.response_body_bytes > (ABNORMAL_HEAD_BYTES + ABNORMAL_TAIL_BYTES) as u64 {
            self.response_truncated = true;
        }
    }

    fn write_abnormal_trace(&mut self) -> Result<(), String> {
        if let Some(request) = self.request.take() {
            let start_line = format!("{} {} HTTP/1.1", request.method, request.target);
            write_http_prefix_and_body(
                &self.request_path,
                &format_headers(&start_line, &request.headers),
                request.body.as_ref(),
                self.limits.file_bytes,
            )?;
        }

        let mut file = File::create(&self.response_path).map_err(|error| {
            format!(
                "创建 HTTP 响应文件 {} 失败: {error}",
                self.response_path.display()
            )
        })?;
        let mut remaining = self.limits.file_bytes;
        write_trace_bytes(&mut file, self.response_prefix.as_bytes(), &mut remaining)?;
        write_trace_bytes(&mut file, &self.response_head, &mut remaining)?;
        if self.response_truncated {
            write_trace_bytes(&mut file, ABNORMAL_TRUNCATION_MARKER, &mut remaining)?;
        }
        let tail: Vec<u8> = self.response_tail.iter().copied().collect();
        write_trace_bytes(&mut file, &tail, &mut remaining)?;
        file.flush().map_err(|error| {
            format!(
                "刷新 HTTP 响应文件 {} 失败: {error}",
                self.response_path.display()
            )
        })
    }

    fn write_summary(&self, status: u16, result: &str, elapsed: Duration) -> Result<(), String> {
        let started = DateTime::<Local>::from(self.started_at);
        let path = self
            .request_target
            .split('?')
            .next()
            .unwrap_or(&self.request_target)
            .to_string();
        let summary = HttpTraceSummary {
            id: self.id.clone(),
            started_at: started.to_rfc3339_opts(SecondsFormat::Millis, true),
            method: self.request_method.clone(),
            path,
            status,
            elapsed_ms: elapsed.as_millis() as u64,
            protocol: protocol_from_id(&self.id),
            stream: self.response_stream,
            request_bytes: self.request_body_bytes,
            response_bytes: self.response_body_bytes,
            request_truncated: self.request_file_bytes > self.limits.file_bytes,
            response_truncated: self.response_truncated,
            result: result.to_string(),
        };
        let bytes = serde_json::to_vec(&summary)
            .map_err(|error| format!("序列化 HTTP 日志元数据失败: {error}"))?;
        fs::write(self.summary_path(), bytes).map_err(|error| {
            format!(
                "写入 HTTP 日志元数据 {} 失败: {error}",
                self.summary_path().display()
            )
        })
    }

    fn summary_path(&self) -> PathBuf {
        summary_path(&self.response_path, &self.id)
    }
}

impl Drop for HttpTraceSession {
    fn drop(&mut self) {
        self.finished = true;
    }
}

fn parse_http_message(bytes: &[u8], total_size: u64) -> Option<ParsedHttpMessage> {
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
        body_bytes: total_size.saturating_sub((separator + 4) as u64),
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

fn summary_path(response_path: &Path, id: &str) -> PathBuf {
    response_path.with_file_name(format!("{id}.trace.json"))
}

fn read_http_prefix(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(HTTP_PREFIX_READ_BYTES);
    file.take(HTTP_PREFIX_READ_BYTES as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

fn write_trace_bytes(file: &mut File, bytes: &[u8], remaining: &mut u64) -> Result<(), String> {
    let take = bytes.len().min(*remaining as usize);
    if take > 0 {
        file.write_all(&bytes[..take])
            .map_err(|error| format!("写入 HTTP 日志正文失败: {error}"))?;
        *remaining -= take as u64;
    }
    Ok(())
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
}
