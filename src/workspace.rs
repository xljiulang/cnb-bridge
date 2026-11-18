//! Repository provisioning and CNB workspace lifecycle.

use crate::api::{ApiError, Branch, CnbApi, Namespace, Repo, WorkspaceDetail};
use git2::{RemoteCallbacks, Repository, Signature};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Description written into the generated repository.
pub const REPO_DESCRIPTION: &str = "CNB Bridge workspace agent";

const TEMPLATE_MARKER: &str = ".cnb-ai-bridge/workspace.json";
const TEMPLATE_SCHEMA: u64 = 3;
const DEFAULT_BRANCH: &str = "main";
const LEGACY_BRANCH: &str = "master";

const CNB_YML: &str = include_str!("../templates/cnb-ai-bridge/.cnb.yml");
const WORKSPACE_MARKER: &str =
    include_str!("../templates/cnb-ai-bridge/.cnb-ai-bridge/workspace.json");
const AGENT_MJS: &str = include_str!("../templates/cnb-ai-bridge/agent/agent.mjs");
const SUPERVISOR_SH: &str = include_str!("../templates/cnb-ai-bridge/agent/supervisor.sh");
const TEMPLATE_README: &str = include_str!("../templates/cnb-ai-bridge/README.md");

/// Result of making sure the bridge repository exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub slug: String,
    pub branch: String,
    pub created: bool,
}

/// A running workspace and its SSH access information.
#[derive(Debug, Clone)]
pub struct WorkspaceSession {
    pub sn: String,
    pub detail: WorkspaceDetail,
}

/// Errors raised while locating or initializing the bridge repository.
#[derive(Debug, thiserror::Error)]
pub enum RepoProvisionError {
    #[error("{0}")]
    Api(#[from] ApiError),
    #[error("{0}")]
    Message(String),
    #[error(
        "当前 CNB 授权不能自动创建仓库。请在 {namespace} 下创建私有空仓库 {slug}，完成后回到本应用点击“重新检查仓库”。"
    )]
    ManualCreationRequired {
        namespace: String,
        slug: String,
        url: String,
    },
}

fn bridge_branch_candidates(branches: &[Branch]) -> Vec<&str> {
    [DEFAULT_BRANCH, LEGACY_BRANCH]
        .into_iter()
        .filter(|preferred| branches.iter().any(|branch| branch.name == *preferred))
        .collect()
}

fn is_missing_group_scope(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::Remote {
            status: reqwest::StatusCode::FORBIDDEN,
            message,
            ..
        } if message
            .to_ascii_lowercase()
            .contains("group-resource:rw")
    )
}

fn repo_creation_url(namespace: &str) -> String {
    format!("https://cnb.cool/new/repos?group={namespace}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RepoTarget {
    Existing(String),
    Create { namespace: String, slug: String },
}

fn resolve_repo_target(
    repo_name: &str,
    namespace_hint: &str,
    repos: &[Repo],
    namespaces: &[Namespace],
) -> Result<RepoTarget, String> {
    let namespace_hint = namespace_hint.trim();
    if namespace_hint.is_empty() {
        let matches: Vec<&Repo> = repos.iter().filter(|repo| repo.name == repo_name).collect();
        match matches.as_slice() {
            [repo] if !repo.path.trim().is_empty() => {
                return Ok(RepoTarget::Existing(repo.path.clone()));
            }
            [repo] => {
                return Err(format!("CNB 返回的仓库 {} 缺少完整路径", repo.name));
            }
            matches if matches.len() > 1 => {
                return Err(format!(
                    "检测到多个名为 {repo_name} 的仓库，请先在“设置 → 仓库命名空间”中选择。"
                ));
            }
            _ => {}
        }
    } else {
        let slug = format!("{namespace_hint}/{repo_name}");
        if repos.iter().any(|repo| repo.path == slug) {
            return Ok(RepoTarget::Existing(slug));
        }
    }

    let namespace = if namespace_hint.is_empty() {
        match namespaces {
            [namespace] => namespace.path.clone(),
            [] => {
                return Err("当前账号没有可创建仓库的 CNB 命名空间。".to_string());
            }
            _ => {
                return Err(
                    "检测到多个可建仓的 CNB 命名空间，请先在“设置 → 仓库命名空间”中选择。"
                        .to_string(),
                );
            }
        }
    } else if namespaces
        .iter()
        .any(|namespace| namespace.path == namespace_hint)
    {
        namespace_hint.to_string()
    } else {
        return Err(format!(
            "配置的 CNB 命名空间 {namespace_hint} 不可用，请先在“设置 → 仓库命名空间”中重新选择。"
        ));
    };

    Ok(RepoTarget::Create {
        slug: format!("{namespace}/{repo_name}"),
        namespace,
    })
}

/// Ensures the per-user bridge repository and its template files exist.
pub struct RepoProvisioner<'a> {
    api: &'a CnbApi,
    remote_base_url: String,
}

impl<'a> RepoProvisioner<'a> {
    /// Creates a provisioner using the given API client.
    pub fn new(api: &'a CnbApi) -> Self {
        Self {
            api,
            remote_base_url: "https://cnb.cool".to_string(),
        }
    }

    #[cfg(test)]
    fn with_remote_base_url(api: &'a CnbApi, remote_base_url: &str) -> Self {
        Self {
            api,
            remote_base_url: remote_base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Returns the repository slug, creating and pushing the template if needed.
    pub async fn ensure(
        &self,
        token: &str,
        repo_name: &str,
        namespace_hint: &str,
        allow_create: bool,
    ) -> Result<RepoInfo, RepoProvisionError> {
        let (repos, namespaces) = tokio::try_join!(
            self.api.list_repos(token),
            self.api.list_owned_namespaces(token)
        )?;
        let target = resolve_repo_target(repo_name, namespace_hint, &repos, &namespaces)
            .map_err(RepoProvisionError::Message)?;
        match target {
            RepoTarget::Existing(slug) => {
                let branches = match self.api.list_branches(token, &slug).await {
                    Ok(branches) => branches,
                    Err(ApiError::Remote {
                        status: reqwest::StatusCode::NOT_FOUND,
                        ..
                    }) => Vec::new(),
                    Err(error) => return Err(RepoProvisionError::Api(error)),
                };
                if branches.is_empty() {
                    let remote_url = format!("{}/{slug}", self.remote_base_url);
                    push_template_with_retry(self.api, token, &remote_url, &slug, DEFAULT_BRANCH)
                        .await?;
                    return Ok(RepoInfo {
                        slug,
                        branch: DEFAULT_BRANCH.to_string(),
                        created: false,
                    });
                }
                for branch in bridge_branch_candidates(&branches) {
                    if self
                        .api
                        .file_exists(token, &slug, TEMPLATE_MARKER, branch)
                        .await?
                    {
                        let schema = template_schema(self.api, token, &slug, branch).await?;
                        if schema < TEMPLATE_SCHEMA {
                            let remote_url = format!("{}/{slug}", self.remote_base_url);
                            let token_for_sync = token.to_string();
                            let slug_for_sync = slug.clone();
                            let branch_for_sync = branch.to_string();
                            tokio::task::spawn_blocking(move || {
                                sync_template_to_existing(
                                    &token_for_sync,
                                    &remote_url,
                                    &slug_for_sync,
                                    &branch_for_sync,
                                )
                            })
                            .await
                            .map_err(|error| {
                                RepoProvisionError::Message(format!("模板同步任务失败: {error}"))
                            })?
                            .map_err(RepoProvisionError::Message)?;
                            confirm_template_branch(self.api, token, &slug, branch).await?;
                        }
                        return Ok(RepoInfo {
                            slug,
                            branch: branch.to_string(),
                            created: false,
                        });
                    }
                }
                Err(RepoProvisionError::Message(format!(
                    "仓库 {slug} 已有内容但缺少 CNB Bridge 标记，请换一个仓库名"
                )))
            }
            RepoTarget::Create { namespace, slug } => {
                if !allow_create {
                    return Err(RepoProvisionError::ManualCreationRequired {
                        url: repo_creation_url(&namespace),
                        namespace,
                        slug,
                    });
                }
                match self
                    .api
                    .create_repo(token, &namespace, repo_name, REPO_DESCRIPTION)
                    .await
                {
                    Ok(()) => {}
                    Err(error) if is_missing_group_scope(&error) => {
                        return Err(RepoProvisionError::ManualCreationRequired {
                            url: repo_creation_url(&namespace),
                            namespace,
                            slug,
                        });
                    }
                    Err(error) => return Err(RepoProvisionError::Api(error)),
                }
                let remote_url = format!("{}/{slug}", self.remote_base_url);
                push_template_with_retry(self.api, token, &remote_url, &slug, DEFAULT_BRANCH)
                    .await?;
                Ok(RepoInfo {
                    slug,
                    branch: DEFAULT_BRANCH.to_string(),
                    created: true,
                })
            }
        }
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
            let started = self.api.start_workspace(token, slug, branch).await?;
            let started_sn = started
                .get("sn")
                .or_else(|| started.pointer("/result/sn"))
                .and_then(serde_json::Value::as_str)
                .filter(|sn| !sn.is_empty())
                .map(str::to_string);
            for _ in 0..60 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if let Some(candidate) = started_sn.as_deref() {
                    if let Ok(detail) = self.api.workspace_detail(token, slug, candidate).await {
                        return Ok(WorkspaceSession {
                            sn: candidate.to_string(),
                            detail,
                        });
                    }
                }
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

fn push_template_to_remote(
    _slug: &str,
    token: &str,
    branch: &str,
    remote_url: &str,
) -> Result<(), String> {
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
        .remote_anonymous(remote_url)
        .map_err(|error| error.to_string())?;
    let mut callbacks = credentials_callbacks(token);
    let rejected = Arc::new(Mutex::new(None::<String>));
    let rejected_by_callback = Arc::clone(&rejected);
    callbacks.push_update_reference(move |reference, status| {
        if let Some(status) = status {
            if let Ok(mut rejected) = rejected_by_callback.lock() {
                *rejected = Some(format!("{reference}: {status}"));
            }
            return Err(git2::Error::from_str(status));
        }
        Ok(())
    });
    let mut push = git2::PushOptions::new();
    push.remote_callbacks(callbacks);
    let refspec = format!("HEAD:refs/heads/{branch}");
    let result = remote.push(&[refspec.as_str()], Some(&mut push));
    if let Some(reason) = rejected
        .lock()
        .ok()
        .and_then(|mut rejection| rejection.take())
    {
        return Err(format!("推送分支 {branch} 被 CNB 拒绝: {reason}"));
    }
    result.map_err(|error| format!("推送分支 {branch} 失败: {error}"))
}

fn sync_template_to_existing(
    token: &str,
    remote_url: &str,
    _slug: &str,
    branch: &str,
) -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let mut callbacks = credentials_callbacks(token);
    callbacks.transfer_progress(|_| true);
    let mut fetch_options = git2::FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fetch_options);
    let repo = builder
        .clone(remote_url, dir.path())
        .map_err(|error| format!("拉取现有仓库 {branch} 失败: {error}"))?;
    let remote_branch = repo
        .find_reference(&format!("refs/remotes/origin/{branch}"))
        .map_err(|error| format!("现有仓库缺少分支 {branch}: {error}"))?;
    let target = remote_branch
        .target()
        .ok_or_else(|| format!("现有仓库分支 {branch} 没有目标提交"))?;
    repo.reference(
        &format!("refs/heads/{branch}"),
        target,
        true,
        "checkout managed template branch",
    )
    .map_err(|error| format!("切换现有仓库分支 {branch} 失败: {error}"))?;
    repo.set_head(&format!("refs/heads/{branch}"))
        .map_err(|error| format!("设置现有仓库分支 {branch} 失败: {error}"))?;
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .map_err(|error| format!("检出现有仓库分支 {branch} 失败: {error}"))?;
    let preserved_root_files = [".cnb.yml", "README.md"]
        .into_iter()
        .filter_map(|path| {
            let full = dir.path().join(path);
            std::fs::read(&full).ok().map(|contents| (path, contents))
        })
        .collect::<Vec<_>>();
    write_template(dir.path())?;
    for (path, contents) in preserved_root_files {
        std::fs::write(dir.path().join(path), contents)
            .map_err(|error| format!("恢复用户管理的仓库文件 {path} 失败: {error}"))?;
    }
    let obsolete_models = dir.path().join("agent/models.json");
    if obsolete_models.is_file() {
        std::fs::remove_file(&obsolete_models).map_err(|error| {
            format!(
                "删除废弃模型目录 {} 失败: {error}",
                obsolete_models.display()
            )
        })?;
    }

    let mut index = repo.index().map_err(|error| error.to_string())?;
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .map_err(|error| error.to_string())?;
    index.write().map_err(|error| error.to_string())?;
    let tree_id = index.write_tree().map_err(|error| error.to_string())?;
    let head = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| format!("读取现有仓库提交失败: {error}"))?;
    if head.tree_id() == tree_id {
        return Ok(());
    }

    let tree = repo.find_tree(tree_id).map_err(|error| error.to_string())?;
    let signature =
        Signature::now("CNB Bridge", "cnb-bridge@localhost").map_err(|e| e.to_string())?;
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "升级 CNB Bridge 工作空间模板",
        &tree,
        &[&head],
    )
    .map_err(|error| format!("提交模板升级失败: {error}"))?;

    let mut remote = repo
        .find_remote("origin")
        .map_err(|error| format!("读取 Git 远端失败: {error}"))?;
    let mut push = git2::PushOptions::new();
    push.remote_callbacks(credentials_callbacks(token));
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote
        .push(&[refspec.as_str()], Some(&mut push))
        .map_err(|error| format!("推送模板升级到 {branch} 失败: {error}"))
}

async fn push_template_with_retry(
    api: &CnbApi,
    token: &str,
    remote_url: &str,
    slug: &str,
    branch: &str,
) -> Result<(), RepoProvisionError> {
    let mut last_error = String::new();
    for attempt in 0..5 {
        if confirm_template_branch(api, token, slug, branch)
            .await
            .is_ok()
        {
            return Ok(());
        }
        let token_for_push = token.to_string();
        let remote_url_for_push = remote_url.to_string();
        let slug_for_push = slug.to_string();
        let branch_for_push = branch.to_string();
        match tokio::task::spawn_blocking(move || {
            push_template_to_remote(
                &slug_for_push,
                &token_for_push,
                &branch_for_push,
                &remote_url_for_push,
            )
        })
        .await
        .map_err(|error| RepoProvisionError::Message(format!("模板推送任务失败: {error}")))?
        {
            Ok(()) => match confirm_template_branch(api, token, slug, branch).await {
                Ok(()) => return Ok(()),
                Err(error) => last_error = error.to_string(),
            },
            Err(error) => {
                last_error = error;
            }
        }
        if attempt < 4 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    Err(RepoProvisionError::Message(format!(
        "未能确认模板已推送到 {slug} 的 {branch} 分支: {last_error}"
    )))
}

async fn confirm_template_branch(
    api: &CnbApi,
    token: &str,
    slug: &str,
    branch: &str,
) -> Result<(), RepoProvisionError> {
    let branches = api.list_branches(token, slug).await?;
    if !branches.iter().any(|candidate| candidate.name == branch) {
        return Err(RepoProvisionError::Message(format!(
            "推送后未找到分支 {branch}"
        )));
    }
    let schema = template_schema(api, token, slug, branch).await?;
    if schema < TEMPLATE_SCHEMA {
        return Err(RepoProvisionError::Message(format!(
            "分支 {branch} 的模板版本为 {schema}，尚未升级到 {TEMPLATE_SCHEMA}"
        )));
    }
    Ok(())
}

async fn template_schema(
    api: &CnbApi,
    token: &str,
    slug: &str,
    branch: &str,
) -> Result<u64, RepoProvisionError> {
    let text = api.file_text(token, slug, TEMPLATE_MARKER, branch).await?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| RepoProvisionError::Message(format!("模板标记不是有效 JSON: {error}")))?;
    value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| RepoProvisionError::Message("模板标记缺少 schema".to_string()))
}

fn write_template(dir: &Path) -> Result<(), String> {
    let files = [
        (".cnb.yml", CNB_YML),
        (TEMPLATE_MARKER, WORKSPACE_MARKER),
        ("agent/agent.mjs", AGENT_MJS),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ApiError, Branch, Namespace, Repo};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn repo(name: &str, path: &str) -> Repo {
        Repo {
            name: name.to_string(),
            path: path.to_string(),
            web_url: None,
        }
    }

    fn namespace(path: &str) -> Namespace {
        Namespace {
            name: path.to_string(),
            path: path.to_string(),
            access_role: "Owner".to_string(),
        }
    }

    fn marker_body(schema: u64) -> serde_json::Value {
        use base64::Engine as _;

        let content = format!(r#"{{"product":"cnb-ai-bridge","schema":{schema}}}"#);
        serde_json::json!({
            "type": "blob",
            "encoding": "base64",
            "content": base64::engine::general_purpose::STANDARD.encode(content)
        })
    }

    #[test]
    fn resolves_repo_targets_without_using_login_username() {
        let cases = [
            (
                "formats a new repo under the only owned namespace",
                "",
                vec![],
                vec![namespace("xljiulang")],
                Ok(RepoTarget::Create {
                    namespace: "xljiulang".to_string(),
                    slug: "xljiulang/cnb-ai-bridge".to_string(),
                }),
            ),
            (
                "reuses a unique existing repo",
                "",
                vec![repo("cnb-ai-bridge", "xljiulang/cnb-ai-bridge")],
                vec![],
                Ok(RepoTarget::Existing("xljiulang/cnb-ai-bridge".to_string())),
            ),
            (
                "uses the configured namespace",
                "team-space",
                vec![],
                vec![namespace("xljiulang"), namespace("team-space")],
                Ok(RepoTarget::Create {
                    namespace: "team-space".to_string(),
                    slug: "team-space/cnb-ai-bridge".to_string(),
                }),
            ),
            (
                "rejects a stale configured namespace",
                "removed-team",
                vec![],
                vec![namespace("xljiulang")],
                Err("removed-team"),
            ),
            (
                "rejects duplicate repo names",
                "",
                vec![
                    repo("cnb-ai-bridge", "xljiulang/cnb-ai-bridge"),
                    repo("cnb-ai-bridge", "team-space/cnb-ai-bridge"),
                ],
                vec![],
                Err("多个"),
            ),
            (
                "rejects ambiguous namespace selection",
                "",
                vec![],
                vec![namespace("xljiulang"), namespace("team-space")],
                Err("设置"),
            ),
            (
                "rejects accounts without an owned namespace",
                "",
                vec![],
                vec![],
                Err("没有"),
            ),
        ];

        for (name, hint, repos, namespaces, expected) in cases {
            let actual = resolve_repo_target("cnb-ai-bridge", hint, &repos, &namespaces);
            match expected {
                Ok(expected) => assert_eq!(actual, Ok(expected), "{name}"),
                Err(message) => {
                    let error = actual.expect_err(name);
                    assert!(error.contains(message), "{name}: {error}");
                }
            }
        }
    }

    #[test]
    fn orders_bridge_branch_candidates_by_preference() {
        let main = Branch {
            name: "main".to_string(),
        };
        let master = Branch {
            name: "master".to_string(),
        };
        let feature = Branch {
            name: "feature".to_string(),
        };

        assert_eq!(
            bridge_branch_candidates(&[master.clone(), main.clone(), feature]),
            vec!["main", "master"]
        );
        assert_eq!(bridge_branch_candidates(&[master]), vec!["master"]);
        assert!(bridge_branch_candidates(&[]).is_empty());
    }

    #[test]
    fn managed_template_uses_cnb_default_idle_recycling() {
        assert!(!CNB_YML.contains("keepAliveTimeout"));
        let marker: serde_json::Value = serde_json::from_str(WORKSPACE_MARKER).unwrap();
        assert_eq!(marker["schema"].as_u64(), Some(TEMPLATE_SCHEMA));
    }

    #[test]
    fn ignores_unrelated_branches_when_looking_for_the_bridge_marker() {
        let branch = Branch {
            name: "feature".to_string(),
        };
        assert!(bridge_branch_candidates(&[branch]).is_empty());
    }

    #[tokio::test]
    async fn guides_user_when_create_repo_scope_is_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
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
        Mock::given(method("POST"))
            .and(path("/xljiulang/-/repos"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errcode": 10,
                "errmsg": "High-risk operation. Application \"cnb_cli\" is missing required scopes: group-resource:rw"
            })))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let error = RepoProvisioner::new(&api)
            .ensure("token", "cnb-ai-bridge", "", true)
            .await
            .unwrap_err();

        match error {
            RepoProvisionError::ManualCreationRequired {
                namespace,
                slug,
                url,
            } => {
                assert_eq!(namespace, "xljiulang");
                assert_eq!(slug, "xljiulang/cnb-ai-bridge");
                assert_eq!(url, "https://cnb.cool/new/repos?group=xljiulang");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn preserves_unrelated_create_repo_forbidden_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
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
        Mock::given(method("POST"))
            .and(path("/xljiulang/-/repos"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errcode": 10,
                "errmsg": "forbidden"
            })))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let error = RepoProvisioner::new(&api)
            .ensure("token", "cnb-ai-bridge", "", true)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            RepoProvisionError::Api(ApiError::Remote {
                status: reqwest::StatusCode::FORBIDDEN,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn reuses_existing_repo_with_bridge_marker() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "cnb-ai-bridge",
                    "path": "xljiulang/cnb-ai-bridge",
                    "web_url": "https://cnb.cool/xljiulang/cnb-ai-bridge"
                }
            ])))
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
        Mock::given(method("GET"))
            .and(path("/xljiulang/cnb-ai-bridge/-/git/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "main",
                    "commit": { "sha": "abc123" },
                    "protected": false,
                    "locked": false
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/xljiulang/cnb-ai-bridge/-/git/contents/.cnb-ai-bridge/workspace.json",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(marker_body(3)))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let repo = RepoProvisioner::new(&api)
            .ensure("token", "cnb-ai-bridge", "", true)
            .await
            .unwrap();

        assert_eq!(repo.slug, "xljiulang/cnb-ai-bridge");
        assert_eq!(repo.branch, "main");
        assert!(!repo.created);
    }

    #[tokio::test]
    async fn refuses_to_initialize_repo_with_unrelated_content() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "cnb-ai-bridge",
                    "path": "xljiulang/cnb-ai-bridge",
                    "web_url": "https://cnb.cool/xljiulang/cnb-ai-bridge"
                }
            ])))
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
        Mock::given(method("GET"))
            .and(path(
                "/xljiulang/cnb-ai-bridge/-/git/contents/.cnb-ai-bridge/workspace.json",
            ))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "errcode": 5,
                "errmsg": "Resource not found."
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xljiulang/cnb-ai-bridge/-/git/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "main",
                    "commit": { "sha": "abc123" },
                    "protected": false,
                    "locked": false
                }
            ])))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let error = RepoProvisioner::new(&api)
            .ensure("token", "cnb-ai-bridge", "", true)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("已有内容"), "{error}");
    }

    #[tokio::test]
    async fn reuses_master_when_it_contains_the_bridge_marker() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "cnb-ai-bridge",
                    "path": "xljiulang/cnb-ai-bridge",
                    "web_url": "https://cnb.cool/xljiulang/cnb-ai-bridge"
                }
            ])))
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
        Mock::given(method("GET"))
            .and(path("/xljiulang/cnb-ai-bridge/-/git/branches"))
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
        Mock::given(method("GET"))
            .and(path(
                "/xljiulang/cnb-ai-bridge/-/git/contents/.cnb-ai-bridge/workspace.json",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(marker_body(3)))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let repo = RepoProvisioner::new(&api)
            .ensure("token", "cnb-ai-bridge", "", true)
            .await
            .unwrap();

        assert_eq!(repo.branch, "master");
    }

    #[tokio::test]
    async fn retry_does_not_call_high_risk_create_again() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
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
        let api = CnbApi::with_base_url(server.uri());

        let error = RepoProvisioner::new(&api)
            .ensure("token", "cnb-ai-bridge", "", false)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            RepoProvisionError::ManualCreationRequired { .. }
        ));
    }

    #[tokio::test]
    async fn confirms_template_branch_and_marker_after_push() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xljiulang/cnb-ai-bridge/-/git/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "main",
                    "commit": { "sha": "abc123" },
                    "protected": false,
                    "locked": false
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/xljiulang/cnb-ai-bridge/-/git/contents/.cnb-ai-bridge/workspace.json",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(marker_body(3)))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        confirm_template_branch(&api, "token", "xljiulang/cnb-ai-bridge", "main")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejects_push_success_when_branch_never_appears() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xljiulang/cnb-ai-bridge/-/git/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let error = confirm_template_branch(&api, "token", "xljiulang/cnb-ai-bridge", "main")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("main"), "{error}");
    }

    #[test]
    fn reports_rejected_reference_updates() {
        let dir = tempfile::tempdir().unwrap();
        let remote_path = dir.path().join("remote.git");
        Repository::init_bare(&remote_path).unwrap();

        let seed_dir = tempfile::tempdir().unwrap();
        let seed = Repository::init(seed_dir.path()).unwrap();
        let signature = Signature::now("CNB Bridge", "cnb-bridge@localhost").unwrap();
        let tree_id = seed.index().unwrap().write_tree().unwrap();
        let tree = seed.find_tree(tree_id).unwrap();
        seed.commit(Some("HEAD"), &signature, &signature, "seed", &tree, &[])
            .unwrap();
        let mut seed_remote = seed
            .remote_anonymous(remote_path.to_str().unwrap())
            .unwrap();
        seed_remote.push(&["HEAD:refs/heads/main"], None).unwrap();

        let error = push_template_to_remote(
            "xljiulang/cnb-ai-bridge",
            "token",
            "main",
            remote_path.to_str().unwrap(),
        )
        .unwrap_err();

        assert!(error.contains("main"), "{error}");
        assert!(error.contains("NotFastForward"), "{error}");
    }

    #[test]
    fn libgit2_has_https_transport() {
        assert!(git2::Version::get().https());
    }

    #[tokio::test]
    async fn initializes_empty_repo_on_main_and_verifies_the_push() {
        let remote_root = tempfile::tempdir().unwrap();
        let remote_path = remote_root.path().join("xljiulang/cnb-ai-bridge");
        std::fs::create_dir_all(remote_path.parent().unwrap()).unwrap();
        Repository::init_bare(&remote_path).unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "cnb-ai-bridge",
                    "path": "xljiulang/cnb-ai-bridge",
                    "web_url": "https://cnb.cool/xljiulang/cnb-ai-bridge"
                }
            ])))
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
        let branch_reads = Arc::new(AtomicUsize::new(0));
        let branch_reads_for_response = Arc::clone(&branch_reads);
        Mock::given(method("GET"))
            .and(path("/xljiulang/cnb-ai-bridge/-/git/branches"))
            .respond_with(move |_request: &wiremock::Request| {
                if branch_reads_for_response.fetch_add(1, Ordering::SeqCst) < 2 {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!([]))
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!([
                        {
                            "name": "main",
                            "commit": { "sha": "abc123" },
                            "protected": false,
                            "locked": false
                        }
                    ]))
                }
            })
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/xljiulang/cnb-ai-bridge/-/git/contents/.cnb-ai-bridge/workspace.json",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(marker_body(3)))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());
        let provisioner =
            RepoProvisioner::with_remote_base_url(&api, remote_root.path().to_str().unwrap());

        let repo = provisioner
            .ensure("token", "cnb-ai-bridge", "", true)
            .await
            .unwrap();

        assert_eq!(repo.branch, "main");
        let remote = Repository::open_bare(&remote_path).unwrap();
        assert!(remote.find_reference("refs/heads/main").is_ok());
    }

    #[tokio::test]
    async fn starts_a_closed_workspace_and_returns_its_connection_detail() {
        let server = MockServer::start().await;
        let list_reads = Arc::new(AtomicUsize::new(0));
        let list_reads_for_response = Arc::clone(&list_reads);
        Mock::given(method("GET"))
            .and(path("/workspace/list"))
            .respond_with(move |_request: &wiremock::Request| {
                if list_reads_for_response.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({ "list": [] }))
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "list": [{ "sn": "ws-new", "status": "running" }]
                    }))
                }
            })
            .mount(&server)
            .await;
        let start_calls = Arc::new(AtomicUsize::new(0));
        let start_calls_for_response = Arc::clone(&start_calls);
        Mock::given(method("POST"))
            .and(path("/xljiulang/cnb-ai-bridge/-/workspace/start"))
            .respond_with(move |_request: &wiremock::Request| {
                start_calls_for_response.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(serde_json::json!({}))
            })
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xljiulang/cnb-ai-bridge/-/workspace/detail/ws-new"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "remoteSsh": "cnb@example.test"
            })))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let session = WorkspaceManager::new(&api)
            .ensure_running("token", "xljiulang/cnb-ai-bridge", "main")
            .await
            .unwrap();

        assert_eq!(session.sn, "ws-new");
        assert_eq!(
            session.detail.remote_ssh.as_deref(),
            Some("cnb@example.test")
        );
        assert_eq!(start_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reuses_a_running_workspace_without_starting_another() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/workspace/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "list": [{ "sn": "ws-running", "status": "running" }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/xljiulang/cnb-ai-bridge/-/workspace/detail/ws-running",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "remoteSsh": "cnb@example.test"
            })))
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());

        let session = WorkspaceManager::new(&api)
            .ensure_running("token", "xljiulang/cnb-ai-bridge", "main")
            .await
            .unwrap();

        assert_eq!(session.sn, "ws-running");
    }

    #[tokio::test]
    async fn upgrades_managed_template_and_removes_keepalive_override() {
        let remote_root = tempfile::tempdir().unwrap();
        let remote_path = remote_root.path().join("xljiulang/cnb-ai-bridge");
        std::fs::create_dir_all(remote_path.parent().unwrap()).unwrap();
        Repository::init_bare(&remote_path).unwrap();
        let seed_dir = tempfile::tempdir().unwrap();
        let seed = Repository::init(seed_dir.path()).unwrap();
        write_template(seed_dir.path()).unwrap();
        std::fs::create_dir_all(seed_dir.path().join("agent")).unwrap();
        std::fs::write(
            seed_dir.path().join("agent/models.json"),
            r#"{"models":[{"id":"glm-5.3-flash"}]}"#,
        )
        .unwrap();
        std::fs::write(
            seed_dir.path().join(TEMPLATE_MARKER),
            r#"{"product":"cnb-ai-bridge","schema":1}"#,
        )
        .unwrap();
        std::fs::write(
            seed_dir.path().join(".cnb.yml"),
            "main:\n  push:\n    - services:\n        - name: vscode\n          options:\n            keepAliveTimeout: 18h\n",
        )
        .unwrap();
        let signature = Signature::now("CNB Bridge", "cnb-bridge@localhost").unwrap();
        let mut index = seed.index().unwrap();
        index
            .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = seed.find_tree(tree_id).unwrap();
        seed.commit(
            Some("HEAD"),
            &signature,
            &signature,
            "seed old template",
            &tree,
            &[],
        )
        .unwrap();
        seed.remote_anonymous(remote_path.to_str().unwrap())
            .unwrap()
            .push(&["HEAD:refs/heads/main"], None)
            .unwrap();
        Repository::open_bare(&remote_path)
            .unwrap()
            .set_head("refs/heads/main")
            .unwrap();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "name": "cnb-ai-bridge", "path": "xljiulang/cnb-ai-bridge" }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user/groups"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "name": "xljiulang", "path": "xljiulang", "access_role": "Owner" }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xljiulang/cnb-ai-bridge/-/git/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "name": "main" }
            ])))
            .mount(&server)
            .await;
        let marker_reads = Arc::new(AtomicUsize::new(0));
        let marker_reads_for_response = Arc::clone(&marker_reads);
        Mock::given(method("GET"))
            .and(path(
                "/xljiulang/cnb-ai-bridge/-/git/contents/.cnb-ai-bridge/workspace.json",
            ))
            .respond_with(move |_request: &wiremock::Request| {
                let schema = if marker_reads_for_response.fetch_add(1, Ordering::SeqCst) < 2 {
                    1
                } else {
                    3
                };
                ResponseTemplate::new(200).set_body_json(marker_body(schema))
            })
            .mount(&server)
            .await;
        let api = CnbApi::with_base_url(server.uri());
        let provisioner =
            RepoProvisioner::with_remote_base_url(&api, remote_root.path().to_str().unwrap());

        provisioner
            .ensure("token", "cnb-ai-bridge", "", true)
            .await
            .unwrap();

        let remote = Repository::open_bare(&remote_path).unwrap();
        let head = remote.head().unwrap().peel_to_commit().unwrap();
        let tree = head.tree().unwrap();
        let marker = tree.get_path(Path::new(TEMPLATE_MARKER)).unwrap();
        let marker_blob = remote.find_blob(marker.id()).unwrap();
        let marker_text = std::str::from_utf8(marker_blob.content()).unwrap();
        assert!(marker_text.contains(r#""schema": 3"#));
        assert!(tree.get_path(Path::new("agent/models.json")).is_err());
        let cnb_yml = tree.get_path(Path::new(".cnb.yml")).unwrap();
        let cnb_yml_blob = remote.find_blob(cnb_yml.id()).unwrap();
        let yml_text = std::str::from_utf8(cnb_yml_blob.content()).unwrap();
        assert!(yml_text.contains("keepAliveTimeout"));
    }
}
