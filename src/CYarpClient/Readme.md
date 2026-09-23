# 客户端接入指南（CNB 工作空间侧）

把 CNB 工作空间里的 AI 端点，通过隧道发布到你的固定域名。本文只讲**工作空间侧**（CNB 仓库里的
`.cnb.yml` 与 `cyarp-start.sh`）；服务端的搭建与配置见 [`../CYarpServer/Readme.md`](../CYarpServer/Readme.md)。

```
公网客户端 ──Bearer ApiKey──▶ http://<你的域名>/<org>/<repo>/v1/chat/completions
                                      │  Kestrel 直接对外
                                      ▼
                              CYarpServer (VPS 系统服务)
                                      │  长连接隧道（工作空间主动向外拨号，所以子域名漂移无所谓）
                                      ▼
                       CYarpClient ← 跑在 CNB 工作空间（本目录的载荷）
                                      │  Authorization: Bearer $CNB_TOKEN
                                      ▼
                https://api.cnb.cool/$CNB_REPO_SLUG/-/ai/chat/completions
```

架构与设计取舍见仓库根目录 [`README.md`](../../README.md)。

---

## 0. 需要准备什么

| 项 | 说明 | 是否必需 |
|---|---|---|
| 已跑起来的 CYarpServer | 记下它的公网地址，例如 `http://ai.example.com` | 必需 |
| 要暴露 AI 端点的 CNB 仓库 | **载荷要放进这个仓库**：隧道出口是 `https://api.cnb.cool/$CNB_REPO_SLUG/-/ai`，而 `CNB_REPO_SLUG` 与 `CNB_TOKEN` 的作用域都是「工作空间所在仓库」 | 必需 |
| `CNB_TOKEN` | **平台自动注入**，每次流水线运行新铸一把，不需要申请、不要写进仓库 | 不用准备 |

客户端**不携带任何公网 key**：握手只发 CNB token。公网调用方的 `ApiKey` 只存在于服务端与调用方之间。

---

## 1. 本目录就是「CNB 仓库根内容」

把本目录里的东西原样放到 CNB 仓库根，即得到：

```
<CNB 仓库>/
  .cnb.yml           CNB 流水线配置（启动 stage + 保活 cron）
  cyarp-start.sh     启动脚本（按自身目录找 CYarpClient）
  CYarpClient        linux-x64 Native AOT 客户端（约 8 MB）
  appsettings.json   客户端非敏感默认配置（日志级别）
  Readme.md          本文件
  .gitattributes     强制脚本/YAML 为 LF、二进制不做行尾转换
  .gitignore         忽略运行时的 logs/
```

在本工具仓库里，这些文件的**源**在 `src/CYarpClient/`，`pwsh ./scripts/publish.ps1` 会把
`CYarpClient`（AOT 产物）与这几个文件一起放进 `artifacts/client/`。

落地到 CNB 仓库，两种方式任选其一：

```bash
# 方式一：把发布产物同步到 CNB 仓库的工作副本
rsync -a --delete artifacts/client/ /path/to/cnb-some-repo/
cd /path/to/cnb-some-repo && git add -A && git commit -m "update cyarp payload" && git push

# 方式二：直接在 cnb.cool 上新建仓库，把这几个文件按上面的结构推上去
```

如果希望载荷里只有运行必需的文件，同步时加 `--exclude Readme.md`；留着也无妨，它就是那个仓库的说明文档。

> 客户端运行时把 `cyarp.log` 与按天文件日志（`logs/log<yyyyMMdd>.txt`）都写在 `<载荷根>/logs/`。
> 载荷自带 `.gitignore`（内容就是 `logs/` 这一条目录级规则），所以不会再污染 `git status`；
> **不要**把它改成带具体日期的文件名——那样第二天就会漏忽略。
>
> 载荷同样自带 `.gitattributes`：`*.sh` / `*.yml` 强制 `eol=lf`，`CYarpClient` 标记为 binary。
> 在 Windows（`core.autocrlf=true`）上克隆也不会把脚本变成 CRLF，从根上避免 shebang 与 YAML 解析出问题。

---

## 2. 改 `.cnb.yml` 里的两处配置

`.cnb.yml` 已经可用，只需要改两处（文件里都标了「改这里」）：

```yaml
# 1/2 workspace：你的 CYarpServer 地址（加 TLS 后改 https://）
CYARP_SERVER_URI: "http://ai.example.com"

# 2/2 keepalive cron：服务端公网基址；留空则只做工作空间状态检查
FIXED_URL: "http://ai.example.com"
```

保活 cron（L1/L2）挂在同一个文件里**以分支名命名的顶层键**下（本载荷用 `master:`）。
这个键名指定「定时任务执行时拉取哪个分支的代码」，CNB 明确要求它**是仓库里真实存在的单一分支名**
（定时任务不支持 glob）。**写错分支名 = 定时任务永不触发 = 保活自愈层失效**，这是最容易踩的坑：

- 换仓库前先自查实际分支：`git ls-remote --heads <仓库地址>`；
- 若默认分支是 `main`，需把顶层 `master:` 与 cron 里两处 `{"branch":"master"}` 一起改成 `main`；
- 定时任务的 `CNB_TOKEN` 以「最后修改本配置的用户」身份签发，该用户被移出仓库后任务会失败
  （由其他有权限的人重新改一次配置即可恢复）。

探活打的是 `$FIXED_URL/$CNB_REPO_SLUG/v1/models`，该端点免鉴权，所以 **cron 里不需要任何 key**。

没改域名就开工作空间时，启动脚本会立刻打印 `CYARP_SERVER_URI is required, e.g. http://ai.example.com`
并以退出码 1 结束（构建变红），不会静默不建隧道。

---

## 3. 启动并验证

仓库页面点「云开发」启动工作空间，构建日志出现下面两行即隧道已通：

```
===== cyarp client started, pid=123 =====
===== cyarp client: tunnel established =====
```

脚本同时会打印本工作空间的 `clientId` 与公网基址：

```
===== clientId   : org/repo =====
===== public base: <your-domain>/org/repo/v1/   (http:// now, https:// once TLS is enabled) =====
```

`clientId` 固定就是 `CNB_REPO_SLUG`（含 `/`），不可配置，所以公网前缀不可能与隧道注册的 id 不一致。

启动脚本做的事：校验 `CYARP_SERVER_URI` / `CNB_TOKEN` → 幂等杀掉旧进程 → 把配置全部导出为环境变量
（token 一个字节都不落盘）→ `nohup` 拉起 → 轮询日志确认隧道建立，成功才让构建变绿。

---

## 4. 公网调用与路由规则

公网只有两条接口，`clientId` 由 `{org}` 与 `{repo}` 两段拼成（即仓库 slug，大小写敏感）：

| 请求 | 行为 |
|---|---|
| `GET /{org}/{repo}/v1/models` | **免鉴权**（本地应答、不耗额度）：在线 `200`，隧道断了 `503` |
| `POST /{org}/{repo}/v1/chat/completions` | 需要 `Authorization: Bearer <ApiKey>`；经隧道转发，上游不支持非流式（必须 `stream:true`） |
| `GET /` | **免鉴权**：在线工作空间的**对象数组**，每个对象含 `clientId`、`connectedAt`、`totalCredits`、`remainingCredits`、`stale`、`updatedAt`、`error`；无在线时为 `[]` |
| `GET /v1/models` | **免鉴权**：`ProxyModels` 配置的模型列表；**不检查在线状态**（恒 `200`），与带前缀版本的「离线 503」刻意区分 |
| `POST /v1/chat/completions` | 需要 `Authorization: Bearer <ApiKey>`；在当前在线的工作空间里挑**剩余 AI 额度最多**的那个转发（额度未知时退回随机）；无在线则 `502` |
| 其它路径（单段/三段前缀、`/v1/embeddings`…） | `404` |
| 方法不匹配（如 `POST .../v1/models`） | `405` |
| chat 的 `ApiKey` 缺失/错误 | `401` |

组织名恰好叫 `cyarp` 也没问题：`/cyarp/<repo>/v1/chat/completions` 这类公网路径照样要求 `ApiKey`，
只有真正的 `/cyarp` 握手端点免鉴权。

调用方有两种接法：

- **统一 base_url**（不必知道 workpace slug）：`base_url = <base>/v1`、`api_key = <ApiKey>`。请求会发给剩余额度最多的那个在线工作空间；额度读不到时退回随机。
- **固定工作空间**：`base_url = <base>/org/repo/v1`。
- 查看当前有哪些工作空间在线、各自剩多少额度：`curl <base>/`。

> 额度由**服务端**每 2 分钟经本工作空间的隧道去读
> `https://api.cnb.cool/{org}/-/charge/quota` 与 `/-/charge/volume`（剩余 = 总额度 − 已用 − 冻结，
> 单位 credit）。所以**客户端不需要做任何事、也不用改配置**：只要能建隧道，服务端就会自己去问。
> `GET /` 里的 `connectedAt` 是**当前这条隧道**建立的时刻（`+08:00`，重连后刷新），
> 它来回跳说明本工作空间的隧道在反复重建；`updatedAt` 是额度读数的时间，同一个 `+08:00` 偏移。
> `totalCredits` 是该组织当月总额度、`remainingCredits` 是剩余，已用 = 两者之差（单位 credit）。
> 该数组按**剩余额度倒序**排列（额度多的在前，读不到额度的排最后，并列按 clientId 升序），
> 所以谁在前面一眼就能看出谁剩得多。
> 2026-09 实测：工作空间里的 `CNB_TOKEN` **有** `group-resource:r` 权限，两个接口都返回 `200`
> （用 `bash scripts/verify-endpoint.sh` 第 5 节可以自己复现，它会打印完整响应体并算出剩余额度）。
> 万一读不到（接口 401/403、隧道抖动、超时），`GET /` 里该工作空间的 `remainingCredits` 是 `null`
> 且 `error` 非空，路由对它退化为「未知」——不会报错，只是不做额度优选。额度是**顶级组织级**的，
> 同组织的多个工作空间读数相同，所以优选真正有意义的是跨组织场景。

验收清单（假设工作空间 `org/repo` 已在线，`<base>` = `http://你的域名`）：

```bash
# 0) 有哪些工作空间在线 + 各自剩余额度（免鉴权）
curl -s $BASE/

# 1) 该工作空间的隧道状态（在线 200 / 隧道假死 503；本地应答、免鉴权、不耗额度）
curl -s -o /dev/null -w '%{http_code}\n' $BASE/org/repo/v1/models
# 期望 200

# 2) 真实调用（上游不支持非流式，必须 stream:true）：带前缀=指定工作空间，不带=挑额度最多的
curl -sN $BASE/v1/chat/completions \
  -H "Authorization: Bearer $ApiKey" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}],"stream":true}'
# 期望 SSE 分帧返回

# 3) 鉴权与路由边界
curl -s -o /dev/null -w '%{http_code}\n' $BASE/org/repo/v1/models -H "Authorization: Bearer wrong"   # 200（models 免鉴权）
curl -s -o /dev/null -w '%{http_code}\n' $BASE/ghost/repo/v1/models                                    # 503（离线也免鉴权）
curl -s -o /dev/null -w '%{http_code}\n' -X POST $BASE/ghost/repo/v1/chat/completions -H "Authorization: Bearer $ApiKey" -H 'Content-Type: application/json' -d '{}'  # 502
curl -s -o /dev/null -w '%{http_code}\n' -X POST $BASE/org/repo/v1/chat/completions -H 'Content-Type: application/json' -d '{}'  # 401（不带 key）
curl -s -o /dev/null -w '%{http_code}\n' $BASE/v1/models                                              # 200（裸路径，免鉴权）
```

OpenAI 客户端接入时：`base_url = <base>/org/repo/v1`，`api_key = <ApiKey>`。

> 上游会改写 `model` 字段：请求 `deepseek-v4-flash` 时响应里的 `model` 是 `deepseek-v4.1-flash`，
> `id` 是 32 位 hex（不是 `chatcmpl-` 前缀）。客户端若有严格校验需自行适配。

---

## 5. 保活与自愈

平台回收规则（官方 `docs.cnb.cool/zh/workspaces/workspace-recycling`）：页面关闭后 10 分钟无操作回收
（`keepAliveTimeout` 默认 600000ms）、单会话上限默认 18h、凌晨 4:00–6:00 强制回收。

| 层 | 手段 | 防的是什么 |
|---|---|---|
| L0 | `services.vscode.options.keepAliveTimeout: 18h` + 客户端 30s 心跳长连接 | 页面关闭 10 分钟即回收 |
| L1 | 分支键（本载荷为 `master:`）下的 cron 每 5 分钟查 `workspace/list`，无 running → `workspace/start` | 被平台回收后自动拉起 |
| L2 | cron 探活 `$FIXED_URL/$CNB_REPO_SLUG/v1/models`（**不带 key**），**连续两次拿到 `503`** 才 `stop + start` | 「假死」：workspace 显示 running 但隧道已断 |

> L2 只认 `503`（= 服务端活着但隧道断了，唯一可信的假死信号）。若探活拿到 `000`（超时/连不上）、
> `401`、`404`、`5xx`，说明**探活链路本身**不可信（服务端宕机、网络抖动、配置还没生效等），
> 此时脚本只记日志、不动工作空间 —— 否则服务端一挂就会每 5 分钟重建一次工作空间。

隧道断开由 CYarpClient 自己重连（5s/10s）；每次 cron 触发都是流水线新铸造的 `CNB_TOKEN`，工作空间重建后新 token 随新进程自动生效。

两个已在脚本里处理过的坑，自己改脚本时别踩回去：

1. `CYarpClientOptions__ConnectHeaders__X-CYarp-Client` 这种**带连字符的变量名不能用 `export`**（bash 报 `not a valid identifier`），必须用 `env "NAME=value"`。
2. CYarp 2.0.3 的就绪日志是中文「连接到服务器…成功」，判定要同时匹配中英文，否则构建永远不会变绿。

---

## 6. 配置参考

### 环境变量（由 `cyarp-start.sh` 读取）

| 变量 | 值 | 说明 |
|---|---|---|
| `CYARP_SERVER_URI` | `http://你的域名` | 服务端地址（**必填**）；加 TLS 后改 `https://` |
| `CYARP_RUN_DIR` | `<载荷根>/logs` | 客户端工作目录（`cyarp.log` 落在这里），可选 |
| `CYARP_BIN` | `<载荷根>/CYarpClient` | 客户端二进制路径（默认按脚本自身目录推导），可选 |

`clientId` 不是可配项：脚本固定用 `CNB_REPO_SLUG`，环境变量 `CYARP_CLIENT_ID` 已移除。

### 脚本内部导出的配置（token 只存在环境变量里，不落盘）

| 环境变量 | 值 |
|---|---|
| `CYarpClientOptions__ServerUri` | `$CYARP_SERVER_URI` |
| `CYarpClientOptions__TargetUri` | `https://api.cnb.cool/$CNB_REPO_SLUG/-/ai` |
| `CYarpClientOptions__ConnectHeaders__Authorization` | `Bearer $CNB_TOKEN` |
| `CYarpClientOptions__ConnectHeaders__X-CYarp-Client` | `$CNB_REPO_SLUG` |
| `CYarpClientOptions__KeepAliveInterval` | `00:00:30` |
| `CYarpClientOptions__ConnectTimeout` | `00:00:10` |

### 客户端产物升级

把新的 `CYarpClient` 放进载荷（覆盖旧文件）并同步到 CNB 仓库后，重启工作空间（或在工作空间里重跑该 stage）即可；
启动脚本会先 `pkill` 掉旧进程再拉起新的。

---

## 7. 故障排查

| 症状 | 可能原因 | 处理 |
|---|---|---|
| 一开工作空间就失败，日志里是 `CYARP_SERVER_URI is required, e.g. http://ai.example.com` | `.cnb.yml` 里的域名还没改（或没同步到 CNB 仓库） | 按第 2 节改 `CYARP_SERVER_URI`；确认改动已推到 CNB 仓库的默认分支 |
| 一开工作空间就失败，日志里是 `client binary not found or not executable: .../CYarpClient` | `cyarp-start.sh` 与 `CYarpClient` 没在同一目录（载荷结构被改动），或二进制没随仓库提交 | 保持载荷结构不变；确认 `CYarpClient` 已提交（注意 `.gitignore` 别把它排除） |
| 构建日志停在 `cyarp client: no tunnel after 30s` | 域名/端口不通、`CYARP_SERVER_URI` 写错、服务端没起 | 看脚本打印的 `cyarp.log` 尾部；在 VPS 上 `curl -i http://127.0.0.1/cyarp`（有响应说明服务在监听） |
| 客户端日志 `Connect failure ... Connection refused` | 服务端未监听该地址/端口，或被防火墙拦 | 确认服务端 `systemctl status CYarpServer` 与 `Now listening on: ...`；放行端口 |
| 客户端日志出现 `连接到服务器...成功` 但公网仍 `502`/`404` | 公网 URL 的 `{org}/{repo}` 与实际工作空间仓库不一致 | `clientId` 固定取 `CNB_REPO_SLUG`，核对 URL 前缀是否与仓库 slug 一致（大小写敏感） |
| `/{org}/{repo}/v1/models` 返回 `503` | 服务端活着但该工作空间的隧道断了（假死） | cron 会自动 `stop + start`；也可手动重启工作空间 |
| 工作空间被平台回收后没人拉起（端点长期不通、`workspace/list` 显示没有 running） | cron 的**分支键名**与仓库真实分支不一致（例如写成 `main:` 而后端仓库只有 `master`），或改动没推到该分支 | `git ls-remote --heads <仓库地址>` 查真实分支；把 `.cnb.yml` 顶层键与 cron 里两处 `{"branch":"..."}` 改成该分支；确认改动已推到那个分支。仓库页「定时任务」页应能看到该任务 |
| cron 日志里出现 `endpoint probe inconclusive (http=000/401/5xx)` | 探活链路不可信（服务端宕机、网络抖动、配置未生效）。脚本**故意**不重建工作空间 | 先修服务端/域名；这是预期行为，不是故障 |
| chat 返回 `401` | 没带 key 或 key 与服务端 `ApiKey` 不一致 | 加 `-H "Authorization: Bearer <ApiKey>"`；注意 `v1/models` 免鉴权，**它返回 401 才说明服务端键名或配置有问题** |
| chat 返回 `502` | 前缀写错/大小写不符，或用了裸路径 | 用形如 `<base>/<org>/<repo>/v1/chat/completions` 的地址 |
| 上游返回 401/412 之类 | 上游鉴权/网络策略 | 412 表示请求不是从 CNB 工作空间内发出；401 多半是 `CNB_TOKEN` 过期，重启工作空间让平台重铸 |
| `GET /` 里本工作空间的 `remainingCredits` 一直是 `null`、`error` 非空 | 服务端读不到额度：可能该 `CNB_TOKEN` 没有 `group-resource:r` 权限（`401/403`）、仓库在子组织下（额度接口要根组织，可能 `403/404`）、或隧道刚连上还没到下一个探测周期 | 先看服务端日志里的 `AI quota probe of workspace ... failed`；在工作空间里跑 `scripts/verify-endpoint.sh` 的第 5 节直连试一次。**这是退化不是故障**：chat 照常可用，只是不做额度优选 |

常用观察点（都在工作空间内）：

```bash
tail -f logs/cyarp.log                        # 客户端控制台输出（脚本 stdout 重定向到这里）
tail -f logs/log$(date +%Y%m%d).txt           # 客户端文件日志（相对载荷根）
```

> 默认 `CYARP_RUN_DIR=<载荷根>/logs`，所以 `cyarp.log` 与按天文件日志都落在载荷根的 `logs/` 下
> （该目录已被载荷自带的 `.gitignore` 忽略）。

---

## 8. 安全须知

- **隧道入站不做鉴权**：客户端只带 CNB token（不具备公网密钥语义），服务端不做准入拒绝。因此能访问到服务端端口的人
  都可以用任意 `X-CYarp-Client` 注册隧道，把该工作空间的请求引到自己那里（能看到请求体），并把真正的工作空间踢下线。
  要收紧需在服务端 `CYarpTunnelAuthenticationHandler` 里加共享密钥校验（服务端配 `TunnelKey`、工作空间注入对应环境变量）。
- **`GET /{org}/{repo}/v1/models` 与 `GET /` 免鉴权**：前者是本地应答的静态模型列表（不耗额度、不触上游），
  免鉴权是为了让保活 cron 不带任何密钥探活；后者是发现接口，会暴露有哪些工作空间在线**以及各组织的剩余
  AI 额度数字**（不含任何 token）。
- **现在没有 TLS**：CNB token（握手头 + 隧道内注入）与 API 内容都是明文。上 TLS 时把 `CYARP_SERVER_URI` 改成
  `https://`、`FIXED_URL` 改成 `https://`，服务端加证书即可，代码不用动。
- **预编译客户端的信任链最弱**：`CYarpClient` 是 8 MB 的 Native AOT 单文件可执行程序（stripped、无签名），
  而它会拿到 `CNB_TOKEN` 并代理 AI 请求。当前没有 hash/签名校验，升级方式就是人工覆盖文件。
  想收紧可以：在载荷里加一份 `CYarpClient.sha256` 并由启动脚本校验、或改成工作空间内从源码构建、
  或重新入库时改走 git-lfs 并附校验值。

---

## 9. 仓库体积备注（维护者）

历史提交里曾有 4 个约 8 MB 的客户端二进制（`deploy/cyarp/CYarpClient`，3 个不同 hash），
合计约 32 MB 常驻在 git 历史中。现在载荷不入库（`artifacts/` 被忽略），新提交不会再增重。

如需彻底瘦身，可重写历史（**会改所有 commit hash，需要强推并通知协作者**）：

```bash
git filter-repo --path deploy/cyarp/CYarpClient --invert-paths
git push --force-with-lease origin --all
```

若以后确实要把二进制重新入库，建议改用 git-lfs（本机已装 `git-lfs`）：

```bash
git lfs install
git lfs track "CYarpClient"
```
