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

pub(super) fn should_stop_idle_workspace(
    active_connections: usize,
    idle_for: Duration,
    idle_timeout: Duration,
) -> bool {
    active_connections == 0 && idle_for >= idle_timeout
}

pub(super) struct GatewayActivity {
    active_connections: AtomicUsize,
    last_activity: Mutex<Instant>,
}

impl GatewayActivity {
    pub(super) fn new() -> Self {
        Self {
            active_connections: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
        }
    }

    pub(super) fn enter(self: &Arc<Self>) -> ActivityGuard {
        self.active_connections.fetch_add(1, Ordering::SeqCst);
        self.touch();
        ActivityGuard {
            activity: Arc::clone(self),
        }
    }

    fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
    }

    pub(super) fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::SeqCst)
    }

    pub(super) fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .elapsed()
    }
}

pub(super) struct ActivityGuard {
    activity: Arc<GatewayActivity>,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.activity
            .active_connections
            .fetch_sub(1, Ordering::SeqCst);
        self.activity.touch();
    }
}

/// Handle owning one gateway listener and its monitoring task.
pub struct GatewayHandle {
    port: u16,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    trace_cleanup_task: tokio::task::JoinHandle<()>,
    activity: Arc<GatewayActivity>,
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
        let listener = TcpListener::bind(("127.0.0.1", config.port))
            .await
            .map_err(|error| format!("绑定本地网关端口 {} 失败: {error}", config.port))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("读取本地网关端口失败: {error}"))?
            .port();
        let (shutdown, _) = watch::channel(false);
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
                        if !should_stop_idle_workspace(
                            idle_activity.active_connections(),
                            idle_activity.idle_for(),
                            idle_timeout,
                        ) {
                            continue;
                        }
                        let Some(bridge) = idle_bridge.upgrade() else {
                            break;
                        };
                        if bridge.stop_idle_workspace().await {
                            idle_activity.touch();
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
    }
}
