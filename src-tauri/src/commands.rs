use cnb_bridge::account::AccountStatus;
use cnb_bridge::api::{ApiError, CnbApi, CnbUsageSummary, Namespace};
use cnb_bridge::http_trace::{HttpTraceStore, HttpTraceSummary};
use cnb_bridge::logging::LogEntry;
use cnb_bridge::settings::Settings;
use serde::Serialize;
use tauri::{AppHandle, State};

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
    let autostart_warning = state::apply_autostart(&app, settings.launch_at_login).err();
    let mut snapshot = state.snapshot().await?;
    if let Some(warning) = autostart_warning {
        snapshot.version = format!("{} (autostart: {warning})", snapshot.version);
    }
    Ok(snapshot)
}

#[tauri::command]
pub async fn start_login(state: State<'_, DesktopState>) -> Result<AccountStatus, CommandError> {
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
pub async fn start_bridge(state: State<'_, DesktopState>) -> Result<AppSnapshot, CommandError> {
    state
        .bridge
        .start()
        .await
        .map_err(|error| CommandError::new("bridge_start_failed", error.to_string()))?;
    state.snapshot().await
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

#[tauri::command]
pub fn open_external(url: String) -> Result<(), CommandError> {
    if !url.starts_with("https://cnb.cool/") {
        return Err(CommandError::new(
            "invalid_external_url",
            "只允许打开 cnb.cool 的安全链接",
        ));
    }
    tauri_plugin_opener::open_url(&url, None::<&str>)
        .map_err(|error| CommandError::new("open_external_failed", error.to_string()))
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
