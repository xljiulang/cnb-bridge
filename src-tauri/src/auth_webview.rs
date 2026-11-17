use crate::state::DesktopState;
use cnb_bridge::account::AccountStatus;
use cnb_bridge::api::CnbApi;
use cnb_bridge::paths;
use cnb_bridge::web_auth::{
    is_allowed_auth_url, session_from_cookies, CreatedPat, WebAuthClient, WebSession,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use url::Url;

const AUTH_URL: &str = "https://cnb.cool/?s_url=%2Fprofile%2Ftoken";
const WINDOW_LABEL: &str = "cnb-auth";
const TOKEN_DESCRIPTION: &str = "CNB Bridge";

/// Opens the CNB web authorization window.
pub fn launch(app: tauri::AppHandle) -> Result<AccountStatus, String> {
    let state = app.state::<DesktopState>();
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(state.account.status());
    }
    state.account.begin_token_login();
    if let Some(previous) = state
        .login_cancel
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        previous.store(true, Ordering::SeqCst);
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *state
        .login_cancel
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&cancel));
    let generation = state.login_generation.fetch_add(1, Ordering::SeqCst) + 1;

    let url = Url::parse(AUTH_URL).map_err(|error| error.to_string())?;
    if let Err(error) = WebviewWindowBuilder::new(&app, WINDOW_LABEL, WebviewUrl::External(url))
        .title("CNB 扫码绑定")
        .inner_size(680.0, 700.0)
        .min_inner_size(520.0, 560.0)
        .resizable(true)
        .center()
        .data_directory(paths::auth_webview_dir())
        .on_navigation(is_allowed_auth_url)
        .build()
    {
        state.account.fail_token_login(error.to_string());
        return Err(error.to_string());
    }

    let account = Arc::clone(&state.account);
    let bridge = Arc::clone(&state.bridge);
    let logger = Arc::clone(&state.logger);
    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = authorize(
            app_for_task.clone(),
            Arc::clone(&cancel),
            Arc::clone(&bridge),
            Arc::clone(&logger),
        )
        .await;
        match result {
            Ok(authorized) => {
                let status = account
                    .complete_token_login(authorized.pat.token.clone(), authorized.username, None)
                    .map(|_| account.set_needs_organization(authorized.needs_organization));
                match status {
                    Ok(_) => {
                        logger.log("info", "CNB personal access token is ready");
                        if !authorized.pat.token_id.trim().is_empty() {
                            let client = WebAuthClient::new();
                            if let Err(error) = client
                                .delete_old_tokens(
                                    &authorized.session,
                                    TOKEN_DESCRIPTION,
                                    &authorized.pat.token_id,
                                )
                                .await
                            {
                                logger.log("warn", format!("清理旧 CNB Bridge 令牌失败: {error}"));
                            } else if let Err(error) = client
                                .rename_token(
                                    &authorized.session,
                                    &authorized.pat.token_id,
                                    TOKEN_DESCRIPTION,
                                )
                                .await
                            {
                                logger.log("warn", format!("重命名 CNB Bridge 令牌失败: {error}"));
                            }
                        }
                        bridge.account_connected().await;
                    }
                    Err(error) => {
                        account.fail_token_login(error.to_string());
                        logger.log("error", format!("保存 CNB 访问令牌失败: {error}"));
                    }
                }
            }
            Err(error) => {
                account.fail_token_login(error.clone());
                logger.log("error", format!("CNB 扫码绑定失败: {error}"));
            }
        }
        close_and_clear(&app_for_task);
        if let Some(state) = app_for_task.try_state::<DesktopState>() {
            let _ = state
                .login_cancel
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
        }
        let _ = app_for_task.emit("cnb-bridge://snapshot", generation);
    });

    Ok(state.account.status())
}

/// Cancels an active WebView authorization.
pub fn cancel(app: &tauri::AppHandle) -> AccountStatus {
    let state = app.state::<DesktopState>();
    if let Some(cancel) = state
        .login_cancel
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        cancel.store(true, Ordering::SeqCst);
    }
    close_and_clear(app);
    state.account.fail_token_login("扫码绑定已取消");
    state.account.status()
}

async fn authorize(
    app: tauri::AppHandle,
    cancel: Arc<AtomicBool>,
    bridge: Arc<cnb_bridge::bridge::Bridge>,
    logger: Arc<cnb_bridge::logging::Logger>,
) -> Result<AuthorizationResult, String> {
    let session = wait_for_session(&app, cancel).await?;
    let client = WebAuthClient::new();
    let namespaces = client
        .owned_namespaces(&session)
        .await
        .map_err(|error| format!("读取 CNB 命名空间失败: {error}"))?;
    let namespace = (namespaces.len() == 1).then(|| namespaces[0].path.as_str());
    let public_key = bridge.ensure_ssh_key()?;
    client
        .ensure_ssh_key(&session, TOKEN_DESCRIPTION, &public_key)
        .await
        .map_err(|error| format!("自动添加 SSH 公钥失败: {error}"))?;
    let temporary_description = format!(
        "{TOKEN_DESCRIPTION} {}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let pat = client
        .create_pat(&session, &temporary_description, namespace)
        .await
        .map_err(|error| format!("自动创建访问令牌失败: {error}"))?;
    let api = CnbApi::new();
    let user = api
        .current_user(&pat.token)
        .await
        .map_err(|error| format!("新访问令牌验证失败: {error}"))?;
    let owned_namespaces = api
        .list_owned_namespaces(&pat.token)
        .await
        .map_err(|error| format!("检查 CNB Owner 组织失败: {error}"))?;
    let username = (!user.username.trim().is_empty()).then_some(user.username);
    if username.is_none() {
        return Err("CNB 未返回账号名，无法保存账号凭据".to_string());
    }
    let mut needs_organization = owned_namespaces.is_empty();
    if needs_organization {
        let organization_path = username.as_deref().unwrap_or_default();
        if organization_path.trim().is_empty() {
            logger.log("error", "CNB 未返回账号名，无法自动创建根组织");
        } else {
            logger.log(
                "info",
                format!("开始为 CNB 账号 {organization_path} 自动创建根组织"),
            );
            match api
                .ensure_organization(
                    &pat.token,
                    organization_path,
                    "CNB Bridge",
                    "CNB Bridge root organization",
                )
                .await
            {
                Ok(_) => {
                    needs_organization = false;
                    logger.log("info", format!("CNB 根组织 {organization_path} 已就绪"));
                }
                Err(error) => {
                    logger.log("error", format!("自动创建 CNB 根组织失败: {error}"));
                }
            }
        }
    }
    Ok(AuthorizationResult {
        pat,
        username,
        session,
        needs_organization,
    })
}

struct AuthorizationResult {
    pat: CreatedPat,
    username: Option<String>,
    session: WebSession,
    needs_organization: bool,
}

async fn wait_for_session(
    app: &tauri::AppHandle,
    cancel: Arc<AtomicBool>,
) -> Result<WebSession, String> {
    let cookie_url = Url::parse("https://cnb.cool/").map_err(|error| error.to_string())?;
    let client = WebAuthClient::new();
    for _ in 0..300 {
        if cancel.load(Ordering::SeqCst) {
            return Err("扫码绑定已取消".to_string());
        }
        let Some(window) = app.get_webview_window(WINDOW_LABEL) else {
            return Err("扫码窗口已关闭".to_string());
        };
        if let Ok(cookies) = window.cookies_for_url(cookie_url.clone()) {
            if let Ok(session) = session_from_cookies(
                cookies
                    .into_iter()
                    .map(|cookie| (cookie.name().to_string(), cookie.value().to_string())),
            ) {
                if client.owned_namespaces(&session).await.is_ok() {
                    return Ok(session);
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err("扫码登录超时，请重试".to_string())
}

fn close_and_clear(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.clear_all_browsing_data();
        let _ = window.destroy();
    }
}

/// Clears all persistent data used by the remote auth WebView.
pub fn clear_web_data(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.clear_all_browsing_data();
    } else {
        let _ = std::fs::remove_dir_all(paths::auth_webview_dir());
    }
}
