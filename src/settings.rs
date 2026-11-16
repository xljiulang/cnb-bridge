//! Persisted user settings.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default repository created in the user's CNB namespace.
pub const DEFAULT_REPO_NAME: &str = "cnb-ai-bridge";

/// Default local port used by both the SSH forward and the workspace agent.
pub const DEFAULT_LOCAL_PORT: u16 = 11233;

/// User controlled desktop settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub repo_name: String,
    pub local_port: u16,
    pub minimize_to_tray: bool,
    pub launch_at_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            repo_name: DEFAULT_REPO_NAME.to_string(),
            local_port: DEFAULT_LOCAL_PORT,
            minimize_to_tray: true,
            launch_at_login: false,
        }
    }
}

impl Settings {
    /// Validates values before they are persisted or applied.
    pub fn validate(&self) -> Result<(), String> {
        if self.repo_name.trim().is_empty() {
            return Err("仓库名不能为空".to_string());
        }
        if self.repo_name.len() > 100 {
            return Err("仓库名不能超过 100 个字符".to_string());
        }
        if self.repo_name.chars().any(|c| {
            !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.')
        }) {
            return Err("仓库名只能包含小写字母、数字、-、_、.".to_string());
        }
        if self.local_port == 0 {
            return Err("本地端口必须大于 0".to_string());
        }
        Ok(())
    }
}

/// Loads and stores [`Settings`] as JSON.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Creates a store backed by the given file.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads settings, returning defaults when the file does not exist.
    pub fn load(&self) -> Result<Settings, String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default())
            }
            Err(error) => return Err(format!("读取设置 {} 失败: {error}", self.path.display())),
        };

        let settings: Settings = serde_json::from_slice(&bytes)
            .map_err(|error| format!("解析设置 {} 失败: {error}", self.path.display()))?;
        settings.validate()?;
        Ok(settings)
    }

    /// Persists settings atomically.
    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        settings.validate()?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建设置目录 {} 失败: {error}", parent.display()))?;
        }

        let bytes = serde_json::to_vec_pretty(settings)
            .map_err(|error| format!("序列化设置失败: {error}"))?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, bytes)
            .map_err(|error| format!("写入临时设置 {} 失败: {error}", temp.display()))?;
        std::fs::rename(&temp, &self.path)
            .map_err(|error| format!("替换设置 {} 失败: {error}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_cnb_ai_bridge_and_11233() {
        let settings = Settings::default();
        assert_eq!(settings.repo_name, "cnb-ai-bridge");
        assert_eq!(settings.local_port, 11233);
        assert!(settings.minimize_to_tray);
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(dir.path().join("settings.json"));
        let settings = Settings {
            repo_name: "my-bridge".to_string(),
            local_port: 12000,
            minimize_to_tray: false,
            launch_at_login: true,
        };
        store.save(&settings).unwrap();
        assert_eq!(store.load().unwrap(), settings);
    }

    #[test]
    fn rejects_invalid_repo_name_and_port() {
        let settings = Settings {
            repo_name: "Bad Name".to_string(),
            ..Settings::default()
        };
        assert!(settings.validate().is_err());

        let settings = Settings {
            local_port: 0,
            ..Settings::default()
        };
        assert!(settings.validate().is_err());
    }
}
