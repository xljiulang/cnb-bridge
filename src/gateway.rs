//! Persistent loopback gateway that activates CNB only when needed.

use crate::bridge::Bridge;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::{timeout, Duration};

/// Gateway timing parameters.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub port: u16,
    pub activation_timeout: Duration,
    pub idle_timeout: Duration,
}

impl GatewayConfig {
    /// Production gateway settings.
    pub fn new(port: u16) -> Self {
        Self {
            port,
            activation_timeout: Duration::from_secs(180),
            idle_timeout: Duration::from_secs(5 * 60),
        }
    }
}

fn should_close_idle_tunnel(
    active_connections: usize,
    idle_for: Duration,
    idle_timeout: Duration,
) -> bool {
    active_connections == 0 && idle_for >= idle_timeout
}

struct GatewayActivity {
    active_connections: AtomicUsize,
    last_activity: Mutex<Instant>,
}

impl GatewayActivity {
    fn new() -> Self {
        Self {
            active_connections: AtomicUsize::new(0),
            last_activity: Mutex::new(Instant::now()),
        }
    }

    fn enter(&self) {
        self.active_connections.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }

    fn leave(&self) {
        self.active_connections.fetch_sub(1, Ordering::SeqCst);
        self.touch();
    }

    fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
    }

    fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::SeqCst)
    }

    fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .elapsed()
    }
}

/// Handle owning one gateway listener and its monitoring task.
pub struct GatewayHandle {
    port: u16,
    shutdown: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    activity: Arc<GatewayActivity>,
}

impl GatewayHandle {
    /// Binds the configured loopback port and starts serving requests.
    pub async fn start(bridge: Arc<Bridge>, config: GatewayConfig) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", config.port))
            .await
            .map_err(|error| format!("绑定本地网关端口 {} 失败: {error}", config.port))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("读取本地网关端口失败: {error}"))?
            .port();
        let (shutdown, _shutdown_rx) = watch::channel(false);
        let activity = Arc::new(GatewayActivity::new());

        let accept_activity = Arc::clone(&activity);
        let accept_shutdown = shutdown.subscribe();
        let weak_bridge = Arc::downgrade(&bridge);
        let activation_timeout = config.activation_timeout;
        let accept_task = tokio::spawn(async move {
            let mut shutdown_rx = accept_shutdown;
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    accepted = listener.accept() => {
                        let Ok((stream, _peer)) = accepted else {
                            break;
                        };
                        let Some(bridge) = weak_bridge.upgrade() else {
                            break;
                        };
                        let activity = Arc::clone(&accept_activity);
                        tokio::spawn(async move {
                            activity.enter();
                            handle_connection(bridge, stream, activation_timeout).await;
                            activity.leave();
                        });
                    }
                }
            }
        });

        let idle_bridge = Arc::downgrade(&bridge);
        let idle_activity = Arc::clone(&activity);
        let idle_timeout = config.idle_timeout;
        let idle_shutdown = shutdown.subscribe();
        let idle_task = tokio::spawn(async move {
            let mut shutdown_rx = idle_shutdown;
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    _ = interval.tick() => {
                        if !should_close_idle_tunnel(
                            idle_activity.active_connections(),
                            idle_activity.idle_for(),
                            idle_timeout,
                        ) {
                            continue;
                        }
                        let Some(bridge) = idle_bridge.upgrade() else {
                            break;
                        };
                        if bridge.sleep_tunnel().await {
                            idle_activity.touch();
                        }
                    }
                }
            }
        });

        Ok(Self {
            port,
            shutdown,
            tasks: vec![accept_task, idle_task],
            activity,
        })
    }

    /// Local port bound by this gateway.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Number of TCP requests currently being served.
    pub fn active_connections(&self) -> usize {
        self.activity.active_connections()
    }

    /// Stops accepting new requests and ends the idle monitor.
    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        for task in self.tasks {
            task.abort();
            let _ = task.await;
        }
    }
}

async fn handle_connection(bridge: Arc<Bridge>, mut stream: TcpStream, activation_timeout: Duration) {
    if is_health_request(&mut stream).await {
        let mut request = [0_u8; 1024];
        let _ = timeout(Duration::from_millis(100), stream.read(&mut request)).await;
        let body = br#"{"status":"ok","bridge":"local-gateway"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(body).await;
        return;
    }

    let mut upstream = None;
    for _ in 0..2 {
        let activation_bridge = Arc::clone(&bridge);
        let activation =
            tokio::spawn(async move { activation_bridge.ensure_active().await });
        let port = match timeout(activation_timeout, activation).await {
            Ok(Ok(Ok(port))) => port,
            Ok(Ok(Err(_))) | Ok(Err(_)) | Err(_) => return,
        };
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(stream) => {
                upstream = Some(stream);
                break;
            }
            Err(error) => {
                bridge
                    .invalidate_tunnel(port, format!("连接动态隧道端口 {port} 失败: {error}"))
                    .await;
            }
        }
    }
    let Some(mut upstream) = upstream else {
        return;
    };
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
}

async fn is_health_request(stream: &mut TcpStream) -> bool {
    let mut buffer = [0_u8; 512];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let Ok(read) = timeout(remaining, stream.peek(&mut buffer)).await else {
            return false;
        };
        let Ok(read) = read else {
            return false;
        };
        if read == 0 {
            return false;
        }
        let request = String::from_utf8_lossy(&buffer[..read]);
        if request.contains("\r\n") || read == buffer.len() {
            return request.starts_with("GET /healthz ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountStore;
    use crate::logging::Logger;
    use crate::settings::SettingsStore;
    use crate::ssh_key::SshKeyStore;
    use tokio::io::AsyncReadExt;

    #[test]
    fn gateway_config_uses_request_driven_defaults() {
        let config = GatewayConfig::new(11233);
        assert_eq!(config.activation_timeout, Duration::from_secs(180));
        assert_eq!(config.idle_timeout, Duration::from_secs(5 * 60));
    }

    #[test]
    fn only_closes_after_five_idle_minutes_without_active_connections() {
        let timeout = Duration::from_secs(5 * 60);
        assert!(!should_close_idle_tunnel(
            1,
            Duration::from_secs(301),
            timeout
        ));
        assert!(!should_close_idle_tunnel(
            0,
            Duration::from_secs(299),
            timeout
        ));
        assert!(should_close_idle_tunnel(0, timeout, timeout));
    }

    #[tokio::test]
    async fn activity_tracks_connections_and_last_use() {
        let activity = GatewayActivity::new();
        activity.enter();
        activity.enter();
        assert_eq!(activity.active_connections(), 2);
        activity.leave();
        assert_eq!(activity.active_connections(), 1);
        activity.leave();
        assert_eq!(activity.active_connections(), 0);
        assert!(activity.idle_for() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn health_request_is_answered_locally_without_activation() {
        let temp = tempfile::tempdir().unwrap();
        let account = crate::account::AccountManager::with_store(AccountStore::new(
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
        let gateway = GatewayHandle::start(
            Arc::clone(&bridge),
            GatewayConfig {
                port: 0,
                activation_timeout: Duration::from_secs(1),
                idle_timeout: Duration::from_secs(60),
            },
        )
        .await
        .unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", gateway.port()))
            .await
            .unwrap();
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("local-gateway"));
        assert_eq!(bridge.snapshot().await.state, crate::bridge::BridgeState::SignedOut);
        gateway.stop().await;
    }
}
