use super::format::{
    format_headers, protocol_from_id, write_http_prefix_and_body, write_trace_bytes,
};
use super::summary::summary_path;
use super::{HttpTraceLimits, HttpTraceSummary};
use crate::settings::HttpLogFilter;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Local, SecondsFormat};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub(super) const ABNORMAL_HEAD_BYTES: usize = 4 * 1024 * 1024;
pub(super) const ABNORMAL_TAIL_BYTES: usize = 4 * 1024 * 1024;
pub(super) const ABNORMAL_TRUNCATION_MARKER: &[u8] = b"\r\n\r\n[trace truncated]\r\n\r\n";

enum TraceFileCommand {
    Write(Vec<u8>),
    Flush(std::sync::mpsc::Sender<Result<(), String>>),
    Stop,
}

struct AsyncTraceWriter {
    sender: SyncSender<TraceFileCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
    dropped: Arc<AtomicU64>,
}

impl AsyncTraceWriter {
    fn new(path: &PathBuf) -> Result<Self, String> {
        let file = File::create(path)
            .map_err(|error| format!("创建 HTTP 响应文件 {} 失败: {error}", path.display()))?;
        let (sender, receiver) = sync_channel(64);
        let thread = std::thread::Builder::new()
            .name("http-trace-writer".to_string())
            .spawn(move || trace_writer_loop(file, receiver))
            .map_err(|error| format!("启动 HTTP 日志写盘线程失败: {error}"))?;
        Ok(Self {
            sender,
            thread: Some(thread),
            dropped: Arc::new(AtomicU64::new(0)),
        })
    }

    fn try_write(&self, bytes: Vec<u8>) -> bool {
        if self.sender.try_send(TraceFileCommand::Write(bytes)).is_ok() {
            true
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    fn dropped_chunks(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn flush(&mut self) -> Result<(), String> {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.sender
            .send(TraceFileCommand::Flush(sender))
            .map_err(|_| "HTTP 日志写盘线程已停止".to_string())?;
        receiver
            .recv()
            .map_err(|_| "HTTP 日志写盘线程未返回结果".to_string())?
    }

    fn shutdown(&mut self) {
        let _ = self.sender.send(TraceFileCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AsyncTraceWriter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn trace_writer_loop(file: File, receiver: Receiver<TraceFileCommand>) {
    let mut writer = BufWriter::with_capacity(32 * 1024, file);
    while let Ok(command) = receiver.recv() {
        match command {
            TraceFileCommand::Write(bytes) => {
                if let Err(error) = writer.write_all(&bytes) {
                    eprintln!("writing HTTP trace failed: {error}");
                    break;
                }
            }
            TraceFileCommand::Flush(result) => {
                let _ = result.send(
                    writer
                        .flush()
                        .map_err(|error| format!("刷新 HTTP 日志失败: {error}")),
                );
            }
            TraceFileCommand::Stop => break,
        }
    }
    let _ = writer.flush();
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
    response_writer: Option<AsyncTraceWriter>,
    response_file_bytes: u64,
    response_body_bytes: u64,
    response_truncated: bool,
    response_head: Vec<u8>,
    response_tail: VecDeque<u8>,
    response_stream: bool,
}

impl HttpTraceSession {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        id: String,
        request_path: PathBuf,
        response_path: PathBuf,
        started: std::time::Instant,
        started_at: SystemTime,
        limits: HttpTraceLimits,
        filter: HttpLogFilter,
    ) -> Self {
        Self {
            id,
            request_path,
            response_path,
            started,
            started_at,
            response_started: false,
            finished: false,
            limits,
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
        }
    }

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
            let writer = AsyncTraceWriter::new(&self.response_path)?;
            if !writer.try_write(self.response_prefix.as_bytes()[..take].to_vec()) {
                return Err("HTTP 日志写盘队列在写入响应头时已满".to_string());
            }
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
                if !writer.try_write(body[..take].to_vec()) {
                    self.response_truncated = true;
                    return Ok(());
                }
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
                if writer.dropped_chunks() > 0 {
                    self.response_truncated = true;
                }
                writer.flush()?;
                writer.shutdown();
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
