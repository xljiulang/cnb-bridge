//! Raw upstream HTTP request and response traces.

mod format;
mod session;
mod store;
mod summary;

pub use session::HttpTraceSession;
pub use store::HttpTraceStore;

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests;
