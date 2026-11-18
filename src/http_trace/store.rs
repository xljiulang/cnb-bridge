use super::format::{next_trace_id, trace_start_ms, valid_trace_id};
use super::session::HttpTraceSession;
use super::summary::{flat_summary, legacy_summary};
use super::{HttpTraceLimits, HttpTraceSummary};
use crate::settings::HttpLogFilter;
use chrono::Utc;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
        Ok(HttpTraceSession::new(
            id,
            request_path,
            response_path,
            started,
            started_at,
            self.limits,
            filter,
        ))
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
            if let Some(summary) = flat_summary(&self.root, &self.limits, id) {
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
