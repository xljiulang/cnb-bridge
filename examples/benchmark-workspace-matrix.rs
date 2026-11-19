use anyhow::{bail, Context, Result};
use cnb_bridge::account::{AccountBridgeSettings, AccountManager, AccountState};
use cnb_bridge::api::{CnbApi, DEFAULT_API_URL};
use cnb_bridge::bridge::{Bridge, BridgeState};
use cnb_bridge::logging::Logger;
use cnb_bridge::settings::SettingsStore;
use cnb_bridge::ssh_key::SshKeyStore;
use cnb_bridge::workspace::RepoProvisioner;
use git2::build::CheckoutBuilder;
use git2::{FetchOptions, PushOptions, RemoteCallbacks, Signature};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_RUNS: usize = 3;
const WORKSPACE_RECYCLE_TIMEOUT: Duration = Duration::from_secs(180);
const START_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug)]
struct Args {
    repo_slug: String,
    branch: String,
    runs: usize,
    repo_config: PathBuf,
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunResult {
    run: usize,
    success: bool,
    workspace_ms: Option<u64>,
    ssh_ms: Option<u64>,
    agent_health_ms: Option<u64>,
    total_ms: u64,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkReport {
    repo_slug: String,
    branch: String,
    config: String,
    runs: Vec<RunResult>,
    failure_count: usize,
    failure_rate: f64,
    workspace_median_ms: Option<u64>,
    ssh_median_ms: Option<u64>,
    agent_health_median_ms: Option<u64>,
    total_median_ms: Option<u64>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = parse_args()?;
    let config = fs::read_to_string(&args.repo_config)
        .with_context(|| format!("读取配置 {} 失败", args.repo_config.display()))?;
    validate_config(&config, &args.repo_config)?;

    let (namespace, repo_name) = args
        .repo_slug
        .split_once('/')
        .filter(|(namespace, repo)| {
            !namespace.is_empty() && !repo.is_empty() && !repo.contains('/')
        })
        .with_context(|| format!("RepoSlug 必须为 owner/repo: {}", args.repo_slug))?;

    let manager = AccountManager::new();
    let status = manager.status();
    if status.state != AccountState::Connected {
        bail!("当前没有已连接的 CNB 账号，请先登录 CNB Bridge");
    }
    let token = manager
        .access_token()
        .await
        .context("读取当前登录凭据失败")?;
    let api = CnbApi::new();
    let provisioned = RepoProvisioner::new(&api)
        .ensure(&token, repo_name, namespace, true)
        .await
        .context("准备独立 benchmark 仓库失败")?;
    if !provisioned.slug.eq_ignore_ascii_case(&args.repo_slug) {
        bail!(
            "请求的 benchmark 仓库 {} 被解析成了 {}",
            args.repo_slug,
            provisioned.slug
        );
    }
    if !provisioned.branch.eq_ignore_ascii_case(&args.branch) {
        bail!(
            "仓库 {} 的桥接分支为 {}，与参数 -Branch {} 不一致",
            provisioned.slug,
            provisioned.branch,
            args.branch
        );
    }

    publish_config(&token, &args.repo_slug, &args.branch, config.as_bytes()).await?;
    confirm_config(
        &api,
        &token,
        &args.repo_slug,
        &args.branch,
        config.as_bytes(),
    )
    .await?;

    let original_settings = manager.active_bridge_settings();
    manager
        .update_active_bridge_settings(AccountBridgeSettings {
            repo_namespace: namespace.to_string(),
            repo_name: repo_name.to_string(),
        })
        .context("临时切换活动账号的 benchmark 仓库失败")?;

    let matrix_result = run_matrix(&args, &token, &api).await;

    let restore_result = match original_settings {
        Some(settings) => manager
            .update_active_bridge_settings(settings)
            .context("恢复活动账号原来的仓库设置失败"),
        None => Ok(()),
    };
    match (matrix_result, restore_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(report), Ok(())) => {
            print_report(&report)?;
            if let Some(path) = args.output {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("创建结果目录 {} 失败", parent.display()))?;
                }
                fs::write(&path, serde_json::to_vec_pretty(&report)?)
                    .with_context(|| format!("写入结果 {} 失败", path.display()))?;
                println!("Result written to {}", path.display());
            }
            Ok(())
        }
    }
}

async fn run_matrix(args: &Args, token: &str, api: &CnbApi) -> Result<BenchmarkReport> {
    let mut runs = Vec::with_capacity(args.runs);
    for run in 1..=args.runs {
        stop_running_workspaces(token, api, &args.repo_slug).await?;
        wait_for_no_running_workspace(token, api, &args.repo_slug, WORKSPACE_RECYCLE_TIMEOUT)
            .await?;
        println!(
            "Run {run}/{}: cold workspace is stopped, starting bridge...",
            args.runs
        );

        let temp = tempfile::tempdir().context("创建临时运行目录失败")?;
        let logger = Arc::new(Logger::new(256));
        let account = AccountManager::new();
        let bridge = Bridge::new(
            account,
            SettingsStore::new(temp.path().join("settings.json")),
            SshKeyStore::default_path(),
            Arc::clone(&logger),
        );
        let started = Instant::now();
        let result = tokio::time::timeout(START_TIMEOUT, bridge.start()).await;
        let total_ms = elapsed_ms(started.elapsed());
        let (success, error) = match result {
            Ok(Ok(snapshot)) if snapshot.state == BridgeState::Ready => (true, None),
            Ok(Ok(snapshot)) => (
                false,
                Some(format!("桥接在完成前进入状态 {:?}", snapshot.state)),
            ),
            Ok(Err(error)) => (false, Some(error.to_string())),
            Err(_) => (false, Some("冷启动超过 180 秒".to_string())),
        };
        let log_text = logger
            .recent(256)
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>()
            .join("\n");
        let run_result = RunResult {
            run,
            success,
            workspace_ms: phase_ms(&log_text, "workspace"),
            ssh_ms: phase_ms(&log_text, "ssh"),
            agent_health_ms: phase_ms(&log_text, "agent_health"),
            total_ms,
            error,
        };
        println!(
            "Run {run}/{}: success={}, workspace={:?}ms, ssh={:?}ms, agent_health={:?}ms, total={}ms",
            args.runs,
            run_result.success,
            run_result.workspace_ms,
            run_result.ssh_ms,
            run_result.agent_health_ms,
            run_result.total_ms
        );
        runs.push(run_result);

        bridge.disconnect().await;
        stop_running_workspaces(token, api, &args.repo_slug).await?;
        wait_for_no_running_workspace(token, api, &args.repo_slug, WORKSPACE_RECYCLE_TIMEOUT)
            .await?;
    }

    let failure_count = runs.iter().filter(|run| !run.success).count();
    Ok(BenchmarkReport {
        repo_slug: args.repo_slug.clone(),
        branch: args.branch.clone(),
        config: args.repo_config.display().to_string(),
        failure_rate: failure_count as f64 / runs.len() as f64,
        failure_count,
        workspace_median_ms: median(runs.iter().filter_map(|run| run.workspace_ms).collect()),
        ssh_median_ms: median(runs.iter().filter_map(|run| run.ssh_ms).collect()),
        agent_health_median_ms: median(runs.iter().filter_map(|run| run.agent_health_ms).collect()),
        total_median_ms: median(
            runs.iter()
                .filter(|run| run.success)
                .map(|run| run.total_ms)
                .collect(),
        ),
        runs,
    })
}

async fn stop_running_workspaces(token: &str, api: &CnbApi, slug: &str) -> Result<()> {
    let workspaces = api
        .list_workspaces(token, slug)
        .await
        .with_context(|| format!("读取 {slug} 的 workspace 列表失败"))?;
    if !workspaces
        .list
        .iter()
        .any(|workspace| workspace.status == "running")
    {
        return Ok(());
    }
    let endpoint = std::env::var("CNB_BRIDGE_API_URL")
        .unwrap_or_else(|_| DEFAULT_API_URL.to_string())
        .trim_end_matches('/')
        .to_string();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("创建 benchmark workspace 控制客户端失败")?;
    for workspace in workspaces
        .list
        .into_iter()
        .filter(|workspace| workspace.status == "running")
    {
        println!("Stopping benchmark workspace {}", workspace.sn);
        let response = http
            .post(format!("{endpoint}/workspace/stop"))
            .bearer_auth(token)
            .header("Accept", "application/vnd.cnb.api+json")
            .json(&serde_json::json!({ "sn": workspace.sn }))
            .send()
            .await
            .with_context(|| format!("请求停止 workspace {} 失败", workspace.sn))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("读取 workspace 停止响应失败")?;
        if status.is_success() || is_already_ended_success_response(status, &body) {
            continue;
        }
        bail!(
            "停止 benchmark workspace {} 失败: HTTP {} {}",
            workspace.sn,
            status.as_u16(),
            body
        );
    }
    Ok(())
}

fn is_already_ended_success_response(status: reqwest::StatusCode, body: &str) -> bool {
    if status != reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        return false;
    }
    let normalized = body
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    normalized.contains("stop_fail_wrong_status") && normalized.contains("already end: success")
}

async fn wait_for_no_running_workspace(
    token: &str,
    api: &CnbApi,
    slug: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let workspaces = api
            .list_workspaces(token, slug)
            .await
            .with_context(|| format!("检查 {slug} 的 workspace 状态失败"))?;
        if !workspaces
            .list
            .iter()
            .any(|workspace| workspace.status == "running")
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("等待 {slug} 的 workspace 停止超时；请先在 CNB 控制台停止运行中的云主机");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn publish_config(token: &str, slug: &str, branch: &str, config: &[u8]) -> Result<()> {
    let token = token.to_string();
    let slug = slug.to_string();
    let branch = branch.to_string();
    let config = config.to_vec();
    tokio::task::spawn_blocking(move || push_config_file(&token, &slug, &branch, &config))
        .await
        .context("配置推送任务异常退出")??;
    Ok(())
}

fn push_config_file(token: &str, slug: &str, branch: &str, config: &[u8]) -> Result<()> {
    let temp = tempfile::tempdir().context("创建 benchmark 仓库临时目录失败")?;
    let url = format!("https://cnb.cool/{slug}");
    let mut fetch = FetchOptions::new();
    fetch.remote_callbacks(git_callbacks(token));
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fetch);
    let repo = builder
        .clone(&url, temp.path())
        .with_context(|| format!("克隆 benchmark 仓库 {slug} 失败"))?;

    let remote_branch = repo
        .find_reference(&format!("refs/remotes/origin/{branch}"))
        .or_else(|_| repo.find_reference("refs/remotes/origin/HEAD"))
        .with_context(|| format!("benchmark 仓库缺少分支 {branch}"))?;
    let target = remote_branch
        .target()
        .context("benchmark 分支没有目标提交")?;
    repo.reference(
        &format!("refs/heads/{branch}"),
        target,
        true,
        "checkout benchmark branch",
    )
    .context("创建 benchmark 本地分支失败")?;
    repo.set_head(&format!("refs/heads/{branch}"))
        .context("设置 benchmark HEAD 失败")?;
    repo.checkout_head(Some(CheckoutBuilder::new().force()))
        .context("检岀 benchmark 分支失败")?;

    fs::write(temp.path().join(".cnb.yml"), config).context("写入 benchmark .cnb.yml 失败")?;
    let mut index = repo.index().context("读取 benchmark Git 索引失败")?;
    index
        .add_path(Path::new(".cnb.yml"))
        .context("加入 benchmark 配置到 Git 索引失败")?;
    index.write().context("写入 benchmark Git 索引失败")?;
    let tree_id = index.write_tree().context("生成 benchmark Git tree 失败")?;
    let head = repo.head().and_then(|head| head.peel_to_commit()).ok();
    if head.as_ref().is_some_and(|head| head.tree_id() == tree_id) {
        println!("Benchmark config is already current");
        return Ok(());
    }
    let tree = repo
        .find_tree(tree_id)
        .context("读取 benchmark Git tree 失败")?;
    let signature = Signature::now("CNB Bridge Benchmark", "cnb-bridge@localhost")
        .context("创建 benchmark 提交签名失败")?;
    let parents = head.iter().collect::<Vec<_>>();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "benchmark: update workspace startup config",
        &tree,
        &parents,
    )
    .context("提交 benchmark 配置失败")?;

    let mut remote = repo
        .find_remote("origin")
        .context("读取 benchmark Git 远端失败")?;
    let mut push = PushOptions::new();
    push.remote_callbacks(git_callbacks(token));
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote
        .push(&[refspec.as_str()], Some(&mut push))
        .with_context(|| format!("推送 benchmark 配置到 {branch} 失败"))
}

async fn confirm_config(
    api: &CnbApi,
    token: &str,
    slug: &str,
    branch: &str,
    expected: &[u8],
) -> Result<()> {
    let expected = std::str::from_utf8(expected).context("benchmark 配置不是 UTF-8")?;
    for attempt in 0..10 {
        match api.file_text(token, slug, ".cnb.yml", branch).await {
            Ok(actual) if actual == expected => return Ok(()),
            Ok(_) | Err(_) if attempt < 9 => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok(_) => bail!("推送后读取到的 .cnb.yml 与本地 benchmark 配置不一致"),
            Err(error) => return Err(error).context("确认 benchmark 配置失败"),
        }
    }
    unreachable!()
}

fn git_callbacks(token: &str) -> RemoteCallbacks<'static> {
    let token = token.to_string();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, _username, _allowed| {
        git2::Cred::userpass_plaintext("cnb", &token)
    });
    callbacks
}

fn phase_ms(log: &str, phase: &str) -> Option<u64> {
    let marker = format!("bridge phase {phase} completed; elapsed=");
    log.lines().find_map(|line| {
        line.strip_prefix(&marker)
            .and_then(|value| value.strip_suffix("ms"))
            .and_then(|value| value.parse().ok())
    })
}

fn median(mut values: Vec<u64>) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[middle]
    } else {
        values[middle - 1].saturating_add(values[middle]) / 2
    })
}

fn elapsed_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn validate_config(config: &str, path: &Path) -> Result<()> {
    if !config.contains("start-cnb-ai-bridge")
        || !config
            .lines()
            .any(|line| line.trim_start().starts_with("image:"))
    {
        bail!("{} 不是有效的 CNB Bridge workspace 配置", path.display());
    }
    Ok(())
}

fn print_report(report: &BenchmarkReport) -> Result<()> {
    println!(
        "Median: workspace={:?}ms, ssh={:?}ms, agent_health={:?}ms, total={:?}ms, failure_rate={:.1}%",
        report.workspace_median_ms,
        report.ssh_median_ms,
        report.agent_health_median_ms,
        report.total_median_ms,
        report.failure_rate * 100.0
    );
    println!("{}", serde_json::to_string_pretty(report)?);
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut repo_slug = None;
    let mut branch = None;
    let mut runs = DEFAULT_RUNS;
    let mut repo_config = None;
    let mut output = None;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("缺少参数 {argument} 的值"))
        };
        match argument.as_str() {
            "--repo-slug" => repo_slug = Some(value()?),
            "--branch" => branch = Some(value()?),
            "--runs" => runs = value()?.parse().context("--runs 必须是正整数")?,
            "--repo-config" => repo_config = Some(PathBuf::from(value()?)),
            "--output" => output = Some(PathBuf::from(value()?)),
            other => bail!("未知参数: {other}"),
        }
    }
    if !(1..=20).contains(&runs) {
        bail!("--runs 必须位于 1..=20");
    }
    Ok(Args {
        repo_slug: repo_slug.context("缺少 --repo-slug")?,
        branch: branch.context("缺少 --branch")?,
        runs,
        repo_config: repo_config.context("缺少 --repo-config")?,
        output,
    })
}
