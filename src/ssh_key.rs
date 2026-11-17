//! Dedicated SSH key generation and DPAPI encrypted storage.

use crate::dpapi;
use crate::paths;
use rand::rngs::OsRng;
use ssh_key::{Algorithm, HashAlg, PrivateKey};
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::xxh3_64;

const RESOURCE_NAME_PREFIX: &str = "cnb-bridge-";

/// CNB Bridge never reuses a user's existing `~/.ssh` keys.
#[derive(Debug, Clone)]
pub struct SshKeyStore {
    private_path: PathBuf,
    public_path: PathBuf,
}

/// Returns the standard SHA-256 fingerprint for an OpenSSH public key.
pub fn public_key_fingerprint(public_key: &str) -> Result<String, String> {
    let public_key = ssh_key::PublicKey::from_openssh(public_key.trim())
        .map_err(|error| format!("解析 SSH 公钥失败: {error}"))?;
    Ok(public_key.fingerprint(HashAlg::Sha256).to_string())
}

/// Returns the XXH3-64 hash of the canonical OpenSSH public key body.
pub fn public_key_hash(public_key: &str) -> Result<u64, String> {
    let public_key = ssh_key::PublicKey::from_openssh(public_key.trim())
        .map_err(|error| format!("解析 SSH 公钥失败: {error}"))?;
    let bytes = public_key
        .to_bytes()
        .map_err(|error| format!("编码 SSH 公钥失败: {error}"))?;
    Ok(xxh3_64(&bytes))
}

/// Returns the stable CNB resource name for an OpenSSH public key.
pub fn public_key_resource_name(public_key: &str) -> Result<String, String> {
    Ok(format!(
        "{RESOURCE_NAME_PREFIX}{}",
        public_key_hash(public_key)?
    ))
}

impl SshKeyStore {
    /// Creates a store at explicit paths.
    pub fn new(private_path: impl Into<PathBuf>, public_path: impl Into<PathBuf>) -> Self {
        Self {
            private_path: private_path.into(),
            public_path: public_path.into(),
        }
    }

    /// Creates the default store under `%APPDATA%\cnb-bridge`.
    pub fn default_path() -> Self {
        Self::new(paths::ssh_key_path(), paths::ssh_public_key_path())
    }

    /// Returns the encrypted private key path.
    pub fn private_path(&self) -> &Path {
        &self.private_path
    }

    /// Returns the public key path.
    pub fn public_path(&self) -> &Path {
        &self.public_path
    }

    /// Ensures a dedicated key exists and returns its OpenSSH public key text.
    pub fn ensure(&self) -> Result<String, String> {
        if self.private_path.exists() && self.public_path.exists() {
            return self.public_key();
        }
        self.regenerate()
    }

    /// Generates a new Ed25519 key pair, replacing any previous key.
    pub fn regenerate(&self) -> Result<String, String> {
        let private = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)
            .map_err(|error| format!("生成 SSH 密钥失败: {error}"))?;
        let private_text = private
            .to_openssh(ssh_key::LineEnding::LF)
            .map_err(|error| format!("导出 SSH 私钥失败: {error}"))?
            .to_string();
        let public_text = private
            .public_key()
            .to_openssh()
            .map_err(|error| format!("导出 SSH 公钥失败: {error}"))?;
        self.save_private(&private_text)?;
        if let Some(parent) = self.public_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建公钥目录失败: {error}"))?;
        }
        std::fs::write(&self.public_path, public_text.as_bytes())
            .map_err(|error| format!("写入 SSH 公钥失败: {error}"))?;
        Ok(public_text)
    }

    /// Loads the decrypted private key in OpenSSH format.
    pub fn private_key_text(&self) -> Result<String, String> {
        let encrypted = std::fs::read(&self.private_path)
            .map_err(|error| format!("读取 SSH 私钥失败: {error}"))?;
        let plaintext = dpapi::unprotect(&encrypted)?;
        String::from_utf8(plaintext).map_err(|error| format!("SSH 私钥不是 UTF-8: {error}"))
    }

    /// Loads the public key text.
    pub fn public_key(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.public_path)
            .map(|text| text.trim().to_string())
            .map_err(|error| format!("读取 SSH 公钥失败: {error}"))
    }

    /// Deletes the stored key pair.
    pub fn remove(&self) -> Result<(), String> {
        for path in [&self.private_path, &self.public_path] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("删除 {} 失败: {error}", path.display())),
            }
        }
        Ok(())
    }

    fn save_private(&self, text: &str) -> Result<(), String> {
        let encrypted = dpapi::protect(text.as_bytes())?;
        if let Some(parent) = self.private_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("创建私钥目录失败: {error}"))?;
        }
        let temp = self.private_path.with_extension("bin.tmp");
        std::fs::write(&temp, &encrypted)
            .map_err(|error| format!("写入临时 SSH 私钥失败: {error}"))?;
        std::fs::rename(&temp, &self.private_path)
            .map_err(|error| format!("替换 SSH 私钥失败: {error}"))?;
        Ok(())
    }
}

#[test]
fn computes_the_standard_sha256_public_key_fingerprint() {
    let public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti user@example.com";

    assert_eq!(
        public_key_fingerprint(public_key).unwrap(),
        "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ"
    );
}

#[test]
fn computes_the_canonical_xxh3_public_key_hash() {
    let public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti user@example.com";

    assert_eq!(public_key_hash(public_key).unwrap(), 15459454312938932541);
}

#[test]
fn builds_the_public_key_resource_name_from_its_hash() {
    let public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti user@example.com";

    assert_eq!(
        public_key_resource_name(public_key).unwrap(),
        "cnb-bridge-15459454312938932541"
    );
}

#[test]
fn public_key_hash_ignores_the_comment() {
    let without_comment =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti";
    let with_comment = format!("{without_comment} another@example.com");

    assert_eq!(
        public_key_hash(without_comment).unwrap(),
        public_key_hash(&with_comment).unwrap()
    );
}

#[test]
fn public_key_hash_rejects_invalid_key_material() {
    assert!(public_key_hash("not-a-public-key")
        .unwrap_err()
        .contains("解析 SSH 公钥失败"));
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn generates_ed25519_key_and_never_writes_plaintext_private_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = SshKeyStore::new(
            dir.path().join("ssh-key.bin"),
            dir.path().join("ssh-key.pub"),
        );
        let public = store.ensure().unwrap();
        assert!(public.starts_with("ssh-ed25519 "));
        let raw = std::fs::read(store.private_path()).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("BEGIN OPENSSH PRIVATE KEY"));
        let private = store.private_key_text().unwrap();
        assert!(private.contains("BEGIN OPENSSH PRIVATE KEY"));
    }
}
