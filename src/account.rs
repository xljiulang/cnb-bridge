//! CNB OAuth device authorization and DPAPI backed credential storage.

use crate::dpapi;
use crate::paths;
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// CNB's public web platform.
pub const DEFAULT_PLATFORM_URL: &str = "https://cnb.cool";

/// Public client id used by the official CNB CLI for device authorization.
pub const DEFAULT_CLIENT_ID: &str = "cnb_cli";

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Persisted OAuth credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_ms: i64,
    pub username: Option<String>,
}

impl Credentials {
    /// Returns true when the access token is missing or expires within two minutes.
    pub fn needs_refresh(&self) -> bool {
        self.expires_at_ms <= Utc::now().timestamp_millis() + 120_000
    }
}

/// Current account state exposed to the desktop UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub state: AccountState,
    pub username: Option<String>,
    pub verification_url: Option<String>,
    pub user_code: Option<String>,
    pub error: Option<String>,
}

/// Account lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AccountState {
    SignedOut,
    SigningIn,
    Connected,
    Failed,
}

impl AccountStatus {
    /// Signed-out status.
    pub fn signed_out() -> Self {
        Self {
            state: AccountState::SignedOut,
            username: None,
            verification_url: None,
            user_code: None,
            error: None,
        }
    }

    /// Connected status.
    pub fn connected(credentials: &Credentials) -> Self {
        Self {
            state: AccountState::Connected,
            username: credentials.username.clone(),
            verification_url: None,
            user_code: None,
            error: None,
        }
    }
}

/// Device authorization challenge returned by CNB.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
    #[serde(default)]
    pub interval: u64,
}

/// Errors raised during the OAuth flows.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("网络请求失败: {0}")]
    Http(#[from] reqwest::Error),
    #[error("OAuth 响应无效: {0}")]
    InvalidResponse(String),
    #[error("授权失败: {0}")]
    Authorization(String),
    #[error("登录被取消")]
    Cancelled,
    #[error("本地凭据存储失败: {0}")]
    Storage(String),
}

impl From<String> for AccountError {
    fn from(value: String) -> Self {
        Self::Storage(value)
    }
}

/// Encrypted credential store.
#[derive(Debug, Clone)]
pub struct AccountStore {
    path: PathBuf,
}

impl AccountStore {
    /// Creates a store at the given path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Creates the default store under `%APPDATA%\cnb-bridge`.
    pub fn default_path() -> Self {
        Self::new(paths::account_path())
    }

    /// Returns the backing path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads and decrypts saved credentials.
    pub fn load(&self) -> Result<Option<Credentials>, AccountError> {
        let encrypted = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AccountError::Storage(format!(
                    "读取登录凭据 {} 失败: {error}",
                    self.path.display()
                )))
            }
        };
        let plaintext = dpapi::unprotect(&encrypted).map_err(AccountError::Storage)?;
        let credentials = serde_json::from_slice(&plaintext)
            .map_err(|error| AccountError::Storage(format!("解析登录凭据失败: {error}")))?;
        Ok(Some(credentials))
    }

    /// Encrypts and atomically saves credentials.
    pub fn save(&self, credentials: &Credentials) -> Result<(), AccountError> {
        let plaintext = serde_json::to_vec(credentials)
            .map_err(|error| AccountError::Storage(format!("序列化登录凭据失败: {error}")))?;
        let encrypted = dpapi::protect(&plaintext).map_err(AccountError::Storage)?;
        write_atomic(&self.path, &encrypted)?;
        Ok(())
    }

    /// Removes saved credentials.
    pub fn remove(&self) -> Result<(), AccountError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AccountError::Storage(format!(
                "删除登录凭据 {} 失败: {error}",
                self.path.display()
            ))),
        }
    }
}

/// Account manager shared by the Tauri command layer.
pub struct AccountManager {
    http: Client,
    store: AccountStore,
    platform_url: String,
    client_id: String,
    credentials: Mutex<Option<Credentials>>,
    status: Mutex<AccountStatus>,
}

impl AccountManager {
    /// Creates a manager and loads any saved credentials.
    pub fn new() -> Arc<Self> {
        let platform_url = std::env::var("CNB_BRIDGE_PLATFORM_URL")
            .unwrap_or_else(|_| DEFAULT_PLATFORM_URL.into());
        let client_id =
            std::env::var("CNB_BRIDGE_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.into());
        Self::with_store(AccountStore::default_path(), platform_url, client_id)
    }

    /// Creates a manager with explicit storage and OAuth configuration.
    pub fn with_store(
        store: AccountStore,
        platform_url: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Arc<Self> {
        let (credentials, status) = match store.load() {
            Ok(Some(credentials)) => {
                let status = AccountStatus::connected(&credentials);
                (Some(credentials), status)
            }
            Ok(None) => (None, AccountStatus::signed_out()),
            Err(error) => {
                let mut status = AccountStatus::signed_out();
                status.state = AccountState::Failed;
                status.error = Some(format!("保存的登录信息无法读取，请重新登录: {error}"));
                (None, status)
            }
        };

        Arc::new(Self {
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
            store,
            platform_url: platform_url.into().trim_end_matches('/').to_string(),
            client_id: client_id.into(),
            credentials: Mutex::new(credentials),
            status: Mutex::new(status),
        })
    }

    /// Current status snapshot.
    pub fn status(&self) -> AccountStatus {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Requests a device challenge and marks the account as signing in.
    pub async fn start_device_login(&self) -> Result<DeviceCode, AccountError> {
        let device = self.request_device_code().await?;
        let mut status = AccountStatus {
            state: AccountState::SigningIn,
            username: None,
            verification_url: Some(device.verification_uri_complete.clone()),
            user_code: Some(device.user_code.clone()),
            error: None,
        };
        if let Some(existing) = self.current_credentials() {
            status.username = existing.username.clone();
        }
        *self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
        Ok(device)
    }

    /// Polls the token endpoint until the user finishes authorization.
    pub async fn finish_device_login(
        &self,
        device: &DeviceCode,
        cancel: Arc<AtomicBool>,
    ) -> Result<AccountStatus, AccountError> {
        let mut interval = Duration::from_secs(device.interval.max(1));
        let deadline =
            std::time::Instant::now() + Duration::from_secs(device.expires_in.max(10) as u64);

        loop {
            if cancel.load(Ordering::SeqCst) {
                return self.fail_login(AccountError::Cancelled);
            }
            if std::time::Instant::now() >= deadline {
                return self.fail_login(AccountError::Authorization("设备码已过期".to_string()));
            }

            tokio::time::sleep(interval).await;
            if cancel.load(Ordering::SeqCst) {
                return self.fail_login(AccountError::Cancelled);
            }

            match self.request_token(device).await? {
                TokenPoll::Ready(credentials) => {
                    self.store
                        .save(&credentials)
                        .map_err(|error| AccountError::Storage(error.to_string()))?;
                    let status = AccountStatus::connected(&credentials);
                    *self
                        .credentials
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(credentials);
                    *self
                        .status
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = status.clone();
                    return Ok(status);
                }
                TokenPoll::Pending => {}
                TokenPoll::SlowDown => interval += Duration::from_secs(5),
            }
        }
    }

    /// Returns a valid access token, refreshing it when necessary.
    pub async fn access_token(&self) -> Result<String, AccountError> {
        let current = self.current_credentials();
        let Some(current) = current else {
            return Err(AccountError::Authorization("尚未登录".to_string()));
        };
        if !current.needs_refresh() {
            return Ok(current.access_token);
        }

        let Some(refresh_token) = current.refresh_token.clone() else {
            return Err(AccountError::Authorization(
                "登录已过期，请重新登录".to_string(),
            ));
        };
        let refreshed = self.refresh_access_token(&refresh_token).await?;
        self.store
            .save(&refreshed)
            .map_err(|error| AccountError::Storage(error.to_string()))?;
        *self
            .credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(refreshed.clone());
        *self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            AccountStatus::connected(&refreshed);
        Ok(refreshed.access_token)
    }

    /// Sets the display username after the CNB profile is read.
    pub fn set_username(&self, username: impl Into<String>) {
        let username = username.into();
        let mut credentials = self
            .credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(current) = credentials.as_mut() {
            current.username = Some(username.clone());
            let _ = self.store.save(current);
        }
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        status.username = Some(username);
    }

    /// Clears all saved credentials.
    pub fn logout(&self) -> Result<AccountStatus, AccountError> {
        *self
            .credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        self.store.remove()?;
        let status = AccountStatus::signed_out();
        *self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = status.clone();
        Ok(status)
    }

    fn current_credentials(&self) -> Option<Credentials> {
        self.credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn fail_login<T>(&self, error: AccountError) -> Result<T, AccountError> {
        let mut status = self.status();
        status.state = AccountState::Failed;
        status.error = Some(error.to_string());
        *self
            .status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
        Err(error)
    }

    async fn request_device_code(&self) -> Result<DeviceCode, AccountError> {
        let body = serde_urlencoded::to_string([("client_id", self.client_id.as_str())])
            .map_err(|error| AccountError::InvalidResponse(error.to_string()))?;
        let response = self
            .http
            .post(format!("{}/oauth2/device/auth", self.platform_url))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(AccountError::Authorization(format!(
                "设备授权请求失败: {text}"
            )));
        }
        response
            .json::<DeviceCode>()
            .await
            .map_err(|error| AccountError::InvalidResponse(error.to_string()))
    }

    async fn request_token(&self, device: &DeviceCode) -> Result<TokenPoll, AccountError> {
        let body = serde_urlencoded::to_string([
            ("grant_type", DEVICE_GRANT),
            ("device_code", device.device_code.as_str()),
            ("client_id", self.client_id.as_str()),
        ])
        .map_err(|error| AccountError::InvalidResponse(error.to_string()))?;
        let response = self
            .http
            .post(format!("{}/oauth2/token", self.platform_url))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;

        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|error| AccountError::InvalidResponse(error.to_string()))?;
        if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
            return match error {
                "authorization_pending" => Ok(TokenPoll::Pending),
                "slow_down" => Ok(TokenPoll::SlowDown),
                "access_denied" => Err(AccountError::Authorization("用户拒绝了授权".to_string())),
                "expired_token" => Err(AccountError::Authorization("设备码已过期".to_string())),
                other => Err(AccountError::Authorization(format!(
                    "{other}: {}",
                    value
                        .get("error_description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                ))),
            };
        }

        let access_token = value
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AccountError::InvalidResponse("缺少 access_token".to_string()))?;
        let expires_in = value
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(3600);
        Ok(TokenPoll::Ready(Credentials {
            access_token: access_token.to_string(),
            refresh_token: value
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            expires_at_ms: Utc::now().timestamp_millis() + expires_in * 1000,
            username: None,
        }))
    }

    async fn refresh_access_token(&self, refresh_token: &str) -> Result<Credentials, AccountError> {
        let body = serde_urlencoded::to_string([
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.client_id.as_str()),
        ])
        .map_err(|error| AccountError::InvalidResponse(error.to_string()))?;
        let response = self
            .http
            .post(format!("{}/oauth2/token", self.platform_url))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;
        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(AccountError::Authorization(format!("刷新登录失败: {text}")));
        }
        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|error| AccountError::InvalidResponse(error.to_string()))?;
        let access_token = value
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AccountError::InvalidResponse("刷新响应缺少 access_token".to_string())
            })?;
        let expires_in = value
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .unwrap_or(3600);
        let username = self
            .current_credentials()
            .and_then(|credentials| credentials.username);
        Ok(Credentials {
            access_token: access_token.to_string(),
            refresh_token: value
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| Some(refresh_token.to_string())),
            expires_at_ms: Utc::now().timestamp_millis() + expires_in * 1000,
            username,
        })
    }
}

enum TokenPoll {
    Ready(Credentials),
    Pending,
    SlowDown,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AccountError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| AccountError::Storage(error.to_string()))?;
    }
    let temp = path.with_extension("bin.tmp");
    std::fs::write(&temp, bytes).map_err(|error| AccountError::Storage(error.to_string()))?;
    std::fs::rename(&temp, path).map_err(|error| AccountError::Storage(error.to_string()))?;
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn store_round_trips_and_does_not_write_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::new(dir.path().join("account-auth.bin"));
        let credentials = Credentials {
            access_token: "access-secret".to_string(),
            refresh_token: Some("refresh-secret".to_string()),
            expires_at_ms: 1_800_000_000_000,
            username: Some("tester".to_string()),
        };
        store.save(&credentials).unwrap();
        assert_eq!(store.load().unwrap(), Some(credentials.clone()));
        let raw = std::fs::read(store.path()).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("access-secret"));
        assert!(!text.contains("refresh-secret"));
    }
}
