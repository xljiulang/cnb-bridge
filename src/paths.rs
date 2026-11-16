//! Filesystem locations used by CNB Bridge.

use std::path::PathBuf;

/// Returns the application data directory.
///
/// `CNB_BRIDGE_DATA_DIR` overrides the default to keep tests and portable
/// builds isolated from the real user profile.
pub fn data_dir() -> PathBuf {
    if let Some(value) = std::env::var_os("CNB_BRIDGE_DATA_DIR") {
        return PathBuf::from(value);
    }

    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("cnb-bridge")
}

/// Creates the application data directory when it does not exist.
pub fn ensure_data_dir() -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Directory containing rolling log files.
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Path of the persisted desktop settings.
pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

/// Path of the DPAPI encrypted personal access token.
pub fn account_path() -> PathBuf {
    data_dir().join("account-auth.bin")
}

/// Path of the DPAPI encrypted dedicated SSH private key.
pub fn ssh_key_path() -> PathBuf {
    data_dir().join("ssh-key.bin")
}

/// Path of the OpenSSH public key shown to the user.
pub fn ssh_public_key_path() -> PathBuf {
    data_dir().join("ssh-key.pub")
}

/// Persistent WebView2 profile used only for CNB web authorization.
pub fn auth_webview_dir() -> PathBuf {
    data_dir().join("auth-webview")
}
