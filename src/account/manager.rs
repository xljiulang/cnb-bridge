use super::model::*;
use super::store::*;
use std::sync::{Arc, Mutex};

/// Account manager shared by the Tauri command layer.
pub struct AccountManager {
    pub(super) store: AccountStore,
    pub(super) book: Mutex<AccountBook>,
    pub(super) status: Mutex<AccountStatus>,
    pub(super) mutation: Mutex<()>,
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
            mutation: Mutex::new(()),
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
                display_name: display_name_for(
                    &account.username,
                    account.credentials.nickname.as_deref(),
                ),
                avatar_url: avatar_url_for(&account.username),
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
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = AccountStatus::signing_in_as(
            Some(account.username.clone()),
            account.credentials.nickname.clone(),
        );
        Ok(())
    }

    /// Ends a cancelled WebView authorization without changing saved accounts.
    pub fn cancel_token_login(&self) -> AccountStatus {
        let status = AccountStatus::signed_out();
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        status
    }

    /// Saves a PAT created by the WebView authorization flow.
    pub fn complete_token_login(
        &self,
        access_token: impl Into<String>,
        username: Option<String>,
        nickname: Option<String>,
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
            nickname: normalized_nickname(nickname.as_deref()),
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

        let _mutation = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        let next = {
            let book = self.book.lock().unwrap_or_else(|e| e.into_inner());
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
            next
        };
        self.persist_book(&next)?;
        *self.book.lock().unwrap_or_else(|e| e.into_inner()) = next;
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
            display_name: None,
            avatar_url: None,
            auth_method: None,
            error: Some(error.into()),
            error_action: action.into(),
            needs_organization: false,
        };
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        status
    }

    /// Clears the current account error without changing account state.
    pub fn dismiss_error(&self) -> AccountStatus {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        status.error = None;
        status.error_action = None;
        status.clone()
    }

    /// Marks a saved-account login as failed.
    pub fn fail_account_login(&self, username: &str, error: impl Into<String>) {
        let nickname = self
            .book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .account(username)
            .and_then(|account| account.credentials.nickname.clone());
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) =
            AccountStatus::failed_with_nickname(Some(username.to_string()), nickname, error);
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

    /// Finds saved credentials by the persisted PAT token ID.
    pub fn credentials_by_token_id(&self, token_id: &str) -> Option<Credentials> {
        let token_id = token_id.trim();
        if token_id.is_empty() {
            return None;
        }
        self.book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .accounts
            .iter()
            .find(|account| account.credentials.token_id.as_deref() == Some(token_id))
            .map(|account| account.credentials.clone())
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
        let _mutation = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        let next = {
            let book = self.book.lock().unwrap_or_else(|e| e.into_inner());
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
            next
        };
        self.persist_book(&next)?;
        *self.book.lock().unwrap_or_else(|e| e.into_inner()) = next;
        Ok(())
    }

    /// Activates an existing account and records the successful login time.
    pub fn activate_account(
        &self,
        username: &str,
        nickname: Option<String>,
    ) -> Result<AccountStatus, AccountError> {
        let _mutation = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        let (next, status) = {
            let book = self.book.lock().unwrap_or_else(|e| e.into_inner());
            let account = book
                .account(username)
                .ok_or_else(|| AccountError::Authorization("账号不存在".to_string()))?;
            let username = account.username.clone();
            let mut credentials = account.credentials.clone();
            credentials.nickname = normalized_nickname(nickname.as_deref());
            let mut next = book.clone();
            let last_login_at = next.next_login_timestamp();
            next.active_username = Some(username.clone());
            let account = next.account_mut(&username).expect("account exists");
            account.credentials = credentials.clone();
            account.last_login_at = last_login_at;
            (next, AccountStatus::connected(&credentials))
        };
        self.persist_book(&next)?;
        *self.book.lock().unwrap_or_else(|e| e.into_inner()) = next;
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        Ok(status)
    }

    /// Refreshes display metadata without replacing the saved access token.
    pub fn update_profile_nickname(
        &self,
        username: &str,
        nickname: Option<&str>,
    ) -> Result<AccountStatus, AccountError> {
        let _mutation = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        let (next, canonical, normalized) = {
            let book = self.book.lock().unwrap_or_else(|e| e.into_inner());
            let account = book
                .account(username)
                .ok_or_else(|| AccountError::Authorization("账号不存在".to_string()))?;
            let canonical = account.username.clone();
            let normalized = normalized_nickname(nickname);
            let mut next = book.clone();
            next.account_mut(&canonical)
                .expect("account exists")
                .credentials
                .nickname = normalized.clone();
            (next, canonical, normalized)
        };
        self.persist_book(&next)?;
        *self.book.lock().unwrap_or_else(|e| e.into_inner()) = next;

        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if status
            .username
            .as_deref()
            .is_some_and(|current| current.eq_ignore_ascii_case(&canonical))
        {
            status.display_name = Some(display_name_for(&canonical, normalized.as_deref()));
        }
        Ok(status.clone())
    }

    /// Clears the active account while preserving all saved credentials.
    pub fn logout(&self) -> Result<AccountStatus, AccountError> {
        let _mutation = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        let next = {
            let book = self.book.lock().unwrap_or_else(|e| e.into_inner());
            let mut next = book.clone();
            next.active_username = None;
            next
        };
        self.persist_book(&next)?;
        *self.book.lock().unwrap_or_else(|e| e.into_inner()) = next;
        let status = AccountStatus::signed_out();
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status.clone();
        Ok(status)
    }

    /// Removes a saved account. The active account must be logged out first.
    pub fn remove_account(&self, username: &str) -> Result<(), AccountError> {
        let _mutation = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        let next = {
            let book = self.book.lock().unwrap_or_else(|e| e.into_inner());
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
            next
        };
        self.persist_book(&next)?;
        *self.book.lock().unwrap_or_else(|e| e.into_inner()) = next;
        Ok(())
    }

    fn persist_book(&self, book: &AccountBook) -> Result<(), AccountError> {
        let move_off_async_worker = tokio::runtime::Handle::try_current().is_ok_and(|handle| {
            handle.runtime_flavor() != tokio::runtime::RuntimeFlavor::CurrentThread
        });
        if move_off_async_worker {
            tokio::task::block_in_place(|| self.store.save_book(book))
        } else {
            self.store.save_book(book)
        }
    }
}
