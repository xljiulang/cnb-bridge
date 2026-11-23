use crate::commands::CommandError;
use crate::tray;
use cnb_bridge::account::{AccountManager, AccountStatus, SavedAccount};
use cnb_bridge::bridge::{Bridge, BridgeSnapshot};
use cnb_bridge::lifecycle::AppLifecycleMutex;
use cnb_bridge::logging::Logger;
use cnb_bridge::model_catalog::{self, ModelSummary};
use cnb_bridge::paths;
use cnb_bridge::settings::{Settings, SettingsStore};
use cnb_bridge::ssh_key::SshKeyStore;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use tauri::{App, AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as AutostartExt;

/// Complete state returned to the Vue renderer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub account: AccountStatus,
    pub accounts: Vec<SavedAccount>,
    pub bridge: BridgeSnapshot,
    pub settings: Settings,
    pub version: String,
    pub logs_path: String,
    pub http_logs_path: String,
    pub models: Vec<ModelSummary>,
}

/// Serializes account-session changes so rapid switches cannot interleave.
#[derive(Default)]
pub struct SessionTransitionGate {
    transition: tokio::sync::Mutex<()>,
}

impl SessionTransitionGate {
    pub async fn run<F, Fut, T>(&self, operation: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let _transition = self.transition.lock().await;
        operation().await
    }
}

/// Shared desktop state.
pub struct DesktopState {
    pub app: AppHandle,
    pub account: Arc<AccountManager>,
    pub bridge: Arc<Bridge>,
    pub settings_store: SettingsStore,
    pub logger: Arc<Logger>,
    pub models: Vec<ModelSummary>,
    pub login_generation: AtomicU64,
    pub organization_create_lock: tokio::sync::Mutex<()>,
    pub session_transition: SessionTransitionGate,
    _lifecycle_mutex: AppLifecycleMutex,
}

impl DesktopState {
    /// Current persisted settings.
    pub fn settings(&self) -> Settings {
        let mut settings = self.settings_store.load().unwrap_or_default();
        if let Some(account_settings) = self.account.active_bridge_settings() {
            settings.repo_namespace = account_settings.repo_namespace;
            settings.repo_name = account_settings.repo_name;
        }
        settings
    }

    /// Builds the snapshot shown by the renderer.
    pub async fn snapshot(&self) -> Result<AppSnapshot, CommandError> {
        let settings = self.settings_store.load().unwrap_or_default();
        let bridge = self.bridge.snapshot().await;
        Ok(AppSnapshot {
            account: bridge.account.clone(),
            accounts: self.account.accounts(),
            bridge,
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
        login_generation: AtomicU64::new(0),
        organization_create_lock: tokio::sync::Mutex::new(()),
        session_transition: SessionTransitionGate::default(),
        _lifecycle_mutex: lifecycle_mutex,
    });

    let handle = app.handle().clone();
    let runtime_changes_bridge = Arc::clone(&bridge);
    let runtime_changes_app = handle.clone();
    let runtime_changes = runtime_changes_bridge.subscribe_runtime_changes();
    tauri::async_runtime::spawn(forward_runtime_changes(runtime_changes, move || {
        let _ = runtime_changes_app.emit("cnb-bridge://snapshot", ());
    }));
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

async fn forward_runtime_changes(mut changes: tokio::sync::watch::Receiver<()>, notify: impl Fn()) {
    while changes.changed().await.is_ok() {
        notify();
    }
}

fn ensure_organization_on_startup(bridge: Arc<Bridge>, app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let status = bridge.snapshot().await.account;
        if status.state != cnb_bridge::account::AccountState::Connected {
            return;
        }
        let Some(username) = status.username else {
            return;
        };
        let Some(state) = app.try_state::<DesktopState>() else {
            return;
        };
        let token = match state.account.access_token().await {
            Ok(token) => token,
            Err(_) => return,
        };
        let api = cnb_bridge::api::CnbApi::new();
        match api.current_user(&token).await {
            Ok(user) if user.username.eq_ignore_ascii_case(&username) => {
                if let Err(error) = state
                    .account
                    .update_profile_nickname(&username, Some(&user.nickname))
                {
                    state
                        .logger
                        .log("warn", format!("启动时保存 CNB 昵称失败: {error}"));
                }
            }
            Ok(_) => {
                invalidate_restored_account(&app, &username, "保存的账号与凭据不匹配");
                return;
            }
            Err(error) if error.is_auth_rejection() => {
                invalidate_restored_account(&app, &username, &format!("登录凭据已失效: {error}"));
                return;
            }
            Err(error) => {
                state
                    .account
                    .fail_account_login(&username, format!("恢复登录失败: {error}"));
                state
                    .logger
                    .log("warn", format!("启动时校验账号 {username} 失败: {error}"));
                bridge.account_signed_out().await;
                let _ = app.emit("cnb-bridge://snapshot", ());
                return;
            }
        }
        match api.list_owned_namespaces(&token).await {
            Ok(_) => {}
            Err(error) if error.is_auth_rejection() => {
                invalidate_restored_account(&app, &username, &format!("登录凭据已失效: {error}"));
                return;
            }
            Err(error) => {
                state
                    .account
                    .fail_account_login(&username, format!("恢复登录失败: {error}"));
                state.logger.log(
                    "warn",
                    format!("启动时读取账号 {username} 组织失败: {error}"),
                );
                bridge.account_signed_out().await;
                let _ = app.emit("cnb-bridge://snapshot", ());
                return;
            }
        }
        match bridge.ensure_account_organization().await {
            Ok(()) => {}
            Err(error) => {
                state
                    .logger
                    .log("warn", format!("启动时检查 CNB Owner 组织失败: {error}"));
            }
        }
        let _ = app.emit("cnb-bridge://snapshot", ());
    });
}

fn invalidate_restored_account(app: &AppHandle, username: &str, message: &str) {
    let Some(state) = app.try_state::<DesktopState>() else {
        return;
    };
    let _ = state.account.logout();
    let _ = state.account.remove_account(username);
    state.account.fail_token_login(message);
    state
        .logger
        .log("warn", format!("已删除失效账号 {username}: {message}"));
    tauri::async_runtime::spawn({
        let bridge = Arc::clone(&state.bridge);
        let app = app.clone();
        async move {
            bridge.account_signed_out().await;
            let _ = app.emit("cnb-bridge://snapshot", ());
        }
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

const EXIT_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Disconnects the local bridge tunnel, then exits the process.
pub fn exit_with_shutdown(app: AppHandle) {
    static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
    if EXIT_REQUESTED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        if let Some(state) = app.try_state::<DesktopState>() {
            let _ = tokio::time::timeout(EXIT_STOP_TIMEOUT, state.bridge.disconnect()).await;
        }
        tray::exit_now(&app);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{mpsc, oneshot};

    #[tokio::test]
    async fn forwards_bridge_runtime_changes_to_the_ui_notifier() {
        let temp = tempfile::tempdir().unwrap();
        let account = AccountManager::with_store(cnb_bridge::account::AccountStore::new(
            temp.path().join("account.bin"),
        ));
        let bridge = Arc::new(Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_notifier = Arc::clone(&calls);
        let changes = bridge.subscribe_runtime_changes();
        let task = tokio::spawn(forward_runtime_changes(changes, move || {
            calls_for_notifier.fetch_add(1, Ordering::SeqCst);
        }));

        bridge.account_signed_out().await;
        tokio::time::timeout(std::time::Duration::from_millis(100), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("runtime change was not forwarded");

        task.abort();
    }

    #[tokio::test]
    async fn session_transition_gate_serializes_rapid_operations() {
        let gate = Arc::new(SessionTransitionGate::default());
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel::<&'static str>();
        let (release_first_tx, release_first_rx) = oneshot::channel();

        let first_gate = Arc::clone(&gate);
        let first_entered = entered_tx.clone();
        let first = tokio::spawn(async move {
            first_gate
                .run(|| async move {
                    first_entered.send("first").unwrap();
                    release_first_rx.await.unwrap();
                    "first"
                })
                .await
        });
        assert_eq!(entered_rx.recv().await, Some("first"));

        let second_gate = Arc::clone(&gate);
        let second_entered = entered_tx.clone();
        let second = tokio::spawn(async move {
            second_gate
                .run(|| async move {
                    second_entered.send("second").unwrap();
                    "second"
                })
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), entered_rx.recv())
                .await
                .is_err()
        );

        release_first_tx.send(()).unwrap();
        assert_eq!(first.await.unwrap(), "first");
        assert_eq!(entered_rx.recv().await, Some("second"));
        assert_eq!(second.await.unwrap(), "second");
    }
}
