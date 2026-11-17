//! End-to-end bridge state machine.

use crate::account::{AccountError, AccountManager, AccountStatus};
use crate::api::{ApiError, CnbApi, Namespace, WorkspaceDetail};
use crate::gateway::{GatewayConfig, GatewayHandle};
use crate::logging::Logger;
use crate::settings::SettingsStore;
use crate::ssh_key::SshKeyStore;
use crate::tunnel::{start_tunnel_with_logger, TunnelHandle, DEFAULT_TUNNEL_PORT};
use crate::workspace::{RepoProvisionError, RepoProvisioner, WorkspaceManager};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

/// High-level bridge state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum BridgeState {
    #[default]
    SignedOut,
    Idle,
    SigningIn,
    PreparingKey,
    PreparingRepo,
    StartingWorkspace,
    Connecting,
    Ready,
    NeedsSshKey,
    NeedsRepoCreation,
    Stopping,
    Failed,
}

/// Serializable state shown by the desktop UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSnapshot {
    pub state: BridgeState,
    pub account: AccountStatus,
    pub username: Option<String>,
    pub repo_slug: Option<String>,
    pub workspace_sn: Option<String>,
    pub local_port: u16,
    pub base_url: Option<String>,
    pub ssh_public_key: Option<String>,
    pub repo_creation_url: Option<String>,
    pub error: Option<String>,
}

/// Errors surfaced by the bridge orchestration.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("{0}")]
    Account(#[from] AccountError),
    #[error("{0}")]
    Api(#[from] ApiError),
    #[error("{0}")]
    RepoProvision(#[from] RepoProvisionError),
    #[error("{0}")]
    Other(String),
}

/// Coordinates account, repository, workspace and SSH lifecycle.
pub struct Bridge {
    api: CnbApi,
    account: Arc<AccountManager>,
    keys: SshKeyStore,
    settings: SettingsStore,
    logger: Arc<Logger>,
    runtime: Mutex<Runtime>,
    transition: Mutex<()>,
    organization_transition: Mutex<()>,
    gateway: Mutex<Option<GatewayHandle>>,
}

#[derive(Default)]
struct Runtime {
    state: Option<BridgeState>,
    username: Option<String>,
    repo_slug: Option<String>,
    workspace_sn: Option<String>,
    detail: Option<WorkspaceDetail>,
    tunnel: Option<TunnelHandle>,
    error: Option<String>,
    ssh_public_key: Option<String>,
    repo_creation_url: Option<String>,
    gateway_port: Option<u16>,
}

impl Bridge {
    /// Creates a bridge instance.
    pub fn new(
        account: Arc<AccountManager>,
        settings: SettingsStore,
        keys: SshKeyStore,
        logger: Arc<Logger>,
    ) -> Self {
        Self {
            api: CnbApi::new(),
            account,
            keys,
            settings,
            logger,
            runtime: Mutex::new(Runtime::default()),
            transition: Mutex::new(()),
            organization_transition: Mutex::new(()),
            gateway: Mutex::new(None),
        }
    }

    /// Starts the complete bridge: login → key → repo → workspace → SSH → health.
    pub async fn start(&self) -> Result<BridgeSnapshot, BridgeError> {
        self.ensure_active().await?;
        Ok(self.snapshot().await)
    }

    /// Ensures a CNB workspace and tunnel are active, coalescing concurrent callers.
    pub async fn ensure_active(&self) -> Result<u16, BridgeError> {
        let _transition = self.transition.lock().await;
        {
            let runtime = self.runtime.lock().await;
            if runtime.state == Some(BridgeState::Ready) {
                if let Some(tunnel) = &runtime.tunnel {
                    if tunnel.failure_reason().is_none() {
                        return Ok(tunnel.local_port());
                    }
                }
            }
        }

        let failed_tunnel = self.runtime.lock().await.tunnel.take();
        if let Some(tunnel) = failed_tunnel {
            tunnel.stop().await;
        }
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(BridgeState::PreparingKey);
            runtime.error = None;
        }
        self.start_locked().await?;
        self.runtime
            .lock()
            .await
            .tunnel
            .as_ref()
            .map(TunnelHandle::local_port)
            .ok_or_else(|| BridgeError::Other("CNB 隧道未建立".to_string()))
    }

    async fn start_locked(&self) -> Result<BridgeSnapshot, BridgeError> {
        let settings = match self.load_settings() {
            Ok(settings) => settings,
            Err(error) => {
                self.set_error(BridgeState::Failed, error.clone()).await;
                return Err(BridgeError::Other(error));
            }
        };
        let manual_creation_pending = self.runtime.lock().await.repo_creation_url.is_some();

        let token = match self.account.access_token().await {
            Ok(token) => token,
            Err(error) => {
                self.set_error(BridgeState::SignedOut, error.to_string())
                    .await;
                return Err(BridgeError::Account(error));
            }
        };

        let user = match self.api.current_user(&token).await {
            Ok(user) => user,
            Err(error) => {
                self.set_error(BridgeState::Failed, error.to_string()).await;
                return Err(BridgeError::Api(error));
            }
        };
        if let Err(error) = self
            .account
            .update_profile_nickname(&user.username, Some(&user.nickname))
        {
            self.logger
                .log("warn", format!("保存 CNB 昵称失败: {error}"));
        }
        self.runtime.lock().await.username = Some(user.username.clone());
        let organization_result = {
            let _organization_transition = self.organization_transition.lock().await;
            self.ensure_organization_for(&token, &user.username).await
        };
        if let Err(error) = organization_result {
            self.logger
                .log("error", format!("CNB 根组织准备失败: {error}"));
            self.set_error(BridgeState::Failed, error.to_string()).await;
            return Err(error);
        }

        let public_key = self.keys.ensure().map_err(BridgeError::Other)?;
        self.runtime.lock().await.ssh_public_key = Some(public_key.clone());

        self.set_state(BridgeState::PreparingRepo).await;
        let provisioner = RepoProvisioner::new(&self.api);
        let repo = match provisioner
            .ensure(
                &token,
                &settings.repo_name,
                &settings.repo_namespace,
                !manual_creation_pending,
            )
            .await
        {
            Ok(repo) => repo,
            Err(RepoProvisionError::ManualCreationRequired {
                namespace,
                slug,
                url,
            }) => {
                let error = RepoProvisionError::ManualCreationRequired {
                    namespace,
                    slug: slug.clone(),
                    url: url.clone(),
                };
                {
                    let mut runtime = self.runtime.lock().await;
                    runtime.repo_slug = Some(slug.clone());
                    runtime.repo_creation_url = Some(url);
                    runtime.error = Some(if manual_creation_pending {
                        format!(
                            "尚未检测到仓库 {slug}。请确认已在 CNB 创建该私有空仓库，然后再次点击“重新检查仓库”。"
                        )
                    } else {
                        error.to_string()
                    });
                }
                self.logger
                    .log("warn", format!("仓库需要手动创建: {error}"));
                self.set_state(BridgeState::NeedsRepoCreation).await;
                return Err(BridgeError::RepoProvision(error));
            }
            Err(error) => {
                self.logger.log("error", format!("仓库准备失败: {error}"));
                self.set_error(BridgeState::Failed, error.to_string()).await;
                return Err(BridgeError::RepoProvision(error));
            }
        };
        {
            let mut runtime = self.runtime.lock().await;
            runtime.repo_slug = Some(repo.slug.clone());
            runtime.repo_creation_url = None;
            runtime.error = None;
        }
        self.logger.log(
            "info",
            format!("bridge repository ready: {} ({})", repo.slug, repo.branch),
        );

        self.set_state(BridgeState::StartingWorkspace).await;
        let manager = WorkspaceManager::new(&self.api);
        let session = manager
            .ensure_running(&token, &repo.slug, &repo.branch)
            .await
            .map_err(BridgeError::Api)?;
        self.runtime.lock().await.workspace_sn = Some(session.sn.clone());

        let remote_ssh = session
            .detail
            .remote_ssh
            .clone()
            .ok_or_else(|| BridgeError::Other("CNB 未返回 SSH 地址".to_string()))?;
        self.set_state(BridgeState::Connecting).await;
        let private_key = self.keys.private_key_text().map_err(BridgeError::Other)?;
        let tunnel = match start_tunnel_with_logger(
            &remote_ssh,
            &private_key,
            0,
            Some(&self.logger),
        )
        .await
        {
            Ok(tunnel) => tunnel,
            Err(error) => {
                self.logger
                    .log("error", format!("SSH 隧道建立失败: {error}"));
                let needs_key = error.contains("公钥")
                    || error.contains("认证")
                    || error.to_ascii_lowercase().contains("auth");
                self.runtime.lock().await.error = Some(error.clone());
                self.set_state(if needs_key {
                    BridgeState::NeedsSshKey
                } else {
                    BridgeState::Failed
                })
                .await;
                return Err(BridgeError::Other(error));
            }
        };
        let tunnel_port = tunnel.local_port();
        self.runtime.lock().await.tunnel = Some(tunnel);
        self.runtime.lock().await.detail = Some(session.detail);

        if let Err(error) = wait_for_health(tunnel_port).await {
            self.set_error(BridgeState::Failed, error.clone()).await;
            return Err(BridgeError::Other(error));
        }
        self.logger.log("info", "AI bridge is ready");
        self.set_state(BridgeState::Ready).await;
        Ok(self.snapshot().await)
    }

    /// Stops the tunnel and the cloud workspace.
    pub async fn stop(&self) -> BridgeSnapshot {
        let _transition = self.transition.lock().await;
        self.stop_locked().await
    }

    async fn stop_locked(&self) -> BridgeSnapshot {
        self.set_state(BridgeState::Stopping).await;
        let (tunnel, sn) = {
            let mut runtime = self.runtime.lock().await;
            (runtime.tunnel.take(), runtime.workspace_sn.clone())
        };
        if let Some(tunnel) = tunnel {
            tunnel.stop().await;
        }
        if let Some(sn) = sn {
            match self.account.access_token().await {
                Ok(token) => {
                    let manager = WorkspaceManager::new(&self.api);
                    if let Err(error) = manager.stop(&token, &sn).await {
                        self.logger
                            .log("warn", format!("停止工作空间失败: {error}"));
                    }
                }
                Err(error) => self
                    .logger
                    .log("warn", format!("关停时无法刷新登录: {error}")),
            }
        }
        let connected = self.is_account_connected();
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(if connected {
                BridgeState::Idle
            } else {
                BridgeState::SignedOut
            });
            runtime.workspace_sn = None;
            runtime.repo_creation_url = None;
        }
        self.snapshot().await
    }

    /// Starts the persistent local gateway.
    pub async fn start_gateway(self: &Arc<Self>, port: u16) -> Result<(), BridgeError> {
        let gateway = match GatewayHandle::start(Arc::clone(self), GatewayConfig::new(port)).await {
            Ok(gateway) => gateway,
            Err(error) => {
                self.set_error(BridgeState::Failed, error.clone()).await;
                return Err(BridgeError::Other(error));
            }
        };
        let gateway_port = gateway.port();
        *self.gateway.lock().await = Some(gateway);
        let mut runtime = self.runtime.lock().await;
        runtime.gateway_port = Some(gateway_port);
        runtime.state = Some(if runtime.tunnel.is_some() {
            BridgeState::Ready
        } else if self.is_account_connected() {
            BridgeState::Idle
        } else {
            BridgeState::SignedOut
        });
        runtime.error = None;
        Ok(())
    }

    /// Rebinds the local gateway when no requests are active.
    pub async fn rebind_gateway(self: &Arc<Self>, port: u16) -> Result<(), BridgeError> {
        let current = self.gateway.lock().await.take();
        if let Some(gateway) = current {
            if gateway.active_connections() != 0 {
                *self.gateway.lock().await = Some(gateway);
                return Err(BridgeError::Other(
                    "本地端点仍有活跃请求，请等待请求结束后再修改端口".to_string(),
                ));
            }
            gateway.stop().await;
        }
        self.start_gateway(port).await
    }

    /// Closes only local resources; the remote workspace is reclaimed by CNB.
    pub async fn shutdown_local(&self) {
        let gateway = self.gateway.lock().await.take();
        if let Some(gateway) = gateway {
            gateway.stop().await;
        }
        let tunnel = self.runtime.lock().await.tunnel.take();
        if let Some(tunnel) = tunnel {
            tunnel.stop().await;
        }
    }

    /// Closes an idle tunnel while keeping the workspace alive.
    pub async fn sleep_tunnel(&self) -> bool {
        let Ok(_transition) = self.transition.try_lock() else {
            return false;
        };
        if self.current_state().await != BridgeState::Ready {
            return false;
        }
        let tunnel = self.runtime.lock().await.tunnel.take();
        let Some(tunnel) = tunnel else {
            return false;
        };
        tunnel.stop().await;
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(BridgeState::Idle);
            runtime.error = None;
        }
        self.logger
            .log("info", "SSH 隧道空闲 5 分钟，已关闭并等待 CNB 自然回收");
        true
    }

    /// Invalidates a stale tunnel so the next request activates a new one.
    pub async fn invalidate_tunnel(&self, port: u16, reason: String) {
        let _transition = self.transition.lock().await;
        let tunnel = {
            let mut runtime = self.runtime.lock().await;
            if runtime
                .tunnel
                .as_ref()
                .is_some_and(|tunnel| tunnel.local_port() == port)
            {
                runtime.tunnel.take()
            } else {
                None
            }
        };
        if let Some(tunnel) = tunnel {
            tunnel.stop().await;
        }
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(BridgeState::Idle);
            runtime.error = Some(reason.clone());
        }
        self.logger.log(
            "warn",
            format!("CNB 隧道已失效，下一次请求将重新连接: {reason}"),
        );
    }

    /// Refreshes the UI state after account binding completes.
    pub async fn account_connected(&self) {
        let mut runtime = self.runtime.lock().await;
        if runtime.gateway_port.is_some() {
            runtime.state = Some(BridgeState::Idle);
        }
        runtime.error = None;
    }

    /// Clears the current bridge error without changing its lifecycle state.
    pub async fn dismiss_error(&self) {
        self.runtime.lock().await.error = None;
    }

    /// Refreshes the UI state after credentials are removed.
    pub async fn account_signed_out(&self) {
        let mut runtime = self.runtime.lock().await;
        runtime.state = Some(BridgeState::SignedOut);
        runtime.username = None;
        runtime.repo_slug = None;
        runtime.workspace_sn = None;
        runtime.error = None;
    }

    /// Returns the current UI snapshot.
    pub async fn snapshot(&self) -> BridgeSnapshot {
        let settings = self.load_settings().unwrap_or_default();
        let runtime = self.runtime.lock().await;
        let state = runtime.state.unwrap_or(BridgeState::SignedOut);
        BridgeSnapshot {
            state,
            account: self.account.status(),
            username: runtime.username.clone(),
            repo_slug: runtime.repo_slug.clone(),
            workspace_sn: runtime.workspace_sn.clone(),
            local_port: settings.local_port,
            base_url: runtime
                .gateway_port
                .filter(|_| self.is_account_connected())
                .map(|port| format!("http://127.0.0.1:{port}/v1")),
            ssh_public_key: runtime.ssh_public_key.clone(),
            repo_creation_url: runtime.repo_creation_url.clone(),
            error: runtime.error.clone(),
        }
    }

    /// Returns the dedicated public key, creating a key pair when needed.
    pub fn ensure_ssh_key(&self) -> Result<String, String> {
        let key = self.keys.ensure()?;
        if let Ok(mut runtime) = self.runtime.try_lock() {
            runtime.ssh_public_key = Some(key.clone());
        }
        Ok(key)
    }

    pub(crate) fn logger_handle(&self) -> Arc<Logger> {
        Arc::clone(&self.logger)
    }

    fn load_settings(&self) -> Result<crate::settings::Settings, String> {
        let mut settings = self.settings.load()?;
        if let Some(account_settings) = self.account.active_bridge_settings() {
            settings.repo_namespace = account_settings.repo_namespace;
            settings.repo_name = account_settings.repo_name;
        }
        Ok(settings)
    }

    pub(crate) fn http_log_filter(&self) -> crate::settings::HttpLogFilter {
        self.settings
            .load()
            .map(|settings| settings.http_log_filter)
            .unwrap_or_default()
    }

    /// Regenerates the dedicated key pair.
    pub async fn regenerate_ssh_key(&self) -> Result<String, String> {
        let key = self.keys.regenerate()?;
        self.runtime.lock().await.ssh_public_key = Some(key.clone());
        Ok(key)
    }

    /// Returns the CNB namespaces in which the current user can create repos.
    pub async fn list_namespaces(&self) -> Result<Vec<Namespace>, BridgeError> {
        let token = self.account.access_token().await?;
        Ok(self.api.list_owned_namespaces(&token).await?)
    }

    /// Ensures the current account owns a root organization.
    ///
    /// The account must already be connected. This is a control-plane-only
    /// operation and never starts a workspace.
    pub async fn ensure_account_organization(&self) -> Result<(), BridgeError> {
        let _organization_transition = self.organization_transition.lock().await;
        let token = self.account.access_token().await?;
        let user = self.api.current_user(&token).await?;
        if let Err(error) = self
            .account
            .update_profile_nickname(&user.username, Some(&user.nickname))
        {
            self.logger
                .log("warn", format!("保存 CNB 昵称失败: {error}"));
        }
        self.ensure_organization_for(&token, &user.username).await
    }

    async fn ensure_organization_for(
        &self,
        token: &str,
        username: &str,
    ) -> Result<(), BridgeError> {
        let namespaces = self.api.list_owned_namespaces(token).await?;
        if !namespaces.is_empty() {
            self.account.set_needs_organization(false);
            return Ok(());
        }
        self.account.set_needs_organization(true);
        let username = username.trim();
        if username.is_empty() {
            return Err(BridgeError::Other(
                "CNB 未返回账号名，无法自动创建根组织".to_string(),
            ));
        }
        self.logger
            .log("info", format!("开始为 CNB 账号 {username} 自动创建根组织"));
        self.api
            .ensure_organization(
                token,
                username,
                "CNB Bridge",
                "CNB Bridge root organization",
            )
            .await?;
        self.account.set_needs_organization(false);
        self.logger
            .log("info", format!("CNB 根组织 {username} 已就绪"));
        Ok(())
    }

    async fn set_state(&self, state: BridgeState) {
        let mut runtime = self.runtime.lock().await;
        runtime.state = Some(state);
    }

    async fn set_error(&self, state: BridgeState, error: String) {
        let mut runtime = self.runtime.lock().await;
        runtime.state = Some(state);
        runtime.error = Some(error);
        runtime.repo_creation_url = None;
    }

    async fn current_state(&self) -> BridgeState {
        self.runtime
            .lock()
            .await
            .state
            .unwrap_or(BridgeState::SignedOut)
    }

    fn is_account_connected(&self) -> bool {
        self.account.status().state == crate::account::AccountState::Connected
    }
}

async fn wait_for_health(port: u16) -> Result<(), String> {
    for _ in 0..30 {
        if check_health_once(port).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Err(format!(
        "agent 健康检查超时：请确认工作空间内 agent 已启动并监听 {DEFAULT_TUNNEL_PORT}"
    ))
}

async fn check_health_once(port: u16) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|error| error.to_string())?;
    let url = format!("http://127.0.0.1:{port}/healthz");
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|error| format!("本机端点健康检查失败: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("本机端点健康检查返回 HTTP {}", response.status()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{AccountBridgeSettings, AccountStore};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_bridge(temp: &tempfile::TempDir) -> Bridge {
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        )
    }

    #[tokio::test]
    async fn dismissing_a_bridge_error_preserves_the_bridge_state() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        bridge
            .set_error(BridgeState::Failed, "扫码窗口已关闭".to_string())
            .await;

        bridge.dismiss_error().await;

        let snapshot = bridge.snapshot().await;
        assert_eq!(snapshot.state, BridgeState::Failed);
        assert_eq!(snapshot.error, None);
    }

    #[test]
    fn serializes_repo_creation_state_for_the_ui() {
        assert_eq!(
            serde_json::to_string(&BridgeState::NeedsRepoCreation).unwrap(),
            r#""needsRepoCreation""#
        );
        assert_eq!(
            serde_json::to_string(&BridgeState::Idle).unwrap(),
            r#""idle""#
        );
    }

    #[test]
    fn effective_settings_use_the_active_account_bridge_settings() {
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        account
            .complete_token_login(
                "token",
                Some("alice".to_string()),
                None,
                None,
                Some(AccountBridgeSettings {
                    repo_namespace: "alice-team".to_string(),
                    repo_name: "alice-bridge".to_string(),
                }),
            )
            .unwrap();
        let settings_store = SettingsStore::new(temp.path().join("settings.json"));
        settings_store
            .save(&crate::settings::Settings {
                repo_namespace: "global-team".to_string(),
                repo_name: "global-bridge".to_string(),
                ..crate::settings::Settings::default()
            })
            .unwrap();
        let bridge = Bridge::new(
            account,
            settings_store,
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        );

        let settings = bridge.load_settings().unwrap();

        assert_eq!(settings.repo_namespace, "alice-team");
        assert_eq!(settings.repo_name, "alice-bridge");
    }

    #[tokio::test]
    async fn idle_tunnel_sleep_is_a_noop_outside_ready_state() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        bridge.runtime.lock().await.state = Some(BridgeState::Idle);

        assert!(!bridge.sleep_tunnel().await);
    }

    #[test]
    fn http_log_filter_changes_take_effect_without_restarting_the_gateway() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        let store = SettingsStore::new(temp.path().join("settings.json"));
        store.save(&crate::settings::Settings::default()).unwrap();
        assert_eq!(
            bridge.http_log_filter(),
            crate::settings::HttpLogFilter::Abnormal
        );

        store
            .save(&crate::settings::Settings {
                http_log_filter: crate::settings::HttpLogFilter::All,
                ..crate::settings::Settings::default()
            })
            .unwrap();
        assert_eq!(
            bridge.http_log_filter(),
            crate::settings::HttpLogFilter::All
        );
    }

    #[tokio::test]
    async fn automatically_creates_a_missing_owner_organization() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "username": "cnb.new-user",
                "nickname": "新用户"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/groups"))
            .and(body_json(serde_json::json!({
                "path": "cnb.new-user",
                "remark": "CNB Bridge",
                "description": "CNB Bridge root organization"
            })))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "cnb.new-user",
                    "path": "cnb.new-user",
                    "access_role": "Owner"
                }
            ])))
            .mount(&server)
            .await;

        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        account
            .complete_token_login("token", Some("cnb.new-user".to_string()), None, None, None)
            .unwrap();
        account.set_needs_organization(true);
        let mut bridge = Bridge::new(
            Arc::clone(&account),
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        );
        bridge.api = CnbApi::with_base_url(server.uri());

        bridge.ensure_account_organization().await.unwrap();

        assert!(!account.status().needs_organization);
        assert_eq!(account.status().display_name.as_deref(), Some("新用户"));
    }
}
