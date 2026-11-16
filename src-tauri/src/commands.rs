use cnb_bridge::account::AccountStatus;
use cnb_bridge::api::Namespace;
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
    state
        .settings_store
        .save(&settings)
        .map_err(|message| CommandError::new("settings_save_failed", message))?;
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
    state
        .account
        .logout()
        .map_err(|error| CommandError::new("logout_failed", error.to_string()))
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
