use super::format::{protocol_from_id, trace_start_ms};
use super::{HttpTraceLimits, HttpTraceSummary};
use chrono::{Local, SecondsFormat, TimeZone};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(super) const HTTP_PREFIX_READ_BYTES: usize = 64 * 1024;

pub(super) fn flat_summary(
    root: &Path,
    limits: &HttpTraceLimits,
    id: &str,
) -> Option<HttpTraceSummary> {
    let request_path = root.join(format!("{id}.request.http"));
    let response_path = root.join(format!("{id}.response.http"));
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
            .map(|metadata| metadata.len() >= limits.file_bytes)
            .unwrap_or(false),
        response_truncated: fs::metadata(&response_path)
            .map(|metadata| metadata.len() >= limits.file_bytes)
            .unwrap_or(false),
        result: result.to_string(),
    })
}

pub(super) fn legacy_summary(path: &Path) -> Option<HttpTraceSummary> {
    let bytes = fs::read(path.join("meta.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn summary_path(response_path: &Path, id: &str) -> PathBuf {
    response_path.with_file_name(format!("{id}.trace.json"))
}

pub(super) fn read_http_prefix(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(HTTP_PREFIX_READ_BYTES);
    file.take(HTTP_PREFIX_READ_BYTES as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
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
