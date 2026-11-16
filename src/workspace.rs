//! Repository provisioning and CNB workspace lifecycle.

use crate::api::{ApiError, CnbApi, WorkspaceDetail};
use git2::{RemoteCallbacks, Repository, Signature};
use std::path::Path;

/// Description written into the generated repository.
pub const REPO_DESCRIPTION: &str = "CNB Bridge workspace agent";

const TEMPLATE_MARKER: &str = ".cnb-ai-bridge/workspace.json";

const CNB_YML: &str = include_str!("../templates/cnb-ai-bridge/.cnb.yml");
const WORKSPACE_MARKER: &str =
    include_str!("../templates/cnb-ai-bridge/.cnb-ai-bridge/workspace.json");
const AGENT_MJS: &str = include_str!("../templates/cnb-ai-bridge/agent/agent.mjs");
const MODELS_JSON: &str = include_str!("../templates/cnb-ai-bridge/agent/models.json");
const SUPERVISOR_SH: &str = include_str!("../templates/cnb-ai-bridge/agent/supervisor.sh");
const TEMPLATE_README: &str = include_str!("../templates/cnb-ai-bridge/README.md");

/// Result of making sure the bridge repository exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub slug: String,
    pub created: bool,
}

/// A running workspace and its SSH access information.
#[derive(Debug, Clone)]
pub struct WorkspaceSession {
    pub sn: String,
    pub detail: WorkspaceDetail,
}

/// Ensures the per-user bridge repository and its template files exist.
pub struct RepoProvisioner<'a> {
    api: &'a CnbApi,
}

impl<'a> RepoProvisioner<'a> {
    /// Creates a provisioner using the given API client.
    pub fn new(api: &'a CnbApi) -> Self {
        Self { api }
    }

    /// Returns the repository slug, creating and pushing the template if needed.
    pub async fn ensure(
        &self,
        token: &str,
        username: &str,
        repo_name: &str,
    ) -> Result<RepoInfo, String> {
        let slug = format!("{username}/{repo_name}");
        let repos = self
            .api
            .list_repos(token)
            .await
            .map_err(|error| error.to_string())?;
        let exists = repos.iter().any(|repo| repo.path == slug);

        if exists {
            let has_marker = self
                .api
                .file_exists(token, &slug, TEMPLATE_MARKER, "master")
                .await
                .map_err(|error| error.to_string())?;
            if !has_marker {
                return Err(format!(
                    "仓库 {slug} 已存在但 master 分支缺少 CNB Bridge 标记 {TEMPLATE_MARKER}，请换一个仓库名"
                ));
            }
            return Ok(RepoInfo {
                slug,
                created: false,
            });
        }

        self.api
            .create_repo(token, username, repo_name, REPO_DESCRIPTION)
            .await
            .map_err(|error| error.to_string())?;
        push_template_with_retry(&slug, token).await?;
        Ok(RepoInfo {
            slug,
            created: true,
        })
    }
}

/// Workspace start/reuse/stop operations.
pub struct WorkspaceManager<'a> {
    api: &'a CnbApi,
}

impl<'a> WorkspaceManager<'a> {
    /// Creates a manager using the given API client.
    pub fn new(api: &'a CnbApi) -> Self {
        Self { api }
    }

    /// Ensures a running workspace exists and returns its SSH information.
    pub async fn ensure_running(
        &self,
        token: &str,
        slug: &str,
        branch: &str,
    ) -> Result<WorkspaceSession, ApiError> {
        let mut sn = running_sn(self.api, token, slug).await?;
        if sn.is_none() {
            self.api.start_workspace(token, slug, branch).await?;
            for _ in 0..60 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                sn = running_sn(self.api, token, slug).await?;
                if sn.is_some() {
                    break;
                }
            }
        }
        let sn =
            sn.ok_or_else(|| ApiError::InvalidResponse("工作空间启动超时（120 秒）".to_string()))?;
        let detail = self.api.workspace_detail(token, slug, &sn).await?;
        Ok(WorkspaceSession { sn, detail })
    }

    /// Stops the workspace when the client exits.
    pub async fn stop(&self, token: &str, sn: &str) -> Result<(), ApiError> {
        self.api.stop_workspace(token, sn).await
    }
}

async fn running_sn(api: &CnbApi, token: &str, slug: &str) -> Result<Option<String>, ApiError> {
    let list = api.list_workspaces(token, slug).await?;
    Ok(list
        .list
        .into_iter()
        .find(|workspace| workspace.status == "running")
        .map(|workspace| workspace.sn))
}

fn push_template(slug: &str, token: &str) -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let repo = Repository::init(dir.path()).map_err(|error| error.to_string())?;
    write_template(dir.path())?;

    let mut index = repo.index().map_err(|error| error.to_string())?;
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .map_err(|error| error.to_string())?;
    index.write().map_err(|error| error.to_string())?;
    let tree_id = index.write_tree().map_err(|error| error.to_string())?;
    let tree = repo.find_tree(tree_id).map_err(|error| error.to_string())?;
    let signature =
        Signature::now("CNB Bridge", "cnb-bridge@localhost").map_err(|e| e.to_string())?;
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "初始化 CNB Bridge 工作空间",
        &tree,
        &[],
    )
    .map_err(|error| error.to_string())?;

    let mut remote = repo
        .remote_anonymous(&repo_url(slug))
        .map_err(|error| error.to_string())?;
    let callbacks = credentials_callbacks(token);
    let mut push = git2::PushOptions::new();
    push.remote_callbacks(callbacks);
    remote
        .push(&["refs/heads/master:refs/heads/master"], Some(&mut push))
        .map_err(|error| error.to_string())
}

async fn push_template_with_retry(slug: &str, token: &str) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 0..5 {
        match push_template(slug, token) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = error;
                if attempt < 4 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }
    Err(format!("推送工作空间模板失败: {last_error}"))
}

fn write_template(dir: &Path) -> Result<(), String> {
    let files = [
        (".cnb.yml", CNB_YML),
        (TEMPLATE_MARKER, WORKSPACE_MARKER),
        ("agent/agent.mjs", AGENT_MJS),
        ("agent/models.json", MODELS_JSON),
        ("agent/supervisor.sh", SUPERVISOR_SH),
        ("README.md", TEMPLATE_README),
    ];
    for (path, contents) in files {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&full, contents).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn credentials_callbacks(token: &str) -> RemoteCallbacks<'static> {
    let token = token.to_string();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, _username, _allowed| {
        git2::Cred::userpass_plaintext("cnb", &token)
    });
    callbacks
}

fn repo_url(slug: &str) -> String {
    format!("https://cnb.cool/{slug}")
}
