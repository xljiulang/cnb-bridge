use axum::http::HeaderMap;
use chrono::{DateTime, Local, TimeZone};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

pub(super) fn format_headers(start_line: &str, headers: &HeaderMap) -> String {
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

pub(super) fn write_http_prefix_and_body(
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

pub(super) fn write_trace_bytes(
    file: &mut File,
    bytes: &[u8],
    remaining: &mut u64,
) -> Result<(), String> {
    let take = bytes.len().min(*remaining as usize);
    if take > 0 {
        file.write_all(&bytes[..take])
            .map_err(|error| format!("写入 HTTP 日志正文失败: {error}"))?;
        *remaining -= take as u64;
    }
    Ok(())
}

pub(super) fn valid_trace_id(id: &str) -> bool {
    id.starts_with("trace_")
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

pub(super) fn trace_start_ms(id: &str) -> Option<i64> {
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

pub(super) fn protocol_from_id(id: &str) -> String {
    match id.rsplit('_').next().unwrap_or("chat") {
        "responses" => "responses->chat".to_string(),
        _ => "chat".to_string(),
    }
}

pub(super) fn next_trace_id(protocol: &str, started_at: SystemTime) -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let started = DateTime::<Local>::from(started_at);
    format!(
        "trace_{}_{}_{}",
        started.format("%Y%m%d-%H%M%S%3f"),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
        protocol
    )
}
