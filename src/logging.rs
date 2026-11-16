//! Small in-memory and rolling file logger for the desktop shell.

use chrono::Local;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One log entry shown by the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub time: String,
    pub level: String,
    pub message: String,
}

/// Recent log buffer plus an optional file sink.
pub struct Logger {
    capacity: usize,
    entries: Mutex<VecDeque<LogEntry>>,
    file: Option<PathBuf>,
}

impl Logger {
    /// Creates a logger retaining at most `capacity` entries.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Mutex::new(VecDeque::with_capacity(capacity.max(1))),
            file: None,
        }
    }

    /// Creates a logger that also appends to `dir/cnb-bridge.log`.
    pub fn with_file(capacity: usize, dir: impl AsRef<Path>) -> Self {
        let mut logger = Self::new(capacity);
        logger.file = Some(dir.as_ref().join("cnb-bridge.log"));
        logger
    }

    /// Appends one sanitized entry.
    pub fn log(&self, level: &str, message: impl Into<String>) {
        let entry = LogEntry {
            time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            level: level.to_string(),
            message: redact_secrets(&message.into()),
        };

        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() == self.capacity {
                entries.pop_front();
            }
            entries.push_back(entry.clone());
        }

        if let Some(path) = &self.file {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                use std::io::Write;
                let _ = writeln!(file, "{} [{}] {}", entry.time, entry.level, entry.message);
            }
        }
    }

    /// Returns the most recent `limit` entries, oldest first.
    pub fn recent(&self, limit: usize) -> Vec<LogEntry> {
        let Ok(entries) = self.entries.lock() else {
            return Vec::new();
        };
        let skip = entries.len().saturating_sub(limit);
        entries.iter().skip(skip).cloned().collect()
    }
}

/// Masks bearer tokens and query token values so logs never contain secrets.
pub fn redact_secrets(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut redact_next = false;
    for (index, part) in value.split_whitespace().enumerate() {
        if index > 0 {
            result.push(' ');
        }
        let lower = part.to_ascii_lowercase();
        if redact_next {
            result.push_str(&mask_secret(part));
            redact_next = false;
        } else if lower == "bearer" {
            result.push_str(part);
            redact_next = true;
        } else if lower.starts_with("token=") {
            result.push_str("token=");
            result.push_str(&mask_secret(&part[6..]));
        } else {
            result.push_str(part);
        }
    }
    result
}

fn mask_secret(value: &str) -> String {
    if value.len() <= 12 {
        value.to_string()
    } else {
        format!("{}...", &value[..8])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_latest_entries() {
        let logger = Logger::new(2);
        logger.log("info", "one");
        logger.log("info", "two");
        logger.log("info", "three");
        let entries = logger.recent(10);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "two");
        assert_eq!(entries[1].message, "three");
    }

    #[test]
    fn redacts_bearer_tokens() {
        let redacted = redact_secrets("Authorization: Bearer abcdefghijklmnop");
        assert!(!redacted.contains("abcdefghijklmnop"));
        assert!(redacted.contains("Bearer abcdefgh..."));
    }
}
