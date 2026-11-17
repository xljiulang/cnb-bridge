use cnb_bridge::account::{AccountBridgeSettings, AccountState, AccountStatus};
use cnb_bridge::api::{ApiError, CnbApi, CnbUsageSummary, Namespace};
use cnb_bridge::bridge::BridgeError;
use cnb_bridge::http_trace::{HttpTraceStore, HttpTraceSummary};
use cnb_bridge::logging::LogEntry;
use cnb_bridge::settings::Settings;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::auth_webview;
use crate::state::{self, AppSnapshot, DesktopState};

/// Serializable command error.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl CommandError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

#[tauri::command]
pub async fn get_snapshot(state: State<'_, DesktopState>) -> Result<AppSnapshot, CommandError> {
    state.snapshot().await
}

#[tauri::command]
pub async fn dismiss_error(state: State<'_, DesktopState>) -> Result<AppSnapshot, CommandError> {
    state.account.dismiss_error();
    state.bridge.dismiss_error().await;
    state.snapshot().await
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, DesktopState>,
    settings: Settings,
) -> Result<AppSnapshot, CommandError> {
    settings
        .validate()
        .map_err(|message| CommandError::new("invalid_settings", message))?;
    let previous = state.settings();
    let port_changed = previous.local_port != settings.local_port;
    if port_changed {
        state
            .bridge
            .rebind_gateway(settings.local_port)
            .await
            .map_err(|error| CommandError::new("gateway_rebind_failed", error.to_string()))?;
    }
    if let Err(message) = state.settings_store.save(&settings) {
        if port_changed {
            let _ = state.bridge.rebind_gateway(previous.local_port).await;
        }
        return Err(CommandError::new("settings_save_failed", message));
    }
    if state.account.status().state == AccountState::Connected {
        state
            .account
            .update_active_bridge_settings(AccountBridgeSettings {
                repo_namespace: settings.repo_namespace.clone(),
                repo_name: settings.repo_name.clone(),
            })
            .map_err(|error| {
                CommandError::new("account_settings_save_failed", error.to_string())
            })?;
    }
    let autostart_warning = state::apply_autostart(&app, settings.launch_at_login).err();
    let mut snapshot = state.snapshot().await?;
    if let Some(warning) = autostart_warning {
        snapshot.version = format!("{} (autostart: {warning})", snapshot.version);
    }
    Ok(snapshot)
}

#[tauri::command]
pub async fn start_login(state: State<'_, DesktopState>) -> Result<AccountStatus, CommandError> {
    if state.account.active_credentials().is_some()
        || state.account.status().state == AccountState::SigningIn
    {
        return Err(CommandError::new(
            "logout_required",
            "请先退出当前账号再扫码登录",
        ));
    }
    auth_webview::launch(state.app.clone())
        .map_err(|error| CommandError::new("login_start_failed", error))
}

#[tauri::command]
pub fn cancel_login(state: State<'_, DesktopState>) -> AccountStatus {
    auth_webview::cancel(&state.app)
}

#[tauri::command]
pub async fn logout(state: State<'_, DesktopState>) -> Result<AccountStatus, CommandError> {
    let _ = state.bridge.stop().await;
    auth_webview::clear_web_data(&state.app);
    let status = state
        .account
        .logout()
        .map_err(|error| CommandError::new("logout_failed", error.to_string()))?;
    state.bridge.account_signed_out().await;
    Ok(status)
}

#[tauri::command]
pub async fn login_with_account(
    state: State<'_, DesktopState>,
    username: String,
) -> Result<AccountStatus, CommandError> {
    if state.account.active_credentials().is_some()
        || state.account.status().state == AccountState::SigningIn
    {
        return Err(CommandError::new(
            "logout_required",
            "请先退出当前账号再选择其他账号",
        ));
    }
    let credentials = state
        .account
        .credentials_for(&username)
        .map_err(|error| CommandError::new("account_not_found", error.to_string()))?;
    state
        .account
        .begin_account_login(&username)
        .map_err(|error| CommandError::new("account_login_failed", error.to_string()))?;

    let api = CnbApi::new();
    let user = match api.current_user(&credentials.access_token).await {
        Ok(user) => user,
        Err(error) => {
            let message = apply_account_validation_error(&state.account, &username, error);
            return Err(CommandError::new("account_login_failed", message));
        }
    };
    if !user.username.eq_ignore_ascii_case(&username) {
        remove_invalid_account(&state.account, &username);
        return Err(CommandError::new(
            "account_identity_mismatch",
            "保存的凭据与账号不匹配，已移除该账号",
        ));
    }
    let namespaces = match api.list_owned_namespaces(&credentials.access_token).await {
        Ok(namespaces) => namespaces,
        Err(error) => {
            let message = apply_account_validation_error(&state.account, &username, error);
            return Err(CommandError::new("account_login_failed", message));
        }
    };

    state
        .account
        .activate_account(&username, Some(user.nickname))
        .map_err(|error| CommandError::new("account_login_failed", error.to_string()))?;
    state.account.set_needs_organization(namespaces.is_empty());
    state.bridge.account_connected().await;
    if namespaces.is_empty() {
        if let Err(error) = state.bridge.ensure_account_organization().await {
            state
                .logger
                .log("warn", format!("账号 {username} 根组织准备失败: {error}"));
        }
    }
    let _ = state.app.emit(
        "cnb-bridge://snapshot",
        state
            .login_generation
            .load(std::sync::atomic::Ordering::SeqCst),
    );
    Ok(state.account.status())
}

#[tauri::command]
pub async fn remove_account(
    state: State<'_, DesktopState>,
    username: String,
) -> Result<AccountStatus, CommandError> {
    if state.account.active_credentials().is_some()
        || state.account.status().state == AccountState::SigningIn
    {
        return Err(CommandError::new(
            "logout_required",
            "请先退出当前账号再移除账号",
        ));
    }
    state
        .account
        .remove_account(&username)
        .map_err(|error| CommandError::new("account_remove_failed", error.to_string()))?;
    Ok(state.account.status())
}

fn apply_account_validation_error(
    account: &cnb_bridge::account::AccountManager,
    username: &str,
    error: ApiError,
) -> String {
    if error.is_auth_rejection() {
        remove_invalid_account(account, username);
        "账号凭据已失效，已从本机移除".to_string()
    } else {
        let message = format!("账号登录失败: {error}");
        account.fail_account_login(username, message.clone());
        message
    }
}

fn remove_invalid_account(account: &cnb_bridge::account::AccountManager, username: &str) {
    let _ = account.remove_account(username);
    account.fail_token_login("账号凭据已失效，已从本机移除");
}

#[tauri::command]
pub async fn start_bridge(state: State<'_, DesktopState>) -> Result<AppSnapshot, CommandError> {
    state
        .bridge
        .start()
        .await
        .map_err(|error| CommandError::new("bridge_start_failed", error.to_string()))?;
    state.snapshot().await
}

#[tauri::command]
pub async fn create_cnb_namespace(
    state: State<'_, DesktopState>,
) -> Result<AccountStatus, CommandError> {
    let _create_guard = state.organization_create_lock.lock().await;
    let current = state.account.status();
    if !current.needs_organization {
        return Ok(current);
    }
    match state.bridge.ensure_account_organization().await {
        Ok(_) => {
            state.bridge.account_connected().await;
            Ok(state.account.status())
        }
        Err(error) => Err(map_organization_create_error(error)),
    }
}

#[tauri::command]
pub async fn stop_bridge(state: State<'_, DesktopState>) -> Result<AppSnapshot, CommandError> {
    let _ = state.bridge.stop().await;
    state.snapshot().await
}

#[tauri::command]
pub async fn list_namespaces(
    state: State<'_, DesktopState>,
) -> Result<Vec<Namespace>, CommandError> {
    state
        .bridge
        .list_namespaces()
        .await
        .map_err(|error| CommandError::new("namespaces_load_failed", error.to_string()))
}

/// Returns current-month AI Credits usage without activating the workspace.
#[tauri::command]
pub async fn get_cnb_usage(
    state: State<'_, DesktopState>,
) -> Result<CnbUsageSummary, CommandError> {
    let token = state.account.access_token().await.map_err(|error| {
        CommandError::new(
            "usage_account_required",
            format!("读取用量前请先登录 CNB: {error}"),
        )
    })?;
    let repo_namespace = state.settings().repo_namespace;
    let preferred = (!repo_namespace.trim().is_empty()).then_some(repo_namespace.as_str());
    CnbApi::new()
        .usage_summary(&token, preferred)
        .await
        .map_err(map_usage_error)
}

fn map_usage_error(error: ApiError) -> CommandError {
    match &error {
        ApiError::Remote { status, .. } if status.as_u16() == 401 => {
            CommandError::new("usage_account_required", "CNB 登录凭据已失效，请重新登录")
        }
        ApiError::Remote { status, .. } if status.as_u16() == 403 => {
            CommandError::new("usage_forbidden", "当前 CNB 账号无权读取组织用量")
        }
        _ => CommandError::new("usage_load_failed", format!("读取 CNB 用量失败: {error}")),
    }
}

fn map_organization_create_error(error: BridgeError) -> CommandError {
    let (api_error, fallback) = match &error {
        BridgeError::Api(error) => (Some(error), error.to_string()),
        BridgeError::Account(error) => {
            return CommandError::new(
                "namespace_permission_denied",
                format!("创建组织前请先登录 CNB: {error}"),
            )
        }
        _ => (None, error.to_string()),
    };
    let status = api_error.and_then(|error| match error {
        ApiError::Remote { status, .. } => Some(status.as_u16()),
        _ => None,
    });
    match (status, api_error) {
        (Some(401 | 403), _) => CommandError::new(
            "namespace_permission_denied",
            "当前 CNB 授权无权创建组织，请退出后重新扫码以获取组织管理权限",
        ),
        (Some(409), _) => CommandError::new(
            "namespace_not_created",
            "组织已存在，但当前账号尚未获得 Owner 权限",
        ),
        (_, Some(ApiError::InvalidResponse(message)))
            if message.contains("尚未获得 Owner 权限") =>
        {
            CommandError::new("namespace_not_created", message.clone())
        }
        _ => CommandError::new(
            "namespace_create_failed",
            format!("创建 CNB 组织失败: {fallback}"),
        ),
    }
}

#[tauri::command]
pub fn open_external(url: String) -> Result<(), CommandError> {
    if !is_allowed_external_url(&url) {
        return Err(CommandError::new(
            "invalid_external_url",
            "只允许打开 HTTP/HTTPS 链接",
        ));
    }
    tauri_plugin_opener::open_url(&url, None::<&str>)
        .map_err(|error| CommandError::new("open_external_failed", error.to_string()))
}

fn is_allowed_external_url(raw: &str) -> bool {
    url::Url::parse(raw)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

#[tauri::command]
pub fn open_logs_folder() -> Result<(), CommandError> {
    let dir = cnb_bridge::paths::logs_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|error| CommandError::new("logs_dir_failed", error.to_string()))?;
    tauri_plugin_opener::open_path(&dir, None::<&str>)
        .map_err(|error| CommandError::new("open_logs_failed", error.to_string()))
}

#[tauri::command]
pub fn get_logs(state: State<'_, DesktopState>, limit: Option<usize>) -> Vec<LogEntry> {
    state.logger.recent(limit.unwrap_or(200))
}

#[tauri::command]
pub fn list_http_traces(limit: Option<usize>) -> Result<Vec<HttpTraceSummary>, CommandError> {
    let store = HttpTraceStore::new(cnb_bridge::paths::logs_dir().join("http-trace"));
    store
        .list(limit.unwrap_or(100))
        .map_err(|error| CommandError::new("http_traces_load_failed", error))
}

#[tauri::command]
pub fn open_http_trace(id: String) -> Result<(), CommandError> {
    let store = HttpTraceStore::new(cnb_bridge::paths::logs_dir().join("http-trace"));
    store
        .record_paths(&id)
        .map_err(|error| CommandError::new("invalid_http_trace", error))?;
    std::fs::create_dir_all(store.root())
        .map_err(|error| CommandError::new("http_trace_dir_failed", error.to_string()))?;
    tauri_plugin_opener::open_path(store.root(), None::<&str>)
        .map_err(|error| CommandError::new("open_http_trace_failed", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_account_validation_error, is_allowed_external_url, map_organization_create_error,
    };
    use cnb_bridge::account::{AccountManager, AccountState, AccountStore};
    use cnb_bridge::api::{ApiError, StatusCode};
    use cnb_bridge::bridge::BridgeError;

    fn signed_out_manager_with_alice() -> (tempfile::TempDir, std::sync::Arc<AccountManager>) {
        let temp = tempfile::tempdir().unwrap();
        let manager =
            AccountManager::with_store(AccountStore::new(temp.path().join("account-auth.bin")));
        manager
            .complete_token_login("alice-token", Some("alice".to_string()), None, None, None)
            .unwrap();
        manager.logout().unwrap();
        (temp, manager)
    }

    #[test]
    fn allows_any_well_formed_http_or_https_url() {
        assert!(is_allowed_external_url(
            "https://github.com/xljiulang/cyarp-cnb"
        ));
        assert!(is_allowed_external_url(
            "https://example.com/arbitrary/path?query=1#fragment"
        ));
        assert!(is_allowed_external_url("http://example.com/"));
    }

    #[test]
    fn rejects_non_http_external_urls() {
        assert!(!is_allowed_external_url(""));
        assert!(!is_allowed_external_url("file:///C:/Windows/win.ini"));
        assert!(!is_allowed_external_url(
            "javascript:alert(document.domain)"
        ));
        assert!(!is_allowed_external_url("data:text/plain,hello"));
        assert!(!is_allowed_external_url("https://"));
    }

    #[test]
    fn maps_organization_permission_failures_to_relogin_guidance() {
        let error = map_organization_create_error(BridgeError::Api(ApiError::Remote {
            status: StatusCode::FORBIDDEN,
            code: Some(10),
            message: "missing group-manage:rw".to_string(),
        }));

        assert_eq!(error.code, "namespace_permission_denied");
        assert!(error.message.contains("重新扫码"));
    }

    #[test]
    fn maps_unconfirmed_conflicts_to_namespace_not_created() {
        let error = map_organization_create_error(BridgeError::Api(ApiError::Remote {
            status: StatusCode::CONFLICT,
            code: None,
            message: "already exists".to_string(),
        }));

        assert_eq!(error.code, "namespace_not_created");
    }

    #[test]
    fn auth_rejection_removes_saved_account() {
        let (_temp, manager) = signed_out_manager_with_alice();

        let message = apply_account_validation_error(
            &manager,
            "alice",
            ApiError::Remote {
                status: StatusCode::UNAUTHORIZED,
                code: None,
                message: "expired".to_string(),
            },
        );

        assert!(manager.accounts().is_empty());
        assert_eq!(manager.status().state, AccountState::Failed);
        assert_eq!(manager.status().username, None);
        assert!(message.contains("失效"));
    }

    #[test]
    fn transient_validation_failure_keeps_saved_account() {
        let (_temp, manager) = signed_out_manager_with_alice();

        let message = apply_account_validation_error(
            &manager,
            "alice",
            ApiError::Remote {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: None,
                message: "offline".to_string(),
            },
        );

        assert_eq!(manager.accounts().len(), 1);
        assert_eq!(manager.status().state, AccountState::Failed);
        assert_eq!(manager.status().username.as_deref(), Some("alice"));
        assert!(message.contains("offline"));
    }
}
