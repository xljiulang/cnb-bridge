use super::model::*;
use crate::dpapi;
use crate::paths;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(super) const ACCOUNT_BOOK_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StoredAccount {
    pub(super) username: String,
    pub(super) credentials: Credentials,
    #[serde(default)]
    pub(super) last_login_at: i64,
    #[serde(default)]
    pub(super) bridge_settings: Option<AccountBridgeSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct AccountBook {
    pub(super) version: u32,
    pub(super) active_username: Option<String>,
    pub(super) accounts: Vec<StoredAccount>,
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
    pub(super) fn active_account(&self) -> Option<&StoredAccount> {
        let active = self.active_username.as_deref()?;
        self.accounts
            .iter()
            .find(|account| account.username.eq_ignore_ascii_case(active))
    }

    pub(super) fn account(&self, username: &str) -> Option<&StoredAccount> {
        self.accounts
            .iter()
            .find(|account| account.username.eq_ignore_ascii_case(username.trim()))
    }

    pub(super) fn account_mut(&mut self, username: &str) -> Option<&mut StoredAccount> {
        self.accounts
            .iter_mut()
            .find(|account| account.username.eq_ignore_ascii_case(username.trim()))
    }

    pub(super) fn next_login_timestamp(&self) -> i64 {
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

    pub(super) fn load_book(&self) -> Result<AccountBook, AccountError> {
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

    pub(super) fn save_book(&self, book: &AccountBook) -> Result<(), AccountError> {
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

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AccountError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| AccountError::Storage(error.to_string()))?;
    }
    let temp = path.with_extension("bin.tmp");
    std::fs::write(&temp, bytes).map_err(|error| AccountError::Storage(error.to_string()))?;
    std::fs::rename(&temp, path).map_err(|error| AccountError::Storage(error.to_string()))?;
    Ok(())
}
