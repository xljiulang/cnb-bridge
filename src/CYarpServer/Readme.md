# 服务端部署指南（CYarpServer）

把 CNB 工作空间里的 AI 端点，发布成固定域名的 OpenAI 兼容 API。本文只讲**服务端**（VPS / Windows 上的
`CYarpServer`）；工作空间侧的接入见 [`../CYarpClient/Readme.md`](../CYarpClient/Readme.md)。

``` 
公网客户端 ──Bearer ApiKey──▶ http://<你的域名>/<org>/<repo>/v1/chat/completions
                                      │  Kestrel 直接对外（无 Docker、无 Nginx/Caddy）
                                      ▼
                              CYarpServer (VPS 系统服务)
                                      │  长连接隧道（工作空间主动向外拨号）
                                      ▼
                       CYarpClient ← 跑在 CNB 工作空间
                                      │  Authorization: Bearer $CNB_TOKEN
                                      ▼
                https://api.cnb.cool/$CNB_REPO_SLUG/-/ai/chat/completions
```

架构与设计取舍见仓库根目录 [`README.md`](../../README.md)。

---

## 1. 需要准备什么

| 项 | 说明 | 是否必需 |
|---|---|---|
| VPS | Linux x64（glibc ≥ 2.17，即 CentOS 7 / Ubuntu 18.04 及以上）或 Windows；有公网 IP | 必需 |
| 域名 | 指向该 VPS 的 A 记录；现在只跑 HTTP，所以不必配证书 | 可选（也可以直接用 IP） |
| `ApiKey` | 公网 chat 调用方要带的密钥（一个服务一份通用 key），自己编一个强随机串 | 必需 |
| `CNB_TOKEN` | **平台自动注入**到工作空间，不需要申请、不要写进仓库 | 不用准备 |

---

## 2. 构建产物（开发机）

前置：.NET 10 SDK。构建机需联网一次（下载 ILCompiler 与 zig 工具链，之后进 NuGet 缓存可离线复现）。

```powershell
# 默认一次产出：server 的 linux-x64 与 win-x64，以及客户端载荷
pwsh ./scripts/publish.ps1

# 只发服务端，或只发某一个架构
pwsh ./scripts/publish.ps1 -SkipClient
pwsh ./scripts/publish.ps1 -SkipClient -ServerRuntimes linux-x64
pwsh ./scripts/publish.ps1 -SkipClient -ServerRuntimeIdentifier win-x64
```

服务端产物（**自包含 + trimmed 单文件**，目标机器不需要装 .NET 运行时）：

| 产物 | 位置 | 大小 |
|---|---|---|
| Linux | `artifacts/server/linux-x64/CYarpServer` | 约 17 MB |
| Windows | `artifacts/server/win-x64/CYarpServer.exe` | 约 15 MB |

> 发布时会有一条 `IL2104: Assembly 'CYarp.Server' produced trim warnings`，这是 CYarp 包内部的裁剪提示，
> 属已知情况（见根 README 第七节）。服务端不使用 Native AOT：`CYarp.Server` 包用
> `Map(pattern, Delegate)` 映射端点，AOT 下首个请求会失败。

---

## 3. 部署（Linux / VPS）

### 3.1 上传

只需两个文件，放在同一个目录（下文以 `/opt/cyarp/` 为例）：

```bash
sudo mkdir -p /opt/cyarp && cd /opt/cyarp

# 在开发机上
scp artifacts/server/linux-x64/CYarpServer   root@<VPS_IP>:/opt/cyarp/
scp artifacts/server/linux-x64/appsettings.json root@<VPS_IP>:/opt/cyarp/

# 在 VPS 上
chmod +x /opt/cyarp/CYarpServer
```

### 3.2 配置

编辑 `/opt/cyarp/appsettings.json`，**至少改掉 `ApiKey`**：

```jsonc
{
  // 公网 chat 调用方必须带这个 key（Authorization: Bearer <ApiKey>）；留空表示不校验
  // 注意：键名是 ApiKey，不是 ProxyKey —— 写成旧名会被当作未配置，公网就不校验了
  "ApiKey": "换成你自己的强随机串",

  // /{org}/{repo}/v1/models 本地返回的模型列表，不占上游额度（JSON 数组，不是逗号字符串）
  "ProxyModels": [ "deepseek-v4-flash", "glm-5.3-flash", "kimi-k3" ],

  // 裸 /v1/chat/completions 在剩余 AI 额度超阈值的工作空间之间随机；Enabled=false 则回到等概率随机
  "AiQuota": {
    "Enabled": true,
    "Interval": "00:02:00",   // 多久经各工作空间的隧道重新读一次额度
    "StaleAfter": "00:06:00", // 超过这个年龄的读数视为「未知」，不再用于路由（默认容忍连续 3 次失败）
    "Timeout": "00:00:10",    // 单个工作空间一轮探测（两个请求合计）的超时
    "MinRemainingCredits": 1  // 剩余额度严格大于它才算「额度充足」，单位 credit（默认 1）
  },

  // 监听地址：不写则默认 http://+:80（80 端口需要 root，ServiceSelf 安装时本来就是 root）
  // "Urls": "http://+:8080",

  "CYarpOptions": {
    "Node": "node0",
    "Client": { "KeepAlive": true, "KeepAliveInterval": "00:00:40" },
    "HttpTunnel": { "MaxTunnelsPerClient": 20, "CreationTimeout": "00:00:10" }
  },
  "Logging": {
    "LogLevel": { "Default": "Information", "CYarp": "Information", "Yarp.ReverseProxy": "Warning" }
  }
}
```

`CYarpOptions` 的生效值会在启动日志里回显（`CYarp options in effect: ...`）；如果这里与文件不一致，说明配置没被读到。

`AiQuota` 是**可选**的：整段删掉就用默认值（即开启、2 分钟一轮）。探测有两个触发源：**工作空间一连上就立刻探一次**
（经由 CYarp 的客户端状态监听），以及启动后立刻一轮 + 之后每个 `Interval` 一轮的节拍。
日志里能看到 `AI quota of workspace '...' (organization ...): N milli-credits remaining`；
读不到时是 `AI quota probe of workspace '...' failed: ...`（**不影响转发**，只是该工作空间在路由里算「未知」）。
一轮是**顺序**探所有工作空间、每个最多占 `Timeout`，所以最坏耗时约为「工作空间数 × 10s」；探测漏拍不补跑，
超时只表现为下一轮被推迟（当前规模下远小于 2 分钟）。

### 3.3 安装并启动为系统服务

```bash
cd /opt/cyarp
sudo ./CYarpServer start      # 安装 + 启动 + 设置开机自启
```

`start` 背后做的事：把本程序写进 `/etc/systemd/system/CYarpServer.service`、执行 `systemctl daemon-reload / start / enable`。

```bash
sudo ./CYarpServer logs                 # 跟随服务日志（通过命名管道订阅服务进程的日志）
sudo ./CYarpServer logs filter=ERROR    # 只看含关键字 ERROR 的日志
sudo ./CYarpServer stop                 # 停止 + 取消开机自启 + 删除 unit 文件

# 服务已经安装好之后，也可以用原生 systemd 命令
sudo systemctl status CYarpServer
sudo systemctl restart CYarpServer
sudo journalctl -u CYarpServer -f       # 需要看 systemd 侧的日志时用这个
```

也可以前台直接跑，方便调试：

```bash
sudo ./CYarpServer        # 不接任何子命令时就是前台运行，Ctrl+C 退出
```

> 启动日志里应能看到 `Now listening on: http://...:80` 与 `CYarp options in effect: ...`。

### 3.4 验证服务端

```bash
curl -i http://<你的域名或IP>/                       # 当前在线工作空间 + 各自剩余额度（无在线时返回 []）
curl -i http://<你的域名或IP>/v1/models              # 配置的模型列表（恒 200）
curl -i http://<你的域名或IP>/<org>/<repo>/v1/models  # 某个工作空间是否在线（离线 503）
```

这三个端点都免鉴权（本地应答、不耗额度）。此时若还没有工作空间接入：

| 请求 | 期望 |
|---|---|
| `GET /`（不需要 key） | `200` + `[]`（没有在线工作空间；有在线时是含 `clientId`/`remainingCredits` 的对象数组） |
| `GET /v1/models`（不需要 key） | `200` + 模型列表（不看在不在线） |
| `GET /{org}/{repo}/v1/models`（不需要 key） | `503`：服务端活着，但该工作空间的隧道还没连上 |
| `POST /{org}/{repo}/v1/chat/completions` + 正确 `ApiKey` | `502`（没有在线工作空间） |
| `POST /v1/chat/completions` + 正确 `ApiKey` | `502`（没有在线工作空间可挑） |
| 同样请求但不带 key | `401`（chat 需要 `ApiKey`；若 key 配空则不校验） |

工作空间接入后，`GET /` 的响应形如：

```json
[{"clientId":"org/repo","connectedAt":"2026-09-23T11:29:00+08:00","totalCredits":1166,"remainingCredits":896.83,"stale":false,"updatedAt":"2026-09-23T16:12:00+08:00","error":null}]
```

`connectedAt` 是**当前这条隧道**建立的时刻（客户端重连后会刷新成新值，可用来发现隧道反复抖动）。
`totalCredits` 是组织当月总额度、`remainingCredits` 是剩余额度（单位都是 credit，已用 = 两者之差）。
`remainingCredits` 为 `null` 说明服务端还没读到额度（`stale` 会是 `true`，`error` 里是原因）。
两个时间字段都固定用 **`+08:00`** 输出，不跟随主机时区。
该数组**按剩余额度倒序**：额度多的在前，`remainingCredits` 为 `null` 的排最后，并列时按 `clientId` 升序；
这只是列表顺序，`stale` 的旧值仍按数字参与排序。
额度是经**该工作空间自己的隧道**向 `api.cnb.cool` 读的（`/{org}/-/charge/quota` 与 `/-/charge/volume`），
因此工作空间的 `CNB_TOKEN` 需要有 `group-resource:r` 权限；没有该权限时读不到，`GET /` 里会显示 `403`，
路由退回随机——功能不受影响。详见第 6 节的接口说明。

### 3.5 升级与迁移

```bash
# 覆盖二进制后重启（unit 已存在，用 systemctl 即可）
scp artifacts/server/linux-x64/CYarpServer root@<VPS_IP>:/opt/cyarp/
chmod +x /opt/cyarp/CYarpServer
sudo systemctl restart CYarpServer
```

注意两点：

- **换目录**：unit 里记录的是绝对路径，且 ServiceSelf 会检查「同名但不同路径」并报错。要搬到别的目录，先 `sudo ./CYarpServer stop` 再在新目录 `sudo ./CYarpServer start`。
- **改端口**：在 `appsettings.json` 里加 `"Urls": "http://+:8080"` 后 `sudo systemctl restart CYarpServer`；systemd 服务不继承你 shell 里的 `ASPNETCORE_URLS`。

### 3.6 防火墙

放行服务端监听端口（默认 80）：

```bash
# ufw
sudo ufw allow 80/tcp
# firewalld
sudo firewall-cmd --permanent --add-port=80/tcp && sudo firewall-cmd --reload
```

---

## 4. 部署（Windows，可选）

ServiceSelf 会把它装成 Windows 服务（同样不需要装 .NET 运行时）：

```powershell
# 在开发机发布 Windows 版（产物在 artifacts/server/win-x64/）
pwsh ./scripts/publish.ps1 -SkipClient -ServerRuntimeIdentifier win-x64

# 把 CYarpServer.exe 与 appsettings.json 拷到目标机器同一目录，例如 C:\cyarp\
# 改 appsettings.json 里的 ApiKey，然后以管理员身份：
C:\cyarp\CYarpServer.exe start     # 安装 + 启动 + 开机自启（SCM 服务）
C:\cyarp\CYarpServer.exe logs      # 跟随服务日志
C:\cyarp\CYarpServer.exe logs filter=ERROR
C:\cyarp\CYarpServer.exe stop      # 停止并删除服务
C:\cyarp\CYarpServer.exe           # 前台运行，方便调试
```

改端口同样在 `appsettings.json` 里加 `"Urls": "http://+:8080"`（默认 `http://+:80` 需要管理员权限才能绑定）。

---

## 5. 下面要做什么

服务端起好之后，到**要暴露 AI 端点的那个 CNB 仓库**里接入客户端：把 `artifacts/client/` 的内容放到
仓库根、改 `.cnb.yml` 里的 `CYARP_SERVER_URI` 与 `FIXED_URL`，然后开工作空间。
完整步骤见 [`../CYarpClient/Readme.md`](../CYarpClient/Readme.md)。

---

## 6. 配置参考

| 键 | 默认 | 说明 |
|---|---|---|
| `ApiKey` | 空 | 公网 **chat** 调用方的 Bearer key；为空则不校验。`v1/models` 永久免鉴权 |
| `ProxyModels` | `[ "deepseek-v4-flash", "glm-5.3-flash", "kimi-k3" ]` | `/{org}/{repo}/v1/models` 本地返回的列表（JSON 数组；写成逗号字符串会在启动时报绑定错误） |
| `AiQuota:Enabled` | `true` | 是否经各工作空间的隧道读额度并据此为裸 chat 选路；`false` 回到等概率随机 |
| `AiQuota:Interval` | `00:02:00` | 额度探测周期（上一轮没跑完时跳过本次触发） |
| `AiQuota:StaleAfter` | `00:06:00` | 超过这个年龄的读数视为「未知」，不再用于路由（默认容忍连续 3 次失败） |
| `AiQuota:MinRemainingCredits` | `1` | 裸 chat 选路要求的最小剩余额度，单位 credit；**严格大于**才算「额度充足」，设为 `0` 即「只要还有正额度就算充足」 |
| `AiQuota:Timeout` | `00:00:10` | 单个工作空间一轮探测（`quota` + `volume` 两个请求合计）的超时 |
| `Urls` | `http://+:80` | Kestrel 监听地址；也可用 `--urls` 启动参数 |
| `CYarpOptions:Client:KeepAliveInterval` | `00:00:40` | 服务端心跳周期 |
| `CYarpOptions:HttpTunnel:MaxTunnelsPerClient` | `20` | 每个工作空间并发 HTTP 隧道（≈并发请求）上限；额度探测的连接也来自这个池 |
| `CYarpOptions:HttpTunnel:CreationTimeout` | `00:00:10` | 隧道建立超时 |
| `Logging:LogLevel:*` | Information | 日志级别 |

环境变量写法：`ApiKey=xxx`、`CYarpOptions__HttpTunnel__MaxTunnelsPerClient=50`（服务由 systemd 拉起时需写进 unit 的 `Environment=`，见 3.5）。

### 公网接口与鉴权

公网只有两条接口，`clientId` 由 `{org}` 与 `{repo}` 两段拼成（即仓库 slug，大小写敏感）：

| 请求 | 行为 |
|---|---|
| `GET /{org}/{repo}/v1/models` | **免鉴权**（本地应答、不耗额度）：该工作空间在线 `200`，隧道断了 `503` |
| `POST /{org}/{repo}/v1/chat/completions` | 需要 `Authorization: Bearer <ApiKey>`；转发到该工作空间，上游收到 `/{org}/{repo}/-/ai/chat/completions`；离线 `502` |
| `GET /` | **免鉴权**：在线工作空间的**对象数组**（`clientId`、`connectedAt`、`totalCredits`、`remainingCredits`、`stale`、`updatedAt`、`error`），无在线时为 `[]` |
| `GET /v1/models` | **免鉴权**：`ProxyModels` 配置的模型列表；**不检查在线状态**（恒 `200`），与带前缀版本的「离线 503」刻意区分 |
| `POST /v1/chat/completions` | 需要 `Authorization: Bearer <ApiKey>`；在**当前在线**且**剩余 AI 额度超过 `MinRemainingCredits`** 的工作空间里随机挑一个转发（都不超阈值时退回「未知」、再退回「额度不足」）；无在线则 `502` |
| 其它路径（单段/三段前缀、`/v1/embeddings`…） | `404` |
| 方法不匹配（如 `POST .../v1/models`） | `405` |
| `/cyarp`、`/cyarp/{tunnelId}`（隧道握手） | 不受 `ApiKey` 约束，握手头里带的是 CNB token；形如 `/cyarp/<repo>/v1/chat/completions` 的公网路径仍要求 key |

调用方有两种接法：

- **统一 base_url**（不必知道工作空间 slug）：`base_url = http://<你的域名>/v1`、`api_key = <ApiKey>`。请求发给某个剩余额度超过 `MinRemainingCredits` 的在线工作空间，因此额度充足的几个一起分摊流量。
- **固定工作空间**：`base_url = http://<你的域名>/<org>/<repo>/v1`。
- 想知道当前有哪些工作空间在线、各自剩多少额度：`curl http://<你的域名>/`。

### 额度从哪来（服务端 → 隧道 → api.cnb.cool）

服务端的后台任务对每个在线工作空间**经它自己的隧道**读一次额度：借 CYarp 的 `IClient.CreateHttpClient`
在隧道上建一个 `HttpClient`（算 HTTP 隧道，并与该工作空间的转发共用连接池），等价于在工作空间里执行：

```bash
curl -s -H "Authorization: Bearer $CNB_TOKEN" -H 'Accept: application/vnd.cnb.api+json' \
  "https://api.cnb.cool/$ORG/-/charge/quota"   # credit_in_milli.total
curl -s -H "Authorization: Bearer $CNB_TOKEN" -H 'Accept: application/vnd.cnb.api+json' \
  "https://api.cnb.cool/$ORG/-/charge/volume"  # credit_in_milli + freeze_credit_in_milli
# 剩余 = total − (credit_in_milli + freeze_credit_in_milli)，单位 milli-credit（1000 = 1 credit）
```

也可以在工作空间里手动跑 `bash scripts/verify-endpoint.sh`（第 5 节）：它会打印这两个响应体的**全部内容**
并算出剩余额度（2026-09 在工作空间内实测两接口均为 `200`，`group-resource:r` 权限具备）。

| 结果 | 含义 | 服务端行为 |
|---|---|---|
| `200` | 读到了 | `GET /` 显示 `remainingCredits`；超过 `MinRemainingCredits` 的工作空间一起分摊裸 chat 流量 |
| `401` / `403` | `CNB_TOKEN` 缺 `group-resource:r` 权限（或仓库在子组织下、该接口要根组织）。工作空间的流水线 token 实测有该权限，出现它多半是权限被别人改过 | `GET /` 的 `error` 写明状态码、`remainingCredits` 为 `null`，路由退回随机 |
| `412` / 连接失败 / 超时 | 隧道断了、网络抖动、`AiQuota:Timeout` 太小 | 同上；且**不会**清掉上一次成功的值，只是等它自然过期（`StaleAfter`） |

> 探测读的是**组织级**额度（顶级组织），所以同一组织下的多个工作空间读数相同——多工作空间优选
> 真正有意义的是**跨组织**场景。额度接口的调用不消耗 AI 额度。用的是 `credit_in_milli.total`
> （免费 + 付费 + 特权，例如实测的 `total=1166000` 就是免费 500 + 天才程序员认证 666），
> **不是只含免费的 `free`**：用 `free` 会把总额度低估，也会把认证等特权额度漏掉。

服务端转发时**丢弃调用方的 `Authorization`（ApiKey）**，换成该工作空间握手时报上来的
`Authorization: Bearer <CNB_TOKEN>`，因此上游鉴权成立、公网 key 不会泄漏。

---

## 7. 故障排查

| 症状 | 可能原因 | 处理 |
|---|---|---|
| 启动即报配置绑定错误（涉及 `ProxyModels`） | `ProxyModels` 必须是 JSON 数组 | 改成 `"ProxyModels": [ "m1", "m2" ]`；旧的逗号分隔字符串不再支持 |
| 启动日志里 `CYarp options in effect` 显示的是库默认值（`00:00:50` / `10`） | appsettings 没被读到，或升级 CYarp 后配置绑定回退路径失效 | 确认 `appsettings.json` 在二进制同目录；见根 README 第七节「为什么 CYarpOptions 要在服务端项目里绑定」 |
| chat 返回 `401` | 请求里的 key 与 `appsettings.json` 的 `ApiKey` 不一致，或压根没带 | `curl` 时加 `-H "Authorization: Bearer <ApiKey>"`；或把 `ApiKey` 清空（仅调试）。注意 `v1/models` 免鉴权，**它返回 401 才说明键名写错了（例如仍写成 `ProxyKey`）** |
| chat 返回 `502` | 没有该前缀的在线工作空间（写成裸路径、前缀写错、大小写不符） | 用形如 `<base>/<org>/<repo>/v1/chat/completions` 的地址，`{org}/{repo}` 与工作空间仓库 slug 逐字符一致（大小写敏感） |
| `/{org}/{repo}/v1/models` 返回 `503` | 服务端活着但该工作空间的隧道断了（假死） | 工作空间的保活 cron 会自动 `stop + start`；也可手动重启工作空间 |
| 调用返回 `404` | 路径形态不对：已发布的只有 `/{org}/{repo}/v1/models`、`/{org}/{repo}/v1/chat/completions`、`GET /`、`GET /v1/models`、`POST /v1/chat/completions` | 单段前缀（如 `org~repo`）、三段前缀、其它 `/v1/*` 都是 404；想要"不指定工作空间"就用裸 `/v1/chat/completions` |
| 裸 `/v1/chat/completions` 返回 `502` | 当前没有任何工作空间在线（无可挑选的候选） | `curl http://<你的域名>/` 看在线列表；确认工作空间已启动且隧道建立 |
| `GET /` 里某个工作空间的 `remainingCredits` 一直是 `null` | 服务端读不到额度：`error` 里会写明原因（`401/403` = 该 `CNB_TOKEN` 无 `group-resource:r` 权限；`403/404` = 仓库在子组织下；超时 = 隧道慢或 `AiQuota:Timeout` 太小）。工作空间一连上就会立刻探一次，因此正常情况不该长期为空——长期为空只能是每次都失败 | 用 `sudo ./CYarpServer logs filter=AI` 看探测日志；在工作空间里跑 `scripts/verify-endpoint.sh` 第 5 节直连验证权限。**这是退化不是故障**：chat 照常可用，只是不做额度优选 |
| 想确认选路是否按额度生效 | 日志级别是 `Information` 时每条裸 chat 会打印 `Routed to workspace '...' (Available/Unknown/Insufficient, ... milli-credits remaining) out of N connected` | `Available` = 额度超阈值、在这些候选之间随机分摊；`Unknown` = 该轮没有超阈值的候选、退回随机；`Insufficient` = 全部已知额度不足（≤ 阈值）；`Disabled` = `AiQuota:Enabled=false` |
| 对话请求返回 `301 Moved Permanently` 且 `Location: https://cnb.cool`（响应体是一小段 HTML） | 服务端把**调用方的 Host**（你的公网域名/IP）转发给了上游；CNB 边缘按 Host 路由，认不出就重定向到官网。这是 2026-09 之前服务端版本的 bug | 更新服务端二进制到包含 Host 修复的版本（`CnbAiTransformer` 里清掉转发 Host）并重启服务；`/v1/models` 这类本地应答不受影响，所以「models 通、对话不通」正是这个症状 |
| 上游返回 401/412 之类 | 上游鉴权/网络策略 | 412 表示请求不是从 CNB 工作空间内发出；401 多半是 `CNB_TOKEN` 过期，重启工作空间让平台重铸 |
| Windows 上启动即报 `SocketException (10013) ... 访问权限`（WSAEACCES） | 端口落在 Windows 保留段（Hyper-V/WSL 预留，如 8081-8180），或用了 <1024 的端口但没提权 | `netsh interface ipv4 show excludedportrange protocol=tcp` 查保留段，换一个端口：在 `appsettings.json` 加 `"Urls": "http://+:18089"` |
| `CYarpServer start` 报「系统已存在同名但不同路径的服务」 | 之前从别的目录安装过 | 在旧目录执行 `sudo ./CYarpServer stop`，或 `sudo systemctl disable --now CYarpServer && sudo rm /etc/systemd/system/CYarpServer.service` |
| `start` 报「无法操作服务：没有root权限」 | 没用 root | 加 `sudo` |

常用观察点：

```bash
sudo ./CYarpServer logs                  # 订阅服务进程的日志（可加 filter=关键字）
sudo journalctl -u CYarpServer -n 100 --no-pager
```

### 三个已知取舍

- **`GET .../v1/models` 与 `GET /` 免鉴权**：models 是本地应答的静态模型列表（不耗额度、不触上游、不含敏感数据），
  免鉴权是为了让工作空间的保活 cron 不带任何密钥探活；`/` 只暴露"哪些工作空间在线"**以及各组织的剩余 AI
  额度数字**（不含任何 token）。知道组织/仓库名的人可以读到模型列表与该工作空间的隧道在线状态。
  要收紧只需把 `GET /` 从 `CreateApiKeyGate` 的白名单里去掉。
- **裸 `POST /v1/chat/completions` 按额度分发**：多工作空间场景下，请求落到**剩余额度超过
  `MinRemainingCredits`** 的某个工作空间（这些候选之间等概率随机，不再是谁多给谁），
  因此上游 token 与额度按被挑中的那个工作空间计；额度读不到（或 `AiQuota:Enabled=false`）时回退为等概率随机。
- **隧道入站不做鉴权**：客户端只带 CNB token（不具备公网密钥语义），服务端不做准入拒绝。因此能访问到服务端端口
  的人都可以用任意 `X-CYarp-Client` 注册隧道，把该工作空间的请求引到自己那里（能看到请求体），并把真正的工作空间
  踢下线。要收紧只需在该 handler 里加一个共享密钥 claim 校验并在失败时返回 `401`。

另外现在**没有 TLS**：Kestrel 直接对外只提供 HTTP，CNB token（握手头 + 隧道内注入）与 API 内容都是明文。
请据此控制暴露面（只监听需要的网卡/端口、用防火墙或 WireGuard 收敛来源）。要加 HTTPS：在 `appsettings.json` 增加
Kestrel 的 HTTPS 端点与证书，然后把客户端 `.cnb.yml` 里的 `CYARP_SERVER_URI` 改成 `https://`、
`FIXED_URL` 改成 `https://`，代码不用动。
