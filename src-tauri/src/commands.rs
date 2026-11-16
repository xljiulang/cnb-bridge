use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cnb_bridge::account::AccountStatus;
use cnb_bridge::api::Namespace;
use cnb_bridge::logging::LogEntry;
use cnb_bridge::settings::Settings;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

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
    let device = state
        .account
        .start_device_login()
        .await
        .map_err(|error| CommandError::new("login_start_failed", error.to_string()))?;
    let _ = tauri_plugin_opener::open_url(&device.verification_uri_complete, None::<&str>);

    let cancel = Arc::new(AtomicBool::new(false));
    *state
        .login_cancel
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&cancel));
    let generation = state.login_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let account = Arc::clone(&state.account);
    let bridge = Arc::clone(&state.bridge);
    let app = state.app.clone();
    tauri::async_runtime::spawn(async move {
        let result = account.finish_device_login(&device, cancel).await;
        if result.is_ok() {
            let _ = bridge.start().await;
        }
        let _ = app.emit("cnb-bridge://snapshot", generation);
    });
    Ok(state.account.status())
}

#[tauri::command]
pub fn cancel_login(state: State<'_, DesktopState>) -> AccountStatus {
    if let Some(cancel) = state
        .login_cancel
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        cancel.store(true, Ordering::SeqCst);
    }
    state.account.status()
}

#[tauri::command]
pub async fn logout(state: State<'_, DesktopState>) -> Result<AccountStatus, CommandError> {
    let _ = state.bridge.stop().await;
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
pub fn ensure_ssh_key(state: State<'_, DesktopState>) -> Result<String, CommandError> {
    state
        .bridge
        .ensure_ssh_key()
        .map_err(|message| CommandError::new("ssh_key_failed", message))
}

#[tauri::command]
pub async fn regenerate_ssh_key(state: State<'_, DesktopState>) -> Result<String, CommandError> {
    state
        .bridge
        .regenerate_ssh_key()
        .await
        .map_err(|message| CommandError::new("ssh_key_regenerate_failed", message))
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
pub fn open_ssh_keys_page() -> Result<(), CommandError> {
    tauri_plugin_opener::open_url("https://cnb.cool/profile/ssh-keys/create", None::<&str>)
        .map_err(|error| CommandError::new("open_ssh_page_failed", error.to_string()))
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
