//! Personal access token storage for CNB Bridge.

use crate::dpapi;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Authentication method persisted with the CNB credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum AuthMethod {
    #[default]
    OAuth,
    PersonalAccessToken,
}

/// Persisted CNB personal access token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub access_token: String,
    pub username: Option<String>,
    #[serde(default)]
    pub auth_method: AuthMethod,
}

/// Current account state exposed to the desktop UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub state: AccountState,
    pub username: Option<String>,
    pub auth_method: Option<AuthMethod>,
    pub error: Option<String>,
    #[serde(default)]
    pub needs_organization: bool,
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
            auth_method: None,
            error: None,
            needs_organization: false,
        }
    }

    /// WebView authorization in progress.
    pub fn signing_in() -> Self {
        Self {
            state: AccountState::SigningIn,
            username: None,
            auth_method: None,
            error: None,
            needs_organization: false,
        }
    }

    /// Connected status backed by a personal access token.
    pub fn connected(credentials: &Credentials) -> Self {
        Self {
            state: AccountState::Connected,
            username: credentials.username.clone(),
            auth_method: Some(credentials.auth_method),
            error: None,
            needs_organization: false,
        }
    }

    /// Updates whether a connected account still needs a root organization.
    pub fn set_needs_organization(&mut self, needs_organization: bool) {
        self.needs_organization = needs_organization;
    }
}

/// Errors raised while loading or saving credentials.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("授权失败: {0}")]
    Authorization(String),
    #[error("本地凭据存储失败: {0}")]
    Storage(String),
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

    /// Loads a PAT and discards legacy OAuth credentials.
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
        let credentials: Credentials = serde_json::from_slice(&plaintext)
            .map_err(|error| AccountError::Storage(format!("解析登录凭据失败: {error}")))?;
        if credentials.auth_method != AuthMethod::PersonalAccessToken {
            self.remove()?;
            return Ok(None);
        }
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
    store: AccountStore,
    credentials: Mutex<Option<Credentials>>,
    status: Mutex<AccountStatus>,
}

impl AccountManager {
    /// Creates a manager and loads any saved PAT.
    pub fn new() -> Arc<Self> {
        Self::with_store(AccountStore::default_path())
    }

    /// Creates a manager with explicit storage.
    pub fn with_store(store: AccountStore) -> Arc<Self> {
        let (credentials, status) = match store.load() {
            Ok(Some(credentials)) => {
                let status = AccountStatus::connected(&credentials);
                (Some(credentials), status)
            }
            Ok(None) => (None, AccountStatus::signed_out()),
            Err(error) => {
                let mut status = AccountStatus::signed_out();
                status.state = AccountState::Failed;
                status.error = Some(format!("保存的登录信息无法读取，请重新扫码: {error}"));
                (None, status)
            }
        };
        Arc::new(Self {
            store,
            credentials: Mutex::new(credentials),
            status: Mutex::new(status),
        })
    }

    /// Current status snapshot.
    pub fn status(&self) -> AccountStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Updates the organization readiness flag for the current connected account.
    pub fn set_needs_organization(&self, needs_organization: bool) -> AccountStatus {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if status.state == AccountState::Connected {
            status.needs_organization = needs_organization;
        }
        status.clone()
    }

    /// Marks WebView authorization as in progress.
    pub fn begin_token_login(&self) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = AccountStatus::signing_in();
    }

    /// Saves a PAT created by the WebView authorization flow.
    pub fn complete_token_login(
        &self,
        access_token: impl Into<String>,
        username: Option<String>,
    ) -> Result<AccountStatus, AccountError> {
        let credentials = Credentials {
            access_token: access_token.into(),
            username,
            auth_method: AuthMethod::PersonalAccessToken,
        };
        if credentials.access_token.trim().is_empty() {
            return Err(AccountError::Authorization(
                "CNB 未返回访问令牌".to_string(),
            ));
        }
        self.store.save(&credentials)?;
        *self.credentials.lock().unwrap_or_else(|e| e.into_inner()) = Some(credentials.clone());
        let status = AccountStatus::connected(&credentials);
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        Ok(status)
    }

    /// Marks WebView authorization as failed.
    pub fn fail_token_login(&self, error: impl Into<String>) {
        let mut status = AccountStatus::signed_out();
        status.state = AccountState::Failed;
        status.error = Some(error.into());
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
    }

    /// Returns the saved PAT.
    pub async fn access_token(&self) -> Result<String, AccountError> {
        self.current_credentials()
            .map(|credentials| credentials.access_token)
            .ok_or_else(|| AccountError::Authorization("尚未扫码绑定".to_string()))
    }

    /// Clears all saved credentials.
    pub fn logout(&self) -> Result<AccountStatus, AccountError> {
        *self.credentials.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.store.remove()?;
        let status = AccountStatus::signed_out();
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        Ok(status)
    }

    fn current_credentials(&self) -> Option<Credentials> {
        self.credentials
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
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
    use crate::dpapi;

    #[test]
    fn store_round_trips_pat_and_hides_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::new(dir.path().join("account-auth.bin"));
        let credentials = Credentials {
            access_token: "personal-access-token".to_string(),
            username: Some("tester".to_string()),
            auth_method: AuthMethod::PersonalAccessToken,
        };
        store.save(&credentials).unwrap();
        assert_eq!(store.load().unwrap(), Some(credentials));
        let raw = std::fs::read(store.path()).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("personal-access-token"));
    }

    #[test]
    fn rejects_legacy_oauth_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account-auth.bin");
        let legacy = serde_json::json!({
            "accessToken": "oauth-token",
            "refreshToken": "oauth-refresh",
            "expiresAtMs": 1_800_000_000_000_i64,
            "username": "legacy-user"
        });
        let encrypted = dpapi::protect(serde_json::to_string(&legacy).unwrap().as_bytes()).unwrap();
        std::fs::write(&path, encrypted).unwrap();
        let store = AccountStore::new(&path);

        assert_eq!(store.load().unwrap(), None);
        assert!(!path.exists());
    }

    #[test]
    fn organization_requirement_is_preserved_only_for_connected_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let manager =
            AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
        manager.begin_token_login();

        let status = manager.set_needs_organization(true);
        assert!(!status.needs_organization);

        let status = manager
            .complete_token_login("personal-access-token", Some("new-user".to_string()))
            .unwrap();
        assert!(!status.needs_organization);

        let status = manager.set_needs_organization(true);
        assert!(status.needs_organization);

        let status = manager.set_needs_organization(false);
        assert!(!status.needs_organization);
    }
}
