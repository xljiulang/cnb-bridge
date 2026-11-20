//! End-to-end bridge state machine.

use crate::account::{AccountError, AccountManager, AccountStatus};
use crate::api::{ApiError, CnbApi, Namespace, WorkspaceDetail};
use crate::gateway::{GatewayConfig, GatewayHandle};
use crate::logging::Logger;
use crate::settings::{Settings, SettingsStore};
use crate::ssh_key::SshKeyStore;
use crate::tunnel::{
    is_ssh_key_authentication_rejection, start_tunnel_with_logger, TunnelHandle,
    DEFAULT_TUNNEL_PORT,
};
use crate::workspace::{RepoProvisionError, RepoProvisioner, WorkspaceManager, WorkspaceSession};
use serde::Serialize;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Instant;
use tokio::sync::{watch, Mutex};

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
    health_client: reqwest::Client,
    http_log_filter: AtomicU8,
    settings_cache: StdMutex<Settings>,
    runtime: Mutex<Runtime>,
    runtime_changes: watch::Sender<()>,
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
    manual_creation_pending: bool,
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
        let initial_settings = settings.load().unwrap_or_default();
        let http_log_filter = initial_settings.http_log_filter;
        let (runtime_changes, _) = watch::channel(());
        Self {
            api: CnbApi::new(),
            account,
            keys,
            settings,
            logger,
            health_client: reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .expect("static health-check client configuration is valid"),
            http_log_filter: AtomicU8::new(encode_http_log_filter(http_log_filter)),
            settings_cache: StdMutex::new(initial_settings),
            runtime: Mutex::new(Runtime::default()),
            runtime_changes,
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

    /// Subscribes to changes that may alter the UI bridge snapshot.
    pub fn subscribe_runtime_changes(&self) -> watch::Receiver<()> {
        self.runtime_changes.subscribe()
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
        self.notify_runtime_changed();
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
        let started = Instant::now();
        let mut phase_started = started;
        let (settings, settings_warning) = self.load_settings_with_fallback();
        if let Some(error) = settings_warning {
            self.logger
                .log("warn", format!("设置文件不可用，使用缓存: {error}"));
        }
        let manual_creation_pending = self.runtime.lock().await.manual_creation_pending;

        let token = match self.account.access_token().await {
            Ok(token) => token,
            Err(error) => {
                self.set_error(BridgeState::SignedOut, error.to_string())
                    .await;
                return Err(BridgeError::Account(error));
            }
        };
        if !manual_creation_pending {
            if let Some((repo_slug, session)) = self.try_reuse_workspace(&token).await {
                self.logger.log(
                    "info",
                    format!(
                        "bridge warm start reused workspace {} ({repo_slug})",
                        session.sn
                    ),
                );
                self.set_state(BridgeState::Connecting).await;
                self.spawn_warm_profile_refresh(token.clone());
                return self
                    .connect_workspace(session, started, &mut phase_started)
                    .await;
            }
        }

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
        log_phase(&self.logger, "account", &mut phase_started);

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
                    runtime.manual_creation_pending = true;
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
            runtime.manual_creation_pending = false;
            runtime.error = None;
        }
        self.logger.log(
            "info",
            format!("bridge repository ready: {} ({})", repo.slug, repo.branch),
        );
        log_phase(&self.logger, "repository", &mut phase_started);

        self.set_state(BridgeState::StartingWorkspace).await;
        let manager = WorkspaceManager::new(&self.api);
        let session = manager
            .ensure_running(&token, &repo.slug, &repo.branch)
            .await
            .map_err(BridgeError::Api)?;
        self.runtime.lock().await.workspace_sn = Some(session.sn.clone());
        log_phase(&self.logger, "workspace", &mut phase_started);
        self.connect_workspace(session, started, &mut phase_started)
            .await
    }

    async fn connect_workspace(
        &self,
        session: WorkspaceSession,
        started: Instant,
        phase_started: &mut Instant,
    ) -> Result<BridgeSnapshot, BridgeError> {
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
                self.runtime.lock().await.error = Some(error.clone());
                self.set_state(tunnel_failure_state(&error)).await;
                return Err(BridgeError::Other(error));
            }
        };
        let tunnel_port = tunnel.local_port();
        log_phase(&self.logger, "ssh", phase_started);
        let tunnel_failure = tunnel.failure_watch();
        self.runtime.lock().await.tunnel = Some(tunnel);
        self.runtime.lock().await.detail = Some(session.detail);

        if let Err(error) = wait_for_health(&self.health_client, tunnel_port, &tunnel_failure).await
        {
            self.fail_tunnel_health(error.clone()).await;
            return Err(BridgeError::Other(error));
        }
        log_phase(&self.logger, "agent_health", phase_started);
        self.logger.log(
            "info",
            format!(
                "AI bridge is ready; total={}ms",
                started.elapsed().as_millis()
            ),
        );
        self.set_state(BridgeState::Ready).await;
        Ok(self.snapshot().await)
    }

    async fn try_reuse_workspace(&self, token: &str) -> Option<(String, WorkspaceSession)> {
        if !self.is_account_connected() {
            return None;
        }
        let (repo_slug, workspace_sn) = {
            let runtime = self.runtime.lock().await;
            if runtime.manual_creation_pending {
                return None;
            }
            (runtime.repo_slug.clone()?, runtime.workspace_sn.clone()?)
        };
        match WorkspaceManager::new(&self.api)
            .reuse_running(token, &repo_slug, &workspace_sn)
            .await
        {
            Ok(session) => Some((repo_slug, session)),
            Err(error) => {
                self.logger.log(
                    "warn",
                    format!("温启动 workspace {workspace_sn} 不可复用，回退完整启动: {error}"),
                );
                None
            }
        }
    }

    fn spawn_warm_profile_refresh(&self, token: String) {
        let api = self.api.clone();
        let account = Arc::clone(&self.account);
        let logger = Arc::clone(&self.logger);
        let runtime_changes = self.runtime_changes.clone();
        tokio::spawn(async move {
            let mut changed = false;
            match api.current_user(&token).await {
                Ok(user) => {
                    match account.update_profile_nickname(&user.username, Some(&user.nickname)) {
                        Ok(_) => changed = true,
                        Err(error) => {
                            logger.log("warn", format!("后台刷新 CNB 昵称失败: {error}"));
                        }
                    }
                }
                Err(error) => {
                    logger.log("warn", format!("后台刷新 CNB 用户资料失败: {error}"));
                }
            }
            match api.list_owned_namespaces(&token).await {
                Ok(namespaces) => {
                    account.set_needs_organization(namespaces.is_empty());
                    changed = true;
                }
                Err(error) => {
                    logger.log("warn", format!("后台刷新 CNB 组织状态失败: {error}"));
                }
            }
            if changed {
                runtime_changes.send_replace(());
            }
        });
    }

    /// Disconnects the local SSH tunnel while preserving the cloud workspace.
    pub async fn disconnect(&self) -> BridgeSnapshot {
        let _transition = self.transition.lock().await;
        self.disconnect_locked().await
    }

    async fn disconnect_locked(&self) -> BridgeSnapshot {
        self.set_state(BridgeState::Stopping).await;
        let tunnel = self.runtime.lock().await.tunnel.take();
        if let Some(tunnel) = tunnel {
            tunnel.stop().await;
        }
        let connected = self.is_account_connected();
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(if connected {
                BridgeState::Idle
            } else {
                BridgeState::SignedOut
            });
            runtime.error = None;
        }
        self.notify_runtime_changed();
        self.snapshot().await
    }

    /// Starts the persistent local gateway.
    pub async fn start_gateway(self: &Arc<Self>, port: u16) -> Result<(), BridgeError> {
        let previous = self.gateway.lock().await.take();
        if let Some(previous) = previous {
            previous.stop().await;
        }
        let gateway = match GatewayHandle::start(Arc::clone(self), GatewayConfig::new(port)).await {
            Ok(gateway) => gateway,
            Err(error) => {
                self.logger
                    .log("error", format!("绑定本地网关失败: {error}"));
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
        drop(runtime);
        self.notify_runtime_changed();
        Ok(())
    }

    /// Rebinds the local gateway when no requests are active.
    pub async fn rebind_gateway(self: &Arc<Self>, port: u16) -> Result<(), BridgeError> {
        let current = self.gateway.lock().await.take();
        let previous_port = if let Some(gateway) = current {
            if gateway.active_connections() != 0 {
                *self.gateway.lock().await = Some(gateway);
                return Err(BridgeError::Other(
                    "本地端点仍有活跃请求，请等待请求结束后再修改端口".to_string(),
                ));
            }
            let previous_port = gateway.port();
            gateway.stop().await;
            previous_port
        } else {
            self.runtime
                .lock()
                .await
                .gateway_port
                .unwrap_or(crate::settings::DEFAULT_LOCAL_PORT)
        };
        match self.start_gateway(port).await {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Err(rollback) = self.start_gateway(previous_port).await {
                    self.set_error(
                        BridgeState::Failed,
                        format!(
                            "切换本地端口失败: {error}; 回滚端口 {previous_port} 失败: {rollback}"
                        ),
                    )
                    .await;
                }
                Err(error)
            }
        }
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

    /// Disconnects an idle SSH tunnel while preserving the cloud workspace.
    pub async fn disconnect_idle_tunnel(&self) -> Option<u16> {
        let Ok(_transition) = self.transition.try_lock() else {
            return None;
        };
        let tunnel_local_port = {
            let runtime = self.runtime.lock().await;
            if runtime.state != Some(BridgeState::Ready) {
                return None;
            }
            runtime.tunnel.as_ref()?.local_port()
        };
        self.disconnect_locked().await;
        Some(tunnel_local_port)
    }

    pub(crate) fn log_idle_disconnect(
        &self,
        tunnel_local_port: u16,
        idle_for: std::time::Duration,
        reason: &str,
        idle_timeout: std::time::Duration,
    ) {
        self.logger.log(
            "info",
            format!(
                "工作空间空闲，正在断开 SSH;timeout_ms={};idle_for_ms={};last_activity_reason={reason};tunnel_local_port={tunnel_local_port}",
                idle_timeout.as_millis(),
                idle_for.as_millis(),
            ),
        );
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
        let Some(tunnel) = tunnel else {
            return;
        };
        tunnel.stop().await;
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(BridgeState::Idle);
            runtime.error = Some(reason.clone());
        }
        self.notify_runtime_changed();
        self.logger.log("warn", reason);
    }

    /// Refreshes the UI state after account binding completes.
    pub async fn account_connected(&self) {
        let mut runtime = self.runtime.lock().await;
        if runtime.gateway_port.is_some() {
            runtime.state = Some(BridgeState::Idle);
        }
        runtime.error = None;
        drop(runtime);
        self.notify_runtime_changed();
    }

    /// Clears the current bridge error without changing its lifecycle state.
    pub async fn dismiss_error(&self) {
        self.runtime.lock().await.error = None;
        self.notify_runtime_changed();
    }

    /// Refreshes the UI state after credentials are removed.
    pub async fn account_signed_out(&self) {
        let mut runtime = self.runtime.lock().await;
        runtime.state = Some(BridgeState::SignedOut);
        runtime.username = None;
        runtime.repo_slug = None;
        runtime.workspace_sn = None;
        runtime.detail = None;
        runtime.error = None;
        runtime.repo_creation_url = None;
        runtime.manual_creation_pending = false;
        drop(runtime);
        self.notify_runtime_changed();
    }

    /// Returns the current UI snapshot.
    pub async fn snapshot(&self) -> BridgeSnapshot {
        let (settings, settings_warning) = self.load_settings_with_fallback();
        let mut runtime = self.runtime.lock().await;
        if runtime.error.is_none() {
            runtime.error = settings_warning;
        }
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

    pub(crate) async fn ready_tunnel_port(&self) -> Option<u16> {
        let runtime = self.runtime.lock().await;
        if runtime.state != Some(BridgeState::Ready) {
            return None;
        }
        let tunnel = runtime.tunnel.as_ref()?;
        if tunnel.failure_reason().is_some() {
            return None;
        }
        Some(tunnel.local_port())
    }

    #[cfg(test)]
    pub(crate) async fn install_test_ready_tunnel(&self, tunnel: TunnelHandle) {
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(BridgeState::Ready);
            runtime.tunnel = Some(tunnel);
            runtime.error = None;
        }
        self.notify_runtime_changed();
    }

    #[cfg(test)]
    pub(crate) async fn install_test_tunnel_state(&self, state: BridgeState, tunnel: TunnelHandle) {
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(state);
            runtime.tunnel = Some(tunnel);
            runtime.error = None;
        }
        self.notify_runtime_changed();
    }

    #[cfg(test)]
    fn load_settings(&self) -> Result<crate::settings::Settings, String> {
        let (settings, warning) = self.load_settings_with_fallback();
        match warning {
            Some(error) => Err(error),
            None => Ok(settings),
        }
    }

    fn load_settings_with_fallback(&self) -> (Settings, Option<String>) {
        let (mut settings, warning) = match self.settings.load() {
            Ok(settings) => {
                *self
                    .settings_cache
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = settings.clone();
                (settings, None)
            }
            Err(error) => (
                self.settings_cache
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone(),
                Some(error),
            ),
        };
        if let Some(account_settings) = self.account.active_bridge_settings() {
            settings.repo_namespace = account_settings.repo_namespace;
            settings.repo_name = account_settings.repo_name;
        }
        (settings, warning)
    }

    pub(crate) fn http_log_filter(&self) -> crate::settings::HttpLogFilter {
        decode_http_log_filter(self.http_log_filter.load(Ordering::Relaxed))
    }

    pub fn set_http_log_filter(&self, filter: crate::settings::HttpLogFilter) {
        self.http_log_filter
            .store(encode_http_log_filter(filter), Ordering::Relaxed);
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
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(state);
        }
        self.notify_runtime_changed();
    }

    async fn set_error(&self, state: BridgeState, error: String) {
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(state);
            runtime.error = Some(error);
        }
        self.notify_runtime_changed();
    }

    fn notify_runtime_changed(&self) {
        self.runtime_changes.send_replace(());
    }

    async fn fail_tunnel_health(&self, error: String) -> String {
        let tunnel = self.runtime.lock().await.tunnel.take();
        if let Some(tunnel) = tunnel {
            tunnel.stop().await;
        }
        self.set_error(BridgeState::Failed, error.clone()).await;
        error
    }

    fn is_account_connected(&self) -> bool {
        self.account.status().state == crate::account::AccountState::Connected
    }
}

fn tunnel_failure_state(error: &str) -> BridgeState {
    if is_ssh_key_authentication_rejection(error) {
        BridgeState::NeedsSshKey
    } else {
        BridgeState::Failed
    }
}

fn log_phase(logger: &Logger, phase: &str, phase_started: &mut Instant) {
    let now = Instant::now();
    logger.log(
        "info",
        format!(
            "bridge phase {phase} completed; elapsed={}ms",
            now.duration_since(*phase_started).as_millis()
        ),
    );
    *phase_started = now;
}

fn encode_http_log_filter(filter: crate::settings::HttpLogFilter) -> u8 {
    match filter {
        crate::settings::HttpLogFilter::Abnormal => 0,
        crate::settings::HttpLogFilter::All => 1,
    }
}

fn decode_http_log_filter(value: u8) -> crate::settings::HttpLogFilter {
    match value {
        1 => crate::settings::HttpLogFilter::All,
        _ => crate::settings::HttpLogFilter::Abnormal,
    }
}

async fn wait_for_health(
    client: &reqwest::Client,
    port: u16,
    failure: &tokio::sync::watch::Receiver<Option<String>>,
) -> Result<(), String> {
    for _ in 0..30 {
        if let Some(reason) = failure.borrow().clone() {
            return Err(reason);
        }
        if check_health_once(client, port).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Err(format!(
        "agent 健康检查超时：请确认工作空间内 agent 已启动并监听 {DEFAULT_TUNNEL_PORT}"
    ))
}

async fn check_health_once(client: &reqwest::Client, port: u16) -> Result<(), String> {
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
    async fn runtime_changes_notify_subscribers_without_blocking() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        let mut changes = bridge.subscribe_runtime_changes();

        bridge
            .set_error(BridgeState::Failed, "状态同步测试".to_string())
            .await;
        tokio::time::timeout(std::time::Duration::from_millis(100), changes.changed())
            .await
            .expect("state change notification timed out")
            .expect("runtime change channel closed");
        assert_eq!(bridge.snapshot().await.state, BridgeState::Failed);

        bridge.dismiss_error().await;
        tokio::time::timeout(std::time::Duration::from_millis(100), changes.changed())
            .await
            .expect("error change notification timed out")
            .expect("runtime change channel closed");
        assert_eq!(bridge.snapshot().await.error, None);
    }

    #[tokio::test]
    async fn warm_profile_refresh_notifies_runtime_subscribers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "username": "alice",
                "nickname": "新昵称"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        account
            .complete_token_login("token", Some("alice".to_string()), None, None, None)
            .unwrap();
        let mut bridge = Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        );
        bridge.api = CnbApi::with_base_url(server.uri());
        let mut changes = bridge.subscribe_runtime_changes();

        bridge.spawn_warm_profile_refresh("token".to_string());
        tokio::time::timeout(std::time::Duration::from_millis(500), changes.changed())
            .await
            .expect("profile refresh notification timed out")
            .expect("runtime change channel closed");

        let account = bridge.snapshot().await.account;
        assert_eq!(account.display_name.as_deref(), Some("新昵称"));
        assert!(account.needs_organization);
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
    fn only_explicit_key_rejection_requests_rekeying() {
        assert_eq!(
            tunnel_failure_state("SSH 公钥认证连接中断: Channel send error"),
            BridgeState::Failed
        );
        assert_eq!(
            tunnel_failure_state("SSH 公钥认证被拒绝：请重新扫码绑定以同步公钥"),
            BridgeState::NeedsSshKey
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
    async fn idle_disconnect_is_a_noop_outside_ready_state() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        bridge.runtime.lock().await.state = Some(BridgeState::Idle);

        assert!(bridge.disconnect_idle_tunnel().await.is_none());
    }

    #[tokio::test]
    async fn idle_disconnect_releases_tunnel_and_preserves_workspace_sn() {
        let server = MockServer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        account
            .complete_token_login("token", Some("alice".to_string()), None, None, None)
            .unwrap();
        let mut bridge = Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        );
        bridge.api = CnbApi::with_base_url(server.uri());
        let (tunnel, address) = TunnelHandle::test_handle().await;
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.state = Some(BridgeState::Ready);
            runtime.workspace_sn = Some("ws-idle".to_string());
            runtime.tunnel = Some(tunnel);
        }

        assert!(bridge.disconnect_idle_tunnel().await.is_some());

        let snapshot = bridge.snapshot().await;
        assert_eq!(snapshot.state, BridgeState::Idle);
        assert_eq!(snapshot.workspace_sn.as_deref(), Some("ws-idle"));
        assert!(tokio::net::TcpListener::bind(address).await.is_ok());
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn idle_disconnect_log_contains_threshold_activity_and_tunnel_port() {
        let temp = tempfile::tempdir().unwrap();
        let logger = Arc::new(Logger::new(16));
        let bridge = Bridge::new(
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin"))),
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::clone(&logger),
        );
        let (tunnel, address) = TunnelHandle::test_handle().await;
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.state = Some(BridgeState::Ready);
            runtime.tunnel = Some(tunnel);
        }

        let port = bridge.disconnect_idle_tunnel().await.unwrap();
        bridge.log_idle_disconnect(
            port,
            std::time::Duration::from_secs(301),
            "tunnel_ready",
            std::time::Duration::from_secs(300),
        );

        let message = logger
            .recent(4)
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(port, address.port());
        assert!(message.contains("timeout_ms=300000"));
        assert!(message.contains("idle_for_ms=301000"));
        assert!(message.contains("last_activity_reason=tunnel_ready"));
        assert!(message.contains(&format!("tunnel_local_port={port}")));
        assert!(!message.to_ascii_lowercase().contains("token"));
    }

    #[tokio::test]
    async fn disconnect_releases_tunnel_and_preserves_workspace_sn() {
        let server = MockServer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        account
            .complete_token_login("token", Some("alice".to_string()), None, None, None)
            .unwrap();
        let mut bridge = Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        );
        bridge.api = CnbApi::with_base_url(server.uri());
        let (tunnel, address) = TunnelHandle::test_handle().await;
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.state = Some(BridgeState::Ready);
            runtime.repo_slug = Some("team/repo".to_string());
            runtime.workspace_sn = Some("ws-stopped".to_string());
            runtime.tunnel = Some(tunnel);
        }

        let snapshot = bridge.disconnect().await;

        assert_eq!(snapshot.state, BridgeState::Idle);
        assert_eq!(snapshot.workspace_sn.as_deref(), Some("ws-stopped"));
        assert!(tokio::net::TcpListener::bind(address).await.is_ok());
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn signing_out_clears_the_previous_workspace_session_and_releases_tunnel() {
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        account
            .complete_token_login("token", Some("alice".to_string()), None, None, None)
            .unwrap();
        let bridge = Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        );
        let (tunnel, address) = TunnelHandle::test_handle().await;
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.state = Some(BridgeState::Ready);
            runtime.username = Some("alice".to_string());
            runtime.repo_slug = Some("alice/repo".to_string());
            runtime.workspace_sn = Some("ws-alice".to_string());
            runtime.detail = Some(crate::api::WorkspaceDetail {
                remote_ssh: Some("ssh.example.com".to_string()),
                ssh: None,
                webide: None,
                jump_url: None,
            });
            runtime.tunnel = Some(tunnel);
        }

        bridge.disconnect().await;
        bridge.account_signed_out().await;

        let snapshot = bridge.snapshot().await;
        assert_eq!(snapshot.state, BridgeState::SignedOut);
        assert_eq!(snapshot.username, None);
        assert_eq!(snapshot.repo_slug, None);
        assert_eq!(snapshot.workspace_sn, None);
        assert!(bridge.runtime.lock().await.detail.is_none());
        assert!(tokio::net::TcpListener::bind(address).await.is_ok());
    }

    #[tokio::test]
    async fn stale_tunnel_invalidation_does_not_change_current_state() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.state = Some(BridgeState::Ready);
            runtime.error = None;
        }

        bridge
            .invalidate_tunnel(65000, "stale request failed".to_string())
            .await;

        let snapshot = bridge.snapshot().await;
        assert_eq!(snapshot.state, BridgeState::Ready);
        assert!(snapshot.error.is_none());
    }

    #[tokio::test]
    async fn tunnel_invalidation_logs_the_actual_agent_connection_failure() {
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        let logger = Arc::new(Logger::new(32));
        let bridge = Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::clone(&logger),
        );
        let (tunnel, address) = TunnelHandle::test_handle().await;
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.state = Some(BridgeState::Ready);
            runtime.tunnel = Some(tunnel);
        }
        let reason = "本地 Gateway 到 Agent 连接失败，正在重建隧道: connection reset".to_string();

        bridge
            .invalidate_tunnel(address.port(), reason.clone())
            .await;

        let messages = logger
            .recent(8)
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(messages.contains(&reason));
        assert!(!messages.contains("CNB 隧道已失效"));
    }

    #[tokio::test]
    async fn health_failure_releases_the_active_tunnel() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        let (tunnel, address) = TunnelHandle::test_handle().await;
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.state = Some(BridgeState::Ready);
            runtime.tunnel = Some(tunnel);
        }

        let error = bridge
            .fail_tunnel_health("health check failed".to_string())
            .await;

        assert_eq!(error, "health check failed");
        assert!(tokio::net::TcpListener::bind(address).await.is_ok());
        let runtime = bridge.runtime.lock().await;
        assert!(runtime.tunnel.is_none());
        assert_eq!(runtime.state, Some(BridgeState::Failed));
        assert_eq!(runtime.error.as_deref(), Some("health check failed"));
    }

    #[tokio::test]
    async fn corrupt_settings_fall_back_to_last_known_values_and_report_error() {
        let temp = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(temp.path().join("settings.json"));
        store
            .save(&crate::settings::Settings {
                local_port: 12000,
                ..crate::settings::Settings::default()
            })
            .unwrap();
        let bridge = Bridge::new(
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin"))),
            store.clone(),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        );
        std::fs::write(store.path(), b"{broken").unwrap();

        let snapshot = bridge.snapshot().await;

        assert_eq!(snapshot.local_port, 12000);
        assert!(snapshot
            .error
            .as_deref()
            .is_some_and(|error| error.contains("解析设置")));
    }

    #[tokio::test]
    async fn later_errors_preserve_pending_manual_repository_creation() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.manual_creation_pending = true;
            runtime.repo_creation_url = Some("https://cnb.cool/new/repos".to_string());
        }

        bridge
            .set_error(BridgeState::Failed, "later failure".to_string())
            .await;

        let snapshot = bridge.snapshot().await;
        assert_eq!(
            snapshot.repo_creation_url.as_deref(),
            Some("https://cnb.cool/new/repos")
        );
        assert!(bridge.runtime.lock().await.manual_creation_pending);
    }

    #[tokio::test]
    async fn warm_context_reuses_only_the_known_workspace() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/team/repo/-/workspace/detail/ws-warm"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "remoteSsh": "cnb@example.test"
            })))
            .mount(&server)
            .await;
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        account
            .complete_token_login("token", Some("alice".to_string()), None, None, None)
            .unwrap();
        let mut bridge = Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::new(Logger::new(32)),
        );
        bridge.api = CnbApi::with_base_url(server.uri());
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.state = Some(BridgeState::Idle);
            runtime.repo_slug = Some("team/repo".to_string());
            runtime.workspace_sn = Some("ws-warm".to_string());
        }

        let reused = bridge.try_reuse_workspace("token").await.unwrap();

        assert_eq!(reused.0, "team/repo");
        assert_eq!(reused.1.sn, "ws-warm");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn manual_repository_state_disables_warm_reuse() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.repo_slug = Some("team/repo".to_string());
            runtime.workspace_sn = Some("ws-old".to_string());
            runtime.manual_creation_pending = true;
        }

        assert!(bridge.try_reuse_workspace("token").await.is_none());
    }

    #[tokio::test]
    async fn failed_warm_workspace_detail_falls_back_to_full_start() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/team/repo/-/workspace/detail/ws-stale"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "errmsg": "workspace not found"
            })))
            .mount(&server)
            .await;
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account.bin")));
        account
            .complete_token_login("token", Some("alice".to_string()), None, None, None)
            .unwrap();
        let logger = Arc::new(Logger::new(32));
        let mut bridge = Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::new(
                temp.path().join("id_ed25519"),
                temp.path().join("id_ed25519.pub"),
            ),
            Arc::clone(&logger),
        );
        bridge.api = CnbApi::with_base_url(server.uri());
        {
            let mut runtime = bridge.runtime.lock().await;
            runtime.repo_slug = Some("team/repo".to_string());
            runtime.workspace_sn = Some("ws-stale".to_string());
        }

        assert!(bridge.try_reuse_workspace("token").await.is_none());
        assert!(logger
            .recent(4)
            .iter()
            .any(|entry| entry.message.contains("回退完整启动")));
    }

    #[test]
    fn phase_logging_reports_elapsed_time_without_sensitive_values() {
        let logger = Logger::new(8);
        let mut phase_started = Instant::now() - std::time::Duration::from_millis(25);

        log_phase(&logger, "repository", &mut phase_started);

        let message = &logger.recent(1)[0].message;
        assert!(message.contains("bridge phase repository completed"));
        assert!(message.contains("elapsed="));
        assert!(!message.to_ascii_lowercase().contains("token"));
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

        bridge.set_http_log_filter(crate::settings::HttpLogFilter::All);
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
