//! SSH.NET-style local forwarding implemented with the pure Rust `russh` crate.

use russh::client;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use std::future::Future;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{timeout, Duration};

use crate::logging::Logger;

/// Official CNB cloud development host key fingerprint.
pub const CNB_HOST_KEY_FINGERPRINT: &str = "SHA256:fnWZvpqd+VAIRJxaZdV1KVMFfDgcCjYrP2VSWQ68T/E";

/// Default loopback port used by the desktop shell and the workspace agent.
pub const DEFAULT_TUNNEL_PORT: u16 = 11233;
const SSH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const SSH_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(1500)];

fn tunnel_client_config() -> client::Config {
    client::Config {
        nodelay: true,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        ..client::Config::default()
    }
}

fn is_retryable_connect_error(error: &russh::Error) -> bool {
    matches!(
        error,
        russh::Error::IO(_)
            | russh::Error::HUP
            | russh::Error::ConnectionTimeout
            | russh::Error::Disconnect
    )
}

struct TunnelAttemptError {
    message: String,
    retryable: bool,
}

impl TunnelAttemptError {
    fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

async fn connect_with_retry<T, F, Fut>(mut attempt: F, logger: Option<&Logger>) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, TunnelAttemptError>>,
{
    let max_attempts = SSH_RETRY_DELAYS.len() + 1;
    for (index, delay) in SSH_RETRY_DELAYS
        .iter()
        .copied()
        .map(Some)
        .chain([None])
        .enumerate()
    {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) if error.retryable && delay.is_some() => {
                if let Some(logger) = logger {
                    logger.log(
                        "warn",
                        format!(
                            "SSH 连接尝试 {}/{} 失败，正在重试: {}",
                            index + 1,
                            max_attempts,
                            error.message
                        ),
                    );
                }
                tokio::time::sleep(delay.unwrap_or_default()).await;
            }
            Err(error) => return Err(error.message),
        }
    }
    unreachable!()
}

struct ClientHandler;

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let fingerprint = server_public_key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();
        Ok(fingerprint == CNB_HOST_KEY_FINGERPRINT)
    }
}

/// A running local forwarding tunnel.
pub struct TunnelHandle {
    local_port: u16,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    failure: watch::Receiver<Option<String>>,
}

impl TunnelHandle {
    /// Local port bound by the tunnel.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// Returns the first fatal tunnel error, if one has occurred.
    pub fn failure_reason(&self) -> Option<String> {
        self.failure.borrow().clone()
    }

    pub(crate) fn failure_watch(&self) -> watch::Receiver<Option<String>> {
        self.failure.clone()
    }

    /// Stops forwarding and releases the local port.
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
    }

    #[cfg(test)]
    pub(crate) async fn test_handle() -> (Self, std::net::SocketAddr) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let (_failure_tx, failure) = watch::channel(None);
        let task = tokio::spawn(async move {
            let _listener = listener;
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                }
            }
        });
        (
            Self {
                local_port: address.port(),
                shutdown,
                task,
                failure,
            },
            address,
        )
    }
}

/// Parses `user@host` or `user@host:port` returned by CNB.
pub fn parse_remote_ssh(value: &str) -> Result<(String, String, u16), String> {
    let value = value.trim().strip_prefix("ssh ").unwrap_or(value).trim();
    let (user_host, port) = if let Some((left, right)) = value.rsplit_once(':') {
        if right.chars().all(|c| c.is_ascii_digit()) {
            let port = right
                .parse::<u16>()
                .map_err(|_| format!("SSH 端口无效: {right}"))?;
            (left, port)
        } else {
            (value, 22)
        }
    } else {
        (value, 22)
    };
    let (user, host) = user_host
        .rsplit_once('@')
        .ok_or_else(|| format!("SSH 地址缺少 user@host: {value}"))?;
    if user.is_empty() || host.is_empty() {
        return Err(format!("SSH 地址不完整: {value}"));
    }
    Ok((user.to_string(), host.to_string(), port))
}

/// Starts a local forwarding tunnel on `local_port`.
pub async fn start_tunnel(
    remote_ssh: &str,
    private_key_text: &str,
    local_port: u16,
) -> Result<TunnelHandle, String> {
    start_tunnel_with_logger(remote_ssh, private_key_text, local_port, None).await
}

/// Starts a local forwarding tunnel and records retry attempts in `logger`.
pub async fn start_tunnel_with_logger(
    remote_ssh: &str,
    private_key_text: &str,
    local_port: u16,
    logger: Option<&Logger>,
) -> Result<TunnelHandle, String> {
    let (user, host, port) = parse_remote_ssh(remote_ssh)?;
    let private_key = PrivateKey::from_openssh(private_key_text)
        .map_err(|error| format!("解析专用 SSH 私钥失败: {error}"))?;

    let mut session = connect_with_retry(
        || {
            let config = Arc::new(tunnel_client_config());
            let host = host.as_str();
            async move {
                match timeout(
                    SSH_HANDSHAKE_TIMEOUT,
                    client::connect(config, (host, port), ClientHandler),
                )
                .await
                {
                    Ok(Ok(session)) => Ok(session),
                    Ok(Err(error)) if is_retryable_connect_error(&error) => Err(
                        TunnelAttemptError::retryable(format!("连接 CNB SSH 失败: {error}")),
                    ),
                    Ok(Err(error)) => Err(TunnelAttemptError::fatal(format!(
                        "连接 CNB SSH 失败: {error}"
                    ))),
                    Err(_) => Err(TunnelAttemptError::retryable("连接 CNB SSH 超时")),
                }
            }
        },
        logger,
    )
    .await?;
    let none_auth = timeout(SSH_HANDSHAKE_TIMEOUT, session.authenticate_none(&user))
        .await
        .map_err(|_| "CNB SSH 认证超时".to_string())?
        .map_err(|error| format!("SSH 认证失败: {error}"))?;
    let auth = if none_auth.success() {
        none_auth
    } else {
        timeout(
            SSH_HANDSHAKE_TIMEOUT,
            session.authenticate_publickey(
                &user,
                PrivateKeyWithHashAlg::new(Arc::new(private_key), None),
            ),
        )
        .await
        .map_err(|_| "CNB SSH 认证超时".to_string())?
        .map_err(|error| format!("SSH 公钥认证失败: {error}"))?
    };
    if !auth.success() {
        return Err("SSH 公钥认证失败：请重新扫码绑定以同步公钥".to_string());
    }

    let session = Arc::new(session);
    let listener = TcpListener::bind(("127.0.0.1", local_port))
        .await
        .map_err(|error| format!("绑定本机端口 {local_port} 失败: {error}"))?;
    let local_port = listener
        .local_addr()
        .map_err(|error| format!("读取本机隧道端口失败: {error}"))?
        .port();
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let (failure_tx, failure_rx) = watch::channel(None::<String>);

    let accept_session = Arc::clone(&session);
    let accept_failure_tx = failure_tx.clone();
    let mut accept_failure_rx = failure_rx.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                changed = accept_failure_rx.changed() => {
                    if changed.is_err() || accept_failure_rx.borrow().is_some() {
                        break;
                    }
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
                accepted = listener.accept() => {
                    let (mut socket, _peer) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            let _ = accept_failure_tx.send(Some(format!(
                                "本机隧道监听失败: {error}"
                            )));
                            break;
                        }
                    };
                    let session = Arc::clone(&accept_session);
                    let failure_tx = accept_failure_tx.clone();
                    connections.spawn(async move {
                        let channel = match session
                            .channel_open_direct_tcpip(
                                "127.0.0.1",
                                11233,
                                "127.0.0.1",
                                local_port as u32,
                            )
                            .await {
                                Ok(channel) => channel,
                                Err(error) => {
                                    let _ = failure_tx.send(Some(format!(
                                        "CNB SSH 通道已失效: {error}"
                                    )));
                                    return;
                                }
                        };
                        let mut stream = channel.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                    });
                }
            }
        }
        stop_connections(&mut connections).await;
    });

    Ok(TunnelHandle {
        local_port,
        shutdown,
        task,
        failure: failure_rx,
    })
}

async fn stop_connections(connections: &mut JoinSet<()>) {
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::Logger;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn parses_default_and_explicit_ports() {
        assert_eq!(
            parse_remote_ssh("cnb-abc-123@cnb.space").unwrap(),
            ("cnb-abc-123".to_string(), "cnb.space".to_string(), 22)
        );
        assert_eq!(
            parse_remote_ssh("ssh user@example.test:2222").unwrap(),
            ("user".to_string(), "example.test".to_string(), 2222)
        );
    }

    #[tokio::test]
    async fn failure_signal_releases_the_listener_and_reports_the_reason() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, _) = watch::channel(false);
        let (failure_tx, failure_rx) = watch::channel(None::<String>);
        let mut listener_failure = failure_rx.clone();
        let task = tokio::spawn(async move {
            let _listener = listener;
            let _ = listener_failure.changed().await;
        });
        let handle = TunnelHandle {
            local_port: address.port(),
            shutdown,
            task,
            failure: failure_rx,
        };

        failure_tx
            .send(Some("SSH channel closed".to_string()))
            .unwrap();
        let reason = handle.failure_reason();
        let task = handle.task;
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(reason.as_deref(), Some("SSH channel closed"));
        assert!(TcpListener::bind(address).await.is_ok());
    }

    #[tokio::test]
    async fn retries_a_transient_connection_failure_until_success() {
        let calls = AtomicUsize::new(0);
        let logger = Logger::new(8);

        let result = connect_with_retry(
            || {
                let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if attempt == 1 {
                        Err(TunnelAttemptError::retryable(
                            "连接 CNB SSH 失败: connection reset",
                        ))
                    } else {
                        Ok("connected")
                    }
                }
            },
            Some(&logger),
        )
        .await
        .unwrap();

        assert_eq!(result, "connected");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let logs = logger.recent(8);
        assert!(logs.iter().any(|entry| entry.level == "warn"
            && entry.message.contains("SSH 连接尝试 1/3 失败")
            && entry.message.contains("connection reset")));
    }

    #[tokio::test]
    async fn stops_after_three_transient_failures() {
        let calls = AtomicUsize::new(0);

        let error = connect_with_retry::<(), _, _>(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(TunnelAttemptError::retryable("connection reset")) }
            },
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(error, "connection reset");
    }

    #[tokio::test]
    async fn does_not_retry_fatal_connection_failures() {
        let calls = AtomicUsize::new(0);

        let error = connect_with_retry::<(), _, _>(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(TunnelAttemptError::fatal("SSH 公钥认证失败")) }
            },
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(error, "SSH 公钥认证失败");
    }

    #[test]
    fn client_configures_keepalive_and_low_latency() {
        let config = tunnel_client_config();

        assert!(config.nodelay);
        assert_eq!(config.keepalive_interval, Some(Duration::from_secs(30)));
        assert_eq!(config.keepalive_max, 3);
    }

    #[test]
    fn classifies_connection_resets_as_retryable() {
        let reset = russh::Error::IO(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "remote closed",
        ));

        assert!(is_retryable_connect_error(&reset));
        assert!(!is_retryable_connect_error(&russh::Error::UnknownKey));
        assert!(!is_retryable_connect_error(&russh::Error::WrongServerSig));
    }

    #[test]
    fn classifies_windows_remote_disconnect_as_retryable() {
        let error = russh::Error::IO(std::io::Error::from_raw_os_error(10054));

        assert!(is_retryable_connect_error(&error));
    }

    #[tokio::test]
    async fn stopping_tunnel_drains_connection_tasks() {
        struct DropSignal {
            dropped: Arc<AtomicUsize>,
        }
        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicUsize::new(0));
        let mut connections = JoinSet::new();
        for _ in 0..2 {
            let dropped = Arc::clone(&dropped);
            connections.spawn(async move {
                let _signal = DropSignal { dropped };
                std::future::pending::<()>().await;
            });
        }
        tokio::task::yield_now().await;

        stop_connections(&mut connections).await;

        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        assert!(connections.is_empty());
    }
}
