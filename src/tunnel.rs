//! SSH.NET-style local forwarding implemented with the pure Rust `russh` crate.

use russh::client;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;

/// Official CNB cloud development host key fingerprint.
pub const CNB_HOST_KEY_FINGERPRINT: &str = "SHA256:fnWZvpqd+VAIRJxaZdV1KVMFfDgcCjYrP2VSWQ68T/E";

/// Default loopback port used by the desktop shell and the workspace agent.
pub const DEFAULT_TUNNEL_PORT: u16 = 11233;

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
}

impl TunnelHandle {
    /// Local port bound by the tunnel.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// Stops forwarding and releases the local port.
    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
        let _ = self.task.await;
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
    let (user, host, port) = parse_remote_ssh(remote_ssh)?;
    let private_key = PrivateKey::from_openssh(private_key_text)
        .map_err(|error| format!("解析专用 SSH 私钥失败: {error}"))?;

    let config = Arc::new(client::Config::default());
    let mut session = client::connect(config, (host.as_str(), port), ClientHandler)
        .await
        .map_err(|error| format!("连接 CNB SSH 失败: {error}"))?;
    let auth = session
        .authenticate_publickey(
            user,
            PrivateKeyWithHashAlg::new(Arc::new(private_key), None),
        )
        .await
        .map_err(|error| format!("SSH 公钥认证失败: {error}"))?;
    if !auth.success() {
        return Err("SSH 公钥认证失败：该密钥可能尚未添加到 CNB 公钥管理页".to_string());
    }

    let session = Arc::new(session);
    let listener = TcpListener::bind(("127.0.0.1", local_port))
        .await
        .map_err(|error| format!("绑定本机端口 {local_port} 失败: {error}"))?;
    let (shutdown, mut shutdown_rx) = watch::channel(false);

    let accept_session = Arc::clone(&session);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    let Ok((mut socket, _peer)) = accepted else {
                        break;
                    };
                    let session = Arc::clone(&accept_session);
                    tokio::spawn(async move {
                        let Ok(channel) = session
                            .channel_open_direct_tcpip(
                                "127.0.0.1",
                                11233,
                                "127.0.0.1",
                                local_port as u32,
                            )
                            .await
                        else {
                            return;
                        };
                        let mut stream = channel.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                    });
                }
            }
        }
    });

    let keepalive = Arc::clone(&session);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            if keepalive.send_keepalive(true).await.is_err() {
                break;
            }
        }
    });

    Ok(TunnelHandle {
        local_port,
        shutdown,
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
