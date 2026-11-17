use crate::state::DesktopState;
use cnb_bridge::account::{AccountAction, AccountError, AccountManager, AccountStatus};
use cnb_bridge::api::CnbApi;
use cnb_bridge::logging::Logger;
use cnb_bridge::paths;
use cnb_bridge::ssh_key::public_key_resource_name;
use cnb_bridge::web_auth::{
    is_allowed_auth_url, pat_resource_name, session_from_cookies, CreatedPat, WebAuthClient,
    WebAuthError, WebSession,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use url::Url;

const AUTH_URL: &str = "https://cnb.cool/?s_url=%2Fprofile%2Ftoken";
const WINDOW_LABEL: &str = "cnb-auth";
const IDENTITY_VERIFICATION_URL: &str = "https://cnb.cool/profile/auth";
const IDENTITY_VERIFICATION_MESSAGE: &str =
    "当前 CNB 账号尚未完成实名认证，无法创建访问令牌。请先完成实名认证，然后重新扫码登录。";
const IDENTITY_VERIFICATION_LABEL: &str = "实名认证 · 个人设置";

/// Opens the CNB web authorization window.
pub fn launch(app: tauri::AppHandle) -> Result<AccountStatus, String> {
    let state = app.state::<DesktopState>();
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(state.account.status());
    }
    state.account.begin_token_login();
    let generation = state.login_generation.fetch_add(1, Ordering::SeqCst) + 1;

    let url = Url::parse(AUTH_URL).map_err(|error| error.to_string())?;
    let window = match WebviewWindowBuilder::new(&app, WINDOW_LABEL, WebviewUrl::External(url))
        .title("CNB 扫码绑定")
        .inner_size(680.0, 700.0)
        .min_inner_size(520.0, 560.0)
        .resizable(true)
        .center()
        .data_directory(paths::auth_webview_dir())
        .on_navigation(is_allowed_auth_url)
        .build()
    {
        Ok(window) => window,
        Err(error) => {
            state.account.fail_token_login(error.to_string());
            return Err(error.to_string());
        }
    };
    let close_requested = Arc::new(AtomicBool::new(false));
    let close_signal = Arc::clone(&close_requested);
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            close_signal.store(true, Ordering::SeqCst);
        }
        tauri::WindowEvent::Destroyed => {
            close_signal.store(true, Ordering::SeqCst);
        }
        _ => {}
    });

    let account = Arc::clone(&state.account);
    let bridge = Arc::clone(&state.bridge);
    let logger = Arc::clone(&state.logger);
    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = authorize(
            app_for_task.clone(),
            Arc::clone(&close_requested),
            Arc::clone(&account),
            Arc::clone(&bridge),
            Arc::clone(&logger),
        )
        .await;
        match result {
            Ok(authorized) => match complete_authorized_pat(&account, &authorized, &logger).await {
                Ok(_) => {
                    bridge.account_connected().await;
                }
                Err(error) => {
                    account.fail_token_login(error.to_string());
                    logger.log("error", format!("保存 CNB 访问令牌失败: {error}"));
                }
            },
            Err(AuthorizationFailure::IdentityVerificationRequired) => {
                account.fail_token_login_with_action(
                    IDENTITY_VERIFICATION_MESSAGE,
                    AccountAction {
                        label: IDENTITY_VERIFICATION_LABEL.to_string(),
                        url: IDENTITY_VERIFICATION_URL.to_string(),
                    },
                );
                logger.log("warn", IDENTITY_VERIFICATION_MESSAGE);
            }
            Err(AuthorizationFailure::Message(error)) => {
                account.fail_token_login(error.clone());
                logger.log("error", format!("CNB 扫码绑定失败: {error}"));
            }
            Err(AuthorizationFailure::Cancelled) => {
                account.cancel_token_login();
                bridge.account_signed_out().await;
                logger.log("info", "CNB 扫码登录已取消");
            }
        }
        close_and_clear(&app_for_task);
        let _ = app_for_task.emit("cnb-bridge://snapshot", generation);
    });

    Ok(state.account.status())
}

async fn authorize(
    app: tauri::AppHandle,
    close_requested: Arc<AtomicBool>,
    account: Arc<AccountManager>,
    bridge: Arc<cnb_bridge::bridge::Bridge>,
    logger: Arc<cnb_bridge::logging::Logger>,
) -> Result<AuthorizationResult, AuthorizationFailure> {
    let session = wait_for_session(&app, &close_requested).await?;
    let client = WebAuthClient::new();
    let namespaces = client
        .owned_namespaces(&session)
        .await
        .map_err(|error| format!("读取 CNB 命名空间失败: {error}"))?;
    let namespace = (namespaces.len() == 1).then(|| namespaces[0].path.as_str());
    let public_key = bridge.ensure_ssh_key()?;
    let public_key_name = public_key_resource_name(&public_key)?;
    let pat_name = pat_resource_name(&public_key)?;
    client
        .ensure_ssh_key(&session, &public_key_name, &public_key)
        .await
        .map_err(|error| format!("自动添加 SSH 公钥失败: {error}"))?;
    let api = CnbApi::new();
    let ResolvedPat { pat, reused } =
        resolve_named_pat(&account, &client, &api, &session, &pat_name, namespace).await?;
    let user = api
        .current_user(&pat.token)
        .await
        .map_err(|error| format!("PAT 验证失败，请重新扫码重试: {error}"))?;
    let owned_namespaces = api
        .list_owned_namespaces(&pat.token)
        .await
        .map_err(|error| format!("PAT 无法检查 CNB Owner 组织，请重新扫码重试: {error}"))?;
    let nickname = (!user.nickname.trim().is_empty()).then_some(user.nickname);
    let username = (!user.username.trim().is_empty()).then_some(user.username);
    if username.is_none() {
        return Err("PAT 已就绪，但 CNB 未返回账号名，请重新扫码重试"
            .to_string()
            .into());
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
        nickname,
        needs_organization,
        reused_pat: reused,
    })
}

struct AuthorizationResult {
    pat: CreatedPat,
    username: Option<String>,
    nickname: Option<String>,
    needs_organization: bool,
    reused_pat: bool,
}

#[derive(Debug)]
struct ResolvedPat {
    pat: CreatedPat,
    reused: bool,
}

async fn resolve_named_pat(
    account: &AccountManager,
    web_client: &WebAuthClient,
    api: &CnbApi,
    session: &WebSession,
    description: &str,
    namespace: Option<&str>,
) -> Result<ResolvedPat, AuthorizationFailure> {
    if let Some(token_id) = web_client
        .find_pat_id_by_description(session, description)
        .await
        .map_err(|error| format!("读取同名 PAT 失败: {error}"))?
    {
        if let Some(credentials) = account.credentials_by_token_id(&token_id) {
            match api.current_user(&credentials.access_token).await {
                Ok(_) => {
                    return Ok(ResolvedPat {
                        pat: CreatedPat {
                            token_id,
                            token: credentials.access_token,
                            git_username: "cnb".to_string(),
                        },
                        reused: true,
                    });
                }
                Err(error) if !error.is_auth_rejection() => {
                    return Err(format!("验证本机已有 PAT 失败: {error}").into());
                }
                Err(_) => {}
            }
        }
        web_client
            .delete_token_if_present(session, &token_id)
            .await
            .map_err(|error| format!("删除不可复用的同名 PAT 失败: {error}"))?;
    }

    match web_client.create_pat(session, description, namespace).await {
        Ok(pat) => Ok(ResolvedPat { pat, reused: false }),
        Err(WebAuthError::IdentityVerificationRequired) => {
            Err(AuthorizationFailure::IdentityVerificationRequired)
        }
        Err(error) => Err(format!("创建 PAT 失败: {error}").into()),
    }
}

async fn complete_authorized_pat(
    account: &AccountManager,
    authorized: &AuthorizationResult,
    logger: &Logger,
) -> Result<AccountStatus, AccountError> {
    let new_token_id =
        (!authorized.pat.token_id.trim().is_empty()).then(|| authorized.pat.token_id.clone());
    account.complete_token_login(
        authorized.pat.token.clone(),
        authorized.username.clone(),
        authorized.nickname.clone(),
        new_token_id.clone(),
        None,
    )?;
    let status = account.set_needs_organization(authorized.needs_organization);
    logger.log(
        "info",
        if authorized.reused_pat {
            "Reused the saved CNB personal access token"
        } else {
            "Created a new CNB personal access token"
        },
    );

    Ok(status)
}

#[derive(Debug)]
enum AuthorizationFailure {
    Message(String),
    IdentityVerificationRequired,
    Cancelled,
}

impl From<String> for AuthorizationFailure {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

fn should_poll_session(close_requested: bool, window_present: bool) -> bool {
    !close_requested && window_present
}

async fn wait_for_session(
    app: &tauri::AppHandle,
    close_requested: &AtomicBool,
) -> Result<WebSession, AuthorizationFailure> {
    let cookie_url = Url::parse("https://cnb.cool/").map_err(|error| error.to_string())?;
    let client = WebAuthClient::new();
    for _ in 0..300 {
        let window = app.get_webview_window(WINDOW_LABEL);
        if !should_poll_session(close_requested.load(Ordering::SeqCst), window.is_some()) {
            return Err(AuthorizationFailure::Cancelled);
        }
        let window = window.expect("window presence checked");
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
    Err("扫码登录超时，请重试".to_string().into())
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

#[cfg(test)]
mod tests {
    use super::*;
    use cnb_bridge::account::{AccountManager, AccountState, AccountStore};
    use cnb_bridge::logging::Logger;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn reuses_a_named_pat_backed_by_valid_local_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account-auth.bin")));
        account
            .complete_token_login(
                "saved-token",
                Some("alice".to_string()),
                None,
                Some("existing-id".to_string()),
                None,
            )
            .unwrap();
        let server = MockServer::start().await;
        let description = "cnb-bridge-5484799157515295671";
        Mock::given(method("GET"))
            .and(path("/user/personal_access_tokens"))
            .and(query_param("page", "1"))
            .and(query_param("page_size", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{
                    "token_id": "existing-id",
                    "description": description
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", "Bearer saved-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "username": "alice" })))
            .mount(&server)
            .await;
        let web_client = WebAuthClient::with_base_url(server.uri());
        let api = CnbApi::with_base_url(server.uri());

        let resolved = resolve_named_pat(
            &account,
            &web_client,
            &api,
            &WebSession {
                cookie_header: "session=abc; csrftoken=csrf".to_string(),
                csrf_token: "csrf".to_string(),
            },
            description,
            None,
        )
        .await
        .unwrap();

        assert!(resolved.reused);
        assert_eq!(resolved.pat.token_id, "existing-id");
        assert_eq!(resolved.pat.token, "saved-token");
    }

    #[test]
    fn close_request_prevents_further_session_polls() {
        assert!(should_poll_session(false, true));
        assert!(!should_poll_session(false, false));
        assert!(!should_poll_session(true, true));
        assert!(!should_poll_session(true, false));
    }

    #[tokio::test]
    async fn recreates_an_existing_named_pat_without_local_plaintext() {
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account-auth.bin")));
        let server = MockServer::start().await;
        let description = "cnb-bridge-5484799157515295671";
        Mock::given(method("GET"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{ "token_id": "existing-id", "description": description }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/user/personal_access_tokens/existing-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "result": {
                    "token_id": "new-id",
                    "token": "new-token",
                    "git_username": "cnb"
                }
            })))
            .mount(&server)
            .await;
        let web_client = WebAuthClient::with_base_url(server.uri());
        let api = CnbApi::with_base_url(server.uri());

        let resolved = resolve_named_pat(
            &account,
            &web_client,
            &api,
            &WebSession {
                cookie_header: "session=abc; csrftoken=csrf".to_string(),
                csrf_token: "csrf".to_string(),
            },
            description,
            None,
        )
        .await
        .unwrap();

        assert!(!resolved.reused);
        assert_eq!(resolved.pat.token_id, "new-id");
        assert_eq!(resolved.pat.token, "new-token");
        let methods = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|request| request.method.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(methods, vec!["GET", "DELETE", "POST"]);
    }

    #[tokio::test]
    async fn creates_a_pat_without_deleting_a_legacy_name() {
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account-auth.bin")));
        let server = MockServer::start().await;
        let description = "cnb-bridge-5484799157515295671";
        Mock::given(method("GET"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{
                    "token_id": "legacy-id",
                    "description": "CNB Bridge"
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "result": {
                    "token_id": "new-id",
                    "token": "new-token",
                    "git_username": "cnb"
                }
            })))
            .mount(&server)
            .await;
        let web_client = WebAuthClient::with_base_url(server.uri());
        let api = CnbApi::with_base_url(server.uri());

        let resolved = resolve_named_pat(
            &account,
            &web_client,
            &api,
            &WebSession {
                cookie_header: "session=abc; csrftoken=csrf".to_string(),
                csrf_token: "csrf".to_string(),
            },
            description,
            None,
        )
        .await
        .unwrap();

        assert!(!resolved.reused);
        assert_eq!(resolved.pat.token, "new-token");
        let methods = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|request| request.method.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(methods, vec!["GET", "POST"]);
    }

    #[tokio::test]
    async fn does_not_create_a_pat_when_named_cleanup_fails() {
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account-auth.bin")));
        let server = MockServer::start().await;
        let description = "cnb-bridge-5484799157515295671";
        Mock::given(method("GET"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{ "token_id": "existing-id", "description": description }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/user/personal_access_tokens/existing-id"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "errmsg": "delete failed"
            })))
            .mount(&server)
            .await;
        let web_client = WebAuthClient::with_base_url(server.uri());
        let api = CnbApi::with_base_url(server.uri());

        let error = resolve_named_pat(
            &account,
            &web_client,
            &api,
            &WebSession {
                cookie_header: "session=abc; csrftoken=csrf".to_string(),
                csrf_token: "csrf".to_string(),
            },
            description,
            None,
        )
        .await
        .unwrap_err();

        assert!(format!("{error:?}").contains("删除不可复用的同名 PAT 失败"));
        let methods = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|request| request.method.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(methods, vec!["GET", "DELETE"]);
    }

    #[tokio::test]
    async fn recreates_an_existing_named_pat_when_local_credentials_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account-auth.bin")));
        account
            .complete_token_login(
                "expired-token",
                Some("alice".to_string()),
                None,
                Some("existing-id".to_string()),
                None,
            )
            .unwrap();
        let server = MockServer::start().await;
        let description = "cnb-bridge-5484799157515295671";
        Mock::given(method("GET"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{ "token_id": "existing-id", "description": description }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", "Bearer expired-token"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "message": "forbidden"
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/user/personal_access_tokens/existing-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "result": {
                    "token_id": "new-id",
                    "token": "new-token",
                    "git_username": "cnb"
                }
            })))
            .mount(&server)
            .await;
        let web_client = WebAuthClient::with_base_url(server.uri());
        let api = CnbApi::with_base_url(server.uri());

        let resolved = resolve_named_pat(
            &account,
            &web_client,
            &api,
            &WebSession {
                cookie_header: "session=abc; csrftoken=csrf".to_string(),
                csrf_token: "csrf".to_string(),
            },
            description,
            None,
        )
        .await
        .unwrap();

        assert!(!resolved.reused);
        assert_eq!(resolved.pat.token_id, "new-id");
    }

    #[tokio::test]
    async fn saves_the_new_pat_and_token_id() {
        let temp = tempfile::tempdir().unwrap();
        let account =
            AccountManager::with_store(AccountStore::new(temp.path().join("account-auth.bin")));
        account
            .complete_token_login(
                "old-token",
                Some("alice".to_string()),
                None,
                Some("old-id".to_string()),
                None,
            )
            .unwrap();
        let logger = Logger::new(32);

        let status = complete_authorized_pat(
            &account,
            &AuthorizationResult {
                pat: CreatedPat {
                    token_id: "new-id".to_string(),
                    token: "new-token".to_string(),
                    git_username: "cnb".to_string(),
                },
                username: Some("alice".to_string()),
                nickname: Some("Alice 昵称".to_string()),
                needs_organization: false,
                reused_pat: false,
            },
            &logger,
        )
        .await
        .unwrap();

        assert_eq!(status.state, AccountState::Connected);
        assert_eq!(status.display_name.as_deref(), Some("Alice 昵称"));
        let credentials = account.credentials_for("alice").unwrap();
        assert_eq!(credentials.access_token, "new-token");
        assert_eq!(credentials.token_id.as_deref(), Some("new-id"));
    }
}
