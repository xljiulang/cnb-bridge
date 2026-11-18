//! CNB web-session authorization helpers.

use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use xxhash_rust::xxh3::xxh3_64;

use crate::net;
use crate::ssh_key::public_key_hash;

/// Minimal scopes required by the bridge runtime.
pub const AUTH_SCOPES: &str = "account-profile:r,account-engage:rw,group-manage:rw,group-resource:rw,repo-code:rw,repo-basic-info:r,repo-cnb-trigger:rw,repo-cnb-detail:rw";

/// Builds the stable PAT name for one SSH public key and the current scopes.
pub fn pat_resource_name(public_key: &str) -> Result<String, String> {
    let hash = public_key_hash(public_key)? ^ xxh3_64(AUTH_SCOPES.as_bytes());
    Ok(crate::ssh_key::resource_name(hash))
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SshKeyRecord {
    id: String,
    name: String,
    public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatRecord {
    token_id: String,
    description: String,
}

/// Errors returned by CNB web-session endpoints.
#[derive(Debug, thiserror::Error)]
pub enum WebAuthError {
    #[error("网络请求失败: {message}")]
    Http {
        message: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("CNB 网页接口返回 {status}: {message}")]
    Remote { status: StatusCode, message: String },
    #[error("CNB 网页接口响应无效: {0}")]
    InvalidResponse(String),
    #[error("CNB 账号尚未完成实名认证")]
    IdentityVerificationRequired,
}

impl From<reqwest::Error> for WebAuthError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http {
            message: net::format_error_chain(&error),
            source: error,
        }
    }
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
        let keys = self.list_ssh_keys(session).await?;
        let name_matches = keys
            .iter()
            .filter(|key| key.name == name)
            .collect::<Vec<_>>();
        let exact_match = keys
            .iter()
            .any(|key| key.name == name && key.public_key.trim() == public_key.trim());
        if name_matches.len() == 1 && exact_match {
            return Ok(());
        }
        let conflicting_ids = keys
            .iter()
            .filter(|key| key.name == name || key.public_key.trim() == public_key.trim())
            .map(|key| key.id.clone())
            .collect::<Vec<_>>();
        self.delete_ssh_keys(session, conflicting_ids).await?;
        let encoded = url::form_urlencoded::byte_serialize(public_key.as_bytes())
            .collect::<String>()
            .replace('+', "%20");
        let body = json!({ "name": name, "public_key": encoded });
        match self
            .request(
                Method::POST,
                "/user/sign-ssh-keys",
                session,
                Some(body.clone()),
            )
            .await
        {
            Ok(_) => {}
            Err(WebAuthError::Remote {
                status: StatusCode::CONFLICT,
                message,
            }) if message
                .to_ascii_lowercase()
                .contains("key is already in use") =>
            {
                let keys = self.list_ssh_keys(session).await?;
                let existing_ids = keys
                    .into_iter()
                    .filter(|key| key.public_key.trim() == public_key.trim())
                    .map(|key| key.id)
                    .collect::<Vec<_>>();
                if !existing_ids.is_empty() {
                    self.delete_ssh_keys(session, existing_ids).await?;
                    self.request(Method::POST, "/user/sign-ssh-keys", session, Some(body))
                        .await?;
                }
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    async fn list_ssh_keys(&self, session: &WebSession) -> Result<Vec<SshKeyRecord>, WebAuthError> {
        let mut records = Vec::new();
        let mut page = 1;
        loop {
            let value = self
                .request(
                    Method::GET,
                    &format!("/user/sign-ssh-keys?page={page}&page_size=100"),
                    session,
                    None,
                )
                .await?;
            let page_records = parse_ssh_key_records(unwrap_result(value));
            records.extend(page_records);
            if records.len() < page * 100 {
                break;
            }
            page += 1;
        }
        Ok(records)
    }

    async fn delete_ssh_keys(
        &self,
        session: &WebSession,
        key_ids: Vec<String>,
    ) -> Result<(), WebAuthError> {
        let mut deleted = Vec::new();
        for key_id in key_ids {
            if key_id.is_empty() {
                return Err(WebAuthError::InvalidResponse(
                    "同名 SSH 公钥缺少 id".to_string(),
                ));
            }
            if deleted.contains(&key_id) {
                continue;
            }
            self.request(
                Method::DELETE,
                &format!("/user/sign-ssh-keys/{key_id}"),
                session,
                None,
            )
            .await?;
            deleted.push(key_id);
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
        let value = match self
            .request(
                Method::POST,
                "/user/personal_access_tokens",
                session,
                Some(body),
            )
            .await
        {
            Ok(value) => value,
            Err(WebAuthError::Remote {
                status: StatusCode::PRECONDITION_REQUIRED,
                ..
            }) => return Err(WebAuthError::IdentityVerificationRequired),
            Err(error) => return Err(error),
        };
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

    /// Returns the token ID for an exact PAT description.
    pub async fn find_pat_id_by_description(
        &self,
        session: &WebSession,
        description: &str,
    ) -> Result<Option<String>, WebAuthError> {
        let mut page = 1;
        loop {
            let value = self
                .request(
                    Method::GET,
                    &format!("/user/personal_access_tokens?page={page}&page_size=100"),
                    session,
                    None,
                )
                .await?;
            let records = parse_pat_records(unwrap_result(value));
            let page_len = records.len();
            if let Some(record) = records
                .into_iter()
                .find(|record| record.description == description)
            {
                return Ok(Some(record.token_id));
            }
            if page_len < 100 {
                break;
            }
            page += 1;
        }
        Ok(None)
    }

    /// Deletes a known PAT, treating a missing token as already removed.
    pub async fn delete_token_if_present(
        &self,
        session: &WebSession,
        token_id: &str,
    ) -> Result<(), WebAuthError> {
        match self
            .request(
                Method::DELETE,
                &format!("/user/personal_access_tokens/{token_id}"),
                session,
                None,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(WebAuthError::Remote {
                status: StatusCode::NOT_FOUND,
                ..
            }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        session: &WebSession,
        body: Option<Value>,
    ) -> Result<Value, WebAuthError> {
        let url = format!("{}{}", self.base_url, path);
        if method == Method::GET {
            let response = net::send_idempotent(|| {
                self.http
                    .get(&url)
                    .header("Accept", "application/vnd.cnb.web+json")
                    .header("Cookie", &session.cookie_header)
                    .header("Csrftoken", &session.csrf_token)
                    .send()
            })
            .await?;
            return parse_web_response(response.status, &response.text);
        }
        let mut request = self
            .http
            .request(method, url)
            .header("Accept", "application/vnd.cnb.web+json")
            .header("Cookie", &session.cookie_header)
            .header("Csrftoken", &session.csrf_token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        parse_web_response(status, &text)
    }
}

fn parse_web_response(status: StatusCode, text: &str) -> Result<Value, WebAuthError> {
    if !status.is_success() {
        let message = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|value| {
                value
                    .get("errmsg")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.to_string());
        return Err(WebAuthError::Remote { status, message });
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text)
        .map_err(|error| WebAuthError::InvalidResponse(format!("{error}: {text}")))
}

impl Default for WebAuthClient {
    fn default() -> Self {
        Self::new()
    }
}

fn unwrap_result(value: Value) -> Value {
    value.get("result").cloned().unwrap_or(value)
}

fn parse_ssh_key_records(value: Value) -> Vec<SshKeyRecord> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|key| SshKeyRecord {
            id: key
                .get("id")
                .or_else(|| key.get("key_id"))
                .and_then(scalar_string)
                .unwrap_or_default(),
            name: key
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            public_key: key
                .get("public_key")
                .or_else(|| key.get("publicKey"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
}

fn parse_pat_records(value: Value) -> Vec<PatRecord> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|token| PatRecord {
            token_id: token
                .get("token_id")
                .or_else(|| token.get("id"))
                .and_then(scalar_string)
                .unwrap_or_default(),
            description: token
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
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

    #[test]
    fn derives_the_pat_name_from_the_public_key_and_scope_hashes() {
        let public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti test@example.com";

        assert_eq!(
            pat_resource_name(public_key).unwrap(),
            "cnb-bridge-5484799157515295671"
        );
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
        let name = "cnb-bridge-123";
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{ "id": "1", "name": name, "public_key": public_key }]
            })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .ensure_ssh_key(&session(), name, public_key)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn migrates_a_registered_key_to_the_target_name() {
        let server = MockServer::start().await;
        let public_key = "ssh-ed25519 AAAATEST cnb";
        let target_name = "cnb-bridge-123";
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{
                    "id": "legacy",
                    "name": "CNB Bridge SHA256:LEGACY",
                    "public_key": public_key
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/user/sign-ssh-keys/legacy"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/sign-ssh-keys"))
            .and(body_json(json!({
                "name": target_name,
                "public_key": "ssh-ed25519%20AAAATEST%20cnb"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": {} })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .ensure_ssh_key(&session(), target_name, public_key)
            .await
            .unwrap();

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
    async fn replaces_a_key_with_different_content_at_the_target_name() {
        let server = MockServer::start().await;
        let public_key = "ssh-ed25519 AAAANEW cnb";
        let target_name = "cnb-bridge-123";
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{
                    "id": "conflict",
                    "name": target_name,
                    "public_key": "ssh-ed25519 AAAAOLD cnb"
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/user/sign-ssh-keys/conflict"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/sign-ssh-keys"))
            .and(body_json(json!({
                "name": target_name,
                "public_key": "ssh-ed25519%20AAAANEW%20cnb"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": {} })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .ensure_ssh_key(&session(), target_name, public_key)
            .await
            .unwrap();

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
    async fn replaces_duplicate_keys_at_the_target_name() {
        let server = MockServer::start().await;
        let public_key = "ssh-ed25519 AAAANEW cnb";
        let target_name = "cnb-bridge-123";
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [
                    { "id": "first", "name": target_name, "public_key": public_key },
                    { "id": "second", "name": target_name, "public_key": public_key }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/user/sign-ssh-keys/first"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/user/sign-ssh-keys/second"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": {} })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .ensure_ssh_key(&session(), target_name, public_key)
            .await
            .unwrap();

        let methods = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|request| request.method.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(methods, vec!["GET", "DELETE", "DELETE", "POST"]);
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
    async fn migrates_a_conflicting_legacy_key_after_a_create_conflict() {
        let server = MockServer::start().await;
        let public_key = "ssh-ed25519 AAAATEST cnb";
        let target_name = "cnb-bridge-123";
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": [] })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{
                    "id": "legacy",
                    "name": "CNB Bridge",
                    "public_key": public_key
                }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "errmsg": "the key is already in use"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/user/sign-ssh-keys/legacy"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/sign-ssh-keys"))
            .and(body_json(json!({
                "name": target_name,
                "public_key": "ssh-ed25519%20AAAATEST%20cnb"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": {} })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .ensure_ssh_key(&session(), target_name, public_key)
            .await
            .unwrap();

        let methods = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .map(|request| request.method.as_str().to_string())
            .collect::<Vec<_>>();
        assert_eq!(methods, vec!["GET", "POST", "GET", "DELETE", "POST"]);
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
    async fn finds_the_pat_id_by_its_exact_description() {
        let server = MockServer::start().await;
        let description = "cnb-bridge-5484799157515295671";
        Mock::given(method("GET"))
            .and(path("/user/personal_access_tokens"))
            .and(query_param("page", "1"))
            .and(query_param("page_size", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [
                    { "token_id": "target", "description": description },
                    { "token_id": "other", "description": "cnb-bridge-1" }
                ]
            })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        assert_eq!(
            client
                .find_pat_id_by_description(&session(), description)
                .await
                .unwrap()
                .as_deref(),
            Some("target")
        );
    }

    #[tokio::test]
    async fn maps_428_to_identity_verification_required() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(428).set_body_json(json!({
                "errmsg": "identity verification required"
            })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        let error = client
            .create_pat(&session(), "CNB Bridge", Some("xljiulang"))
            .await
            .unwrap_err();

        assert!(matches!(error, WebAuthError::IdentityVerificationRequired));
    }

    #[tokio::test]
    async fn keeps_other_create_pat_errors_as_remote_failures() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/personal_access_tokens"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "errmsg": "forbidden"
            })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        let error = client
            .create_pat(&session(), "CNB Bridge", Some("xljiulang"))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            WebAuthError::Remote {
                status: StatusCode::FORBIDDEN,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn treats_a_missing_previous_pat_as_already_deleted() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/user/personal_access_tokens/previous"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "errmsg": "not found"
            })))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        client
            .delete_token_if_present(&session(), "previous")
            .await
            .unwrap();
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

    #[tokio::test]
    async fn retries_transient_owned_namespace_reads() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "xljiulang",
                    "path": "xljiulang",
                    "access_role": "Owner"
                }
            ])))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        let namespaces = client.owned_namespaces(&session()).await.unwrap();

        assert_eq!(namespaces[0].path, "xljiulang");
    }

    #[tokio::test]
    async fn does_not_retry_ssh_key_creation_posts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/user/sign-ssh-keys"))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;
        let client = WebAuthClient::with_base_url(server.uri());

        let error = client
            .ensure_ssh_key(&session(), "CNB Bridge", "ssh-ed25519 public-key")
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            WebAuthError::Remote {
                status: StatusCode::SERVICE_UNAVAILABLE,
                ..
            }
        ));
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
        assert!(AUTH_SCOPES
            .split(',')
            .any(|scope| scope == "group-manage:rw"));
    }
}
