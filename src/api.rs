//! Minimal CNB OpenAPI client used by the bridge lifecycle.

use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Default CNB OpenAPI endpoint.
pub const DEFAULT_API_URL: &str = "https://api.cnb.cool";

/// Errors returned by the CNB API.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("网络请求失败: {0}")]
    Http(#[from] reqwest::Error),
    #[error("CNB API 返回 {status}: {message}")]
    Remote {
        status: StatusCode,
        code: Option<i64>,
        message: String,
    },
    #[error("响应格式无效: {0}")]
    InvalidResponse(String),
}

/// The current CNB user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub nickname: String,
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
        let response = self
            .http
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.cnb.api+json")
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(true);
        }
        if status == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        let text = response.text().await?;
        let (code, message) = parse_remote_error(&text);
        Err(ApiError::Remote {
            status,
            code,
            message,
        })
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
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.cnb.api+json");
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/vnd.cnb.api+json")
                .json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            let (code, message) = parse_remote_error(&text);
            return Err(ApiError::Remote {
                status,
                code,
                message,
            });
        }
        if text.trim().is_empty() {
            serde_json::from_str("null")
                .map_err(|error| ApiError::InvalidResponse(error.to_string()))
        } else {
            serde_json::from_str(&text)
                .map_err(|error| ApiError::InvalidResponse(format!("{error}: {text}")))
        }
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

    #[test]
    fn parses_cnb_error_envelope() {
        let (code, message) =
            parse_remote_error(r#"{"errcode":10024,"errmsg":"resource mismatch"}"#);
        assert_eq!(code, Some(10024));
        assert_eq!(message, "resource mismatch");
    }
}
