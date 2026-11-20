use super::*;

/// Gateway timing parameters.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub port: u16,
    pub activation_timeout: Duration,
    pub idle_timeout: Duration,
    pub upstream_connect_timeout: Duration,
    pub upstream_read_timeout: Duration,
    pub non_stream_timeout: Duration,
}

impl GatewayConfig {
    /// Production gateway settings.
    pub fn new(port: u16) -> Self {
        Self {
            port,
            activation_timeout: Duration::from_secs(180),
            idle_timeout: Duration::from_secs(5 * 60),
            upstream_connect_timeout: Duration::from_secs(5),
            upstream_read_timeout: Duration::from_secs(60),
            non_stream_timeout: Duration::from_secs(120),
        }
    }
}

pub(super) fn gateway_upstream_client(config: &GatewayConfig) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(config.upstream_connect_timeout)
        .read_timeout(config.upstream_read_timeout)
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|error| format!("创建本地 HTTP 客户端失败: {error}"))
}

pub(super) fn should_disconnect_idle_tunnel(
    active_connections: usize,
    idle_for: Duration,
    idle_timeout: Duration,
) -> bool {
    active_connections == 0 && idle_for >= idle_timeout
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActivityReason {
    GatewayStarted,
    TunnelReady,
    RequestStarted,
    RequestFinished,
    IdleDisconnectCompleted,
}

impl ActivityReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::GatewayStarted => "gateway_started",
            Self::TunnelReady => "tunnel_ready",
            Self::RequestStarted => "request_started",
            Self::RequestFinished => "request_finished",
            Self::IdleDisconnectCompleted => "idle_disconnect_completed",
        }
    }
}

pub(super) struct GatewayActivity {
    active_connections: AtomicUsize,
    last_activity: Mutex<Instant>,
    last_activity_reason: Mutex<ActivityReason>,
    tunnel_local_port: Mutex<Option<u16>>,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
    request_gate: Arc<tokio::sync::RwLock<()>>,
}

impl GatewayActivity {
    pub(super) fn new() -> Self {
        Self::with_clock(Instant::now)
    }

    pub(super) fn with_clock(clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        let now = clock();
        Self {
            active_connections: AtomicUsize::new(0),
            last_activity: Mutex::new(now),
            last_activity_reason: Mutex::new(ActivityReason::GatewayStarted),
            tunnel_local_port: Mutex::new(None),
            clock: Box::new(clock),
            request_gate: Arc::new(tokio::sync::RwLock::new(())),
        }
    }

    pub(super) async fn enter(self: &Arc<Self>) -> ActivityGuard {
        self.active_connections.fetch_add(1, Ordering::SeqCst);
        self.touch(ActivityReason::RequestStarted);
        let mut guard = ActivityGuard {
            activity: Arc::clone(self),
            _lease: None,
        };
        guard._lease = Some(Arc::clone(&self.request_gate).read_owned().await);
        guard
    }

    fn touch(&self, reason: ActivityReason) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = (self.clock)();
        *self
            .last_activity_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = reason;
    }

    pub(super) fn tunnel_ready(&self, local_port: u16) {
        let mut known_port = self
            .tunnel_local_port
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *known_port == Some(local_port) {
            return;
        }
        *known_port = Some(local_port);
        drop(known_port);
        self.touch(ActivityReason::TunnelReady);
    }

    pub(super) fn last_activity_reason(&self) -> ActivityReason {
        *self
            .last_activity_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::SeqCst)
    }

    pub(super) fn idle_for(&self) -> Duration {
        let last_activity = *self
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (self.clock)().saturating_duration_since(last_activity)
    }

    pub(super) fn try_claim_idle_disconnect(
        self: &Arc<Self>,
        idle_timeout: Duration,
    ) -> Option<IdleDisconnectGuard> {
        let lease = Arc::clone(&self.request_gate).try_write_owned().ok()?;
        should_disconnect_idle_tunnel(self.active_connections(), self.idle_for(), idle_timeout)
            .then_some(IdleDisconnectGuard { _lease: lease })
    }
}

pub(super) struct ActivityGuard {
    activity: Arc<GatewayActivity>,
    _lease: Option<tokio::sync::OwnedRwLockReadGuard<()>>,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.activity
            .active_connections
            .fetch_sub(1, Ordering::SeqCst);
        self.activity.touch(ActivityReason::RequestFinished);
    }
}

pub(super) struct IdleDisconnectGuard {
    _lease: tokio::sync::OwnedRwLockWriteGuard<()>,
}

/// Handle owning one gateway listener and its monitoring task.
pub struct GatewayHandle {
    port: u16,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    trace_cleanup_task: tokio::task::JoinHandle<()>,
    ready_task: tokio::task::JoinHandle<()>,
    activity: Arc<GatewayActivity>,
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(super) struct IdleMonitorProbe {
    pub(super) disconnect_started: Arc<tokio::sync::Notify>,
    pub(super) disconnect_finished: Arc<tokio::sync::Notify>,
}

impl GatewayHandle {
    /// Binds the configured loopback port and starts serving requests.
    pub async fn start(bridge: Arc<Bridge>, config: GatewayConfig) -> Result<Self, String> {
        Self::start_with_upstream_internal(
            bridge,
            config,
            None,
            crate::paths::logs_dir().join("http-trace"),
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn start_with_upstream(
        bridge: Arc<Bridge>,
        config: GatewayConfig,
        upstream_port: u16,
        trace_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        Self::start_with_upstream_internal(bridge, config, Some(upstream_port), trace_root).await
    }

    pub(super) async fn start_with_upstream_internal(
        bridge: Arc<Bridge>,
        config: GatewayConfig,
        upstream_override: Option<u16>,
        trace_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        #[cfg(test)]
        return Self::start_with_upstream_internal_impl(
            bridge,
            config,
            upstream_override,
            trace_root,
            IdleMonitorProbe::default(),
            Instant::now,
        )
        .await;
        #[cfg(not(test))]
        Self::start_with_upstream_internal_impl(bridge, config, upstream_override, trace_root).await
    }

    #[cfg(test)]
    pub(super) async fn start_with_upstream_internal_impl(
        bridge: Arc<Bridge>,
        config: GatewayConfig,
        upstream_override: Option<u16>,
        trace_root: std::path::PathBuf,
        idle_monitor_probe: IdleMonitorProbe,
        clock: impl Fn() -> Instant + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::start_with_upstream_internal_common(
            bridge,
            config,
            upstream_override,
            trace_root,
            idle_monitor_probe,
            clock,
        )
        .await
    }

    #[cfg(not(test))]
    async fn start_with_upstream_internal_impl(
        bridge: Arc<Bridge>,
        config: GatewayConfig,
        upstream_override: Option<u16>,
        trace_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        Self::start_with_upstream_internal_common(bridge, config, upstream_override, trace_root)
            .await
    }

    async fn start_with_upstream_internal_common(
        bridge: Arc<Bridge>,
        config: GatewayConfig,
        upstream_override: Option<u16>,
        trace_root: std::path::PathBuf,
        #[cfg(test)] idle_monitor_probe: IdleMonitorProbe,
        #[cfg(test)] clock: impl Fn() -> Instant + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", config.port))
            .await
            .map_err(|error| format!("绑定本地网关端口 {} 失败: {error}", config.port))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("读取本地网关端口失败: {error}"))?
            .port();
        let (shutdown, _) = watch::channel(false);
        #[cfg(test)]
        let activity = Arc::new(GatewayActivity::with_clock(clock));
        #[cfg(not(test))]
        let activity = Arc::new(GatewayActivity::new());
        let state = GatewayState {
            bridge: Arc::clone(&bridge),
            activity: Arc::clone(&activity),
            models: Arc::new(ModelResponses::from_catalog(model_catalog::catalog()?)?),
            activation_timeout: config.activation_timeout,
            non_stream_timeout: config.non_stream_timeout,
            client: gateway_upstream_client(&config)?,
            upstream_override,
            trace_store: HttpTraceStore::new(trace_root),
            logger: bridge.logger_handle(),
        };
        let trace_cleanup_store = state.trace_store.clone();
        let app = router(state);
        let mut shutdown_rx = shutdown.subscribe();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    while !*shutdown_rx.borrow() {
                        if shutdown_rx.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await;
        });

        let idle_bridge = Arc::downgrade(&bridge);
        let idle_activity = Arc::clone(&activity);
        let idle_timeout = config.idle_timeout;
        let mut idle_shutdown = shutdown.subscribe();
        let mut runtime_changes = bridge.subscribe_runtime_changes();
        let bridge_for_ready = Arc::clone(&bridge);
        let activity_for_ready = Arc::clone(&activity);
        let mut ready_shutdown = shutdown.subscribe();
        let ready_task = tokio::spawn(async move {
            if let Some(port) = bridge_for_ready.ready_tunnel_port().await {
                activity_for_ready.tunnel_ready(port);
            }
            loop {
                tokio::select! {
                    changed = ready_shutdown.changed() => {
                        if changed.is_err() || *ready_shutdown.borrow() {
                            break;
                        }
                    }
                    changed = runtime_changes.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        if let Some(port) = bridge_for_ready.ready_tunnel_port().await {
                            activity_for_ready.tunnel_ready(port);
                        }
                    }
                }
            }
        });
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    changed = idle_shutdown.changed() => {
                        if changed.is_err() || *idle_shutdown.borrow() {
                            break;
                        }
                    }
                    _ = interval.tick() => {
                        let Some(bridge) = idle_bridge.upgrade() else {
                            break;
                        };
                        let Some(_idle_claim) =
                            idle_activity.try_claim_idle_disconnect(idle_timeout)
                        else {
                            continue;
                        };
                        let idle_for = idle_activity.idle_for();
                        let activity_reason = idle_activity.last_activity_reason();
                        #[cfg(test)]
                        idle_monitor_probe.disconnect_started.notify_waiters();
                        tokio::select! {
                            changed = idle_shutdown.changed() => {
                                if changed.is_err() || *idle_shutdown.borrow() {
                                    break;
                                }
                            }
                            disconnected = bridge.disconnect_idle_tunnel() => {
                                #[cfg(test)]
                                idle_monitor_probe.disconnect_finished.notify_waiters();
                                if let Some(port) = disconnected {
                                    bridge.log_idle_disconnect(
                                        port,
                                        idle_for,
                                        activity_reason.as_str(),
                                        idle_timeout,
                                    );
                                    idle_activity.touch(ActivityReason::IdleDisconnectCompleted);
                                }
                            }
                        }
                    }
                }
            }
        });

        let mut cleanup_shutdown = shutdown.subscribe();
        let trace_cleanup_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    changed = cleanup_shutdown.changed() => {
                        if changed.is_err() || *cleanup_shutdown.borrow() {
                            break;
                        }
                    }
                    _ = interval.tick() => {
                        let store = trace_cleanup_store.clone();
                        let _ = tokio::task::spawn_blocking(move || store.cleanup()).await;
                    }
                }
            }
        });

        Ok(Self {
            port,
            shutdown,
            task,
            trace_cleanup_task,
            activity,
            ready_task,
        })
    }

    /// Local port bound by this gateway.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Number of HTTP requests currently being served.
    pub fn active_connections(&self) -> usize {
        self.activity.active_connections()
    }

    /// Stops accepting new requests and ends the idle monitor.
    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        let mut cleanup_task = self.trace_cleanup_task;
        if tokio::time::timeout(Duration::from_secs(5), &mut cleanup_task)
            .await
            .is_err()
        {
            cleanup_task.abort();
            let _ = cleanup_task.await;
        }
        let mut ready_task = self.ready_task;
        if tokio::time::timeout(Duration::from_secs(5), &mut ready_task)
            .await
            .is_err()
        {
            ready_task.abort();
            let _ = ready_task.await;
        }
    }
}
