use crate::commands::CommandError;
use crate::tray;
use cnb_bridge::account::{AccountManager, AccountStatus};
use cnb_bridge::bridge::{Bridge, BridgeSnapshot};
use cnb_bridge::lifecycle::AppLifecycleMutex;
use cnb_bridge::logging::Logger;
use cnb_bridge::model_catalog::{self, ModelSummary};
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
    pub http_logs_path: String,
    pub models: Vec<ModelSummary>,
}

/// Shared desktop state.
pub struct DesktopState {
    pub app: AppHandle,
    pub account: Arc<AccountManager>,
    pub bridge: Arc<Bridge>,
    pub settings_store: SettingsStore,
    pub logger: Arc<Logger>,
    pub models: Vec<ModelSummary>,
    pub login_cancel: Mutex<Option<Arc<AtomicBool>>>,
    pub login_generation: AtomicU64,
    pub organization_create_lock: tokio::sync::Mutex<()>,
    _lifecycle_mutex: AppLifecycleMutex,
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
            http_logs_path: paths::logs_dir().join("http-trace").display().to_string(),
            models: self.models.clone(),
        })
    }
}

/// Creates state and starts background work after the Tauri app is ready.
pub fn initialize(
    app: &mut App,
    lifecycle_mutex: AppLifecycleMutex,
) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = paths::ensure_data_dir()?;
    std::fs::create_dir_all(data_dir.join("logs"))?;
    std::fs::create_dir_all(paths::logs_dir().join("http-trace"))?;

    let logger = Arc::new(Logger::with_file(200, paths::logs_dir()));
    let account = AccountManager::new();
    let models = model_catalog::summaries().map_err(std::io::Error::other)?;
    let settings_store = SettingsStore::new(paths::settings_path());
    let bridge = Arc::new(Bridge::new(
        Arc::clone(&account),
        settings_store.clone(),
        SshKeyStore::default_path(),
        Arc::clone(&logger),
    ));
    app.manage(DesktopState {
        app: app.handle().clone(),
        account: Arc::clone(&account),
        bridge: Arc::clone(&bridge),
        settings_store,
        logger,
        models,
        login_cancel: Mutex::new(None),
        login_generation: AtomicU64::new(0),
        organization_create_lock: tokio::sync::Mutex::new(()),
        _lifecycle_mutex: lifecycle_mutex,
    });

    let handle = app.handle().clone();
    ensure_organization_on_startup(Arc::clone(&bridge), handle.clone());
    let gateway_bridge = Arc::clone(&bridge);
    let gateway_port = settings_store_for_gateway(&handle);
    tauri::async_runtime::spawn(async move {
        if let Err(error) = gateway_bridge.start_gateway(gateway_port).await {
            if let Some(state) = handle.try_state::<DesktopState>() {
                state
                    .logger
                    .log("error", format!("本地网关启动失败: {error}"));
            }
        }
        let _ = handle.emit("cnb-bridge://snapshot", ());
    });

    Ok(())
}

fn ensure_organization_on_startup(bridge: Arc<Bridge>, app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if bridge.snapshot().await.account.state != cnb_bridge::account::AccountState::Connected {
            return;
        }
        match bridge.ensure_account_organization().await {
            Ok(()) => {}
            Err(error) => {
                if let Some(state) = app.try_state::<DesktopState>() {
                    state
                        .logger
                        .log("warn", format!("启动时检查 CNB Owner 组织失败: {error}"));
                }
            }
        }
        let _ = app.emit("cnb-bridge://snapshot", ());
    });
}

fn settings_store_for_gateway(app: &AppHandle) -> u16 {
    app.try_state::<DesktopState>()
        .map(|state| state.settings().local_port)
        .unwrap_or(cnb_bridge::settings::DEFAULT_LOCAL_PORT)
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
    static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
    if EXIT_REQUESTED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        if let Some(state) = app.try_state::<DesktopState>() {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(6),
                state.bridge.shutdown_local(),
            )
            .await;
        }
        tray::exit_now(&app);
    });
}
