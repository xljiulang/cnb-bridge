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

> 客户端运行时会把自己的文件日志写到 `<载荷根>/logs/`（按天分文件）。不想让它出现在 `git status` 里，
> 就在**载荷仓库**的 `.gitignore` 加一条 `logs/`。
>
> `CYarpClient` 是二进制，别用会改写内容的工具处理它；`cyarp-start.sh` 与 `.cnb.yml` 保持 LF 行尾。

---

## 2. 改 `.cnb.yml` 里的两处配置

`.cnb.yml` 已经可用，只需要改两处（文件里都标了「改这里」）：

```yaml
# 1/2 workspace：你的 CYarpServer 地址（加 TLS 后改 https://）
CYARP_SERVER_URI: "http://ai.example.com"

# 2/2 keepalive cron：服务端公网基址；留空则只做工作空间状态检查
FIXED_URL: "http://ai.example.com"
```

保活 cron（L1/L2）在同一个文件的 `main:` 键下，**必须是 `main:`（对应默认分支名），否则永不触发**；一般不用动。
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
| 其它路径（裸 `/v1/*`、单段/三段前缀、`/v1/embeddings`…） | `404` |
| 方法不匹配（如 `POST .../v1/models`） | `405` |
| chat 的 `ApiKey` 缺失/错误 | `401` |

组织名恰好叫 `cyarp` 也没问题：`/cyarp/<repo>/v1/chat/completions` 这类公网路径照样要求 `ApiKey`，
只有真正的 `/cyarp` 握手端点免鉴权。

验收清单（假设工作空间 `org/repo` 已在线，`<base>` = `http://你的域名`）：

```bash
# 1) 该工作空间的隧道状态（在线 200 / 隧道假死 503；本地应答、免鉴权、不耗额度）
curl -s -o /dev/null -w '%{http_code}\n' $BASE/org/repo/v1/models
# 期望 200

# 2) 真实调用（上游不支持非流式，必须 stream:true）
curl -sN $BASE/org/repo/v1/chat/completions \
  -H "Authorization: Bearer $ApiKey" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}],"stream":true}'
# 期望 SSE 分帧返回

# 3) 鉴权与路由边界
curl -s -o /dev/null -w '%{http_code}\n' $BASE/org/repo/v1/models -H "Authorization: Bearer wrong"   # 200（models 免鉴权）
curl -s -o /dev/null -w '%{http_code}\n' $BASE/ghost/repo/v1/models                                    # 503（离线也免鉴权）
curl -s -o /dev/null -w '%{http_code}\n' -X POST $BASE/ghost/repo/v1/chat/completions -H "Authorization: Bearer $ApiKey" -H 'Content-Type: application/json' -d '{}'  # 502
curl -s -o /dev/null -w '%{http_code}\n' -X POST $BASE/org/repo/v1/chat/completions -H 'Content-Type: application/json' -d '{}'  # 401（不带 key）
curl -s -o /dev/null -w '%{http_code}\n' $BASE/v1/models -H "Authorization: Bearer $ApiKey"            # 404（裸路径未发布）
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
| L1 | `main:` cron 每 5 分钟查 `workspace/list`，无 running → `workspace/start` | 被平台回收后自动拉起 |
| L2 | cron 探活 `$FIXED_URL/$CNB_REPO_SLUG/v1/models`（**不带 key**），连续两次非 200 → `stop + start` | 「假死」：workspace 显示 running 但隧道已断 |

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
| `CYARP_RUN_DIR` | `/workspace/cyarp` | 客户端工作目录（`cyarp.log` 落在这里），可选 |
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
| chat 返回 `401` | 没带 key 或 key 与服务端 `ApiKey` 不一致 | 加 `-H "Authorization: Bearer <ApiKey>"`；注意 `v1/models` 免鉴权，**它返回 401 才说明服务端键名或配置有问题** |
| chat 返回 `502` | 前缀写错/大小写不符，或用了裸路径 | 用形如 `<base>/<org>/<repo>/v1/chat/completions` 的地址 |
| 上游返回 401/412 之类 | 上游鉴权/网络策略 | 412 表示请求不是从 CNB 工作空间内发出；401 多半是 `CNB_TOKEN` 过期，重启工作空间让平台重铸 |

常用观察点（都在工作空间内）：

```bash
tail -f /workspace/cyarp/cyarp.log            # 客户端控制台输出（脚本 stdout 重定向到这里）
tail -f logs/log$(date +%Y%m%d).txt           # 客户端文件日志（相对载荷根）
```

---

## 8. 安全须知

- **隧道入站不做鉴权**：客户端只带 CNB token（不具备公网密钥语义），服务端不做准入拒绝。因此能访问到服务端端口的人
  都可以用任意 `X-CYarp-Client` 注册隧道，把该工作空间的请求引到自己那里（能看到请求体），并把真正的工作空间踢下线。
  要收紧需在服务端 `CYarpTunnelAuthenticationHandler` 里加共享密钥校验（服务端配 `TunnelKey`、工作空间注入对应环境变量）。
- **`GET /{org}/{repo}/v1/models` 免鉴权**：它是本地应答的静态模型列表（不耗额度、不触上游），免鉴权是为了让保活 cron
  不带任何密钥探活；知道组织/仓库名的人可以读到模型列表与该工作空间的隧道在线状态。
- **现在没有 TLS**：CNB token（握手头 + 隧道内注入）与 API 内容都是明文。上 TLS 时把 `CYARP_SERVER_URI` 改成
  `https://`、`FIXED_URL` 改成 `https://`，服务端加证书即可，代码不用动。
