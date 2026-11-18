use crate::settings::DEFAULT_REPO_NAME;
use serde::{Deserialize, Serialize};
use url::Url;

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
    pub nickname: Option<String>,
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
    pub display_name: String,
    pub avatar_url: Option<String>,
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
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
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

pub(super) fn normalized_nickname(nickname: Option<&str>) -> Option<String> {
    nickname
        .map(str::trim)
        .filter(|nickname| !nickname.is_empty())
        .map(str::to_string)
}

pub(super) fn display_name_for(username: &str, nickname: Option<&str>) -> String {
    normalized_nickname(nickname).unwrap_or_else(|| username.trim().to_string())
}

pub(super) fn avatar_url_for(username: &str) -> Option<String> {
    let username = username.trim();
    if username.is_empty() {
        return None;
    }
    let mut url = Url::parse("https://cnb.cool/").ok()?;
    url.path_segments_mut()
        .ok()?
        .extend(["users", username, "avatar", "l"]);
    Some(url.to_string())
}

impl AccountStatus {
    /// Signed-out status.
    pub fn signed_out() -> Self {
        Self {
            state: AccountState::SignedOut,
            username: None,
            display_name: None,
            avatar_url: None,
            auth_method: None,
            error: None,
            error_action: None,
            needs_organization: false,
        }
    }

    /// WebView authorization in progress.
    pub fn signing_in() -> Self {
        Self::signing_in_as(None, None)
    }

    /// WebView authorization or account restoration in progress.
    pub fn signing_in_as(username: Option<String>, nickname: Option<String>) -> Self {
        let display_name = username
            .as_deref()
            .map(|username| display_name_for(username, nickname.as_deref()));
        let avatar_url = username.as_deref().and_then(avatar_url_for);
        Self {
            state: AccountState::SigningIn,
            username,
            display_name,
            avatar_url,
            auth_method: None,
            error: None,
            error_action: None,
            needs_organization: false,
        }
    }

    /// Connected status backed by a personal access token.
    pub fn connected(credentials: &Credentials) -> Self {
        let username = credentials.username.as_deref();
        Self {
            state: AccountState::Connected,
            username: credentials.username.clone(),
            display_name: username
                .map(|username| display_name_for(username, credentials.nickname.as_deref())),
            avatar_url: username.and_then(avatar_url_for),
            auth_method: Some(credentials.auth_method),
            error: None,
            error_action: None,
            needs_organization: false,
        }
    }

    /// Failed login that keeps the selected username visible when available.
    pub fn failed(username: Option<String>, error: impl Into<String>) -> Self {
        Self::failed_with_nickname(username, None, error)
    }

    /// Failed login that preserves saved display metadata when available.
    pub fn failed_with_nickname(
        username: Option<String>,
        nickname: Option<String>,
        error: impl Into<String>,
    ) -> Self {
        let display_name = username
            .as_deref()
            .map(|username| display_name_for(username, nickname.as_deref()));
        let avatar_url = username.as_deref().and_then(avatar_url_for);
        Self {
            state: AccountState::Failed,
            display_name,
            avatar_url,
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
