use crate::commands::CommandError;
use crate::tray;
use cnb_bridge::account::{AccountManager, AccountStatus};
use cnb_bridge::bridge::{Bridge, BridgeSnapshot};
use cnb_bridge::logging::Logger;
use cnb_bridge::paths;
use cnb_bridge::settings::{Settings, SettingsStore};
use cnb_bridge::ssh_key::SshKeyStore;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};
use tauri::{App, AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as AutostartExt;

/// Complete state returned to the Vue renderer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub account: AccountStatus,
    pub bridge: BridgeSnapshot,
    pub settings: Settings,
    pub version: String,
    pub logs_path: String,
}

/// Shared desktop state.
pub struct DesktopState {
    pub app: AppHandle,
    pub account: Arc<AccountManager>,
    pub bridge: Arc<Bridge>,
    pub settings_store: SettingsStore,
    pub logger: Arc<Logger>,
    pub login_cancel: Mutex<Option<Arc<AtomicBool>>>,
    pub login_generation: AtomicU64,
}

impl DesktopState {
    /// Current persisted settings.
    pub fn settings(&self) -> Settings {
        self.settings_store.load().unwrap_or_default()
    }

    /// Builds the snapshot shown by the renderer.
    pub async fn snapshot(&self) -> Result<AppSnapshot, CommandError> {
        let settings = self.settings_store.load().unwrap_or_default();
        Ok(AppSnapshot {
            account: self.account.status(),
            bridge: self.bridge.snapshot().await,
            settings,
            version: env!("CARGO_PKG_VERSION").to_string(),
            logs_path: paths::logs_dir().display().to_string(),
        })
    }
}

/// Creates state and starts background work after the Tauri app is ready.
pub fn initialize(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = paths::ensure_data_dir()?;
    std::fs::create_dir_all(data_dir.join("logs"))?;

    let logger = Arc::new(Logger::with_file(200, paths::logs_dir()));
    let account = AccountManager::new();
    let settings_store = SettingsStore::new(paths::settings_path());
    let bridge = Arc::new(Bridge::new(
        Arc::clone(&account),
        settings_store.clone(),
        SshKeyStore::default_path(),
        Arc::clone(&logger),
    ));
    let supervisor = Arc::clone(&bridge);
    tauri::async_runtime::spawn(async move {
        supervisor.supervise().await;
    });

    app.manage(DesktopState {
        app: app.handle().clone(),
        account: Arc::clone(&account),
        bridge: Arc::clone(&bridge),
        settings_store,
        logger,
        login_cancel: Mutex::new(None),
        login_generation: AtomicU64::new(0),
    });

    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let state = handle.state::<DesktopState>();
        let account_status = state.account.status();
        if account_status.state == cnb_bridge::account::AccountState::Connected {
            if let Err(error) = state.bridge.start().await {
                state
                    .logger
                    .log("error", format!("自动启动桥接失败: {error}"));
            }
            let _ = handle.emit("cnb-bridge://snapshot", ());
        }
    });

    Ok(())
}

/// Applies the Windows auto-start setting.
pub fn apply_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    if enabled {
        autolaunch.enable().map_err(|error| error.to_string())
    } else {
        autolaunch.disable().map_err(|error| error.to_string())
    }
}

/// Stops the bridge and exits the process.
pub fn exit_with_shutdown(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Some(state) = app.try_state::<DesktopState>() {
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(6), state.bridge.stop()).await;
        }
        tray::exit_now(&app);
    });
}
