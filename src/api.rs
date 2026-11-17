//! Minimal CNB OpenAPI client used by the bridge lifecycle.

pub use reqwest::StatusCode;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::net;

/// Default CNB OpenAPI endpoint.
pub const DEFAULT_API_URL: &str = "https://api.cnb.cool";

/// Errors returned by the CNB API.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("网络请求失败: {message}")]
    Http {
        message: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("CNB API 返回 {status}: {message}")]
    Remote {
        status: StatusCode,
        code: Option<i64>,
        message: String,
    },
    #[error("响应格式无效: {0}")]
    InvalidResponse(String),
}

impl ApiError {
    /// Returns whether the remote endpoint definitively rejected the credential.
    pub fn is_auth_rejection(&self) -> bool {
        matches!(
            self,
            Self::Remote {
                status: StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN,
                ..
            }
        )
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http {
            message: net::format_error_chain(&error),
            source: error,
        }
    }
}

/// The current CNB user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub nickname: String,
}

/// A top-level CNB namespace in which the current user can create repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Namespace {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub access_role: String,
}

/// Request body for creating a top-level CNB organization.
#[derive(Debug, Clone, Serialize)]
pub struct CreateGroupRequest<'a> {
    pub path: &'a str,
    pub remark: &'a str,
    pub description: &'a str,
}

/// A repository entry returned by `/user/repos`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub web_url: Option<String>,
}

/// A repository branch returned by the Git API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    #[serde(default)]
    pub name: String,
}

/// A cloud-native development workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Workspace {
    #[serde(default)]
    pub sn: String,
    #[serde(default)]
    pub pipeline_id: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceList {
    #[serde(default)]
    pub list: Vec<Workspace>,
}

/// SSH and IDE access information for a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDetail {
    #[serde(default)]
    pub remote_ssh: Option<String>,
    #[serde(default)]
    pub ssh: Option<String>,
    #[serde(default)]
    pub webide: Option<String>,
    #[serde(default)]
    pub jump_url: Option<String>,
}

/// AI Credits summary for one CNB organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CnbUsageSummary {
    pub namespace: String,
    pub month: String,
    pub dev_cpu_used_seconds: i64,
    pub dev_cpu_total_seconds: i64,
    pub credits_used_milli: i64,
    pub credits_total_milli: i64,
    pub fetched_at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct QuotaItem {
    #[serde(default)]
    total: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct UsageQuota {
    #[serde(default)]
    credit_in_milli: QuotaItem,
    #[serde(default)]
    dev_in_sec: QuotaItem,
}

#[derive(Debug, Clone, Deserialize)]
struct UsageVolume {
    #[serde(default)]
    credit_in_milli: i64,
    #[serde(default)]
    dev_in_sec: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct FileContent {
    #[serde(default)]
    content: String,
    #[serde(default)]
    encoding: String,
}

/// Client for the CNB OpenAPI.
pub struct CnbApi {
    http: Client,
    base_url: String,
}

impl CnbApi {
    /// Creates a client using the official API endpoint.
    pub fn new() -> Self {
        Self::with_base_url(
            std::env::var("CNB_BRIDGE_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string()),
        )
    }

    /// Creates a client with an explicit endpoint (used by tests).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    /// Returns the current user.
    pub async fn current_user(&self, token: &str) -> Result<User, ApiError> {
        self.send_json(Method::GET, "/user", token, None::<&()>)
            .await
    }

    /// Lists top-level namespaces owned by the current user.
    pub async fn list_owned_namespaces(&self, token: &str) -> Result<Vec<Namespace>, ApiError> {
        let namespaces: Vec<Namespace> = self
            .send_json(
                Method::GET,
                "/user/groups?page=1&page_size=100",
                token,
                None::<&()>,
            )
            .await?;
        Ok(namespaces
            .into_iter()
            .filter(|namespace| {
                namespace.access_role.eq_ignore_ascii_case("Owner")
                    && !namespace.path.trim().is_empty()
            })
            .collect())
    }

    /// Creates a root organization and confirms that the account owns it.
    ///
    /// A conflict is treated as success only when the refreshed ownership list
    /// already contains the requested organization path.
    pub async fn ensure_organization(
        &self,
        token: &str,
        path: &str,
        remark: &str,
        description: &str,
    ) -> Result<Vec<Namespace>, ApiError> {
        let body = CreateGroupRequest {
            path,
            remark,
            description,
        };
        match self
            .send_json::<serde_json::Value, _>(Method::POST, "/groups", token, Some(&body))
            .await
        {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error,
                    ApiError::Remote {
                        status: StatusCode::CONFLICT,
                        ..
                    }
                ) =>
            {
                let namespaces = self.list_owned_namespaces(token).await?;
                if namespaces.iter().any(|namespace| namespace.path == path) {
                    return Ok(namespaces);
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        }

        let namespaces = self.list_owned_namespaces(token).await?;
        if namespaces.iter().any(|namespace| namespace.path == path) {
            Ok(namespaces)
        } else {
            Err(ApiError::InvalidResponse(format!(
                "组织 {path} 已创建，但当前账号尚未获得 Owner 权限"
            )))
        }
    }

    /// Returns the current month AI Credits and development CPU quota usage.
    ///
    /// This is a control-plane request and never starts a workspace.
    pub async fn usage_summary(
        &self,
        token: &str,
        preferred_namespace: Option<&str>,
    ) -> Result<CnbUsageSummary, ApiError> {
        let namespaces = self.list_owned_namespaces(token).await?;
        let namespace = preferred_namespace
            .filter(|preferred| {
                namespaces
                    .iter()
                    .any(|namespace| namespace.path == *preferred)
            })
            .map(str::to_string)
            .or_else(|| namespaces.first().map(|namespace| namespace.path.clone()))
            .ok_or_else(|| {
                ApiError::InvalidResponse("当前账号没有可读取用量的 Owner 组织".to_string())
            })?;
        let quota_path = format!("/{namespace}/-/charge/quota");
        let volume_path = format!("/{namespace}/-/charge/volume");
        let (quota, volume) = tokio::join!(
            self.send_json::<UsageQuota, ()>(Method::GET, &quota_path, token, None),
            self.send_json::<UsageVolume, ()>(Method::GET, &volume_path, token, None)
        );
        let quota = quota?;
        let volume = volume?;
        Ok(CnbUsageSummary {
            namespace,
            month: chrono::Local::now().format("%Y-%m").to_string(),
            dev_cpu_used_seconds: volume.dev_in_sec,
            dev_cpu_total_seconds: quota.dev_in_sec.total,
            credits_used_milli: volume.credit_in_milli,
            credits_total_milli: quota.credit_in_milli.total,
            fetched_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Returns whether a file exists on a specific branch.
    pub async fn file_exists(
        &self,
        token: &str,
        slug: &str,
        file_path: &str,
        reference: &str,
    ) -> Result<bool, ApiError> {
        let url = format!(
            "{}/{slug}/-/git/contents/{file_path}?ref={reference}",
            self.base_url
        );
        let response = net::send_idempotent(|| {
            self.http
                .get(&url)
                .bearer_auth(token)
                .header("Accept", "application/vnd.cnb.api+json")
                .send()
        })
        .await?;
        let status = response.status;
        if status.is_success() {
            return Ok(true);
        }
        if status == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let (code, message) = parse_remote_error(&response.text);
        Err(ApiError::Remote {
            status,
            code,
            message,
        })
    }

    /// Returns UTF-8 text from a repository file.
    pub async fn file_text(
        &self,
        token: &str,
        slug: &str,
        file_path: &str,
        reference: &str,
    ) -> Result<String, ApiError> {
        use base64::Engine as _;

        let path = format!("/{slug}/-/git/contents/{file_path}?ref={reference}");
        let content: FileContent = self
            .send_json(Method::GET, &path, token, None::<&()>)
            .await?;
        if content.encoding != "base64" {
            return Err(ApiError::InvalidResponse(format!(
                "文件 {file_path} 未返回 base64 内容"
            )));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content.content)
            .map_err(|error| ApiError::InvalidResponse(format!("文件内容解码失败: {error}")))?;
        String::from_utf8(bytes)
            .map_err(|error| ApiError::InvalidResponse(format!("文件内容不是 UTF-8: {error}")))
    }

    /// Lists branches in a repository.
    pub async fn list_branches(&self, token: &str, slug: &str) -> Result<Vec<Branch>, ApiError> {
        let path = format!("/{slug}/-/git/branches?page=1&page_size=100");
        self.send_json(Method::GET, &path, token, None::<&()>).await
    }

    /// Lists repositories visible to the current user.
    pub async fn list_repos(&self, token: &str) -> Result<Vec<Repo>, ApiError> {
        self.send_json(
            Method::GET,
            "/user/repos?page=1&page_size=100",
            token,
            None::<&()>,
        )
        .await
    }

    /// Creates a private repository under a user or group namespace.
    pub async fn create_repo(
        &self,
        token: &str,
        namespace: &str,
        name: &str,
        description: &str,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({
            "name": name,
            "description": description,
            "visibility": "private"
        });
        let path = format!("/{namespace}/-/repos");
        let _: serde_json::Value = self
            .send_json(Method::POST, &path, token, Some(&body))
            .await?;
        Ok(())
    }

    /// Lists workspaces for a repository.
    pub async fn list_workspaces(
        &self,
        token: &str,
        slug: &str,
    ) -> Result<WorkspaceList, ApiError> {
        let path = format!("/workspace/list?slug={slug}");
        self.send_json(Method::GET, &path, token, None::<&()>).await
    }

    /// Starts or reuses the default dev environment for a repository.
    pub async fn start_workspace(
        &self,
        token: &str,
        slug: &str,
        branch: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let body = serde_json::json!({ "branch": branch });
        let path = format!("/{slug}/-/workspace/start");
        self.send_json(Method::POST, &path, token, Some(&body))
            .await
    }

    /// Fetches the SSH/WebIDE access information for one workspace.
    pub async fn workspace_detail(
        &self,
        token: &str,
        slug: &str,
        sn: &str,
    ) -> Result<WorkspaceDetail, ApiError> {
        let path = format!("/{slug}/-/workspace/detail/{sn}");
        self.send_json(Method::GET, &path, token, None::<&()>).await
    }

    /// Stops a workspace by serial number.
    pub async fn stop_workspace(&self, token: &str, sn: &str) -> Result<(), ApiError> {
        let body = serde_json::json!({ "sn": sn });
        let _: serde_json::Value = self
            .send_json(Method::POST, "/workspace/stop", token, Some(&body))
            .await?;
        Ok(())
    }

    async fn send_json<T, B>(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<&B>,
    ) -> Result<T, ApiError>
    where
        T: for<'de> Deserialize<'de>,
        B: Serialize + ?Sized,
    {
        let url = format!("{}{}", self.base_url, path);
        if method == Method::GET {
            let response = net::send_idempotent(|| {
                self.http
                    .get(&url)
                    .bearer_auth(token)
                    .header("Accept", "application/vnd.cnb.api+json")
                    .send()
            })
            .await?;
            return parse_json_response(response.status, &response.text);
        }
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.cnb.api+json");
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        parse_json_response(status, &text)
    }
}

fn parse_json_response<T>(status: StatusCode, text: &str) -> Result<T, ApiError>
where
    T: for<'de> Deserialize<'de>,
{
    if !status.is_success() {
        let (code, message) = parse_remote_error(text);
        return Err(ApiError::Remote {
            status,
            code,
            message,
        });
    }
    if text.trim().is_empty() {
        serde_json::from_str("null").map_err(|error| ApiError::InvalidResponse(error.to_string()))
    } else {
        serde_json::from_str(text)
            .map_err(|error| ApiError::InvalidResponse(format!("{error}: {text}")))
    }
}

impl Default for CnbApi {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_remote_error(text: &str) -> (Option<i64>, String) {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        let code = value.get("errcode").and_then(|v| v.as_i64());
        let message = value
            .get("errmsg")
            .and_then(|v| v.as_str())
            .unwrap_or(text)
            .to_string();
        return (code, message);
    }
    (None, text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_cnb_error_envelope() {
        let (code, message) =
            parse_remote_error(r#"{"errcode":10024,"errmsg":"resource mismatch"}"#);
        assert_eq!(code, Some(10024));
        assert_eq!(message, "resource mismatch");
    }

    #[test]
    fn classifies_only_401_and_403_as_auth_rejections() {
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            assert!(ApiError::Remote {
                status,
                code: None,
                message: "denied".to_string(),
            }
            .is_auth_rejection());
        }
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(!ApiError::Remote {
                status,
                code: None,
                message: "retry".to_string(),
            }
            .is_auth_rejection());
        }
    }

    #[tokio::test]
    async fn lists_only_owned_namespaces() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .and(query_param("page", "1"))
            .and(query_param("page_size", "100"))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "xljiulang",
                    "path": "xljiulang",
                    "access_role": "Owner"
                },
                {
                    "name": "other-team",
                    "path": "other-team",
                    "access_role": "Master"
                }
            ])))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let namespaces = api.list_owned_namespaces("token").await.unwrap();

        assert_eq!(
            namespaces,
            vec![Namespace {
                name: "xljiulang".to_string(),
                path: "xljiulang".to_string(),
                access_role: "Owner".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn retries_transient_current_user_reads() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "username": "cnb.new-user"
            })))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let user = api.current_user("token").await.unwrap();

        assert_eq!(user.username, "cnb.new-user");
    }

    #[tokio::test]
    async fn creates_repo_under_the_namespace_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/xljiulang/-/repos"))
            .and(header("authorization", "Bearer token"))
            .and(header("content-type", "application/json"))
            .and(header("accept", "application/vnd.cnb.api+json"))
            .and(body_json(serde_json::json!({
                "name": "cnb-ai-bridge",
                "description": "bridge",
                "visibility": "private"
            })))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        api.create_repo("token", "xljiulang", "cnb-ai-bridge", "bridge")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn lists_repository_branches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xljiulang/cnb-ai-bridge/-/git/branches"))
            .and(query_param("page", "1"))
            .and(query_param("page_size", "100"))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "master",
                    "commit": { "sha": "abc123" },
                    "protected": false,
                    "locked": false
                }
            ])))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let branches = api
            .list_branches("token", "xljiulang/cnb-ai-bridge")
            .await
            .unwrap();

        assert_eq!(
            branches,
            vec![Branch {
                name: "master".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn starts_workspace_with_json_branch_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/xljiulang/cnb-ai-bridge/-/workspace/start"))
            .and(header("authorization", "Bearer token"))
            .and(header("content-type", "application/json"))
            .and(header("accept", "application/vnd.cnb.api+json"))
            .and(body_json(serde_json::json!({ "branch": "main" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sn": "pipeline-1"
            })))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let result = api
            .start_workspace("token", "xljiulang/cnb-ai-bridge", "main")
            .await
            .unwrap();

        assert_eq!(result["sn"], "pipeline-1");
    }

    #[tokio::test]
    async fn loads_usage_for_preferred_owner_namespace() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .and(query_param("page", "1"))
            .and(query_param("page_size", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "name": "first", "path": "first", "access_role": "Owner" },
                { "name": "preferred", "path": "preferred", "access_role": "Owner" }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preferred/-/charge/quota"))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dev_in_sec": { "total": 5760000, "free": 4389634 },
                "credit_in_milli": { "total": 1166000, "free": 500000 }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preferred/-/charge/volume"))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dev_in_sec": 1370366,
                "credit_in_milli": 22540
            })))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let usage = api.usage_summary("token", Some("preferred")).await.unwrap();

        assert_eq!(usage.namespace, "preferred");
        assert_eq!(usage.dev_cpu_used_seconds, 1370366);
        assert_eq!(usage.dev_cpu_total_seconds, 5760000);
        assert_eq!(usage.credits_used_milli, 22540);
        assert_eq!(usage.credits_total_milli, 1166000);
    }

    #[tokio::test]
    async fn loads_usage_for_first_owner_without_preference() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "name": "first", "path": "first", "access_role": "Owner" },
                { "name": "second", "path": "second", "access_role": "Owner" }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/first/-/charge/quota"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "credit_in_milli": { "total": 500000, "free": 500000 }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/first/-/charge/volume"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "credit_in_milli": 0
            })))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let usage = api.usage_summary("token", None).await.unwrap();

        assert_eq!(usage.namespace, "first");
        assert_eq!(usage.credits_used_milli, 0);
    }

    #[tokio::test]
    async fn creates_organization_and_confirms_owner_access() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/groups"))
            .and(header("authorization", "Bearer token"))
            .and(header("content-type", "application/json"))
            .and(body_json(serde_json::json!({
                "path": "cnb.new-user",
                "remark": "CNB Bridge",
                "description": "CNB Bridge root organization"
            })))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "cnb.new-user",
                    "path": "cnb.new-user",
                    "access_role": "Owner"
                }
            ])))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let namespaces = api
            .ensure_organization(
                "token",
                "cnb.new-user",
                "CNB Bridge",
                "CNB Bridge root organization",
            )
            .await
            .unwrap();

        assert_eq!(namespaces[0].path, "cnb.new-user");
    }

    #[tokio::test]
    async fn treats_existing_organization_conflict_as_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/groups"))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "cnb.new-user",
                    "path": "cnb.new-user",
                    "access_role": "Owner"
                }
            ])))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let namespaces = api
            .ensure_organization("token", "cnb.new-user", "", "")
            .await
            .unwrap();

        assert_eq!(namespaces.len(), 1);
    }

    #[tokio::test]
    async fn preserves_organization_create_forbidden_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/groups"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errcode": 10,
                "errmsg": "missing group-manage:rw"
            })))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let error = api
            .ensure_organization("token", "cnb.new-user", "", "")
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            ApiError::Remote {
                status: reqwest::StatusCode::FORBIDDEN,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn keeps_conflict_when_existing_group_is_not_owned() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/groups"))
            .respond_with(ResponseTemplate::new(409))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let error = api
            .ensure_organization("token", "cnb.new-user", "", "")
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            ApiError::Remote {
                status: reqwest::StatusCode::CONFLICT,
                ..
            }
        ));
    }
}
