//! End-to-end bridge state machine.

use crate::account::{AccountError, AccountManager, AccountStatus};
use crate::api::{ApiError, CnbApi, Namespace, WorkspaceDetail};
use crate::logging::Logger;
use crate::settings::SettingsStore;
use crate::ssh_key::SshKeyStore;
use crate::tunnel::{start_tunnel, TunnelHandle, DEFAULT_TUNNEL_PORT};
use crate::workspace::{RepoProvisionError, RepoProvisioner, WorkspaceManager};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant};

/// High-level bridge state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum BridgeState {
    #[default]
    SignedOut,
    SigningIn,
    PreparingKey,
    PreparingRepo,
    StartingWorkspace,
    Connecting,
    Reconnecting,
    Ready,
    NeedsSshKey,
    NeedsRepoCreation,
    Stopping,
    Failed,
}

fn recovery_retry_delay(failed_attempts: u32) -> std::time::Duration {
    let seconds = match failed_attempts {
        0 | 1 => 5,
        2 => 15,
        3 => 30,
        _ => 60,
    };
    std::time::Duration::from_secs(seconds)
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
    recovery_cancelled: AtomicBool,
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
            recovery_cancelled: AtomicBool::new(false),
        }
    }

    /// Starts the complete bridge: login → key → repo → workspace → SSH → health.
    pub async fn start(&self) -> Result<BridgeSnapshot, BridgeError> {
        let Ok(_transition) = self.transition.try_lock() else {
            return Ok(self.snapshot().await);
        };
        self.recovery_cancelled.store(false, Ordering::SeqCst);
        let already_ready = {
            let mut runtime = self.runtime.lock().await;
            if runtime.state == Some(BridgeState::Ready) {
                true
            } else {
                runtime.state = Some(BridgeState::PreparingKey);
                runtime.error = None;
                false
            }
        };
        if already_ready {
            return Ok(self.snapshot().await);
        }
        self.start_locked().await
    }

    async fn start_locked(&self) -> Result<BridgeSnapshot, BridgeError> {
        let settings = match self.settings.load() {
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
        self.runtime.lock().await.username = Some(user.username.clone());

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
        let tunnel = match start_tunnel(&remote_ssh, &private_key, settings.local_port).await {
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
        self.runtime.lock().await.tunnel = Some(tunnel);
        self.runtime.lock().await.detail = Some(session.detail);

        if let Err(error) = wait_for_health(settings.local_port).await {
            self.set_error(BridgeState::Failed, error.clone()).await;
            return Err(BridgeError::Other(error));
        }
        self.logger.log("info", "AI bridge is ready");
        self.set_state(BridgeState::Ready).await;
        Ok(self.snapshot().await)
    }

    /// Stops the tunnel and the cloud workspace.
    pub async fn stop(&self) -> BridgeSnapshot {
        self.recovery_cancelled.store(true, Ordering::SeqCst);
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
        self.set_state(BridgeState::SignedOut).await;
        self.runtime.lock().await.repo_creation_url = None;
        self.snapshot().await
    }

    /// Supervises a ready bridge and automatically recovers a closed workspace.
    pub async fn supervise(&self) {
        let mut consecutive_health_failures = 0_u8;
        let mut failed_recovery_attempts = 0_u32;
        let mut next_health_check = Instant::now();
        let mut next_recovery_attempt = Instant::now();

        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if self.recovery_cancelled.load(Ordering::SeqCst) {
                consecutive_health_failures = 0;
                failed_recovery_attempts = 0;
                continue;
            }

            match self.current_state().await {
                BridgeState::Ready => {
                    failed_recovery_attempts = 0;
                    if let Some(reason) = self.tunnel_failure().await {
                        self.logger
                            .log("warn", format!("本机隧道已失效，准备自动恢复: {reason}"));
                        match self.recover_once(Some(reason)).await {
                            Ok(_) => {
                                consecutive_health_failures = 0;
                                next_health_check = Instant::now() + Duration::from_secs(10);
                            }
                            Err(error) => {
                                failed_recovery_attempts = 1;
                                next_recovery_attempt =
                                    Instant::now() + recovery_retry_delay(failed_recovery_attempts);
                                self.logger.log(
                                    "warn",
                                    format!(
                                        "自动恢复第 {failed_recovery_attempts} 次失败: {error}"
                                    ),
                                );
                            }
                        }
                    } else if Instant::now() >= next_health_check {
                        next_health_check = Instant::now() + Duration::from_secs(10);
                        let settings = self.settings.load().unwrap_or_default();
                        match check_health_once(settings.local_port).await {
                            Ok(()) => consecutive_health_failures = 0,
                            Err(error) => {
                                consecutive_health_failures =
                                    consecutive_health_failures.saturating_add(1);
                                if consecutive_health_failures >= 2 {
                                    self.logger.log(
                                        "warn",
                                        format!("连续两次健康检查失败，准备自动恢复: {error}"),
                                    );
                                    match self.recover_once(Some(error)).await {
                                        Ok(_) => {
                                            consecutive_health_failures = 0;
                                            next_health_check =
                                                Instant::now() + Duration::from_secs(10);
                                        }
                                        Err(error) => {
                                            failed_recovery_attempts = 1;
                                            next_recovery_attempt = Instant::now()
                                                + recovery_retry_delay(failed_recovery_attempts);
                                            self.logger.log(
                                                "warn",
                                                format!(
                                                    "自动恢复第 {failed_recovery_attempts} 次失败: {error}"
                                                ),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                BridgeState::Reconnecting => {
                    if Instant::now() >= next_recovery_attempt {
                        match self.recover_once(None).await {
                            Ok(_) => {
                                consecutive_health_failures = 0;
                                failed_recovery_attempts = 0;
                                next_health_check = Instant::now() + Duration::from_secs(10);
                                self.logger.log("info", "桥接自动恢复成功");
                            }
                            Err(error) => {
                                failed_recovery_attempts =
                                    failed_recovery_attempts.saturating_add(1);
                                let delay = recovery_retry_delay(failed_recovery_attempts);
                                next_recovery_attempt = Instant::now() + delay;
                                self.logger.log(
                                    "warn",
                                    format!(
                                        "自动恢复第 {failed_recovery_attempts} 次失败，{} 秒后重试: {error}",
                                        delay.as_secs()
                                    ),
                                );
                            }
                        }
                    }
                }
                _ => {
                    consecutive_health_failures = 0;
                    failed_recovery_attempts = 0;
                    next_health_check = Instant::now() + Duration::from_secs(10);
                    next_recovery_attempt = Instant::now();
                }
            }
        }
    }

    /// Returns the current UI snapshot.
    pub async fn snapshot(&self) -> BridgeSnapshot {
        let settings = self.settings.load().unwrap_or_default();
        let runtime = self.runtime.lock().await;
        let state = runtime.state.unwrap_or(BridgeState::SignedOut);
        BridgeSnapshot {
            state,
            account: self.account.status(),
            username: runtime.username.clone(),
            repo_slug: runtime.repo_slug.clone(),
            workspace_sn: runtime.workspace_sn.clone(),
            local_port: settings.local_port,
            base_url: (runtime.tunnel.is_some())
                .then(|| format!("http://127.0.0.1:{}/v1", settings.local_port)),
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

    async fn tunnel_failure(&self) -> Option<String> {
        self.runtime
            .lock()
            .await
            .tunnel
            .as_ref()
            .and_then(TunnelHandle::failure_reason)
    }

    async fn recover_once(&self, reason: Option<String>) -> Result<BridgeSnapshot, BridgeError> {
        let _transition = self.transition.lock().await;
        if self.recovery_cancelled.load(Ordering::SeqCst) {
            return Ok(self.snapshot().await);
        }
        if !matches!(
            self.current_state().await,
            BridgeState::Ready | BridgeState::Reconnecting
        ) {
            return Ok(self.snapshot().await);
        }

        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = Some(BridgeState::Reconnecting);
            if reason.is_some() {
                runtime.error = reason;
            }
        }

        let tunnel = self.runtime.lock().await.tunnel.take();
        if let Some(tunnel) = tunnel {
            tunnel.stop().await;
            self.logger.log("info", "旧本机隧道已关闭，端口已释放");
        }

        match self.start_locked().await {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                let next_state = if matches!(&error, BridgeError::Account(_)) {
                    BridgeState::SignedOut
                } else {
                    BridgeState::Reconnecting
                };
                let mut runtime = self.runtime.lock().await;
                runtime.state = Some(next_state);
                if runtime.error.is_none() {
                    runtime.error = Some(error.to_string());
                }
                Err(error)
            }
        }
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
        Err(format!(
            "本机端点健康检查返回 HTTP {}",
            response.status()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountStore;
    use std::time::Duration;

    fn test_bridge(temp: &tempfile::TempDir) -> Bridge {
        let account = AccountManager::with_store(AccountStore::new(
            temp.path().join("account.bin"),
        ));
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

    #[test]
    fn serializes_repo_creation_state_for_the_ui() {
        assert_eq!(
            serde_json::to_string(&BridgeState::NeedsRepoCreation).unwrap(),
            r#""needsRepoCreation""#
        );
        assert_eq!(
            serde_json::to_string(&BridgeState::Reconnecting).unwrap(),
            r#""reconnecting""#
        );
    }

    #[tokio::test]
    async fn start_returns_current_snapshot_when_already_ready() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        bridge.runtime.lock().await.state = Some(BridgeState::Ready);

        let snapshot = bridge.start().await.unwrap();

        assert_eq!(snapshot.state, BridgeState::Ready);
    }

    #[tokio::test]
    async fn start_is_admission_controlled_by_the_transition_lock() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        let _active_transition = bridge.transition.lock().await;

        let snapshot = bridge.start().await.unwrap();

        assert_eq!(snapshot.state, BridgeState::SignedOut);
    }

    #[test]
    fn recovery_backoff_is_capped_at_sixty_seconds() {
        assert_eq!(recovery_retry_delay(1), Duration::from_secs(5));
        assert_eq!(recovery_retry_delay(2), Duration::from_secs(15));
        assert_eq!(recovery_retry_delay(3), Duration::from_secs(30));
        assert_eq!(recovery_retry_delay(4), Duration::from_secs(60));
        assert_eq!(recovery_retry_delay(8), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn cancelled_recovery_returns_without_starting_network_work() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = test_bridge(&temp);
        bridge.runtime.lock().await.state = Some(BridgeState::Ready);
        bridge.recovery_cancelled.store(true, Ordering::SeqCst);

        let snapshot = bridge
            .recover_once(Some("workspace closed".to_string()))
            .await
            .unwrap();

        assert_eq!(snapshot.state, BridgeState::Ready);
    }
}
