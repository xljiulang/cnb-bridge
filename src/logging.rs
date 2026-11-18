//! Small in-memory and rolling file logger for the desktop shell.

use chrono::Local;
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const DEFAULT_LOG_LIMIT_BYTES: u64 = 10 * 1024 * 1024;

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
    file: Mutex<Option<File>>,
    path: Option<PathBuf>,
    max_file_bytes: u64,
}

impl Logger {
    /// Creates a logger retaining at most `capacity` entries.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: Mutex::new(VecDeque::with_capacity(capacity.max(1))),
            file: Mutex::new(None),
            path: None,
            max_file_bytes: DEFAULT_LOG_LIMIT_BYTES,
        }
    }

    /// Creates a logger that also appends to `dir/cnb-bridge.log`.
    pub fn with_file(capacity: usize, dir: impl AsRef<Path>) -> Self {
        let mut logger = Self::new(capacity);
        let path = dir.as_ref().join("cnb-bridge.log");
        logger.path = Some(path.clone());
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        *logger
            .file
            .get_mut()
            .unwrap_or_else(|error| error.into_inner()) =
            OpenOptions::new().create(true).append(true).open(path).ok();
        logger
    }

    #[cfg(test)]
    fn with_file_limit(capacity: usize, dir: impl AsRef<Path>, max_file_bytes: u64) -> Self {
        let mut logger = Self::with_file(capacity, dir);
        logger.max_file_bytes = max_file_bytes.max(1);
        logger
    }

    /// Appends one sanitized entry.
    pub fn log(&self, level: &str, message: impl Into<String>) {
        let entry = LogEntry {
            time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            level: level.to_string(),
            message: redact_secrets(&message.into()),
        };

        self.write_file_entry(&format!(
            "{} [{}] {}",
            entry.time, entry.level, entry.message
        ));

        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() == self.capacity {
                entries.pop_front();
            }
            entries.push_back(entry);
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

    fn write_file_entry(&self, line: &str) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Ok(mut file) = self.file.lock() else {
            return;
        };
        let current_size = file
            .as_ref()
            .and_then(|file| file.metadata().ok())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if current_size.saturating_add(line.len() as u64 + 1) > self.max_file_bytes {
            *file = None;
            let rotated = path.with_extension("log.1");
            let _ = std::fs::remove_file(&rotated);
            let _ = std::fs::rename(path, &rotated);
            *file = OpenOptions::new().create(true).append(true).open(path).ok();
        }
        if let Some(file) = file.as_mut() {
            let _ = writeln!(file, "{line}");
        }
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

    #[test]
    fn file_sink_appends_without_reopening_per_entry() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::with_file(4, dir.path());

        logger.log("info", "one");
        logger.log("warn", "two");

        let text = std::fs::read_to_string(dir.path().join("cnb-bridge.log")).unwrap();
        assert!(text.contains("[info] one"));
        assert!(text.contains("[warn] two"));
    }

    #[test]
    fn file_sink_rotates_when_the_size_limit_is_reached() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::with_file_limit(8, dir.path(), 80);

        logger.log("info", "first message that is reasonably long");
        logger.log("info", "second message that forces rotation");

        assert!(dir.path().join("cnb-bridge.log").is_file());
        assert!(dir.path().join("cnb-bridge.log.1").is_file());
        assert!(std::fs::read_to_string(dir.path().join("cnb-bridge.log.1"))
            .unwrap()
            .contains("first message"));
    }
}
