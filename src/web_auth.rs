//! CNB web-session authorization helpers.

use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

/// Minimal scopes required by the bridge runtime.
pub const AUTH_SCOPES: &str = "account-profile:r,account-engage:rw,group-resource:rw,repo-code:rw,repo-basic-info:r,repo-cnb-trigger:rw,repo-cnb-detail:rw";

/// Cookies and CSRF token extracted from the authenticated CNB WebView.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSession {
    pub cookie_header: String,
    pub csrf_token: String,
}

/// A CNB namespace visible to the authenticated web session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebNamespace {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub access_role: String,
}

/// A newly created personal access token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedPat {
    pub token_id: String,
    pub token: String,
    pub git_username: String,
}

/// Errors returned by CNB web-session endpoints.
#[derive(Debug, thiserror::Error)]
pub enum WebAuthError {
    #[error("网络请求失败: {0}")]
    Http(#[from] reqwest::Error),
    #[error("CNB 网页接口返回 {status}: {message}")]
    Remote { status: StatusCode, message: String },
    #[error("CNB 网页接口响应无效: {0}")]
    InvalidResponse(String),
}

/// Client for CNB browser-session endpoints.
pub struct WebAuthClient {
    http: Client,
    base_url: String,
}

impl WebAuthClient {
    /// Creates a client using the configured CNB platform URL.
    pub fn new() -> Self {
        Self::with_base_url(
            std::env::var("CNB_BRIDGE_PLATFORM_URL")
                .unwrap_or_else(|_| "https://cnb.cool".to_string()),
        )
    }

    /// Creates a client with an explicit URL for tests.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    /// Returns Owner namespaces for the current web login.
    pub async fn owned_namespaces(
        &self,
        session: &WebSession,
    ) -> Result<Vec<WebNamespace>, WebAuthError> {
        let value = self
            .request(Method::GET, "/user/groups", session, None)
            .await?;
        let namespaces = serde_json::from_value::<Vec<WebNamespace>>(unwrap_result(value))
            .map_err(|error| WebAuthError::InvalidResponse(error.to_string()))?;
        Ok(namespaces
            .into_iter()
            .filter(|namespace| {
                namespace.access_role.eq_ignore_ascii_case("Owner")
                    && !namespace.path.trim().is_empty()
            })
            .collect())
    }

    /// Uploads an SSH public key unless it is already registered.
    pub async fn ensure_ssh_key(
        &self,
        session: &WebSession,
        name: &str,
        public_key: &str,
    ) -> Result<(), WebAuthError> {
        let value = self
            .request(
                Method::GET,
                "/user/sign-ssh-keys?page=1&page_size=100",
                session,
                None,
            )
            .await?;
        let keys = unwrap_result(value);
        let already_exists = keys.as_array().is_some_and(|keys| {
            keys.iter().any(|key| {
                key.get("public_key")
                    .or_else(|| key.get("publicKey"))
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.trim() == public_key.trim())
            })
        });
        if already_exists {
            return Ok(());
        }
        let encoded = url::form_urlencoded::byte_serialize(public_key.as_bytes())
            .collect::<String>()
            .replace('+', "%20");
        match self
            .request(
                Method::POST,
                "/user/sign-ssh-keys",
                session,
                Some(json!({ "name": name, "public_key": encoded })),
            )
            .await
        {
            Ok(_) => {}
            Err(WebAuthError::Remote {
                status: StatusCode::CONFLICT,
                message,
            }) if message
                .to_ascii_lowercase()
                .contains("key is already in use") => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    /// Creates a permanent CNB personal access token.
    pub async fn create_pat(
        &self,
        session: &WebSession,
        description: &str,
        namespace: Option<&str>,
    ) -> Result<CreatedPat, WebAuthError> {
        let mut body = json!({
            "description": description,
            "scope": AUTH_SCOPES
        });
        if let Some(namespace) = namespace {
            body["slug"] = Value::String(namespace.to_string());
        }
        let value = self
            .request(
                Method::POST,
                "/user/personal_access_tokens",
                session,
                Some(body),
            )
            .await?;
        let value = unwrap_result(value);
        let token = value
            .get("token")
            .or_else(|| value.get("access_token"))
            .and_then(Value::as_str)
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| {
                WebAuthError::InvalidResponse("响应缺少 personal access token".to_string())
            })?;
        let token_id = value
            .get("token_id")
            .or_else(|| value.get("id"))
            .and_then(scalar_string)
            .unwrap_or_default();
        let git_username = value
            .get("git_username")
            .or_else(|| value.get("gitUsername"))
            .and_then(Value::as_str)
            .unwrap_or("cnb");
        Ok(CreatedPat {
            token_id: token_id.to_string(),
            token: token.to_string(),
            git_username: git_username.to_string(),
        })
    }

    /// Deletes old PATs with the same description, excluding the replacement.
    pub async fn delete_old_tokens(
        &self,
        session: &WebSession,
        description: &str,
        keep_token_id: &str,
    ) -> Result<(), WebAuthError> {
        let value = self
            .request(
                Method::GET,
                "/user/personal_access_tokens?page=1&page_size=100",
                session,
                None,
            )
            .await?;
        let value = unwrap_result(value);
        for token in value.as_array().into_iter().flatten() {
            let token_id = token
                .get("token_id")
                .or_else(|| token.get("id"))
                .and_then(scalar_string)
                .unwrap_or_default();
            if token_id.is_empty() || token_id == keep_token_id {
                continue;
            }
            if token.get("description").and_then(Value::as_str) == Some(description) {
                self.request(
                    Method::DELETE,
                    &format!("/user/personal_access_tokens/{token_id}"),
                    session,
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Renames a created token after its replacement has been validated.
    pub async fn rename_token(
        &self,
        session: &WebSession,
        token_id: &str,
        description: &str,
    ) -> Result<(), WebAuthError> {
        self.request(
            Method::PATCH,
            &format!("/user/personal_access_tokens/{token_id}"),
            session,
            Some(json!({ "description": description })),
        )
        .await?;
        Ok(())
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        session: &WebSession,
        body: Option<Value>,
    ) -> Result<Value, WebAuthError> {
        let mut request = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .header("Accept", "application/vnd.cnb.web+json")
            .header("Cookie", &session.cookie_header)
            .header("Csrftoken", &session.csrf_token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| {
                    value
                        .get("errmsg")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or(text);
            return Err(WebAuthError::Remote { status, message });
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text)
            .map_err(|error| WebAuthError::InvalidResponse(format!("{error}: {text}")))
    }
}

impl Default for WebAuthClient {
    fn default() -> Self {
        Self::new()
    }
}

fn unwrap_result(value: Value) -> Value {
    value.get("result").cloned().unwrap_or(value)
}

fn scalar_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

/// Builds the request session from WebView cookies.
pub fn session_from_cookies<I, K, V>(cookies: I) -> Result<WebSession, WebAuthError>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut pairs = Vec::new();
    let mut csrf_token = None;
    for (name, value) in cookies {
        let name = name.as_ref();
        let value = value.as_ref();
        pairs.push(format!("{name}={value}"));
        if name == "csrftoken" {
            csrf_token = Some(value.to_string());
        }
    }
    let csrf_token = csrf_token
        .ok_or_else(|| WebAuthError::InvalidResponse("登录会话缺少 csrftoken".to_string()))?;
    Ok(WebSession {
        cookie_header: pairs.join("; "),
        csrf_token,
    })
}

/// Returns whether a top-level URL is safe for the remote auth WebView.
pub fn is_allowed_auth_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host == "cnb.cool" || host.ends_with(".cnb.cool"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn session() -> WebSession {
        WebSession {
            cookie_header: "session=abc; csrftoken=csrf".to_string(),
            csrf_token: "csrf".to_string(),
        }
    }

    #[tokio::test]
    async fn uploads_missing_ssh_key_with_web_session_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .and(query_param("page", "1"))
            .and(query_param("page_size", "100"))
            .and(header("cookie", "session=abc; csrftoken=csrf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": []
            })))
            .mount(&server)
            .await;
        let public_key = "ssh-ed25519 AAAATEST cnb";
        Mock::given(method("POST"))
            .and(path("/user/sign-ssh-keys"))
            .and(header("cookie", "session=abc; csrftoken=csrf"))
            .and(header("csrftoken", "csrf"))
            .and(header("accept", "application/vnd.cnb.web+json"))
            .and(body_json(json!({
                "name": "CNB Bridge",
                "public_key": "ssh-ed25519%20AAAATEST%20cnb"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": {} })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .ensure_ssh_key(&session(), "CNB Bridge", public_key)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn skips_public_key_that_is_already_registered() {
        let server = MockServer::start().await;
        let public_key = "ssh-ed25519 AAAATEST cnb";
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{ "id": "1", "name": "Existing", "public_key": public_key }]
            })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .ensure_ssh_key(&session(), "CNB Bridge", public_key)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn treats_already_registered_key_conflict_as_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": []
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "errmsg": "the key is already in use"
            })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .ensure_ssh_key(&session(), "CNB Bridge", "ssh-ed25519 AAAATEST cnb")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn creates_scoped_permanent_pat() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/personal_access_tokens"))
            .and(header("cookie", "session=abc; csrftoken=csrf"))
            .and(header("csrftoken", "csrf"))
            .and(body_json(json!({
                "description": "CNB Bridge",
                "scope": AUTH_SCOPES,
                "slug": "xljiulang"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "result": {
                    "token_id": "token-1",
                    "token": "pat-value",
                    "git_username": "cnb"
                }
            })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        let token = client
            .create_pat(&session(), "CNB Bridge", Some("xljiulang"))
            .await
            .unwrap();

        assert_eq!(token.token_id, "token-1");
        assert_eq!(token.token, "pat-value");
        assert_eq!(token.git_username, "cnb");
    }

    #[tokio::test]
    async fn parses_numeric_token_id_without_losing_cleanup_guard() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "result": {
                    "token_id": 123456789,
                    "token": "pat-value",
                    "git_username": "cnb"
                }
            })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        let token = client
            .create_pat(&session(), "CNB Bridge", Some("xljiulang"))
            .await
            .unwrap();

        assert_eq!(token.token_id, "123456789");
    }

    #[tokio::test]
    async fn deletes_only_old_bridge_tokens_after_replacement_exists() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [
                    { "token_id": "old", "description": "CNB Bridge" },
                    { "token_id": "new", "description": "CNB Bridge" },
                    { "token_id": "other", "description": "Other app" }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/user/personal_access_tokens/old"))
            .and(header("csrftoken", "csrf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .delete_old_tokens(&session(), "CNB Bridge", "new")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn renames_replacement_token_after_validation() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/user/personal_access_tokens/new"))
            .and(header("csrftoken", "csrf"))
            .and(body_json(json!({ "description": "CNB Bridge" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .rename_token(&session(), "new", "CNB Bridge")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn returns_only_owner_namespaces_from_web_session() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .and(header("cookie", "session=abc; csrftoken=csrf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "name": "xljiulang", "path": "xljiulang", "access_role": "Owner" },
                { "name": "team", "path": "team", "access_role": "Master" }
            ])))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        let namespaces = client.owned_namespaces(&session()).await.unwrap();

        assert_eq!(namespaces.len(), 1);
        assert_eq!(namespaces[0].path, "xljiulang");
    }

    #[test]
    fn builds_session_cookie_header_and_requires_csrf() {
        let session =
            session_from_cookies([("session", "abc"), ("csrftoken", "csrf"), ("theme", "dark")])
                .unwrap();

        assert_eq!(
            session.cookie_header,
            "session=abc; csrftoken=csrf; theme=dark"
        );
        assert_eq!(session.csrf_token, "csrf");

        assert!(session_from_cookies([("session", "abc")]).is_err());
    }

    #[test]
    fn allows_only_cnb_navigation_in_auth_webview() {
        assert!(is_allowed_auth_url(
            &url::Url::parse("https://cnb.cool/profile/token").unwrap()
        ));
        assert!(is_allowed_auth_url(
            &url::Url::parse("https://api.cnb.cool/user").unwrap()
        ));
        assert!(!is_allowed_auth_url(
            &url::Url::parse("https://example.com/").unwrap()
        ));
        assert!(!is_allowed_auth_url(
            &url::Url::parse("http://cnb.cool/").unwrap()
        ));
    }

    #[test]
    fn auth_scopes_allow_workspace_cleanup() {
        assert!(AUTH_SCOPES
            .split(',')
            .any(|scope| scope == "account-engage:rw"));
    }
}
