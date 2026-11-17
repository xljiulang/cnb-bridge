//! Personal access token storage for CNB Bridge.

use crate::dpapi;
use crate::paths;
use crate::settings::DEFAULT_REPO_NAME;
use chrono::Utc;
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
    #[serde(default)]
    pub token_id: Option<String>,
}

/// Per-account settings used by the bridge runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountBridgeSettings {
    pub repo_namespace: String,
    pub repo_name: String,
}

impl Default for AccountBridgeSettings {
    fn default() -> Self {
        Self {
            repo_namespace: String::new(),
            repo_name: DEFAULT_REPO_NAME.to_string(),
        }
    }
}

/// Saved account information exposed to the desktop UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAccount {
    pub username: String,
    pub last_login_at: i64,
    pub active: bool,
}

/// Optional user action attached to an account error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountAction {
    pub label: String,
    pub url: String,
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
    pub error_action: Option<AccountAction>,
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
            error_action: None,
            needs_organization: false,
        }
    }

    /// WebView authorization in progress.
    pub fn signing_in() -> Self {
        Self::signing_in_as(None)
    }

    /// WebView authorization or account restoration in progress.
    pub fn signing_in_as(username: Option<String>) -> Self {
        Self {
            state: AccountState::SigningIn,
            username,
            auth_method: None,
            error: None,
            error_action: None,
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
            error_action: None,
            needs_organization: false,
        }
    }

    /// Failed login that keeps the selected username visible when available.
    pub fn failed(username: Option<String>, error: impl Into<String>) -> Self {
        Self {
            state: AccountState::Failed,
            username,
            auth_method: None,
            error: Some(error.into()),
            error_action: None,
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

const ACCOUNT_BOOK_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredAccount {
    username: String,
    credentials: Credentials,
    #[serde(default)]
    last_login_at: i64,
    #[serde(default)]
    bridge_settings: Option<AccountBridgeSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct AccountBook {
    version: u32,
    active_username: Option<String>,
    accounts: Vec<StoredAccount>,
}

impl Default for AccountBook {
    fn default() -> Self {
        Self {
            version: ACCOUNT_BOOK_VERSION,
            active_username: None,
            accounts: Vec::new(),
        }
    }
}

impl AccountBook {
    fn active_account(&self) -> Option<&StoredAccount> {
        let active = self.active_username.as_deref()?;
        self.accounts
            .iter()
            .find(|account| account.username.eq_ignore_ascii_case(active))
    }

    fn account(&self, username: &str) -> Option<&StoredAccount> {
        self.accounts
            .iter()
            .find(|account| account.username.eq_ignore_ascii_case(username.trim()))
    }

    fn account_mut(&mut self, username: &str) -> Option<&mut StoredAccount> {
        self.accounts
            .iter_mut()
            .find(|account| account.username.eq_ignore_ascii_case(username.trim()))
    }

    fn next_login_timestamp(&self) -> i64 {
        let latest = self
            .accounts
            .iter()
            .map(|account| account.last_login_at)
            .max()
            .unwrap_or_default();
        Utc::now().timestamp_millis().max(latest + 1)
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

    fn load_book(&self) -> Result<AccountBook, AccountError> {
        let encrypted = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AccountBook::default())
            }
            Err(error) => {
                return Err(AccountError::Storage(format!(
                    "读取登录凭据 {} 失败: {error}",
                    self.path.display()
                )))
            }
        };
        let plaintext = dpapi::unprotect(&encrypted).map_err(AccountError::Storage)?;
        let value: serde_json::Value = serde_json::from_slice(&plaintext)
            .map_err(|error| AccountError::Storage(format!("解析登录凭据失败: {error}")))?;
        if value.get("accounts").is_some() {
            let mut book: AccountBook = serde_json::from_value(value)
                .map_err(|error| AccountError::Storage(format!("解析多账号凭据失败: {error}")))?;
            if book.version > ACCOUNT_BOOK_VERSION {
                return Err(AccountError::Storage(format!(
                    "账号凭据版本 {} 高于当前支持的版本 {ACCOUNT_BOOK_VERSION}",
                    book.version
                )));
            }
            book.version = ACCOUNT_BOOK_VERSION;
            if let Some(active) = book.active_username.take() {
                book.active_username = book
                    .account(&active)
                    .map(|account| account.username.clone());
            }
            return Ok(book);
        }

        let credentials: Credentials = serde_json::from_value(value)
            .map_err(|error| AccountError::Storage(format!("解析旧登录凭据失败: {error}")))?;
        if credentials.auth_method != AuthMethod::PersonalAccessToken {
            self.remove()?;
            return Err(AccountError::Authorization(
                "旧版 OAuth 凭据已清除，请重新扫码登录".to_string(),
            ));
        }
        let username = credentials
            .username
            .clone()
            .map(|username| username.trim().to_string())
            .filter(|username| !username.is_empty())
            .ok_or_else(|| {
                AccountError::Storage("旧登录凭据缺少 CNB 账号名，请重新扫码登录".to_string())
            })?;
        let mut credentials = credentials;
        credentials.username = Some(username.clone());
        let book = AccountBook {
            version: ACCOUNT_BOOK_VERSION,
            active_username: Some(username.clone()),
            accounts: vec![StoredAccount {
                username,
                credentials,
                last_login_at: 0,
                bridge_settings: None,
            }],
        };
        self.save_book(&book)?;
        Ok(book)
    }

    fn save_book(&self, book: &AccountBook) -> Result<(), AccountError> {
        let plaintext = serde_json::to_vec(book)
            .map_err(|error| AccountError::Storage(format!("序列化账号凭据失败: {error}")))?;
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
    book: Mutex<AccountBook>,
    status: Mutex<AccountStatus>,
}

impl AccountManager {
    /// Creates a manager and loads any saved PAT.
    pub fn new() -> Arc<Self> {
        Self::with_store(AccountStore::default_path())
    }

    /// Creates a manager with explicit storage.
    pub fn with_store(store: AccountStore) -> Arc<Self> {
        let (book, status) = match store.load_book() {
            Ok(book) => {
                let status = book
                    .active_account()
                    .map(|account| AccountStatus::connected(&account.credentials))
                    .unwrap_or_else(AccountStatus::signed_out);
                (book, status)
            }
            Err(error) => {
                let status = AccountStatus::failed(
                    None,
                    format!("保存的登录信息无法读取，请重新扫码: {error}"),
                );
                (AccountBook::default(), status)
            }
        };
        Arc::new(Self {
            store,
            book: Mutex::new(book),
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

    /// Saved accounts ordered by most recent successful login.
    pub fn accounts(&self) -> Vec<SavedAccount> {
        let book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        let mut accounts = book
            .accounts
            .iter()
            .map(|account| SavedAccount {
                username: account.username.clone(),
                last_login_at: account.last_login_at,
                active: book
                    .active_username
                    .as_deref()
                    .is_some_and(|active| active.eq_ignore_ascii_case(&account.username)),
            })
            .collect::<Vec<_>>();
        accounts.sort_by(|left, right| {
            right
                .last_login_at
                .cmp(&left.last_login_at)
                .then_with(|| left.username.cmp(&right.username))
        });
        accounts
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

    /// Marks a saved-account login as in progress.
    pub fn begin_account_login(&self, username: &str) -> Result<(), AccountError> {
        let book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        let account = book
            .account(username)
            .ok_or_else(|| AccountError::Authorization("账号不存在".to_string()))?;
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) =
            AccountStatus::signing_in_as(Some(account.username.clone()));
        Ok(())
    }

    /// Saves a PAT created by the WebView authorization flow.
    pub fn complete_token_login(
        &self,
        access_token: impl Into<String>,
        username: Option<String>,
        token_id: Option<String>,
        bridge_settings: Option<AccountBridgeSettings>,
    ) -> Result<AccountStatus, AccountError> {
        let username = username
            .as_deref()
            .map(str::trim)
            .filter(|username| !username.is_empty())
            .ok_or_else(|| AccountError::Authorization("CNB 未返回账号名".to_string()))?;
        let mut credentials = Credentials {
            access_token: access_token.into(),
            username: Some(username.to_string()),
            auth_method: AuthMethod::PersonalAccessToken,
            token_id: token_id
                .map(|token_id| token_id.trim().to_string())
                .filter(|token_id| !token_id.is_empty()),
        };
        if credentials.access_token.trim().is_empty() {
            return Err(AccountError::Authorization(
                "CNB 未返回访问令牌".to_string(),
            ));
        }

        let mut book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = book.clone();
        let last_login_at = next.next_login_timestamp();
        if let Some(account) = next.account_mut(username) {
            let canonical = account.username.clone();
            credentials.username = Some(canonical.clone());
            account.credentials = credentials.clone();
            account.last_login_at = last_login_at;
            if bridge_settings.is_some() {
                account.bridge_settings = bridge_settings;
            }
            next.active_username = Some(canonical);
        } else {
            next.accounts.push(StoredAccount {
                username: username.to_string(),
                credentials: credentials.clone(),
                last_login_at,
                bridge_settings: Some(bridge_settings.unwrap_or_default()),
            });
            next.active_username = Some(username.to_string());
        }
        self.store.save_book(&next)?;
        *book = next;
        let status = AccountStatus::connected(&credentials);
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        Ok(status)
    }

    /// Marks WebView authorization as failed.
    pub fn fail_token_login(&self, error: impl Into<String>) {
        let _ = self.fail_token_login_with_action(error, None);
    }

    /// Marks WebView authorization as failed with an optional user action.
    pub fn fail_token_login_with_action(
        &self,
        error: impl Into<String>,
        action: impl Into<Option<AccountAction>>,
    ) -> AccountStatus {
        let status = AccountStatus {
            state: AccountState::Failed,
            username: None,
            auth_method: None,
            error: Some(error.into()),
            error_action: action.into(),
            needs_organization: false,
        };
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        status
    }

    /// Marks a saved-account login as failed.
    pub fn fail_account_login(&self, username: &str, error: impl Into<String>) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) =
            AccountStatus::failed(Some(username.to_string()), error);
    }

    /// Returns the saved PAT.
    pub async fn access_token(&self) -> Result<String, AccountError> {
        self.active_credentials()
            .map(|credentials| credentials.access_token)
            .ok_or_else(|| AccountError::Authorization("尚未扫码绑定".to_string()))
    }

    /// Returns credentials for a saved account without exposing them to the UI.
    pub fn credentials_for(&self, username: &str) -> Result<Credentials, AccountError> {
        self.book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .account(username)
            .map(|account| account.credentials.clone())
            .ok_or_else(|| AccountError::Authorization("账号不存在".to_string()))
    }

    /// Returns the active account credentials.
    pub fn active_credentials(&self) -> Option<Credentials> {
        self.book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_account()
            .map(|account| account.credentials.clone())
    }

    /// Returns the active account's bridge settings.
    pub fn active_bridge_settings(&self) -> Option<AccountBridgeSettings> {
        self.book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_account()
            .and_then(|account| account.bridge_settings.clone())
    }

    /// Replaces the active account's bridge settings.
    pub fn update_active_bridge_settings(
        &self,
        settings: AccountBridgeSettings,
    ) -> Result<(), AccountError> {
        let mut book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        let active = book
            .active_username
            .clone()
            .ok_or_else(|| AccountError::Authorization("尚未登录".to_string()))?;
        if book.account(&active).is_none() {
            return Err(AccountError::Authorization("活动账号不存在".to_string()));
        }
        let mut next = book.clone();
        next.account_mut(&active)
            .expect("active account exists")
            .bridge_settings = Some(settings);
        self.store.save_book(&next)?;
        *book = next;
        Ok(())
    }

    /// Activates an existing account and records the successful login time.
    pub fn activate_account(&self, username: &str) -> Result<AccountStatus, AccountError> {
        let mut book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        let account = book
            .account(username)
            .ok_or_else(|| AccountError::Authorization("账号不存在".to_string()))?;
        let username = account.username.clone();
        let credentials = account.credentials.clone();
        let mut next = book.clone();
        let last_login_at = next.next_login_timestamp();
        next.active_username = Some(username.clone());
        next.account_mut(&username)
            .expect("account exists")
            .last_login_at = last_login_at;
        self.store.save_book(&next)?;
        let status = AccountStatus::connected(&credentials);
        *book = next;
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        Ok(status)
    }

    /// Clears the active account while preserving all saved credentials.
    pub fn logout(&self) -> Result<AccountStatus, AccountError> {
        let mut book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = book.clone();
        next.active_username = None;
        self.store.save_book(&next)?;
        *book = next;
        let status = AccountStatus::signed_out();
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        Ok(status)
    }

    /// Removes a saved account. The active account must be logged out first.
    pub fn remove_account(&self, username: &str) -> Result<(), AccountError> {
        let mut book = self.book.lock().unwrap_or_else(|e| e.into_inner());
        let account = book
            .account(username)
            .ok_or_else(|| AccountError::Authorization("账号不存在".to_string()))?;
        if book
            .active_username
            .as_deref()
            .is_some_and(|active| active.eq_ignore_ascii_case(&account.username))
        {
            return Err(AccountError::Authorization(
                "请先退出当前账号再移除".to_string(),
            ));
        }
        let canonical = account.username.clone();
        let mut next = book.clone();
        next.accounts
            .retain(|account| !account.username.eq_ignore_ascii_case(&canonical));
        self.store.save_book(&next)?;
        *book = next;
        Ok(())
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
    fn persists_pat_token_id_across_rescans_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::new(dir.path().join("account-auth.bin"));
        let manager = AccountManager::with_store(store.clone());
        manager
            .complete_token_login(
                "first-token",
                Some("alice".to_string()),
                Some("token-1".to_string()),
                None,
            )
            .unwrap();
        manager
            .complete_token_login(
                "second-token",
                Some("alice".to_string()),
                Some("token-2".to_string()),
                None,
            )
            .unwrap();

        assert_eq!(
            manager
                .credentials_for("alice")
                .unwrap()
                .token_id
                .as_deref(),
            Some("token-2")
        );
        assert_eq!(
            AccountManager::with_store(store)
                .credentials_for("alice")
                .unwrap()
                .token_id
                .as_deref(),
            Some("token-2")
        );
    }

    #[test]
    fn migrates_a_version_two_account_book_without_token_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account-auth.bin");
        let settings = AccountBridgeSettings {
            repo_namespace: "alice-team".to_string(),
            repo_name: "alice-bridge".to_string(),
        };
        let version_two = AccountBook {
            version: 2,
            active_username: Some("alice".to_string()),
            accounts: vec![StoredAccount {
                username: "alice".to_string(),
                credentials: Credentials {
                    access_token: "legacy-token".to_string(),
                    username: Some("alice".to_string()),
                    auth_method: AuthMethod::PersonalAccessToken,
                    token_id: None,
                },
                last_login_at: 42,
                bridge_settings: Some(settings.clone()),
            }],
        };
        let encrypted =
            dpapi::protect(serde_json::to_vec(&version_two).unwrap().as_slice()).unwrap();
        std::fs::write(&path, encrypted).unwrap();

        let manager = AccountManager::with_store(AccountStore::new(path));
        let book = manager.book.lock().unwrap();

        assert_eq!(book.version, 3);
        assert_eq!(book.active_username.as_deref(), Some("alice"));
        assert_eq!(book.accounts[0].credentials.token_id, None);
        assert_eq!(book.accounts[0].bridge_settings.as_ref(), Some(&settings));
    }

    #[test]
    fn account_book_round_trips_multiple_accounts_and_hides_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::new(dir.path().join("account-auth.bin"));
        let manager = AccountManager::with_store(store.clone());
        manager
            .complete_token_login("alice-token", Some("alice".to_string()), None, None)
            .unwrap();
        manager
            .complete_token_login("bob-token", Some("bob".to_string()), None, None)
            .unwrap();

        let raw = std::fs::read(store.path()).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("alice-token"));
        assert!(!text.contains("bob-token"));

        let reopened = AccountManager::with_store(store);
        let accounts = reopened.accounts();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].username, "bob");
        assert!(accounts[0].active);
        assert_eq!(accounts[1].username, "alice");
        assert!(!accounts[1].active);
        assert_eq!(reopened.status().username.as_deref(), Some("bob"));
    }

    #[test]
    fn rescanning_same_username_replaces_token_and_preserves_settings() {
        let dir = tempfile::tempdir().unwrap();
        let manager =
            AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
        let settings = AccountBridgeSettings {
            repo_namespace: "alice-team".to_string(),
            repo_name: "my-bridge".to_string(),
        };
        manager
            .complete_token_login(
                "first-token",
                Some("Alice".to_string()),
                None,
                Some(settings.clone()),
            )
            .unwrap();
        manager
            .complete_token_login("second-token", Some("alice".to_string()), None, None)
            .unwrap();

        let accounts = manager.accounts();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].username, "Alice");
        assert_eq!(manager.active_bridge_settings(), Some(settings));
        assert_eq!(
            manager.active_credentials().unwrap().access_token,
            "second-token"
        );
    }

    #[test]
    fn logout_keeps_accounts_and_clears_active_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::new(dir.path().join("account-auth.bin"));
        let manager = AccountManager::with_store(store.clone());
        manager
            .complete_token_login("alice-token", Some("alice".to_string()), None, None)
            .unwrap();

        let status = manager.logout().unwrap();

        assert_eq!(status.state, AccountState::SignedOut);
        assert_eq!(manager.accounts().len(), 1);
        assert!(!manager.accounts()[0].active);
        assert!(manager.active_credentials().is_none());

        let reopened = AccountManager::with_store(store);
        assert_eq!(reopened.status().state, AccountState::SignedOut);
        assert_eq!(reopened.accounts().len(), 1);
    }

    #[test]
    fn account_bridge_settings_follow_the_active_account() {
        let dir = tempfile::tempdir().unwrap();
        let manager =
            AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
        manager
            .complete_token_login(
                "alice-token",
                Some("alice".to_string()),
                None,
                Some(AccountBridgeSettings {
                    repo_namespace: "alice-team".to_string(),
                    repo_name: "alice-bridge".to_string(),
                }),
            )
            .unwrap();
        manager
            .complete_token_login(
                "bob-token",
                Some("bob".to_string()),
                None,
                Some(AccountBridgeSettings {
                    repo_namespace: "bob-team".to_string(),
                    repo_name: "bob-bridge".to_string(),
                }),
            )
            .unwrap();

        manager.logout().unwrap();
        manager.activate_account("alice").unwrap();

        assert_eq!(
            manager.active_bridge_settings(),
            Some(AccountBridgeSettings {
                repo_namespace: "alice-team".to_string(),
                repo_name: "alice-bridge".to_string(),
            })
        );
        assert_eq!(
            manager.active_credentials().unwrap().access_token,
            "alice-token"
        );
    }

    #[test]
    fn active_account_settings_updates_persist_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::new(dir.path().join("account-auth.bin"));
        let manager = AccountManager::with_store(store.clone());
        manager
            .complete_token_login("alice-token", Some("alice".to_string()), None, None)
            .unwrap();
        let settings = AccountBridgeSettings {
            repo_namespace: "alice-team".to_string(),
            repo_name: "updated-bridge".to_string(),
        };

        manager
            .update_active_bridge_settings(settings.clone())
            .unwrap();

        assert_eq!(
            AccountManager::with_store(store).active_bridge_settings(),
            Some(settings)
        );
    }

    #[test]
    fn selecting_saved_account_updates_recent_login_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::new(dir.path().join("account-auth.bin"));
        let manager = AccountManager::with_store(store.clone());
        manager
            .complete_token_login(
                "alice-token",
                Some("alice".to_string()),
                None,
                Some(AccountBridgeSettings {
                    repo_namespace: "alice-team".to_string(),
                    repo_name: "alice-bridge".to_string(),
                }),
            )
            .unwrap();
        manager
            .complete_token_login(
                "bob-token",
                Some("bob".to_string()),
                None,
                Some(AccountBridgeSettings {
                    repo_namespace: "bob-team".to_string(),
                    repo_name: "bob-bridge".to_string(),
                }),
            )
            .unwrap();
        manager.logout().unwrap();

        manager.activate_account("alice").unwrap();
        manager.logout().unwrap();

        let accounts = manager.accounts();
        assert_eq!(
            accounts
                .iter()
                .map(|account| account.username.as_str())
                .collect::<Vec<_>>(),
            vec!["alice", "bob"]
        );
        assert_eq!(
            manager.credentials_for("alice").unwrap().access_token,
            "alice-token"
        );
        assert_eq!(
            manager.active_bridge_settings(),
            None,
            "logging out should not select a new active account"
        );

        let reopened = AccountManager::with_store(store);
        assert_eq!(
            reopened
                .accounts()
                .iter()
                .map(|account| account.username.as_str())
                .collect::<Vec<_>>(),
            vec!["alice", "bob"]
        );
    }

    #[test]
    fn active_account_cannot_be_removed_before_logout() {
        let dir = tempfile::tempdir().unwrap();
        let manager =
            AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
        manager
            .complete_token_login("alice-token", Some("alice".to_string()), None, None)
            .unwrap();

        assert!(manager.remove_account("alice").is_err());

        manager.logout().unwrap();
        manager.remove_account("alice").unwrap();
        assert!(manager.accounts().is_empty());
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
        let manager = AccountManager::with_store(store);

        assert_eq!(manager.status().state, AccountState::Failed);
        assert!(manager.accounts().is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn migrates_legacy_pat_to_active_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account-auth.bin");
        let credentials = Credentials {
            access_token: "legacy-token".to_string(),
            username: Some("legacy-user".to_string()),
            auth_method: AuthMethod::PersonalAccessToken,
            token_id: None,
        };
        let encrypted =
            dpapi::protect(serde_json::to_vec(&credentials).unwrap().as_slice()).unwrap();
        std::fs::write(&path, encrypted).unwrap();
        let store = AccountStore::new(&path);

        let manager = AccountManager::with_store(store.clone());

        assert_eq!(manager.status().state, AccountState::Connected);
        assert_eq!(manager.status().username.as_deref(), Some("legacy-user"));
        assert_eq!(manager.accounts().len(), 1);
        assert!(manager.active_bridge_settings().is_none());
        assert_eq!(
            manager.active_credentials().unwrap().access_token,
            "legacy-token"
        );

        let reopened = AccountManager::with_store(store);
        assert_eq!(reopened.accounts().len(), 1);
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
            .complete_token_login(
                "personal-access-token",
                Some("new-user".to_string()),
                None,
                None,
            )
            .unwrap();
        assert!(!status.needs_organization);

        let status = manager.set_needs_organization(true);
        assert!(status.needs_organization);

        let status = manager.set_needs_organization(false);
        assert!(!status.needs_organization);
    }

    #[test]
    fn identity_verification_failure_carries_action_and_is_cleared_on_login() {
        let dir = tempfile::tempdir().unwrap();
        let manager =
            AccountManager::with_store(AccountStore::new(dir.path().join("account-auth.bin")));
        let action = AccountAction {
            label: "实名认证 · 个人设置".to_string(),
            url: "https://cnb.cool/profile/auth".to_string(),
        };

        let status = manager.fail_token_login_with_action("请先完成实名认证", action.clone());

        assert_eq!(status.error_action, Some(action.clone()));
        assert_eq!(status.error.as_deref(), Some("请先完成实名认证"));

        let status = manager
            .complete_token_login("token", Some("test-user".to_string()), None, None)
            .unwrap();
        assert_eq!(status.error_action, None);
    }
}
