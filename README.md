# CYarp CNB AI 映射：.NET 10 + 两端 Native AOT

把 CNB 工作空间内网独享的 AI 端点，转成**固定域名 + OpenAI 兼容**的 API，不再依赖会漂移的
`https://<subdomain>-9001.cnb.run`。

```
公网客户端 ──Bearer PROXY_KEY──▶ http://<你的域名>/<clientId>/v1/chat/completions
                                      │  (Kestrel 直接对外，无反向代理)
                                      ▼
                                 CYarpServer (VPS, Native AOT, ServiceSelf 常驻)
                                      │  长连接隧道(工作空间主动向外拨号)
                                      ▼
                          CYarpClient ← 跑在 CNB 工作空间里 (Native AOT 单文件)
                                      │  携带 Authorization: Bearer $CNB_TOKEN
                                      ▼
                 https://api.cnb.cool/$CNB_REPO_SLUG/-/ai/chat/completions
```

为什么不再需要 `-9001.cnb.run`：CYarp 是**内网主动向外拨号**建立长连接（frp 同类），
公网入口是你自己的固定域名 VPS。工作空间子域名怎么变都无所谓，隧道断了重连即可。

一个 VPS 可以同时接**多个工作空间**：每个工作空间有自己的 `clientId`
（默认 `<org>~<repo>`），公网用路径前缀区分。

---

## 一、目录结构

```
cyarp-cnb.sln
global.json                     SDK 固定到 .NET 10（rollForward latestFeature）
Directory.Build.props           net10.0 / Nullable / LangVersion 等公共属性
src/
  CYarp.Server/                 隧道服务端库（上游源码 + 本仓库改动）
  CYarp.Client/                 隧道客户端库（上游源码 + 本仓库改动）
  CYarpServer/                  服务端进程：Kestrel + 多工作空间路由 + ServiceSelf
  CYarpClient/                  客户端进程：环境变量注入 + 自动重连 + 内置文件日志
tests/CYarp.Cnb.Tests/          端到端集成测试（进程内起 server/client/假上游）
deploy/
  cyarp-start.sh                CNB 工作空间启动脚本
  cnb.yml.example              .cnb.yml 示例（含保活 cron）
  cyarp/CYarpClient             已编译好的 linux-x64 Native AOT 客户端（入库）
scripts/
  publish-aot.ps1               一键发布 server + client 的 linux-x64 AOT 二进制
  verify-endpoint.sh            验证「必须从工作空间内部发出请求」的前提
```

---

## 二、构建、测试、发布

前置：本机装 .NET 10 SDK（`dotnet --list-sdks` 能看到 10.x）。**不需要** Docker，也**不需要**
目标机器装 .NET。

```powershell
# 构建 + 零 trim/AOT 警告
dotnet build cyarp-cnb.sln -c Release

# 端到端集成测试
dotnet test cyarp-cnb.sln -c Release

# 交叉 AOT 发布 linux-x64（Windows 上即可完成）
pwsh ./scripts/publish-aot.ps1
```

发布产物：

| 产物 | 位置 | 说明 |
|---|---|---|
| `CYarpClient` | `deploy/cyarp/CYarpClient` | 约 8 MB，linux-x64 Native AOT 单文件，**入库**，CNB 工作空间直接用 |
| `CYarpServer` | `artifacts/server/CYarpServer` | 约 18 MB，linux-x64 Native AOT 单文件，上传到 VPS |

> 首次 AOT 发布会联网下载 ILCompiler runtime pack 与 zig 工具链（进 NuGet 缓存），之后可离线复现。

---

## 三、服务端部署（VPS，不用 Docker、不用 Caddy）

Kestrel 直接对外，默认监听 `http://+:80`（可用 `--urls`、`ASPNETCORE_URLS` 或 Kestrel 配置覆盖）。
进程用 [ServiceSelf](https://github.com/xljiulang/ServiceSelf) 自安装为系统服务：Linux 生成 systemd
unit，Windows 走 SCM，同一条命令。

```bash
# 1) 上传两个文件到 VPS 同一个目录，例如 /opt/cyarp/
#    CYarpServer（来自 artifacts/server/）
#    appsettings.json（来自 artifacts/server/）

# 2) 改 appsettings.json 里的 ProxyKey（公网调用方要带的 key）

# 3) 安装并启动为系统服务（root）
sudo ./CYarpServer start

# 常用命令
sudo ./CYarpServer logs            # 跟随服务日志
sudo ./CYarpServer logs filter=ERROR
sudo ./CYarpServer stop            # 停止并删除服务
./CYarpServer                      # 前台运行，方便调试
```

`appsettings.json` 关键项：

```jsonc
{
  "ProxyKey": "CHANGE-ME-public-api-key",     // 为空则不校验（仅建议内网调试时）
  "ProxyModels": "deepseek-v4-flash,glm-5.3-flash,kimi-k3",
  "CYarpOptions": {
    "Node": "node0",
    "Client": { "KeepAlive": true, "KeepAliveInterval": "00:00:40" },
    "HttpTunnel": { "MaxTunnelsPerClient": 20, "CreationTimeout": "00:00:10" }
  }
}
```

### 以后要加 HTTPS

现在的取舍是**先不上 TLS**（见文末风险）。要加时**代码不用动**：

1. 在 `appsettings.json` 增加 Kestrel 的 HTTPS 端点与证书（或再加一层你熟悉的 TLS 终结）；
2. 客户端 `CYARP_SERVER_URI` 从 `ws://` 改成 `wss://`；
3. `deploy/cnb.yml.example` 的 `FIXED_URL` 从 `http://` 改成 `https://`。

---

## 四、CNB 工作空间侧接入

**第 1 步**：把 `deploy/` 目录放进你的仓库（`cyarp-start.sh` + `cyarp/CYarpClient` 必带）。

**第 2 步**：把 `deploy/cyarp-start.sh` 里的环境变量换成你的域名，然后复制
`deploy/cnb.yml.example` 为仓库根目录的 `.cnb.yml`（注意 cron 必须放在 `main:` 键下，
否则永不触发）。

**第 3 步**：仓库页面点「云开发」启动工作空间，构建日志出现下面两行即隧道已通：

```
===== cyarp client started, pid=123 =====
===== cyarp client: tunnel established =====
```

启动脚本做的事：校验 `CYARP_SERVER_URI` / `CNB_TOKEN` → 幂等杀掉旧进程 → 把配置全部导出为
环境变量（token 一个字节都不落盘）→ `nohup` 拉起 → 轮询日志确认隧道建立，成功才让构建变绿。

### `CNB_TOKEN` 从哪来

**平台自动注入，不需要申请。** CNB 在每次流水线运行时铸一把临时凭证放进环境变量
`CNB_TOKEN`，作用域覆盖 `api.cnb.cool` 的团队 API 与 `/-/ai/chat/completions`。它随工作空间
回收而作废，所以任何一端都不落盘缓存——本方案全程只走环境变量。

| 环境变量 | 值 | 说明 |
|---|---|---|
| `CYarpClientOptions__ServerUri` | `ws://ai.example.com` | 你的 CYarpServer 地址（加 TLS 后改 `wss://`） |
| `CYarpClientOptions__TargetUri` | `https://api.cnb.cool/$CNB_REPO_SLUG/-/ai` | 隧道出口指向 CNB 内网 AI |
| `CYarpClientOptions__ConnectHeaders__Authorization` | `Bearer $CNB_TOKEN` | 握手时把 CNB token 交给 Server |
| `CYarpClientOptions__ConnectHeaders__X-CYarp-Client` | `<org>~<repo>` | 工作空间 id，同时是公网路径前缀 |
| `CYarpClientOptions__KeepAliveInterval` | `00:00:30` | 心跳，同时也是工作空间保活信号 |

> 注意：`X-CYarp-Client` 这种**带连字符的变量名不能用 `export`**（bash 会报
> `not a valid identifier`），启动脚本用 `env "NAME=value"` 方式注入。

---

## 五、公网调用

```bash
# 服务存活探测（本地返回，不耗额度）
curl http://ai.example.com/v1/models -H "Authorization: Bearer $PROXY_KEY"

# 单工作空间在线时，也可以不带前缀直接调
curl http://ai.example.com/v1/chat/completions \
  -H "Authorization: Bearer $PROXY_KEY" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}]}'

# 多工作空间：用 <org>~<repo> 前缀指定（OpenAI 客户端把 base_url 设成带前缀的地址即可）
curl http://ai.example.com/org~repo/v1/chat/completions \
  -H "Authorization: Bearer $PROXY_KEY" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}]}'
```

### 路由与鉴权规则

| 请求 | 行为 |
|---|---|
| `/{clientId}/v1/...` | 转发到该工作空间；`/v1` 前缀剥掉，拼到该客户端的 `TargetUri` 上 |
| `/v1/...`（无前缀） | **只有一个**工作空间在线时转发到它；多个在线时返回 `502`（避免打错人） |
| 请求头 `X-CYarp-Client: <clientId>` | 覆盖路径前缀，适合只能改 header 的客户端 |
| `/{clientId}/v1/models` | 该工作空间在线 `200`，**离线 `503`**（隧道假死信号，不耗额度） |
| `/v1/models`（无前缀） | 恒 `200`（纯服务存活探测，与隧道状态无关） |
| 前缀指向未连接的工作空间 | `502` |
| `PROXY_KEY` 缺失/错误 | `401`，请求不会到达任何工作空间 |
| `/cyarp/*`（隧道握手） | 不受 `PROXY_KEY` 约束，握手头里带的是 CNB token |

服务端收到请求后：**丢弃调用方的 `Authorization`（公网 key）**，换成该工作空间握手时报上来的
`Authorization: Bearer <CNB_TOKEN>` 再发往上游，因此上游鉴权成立，公网 key 不会泄漏。
实际效果（集成测试断言的行为）：

```
POST /orgA~repoA/v1/chat/completions
  → 上游收到 POST /orgA/repoA/-/ai/chat/completions
    Authorization: Bearer <CNB_TOKEN>
```

> 上游会改写 `model` 字段：请求 `deepseek-v4-flash` 时响应里的 `model` 是
> `deepseek-v4.1-flash`，`id` 是 32 位 hex（不是 `chatcmpl-` 前缀）。客户端若有严格校验需自行适配。
> CNB 官方 OpenAPI 里 AI 标签只有 `/{repo}/-/ai/chat/completions`，没有 `/v1/models`，
> 所以 `/v1/models` 由 CYarpServer 本地返回（`ProxyModels` 配置驱动），不占上游额度。

---

## 六、保活与自愈

平台回收规则（官方 `docs.cnb.cool/zh/workspaces/workspace-recycling`）：页面关闭后 10 分钟无操作
回收（`keepAliveTimeout` 默认 600000ms）、单会话上限默认 18h、凌晨 4:00–6:00 强制回收。

| 层 | 手段 | 防的是什么 |
|---|---|---|
| L0 | `services.vscode.options.keepAliveTimeout: 18h` | 把「10 分钟无操作即回收」放宽到会话上限 |
| L1 | `main:` cron 每 5 分钟查 `workspace/list`，无 running → `workspace/start` | 被平台回收后自动拉起 |
| L2 | cron 探活 `$FIXED_URL/$CLIENT_ID/v1/models`，连续两次非 200 → `stop + start` | 「假死」：workspace 显示 running 但隧道已断 |

L0 + 客户端 30s 心跳长连接共同把工作空间钉住；隧道断开由 CYarpClient 自己重连（5s/10s）；
被回收或假死由 cron 兜底。每次 cron 触发都是流水线新铸造的 `CNB_TOKEN`，工作空间重建后新 token
随新进程自动生效。

> L2 探的是 `/{clientId}/v1/models`：这是本地应答、不耗 AI 额度，同时**隧道断了会返回 503**，
> 所以能真正识别假死。裸 `/v1/models` 恒 200，只用来判断服务端是否活着。

---

## 七、源码来源与上游差异

`src/CYarp.Server`、`src/CYarp.Client` 来自上游 [xljiulang/CYarp](https://github.com/xljiulang/CYarp)
commit `a88f9ec`，许可证见 `LICENSE`（MIT）。本仓库相对上游的改动都用
`CNB-CN delta` 注释标出，便于日后同步：

| 文件 | 改动 |
|---|---|
| `CYarp.Server/IClient.cs`、`Clients/Client.cs`、`Clients/ClientStateChannel.cs` | 新增 `string? UpstreamAuthorization`，保存客户端握手时报的 CNB token |
| `CYarp.Server/Middlewares/ClientHandler.cs` | 握手时取 `Authorization` 头存入 client；`Results.Forbid()` 换成 403 状态码（本部署不注册认证服务）；handler 改为只收 `HttpContext` |
| `CYarp.Server/Middlewares/TunnelHandler.cs` | 同样改为只收 `HttpContext`，`tunnelId` 从路由值解析 |
| `CYarp.Server/EndpointRouteBuilderExtensions.cs` | 用显式 `RequestDelegate` 映射端点；返回类型改为 `IEndpointConventionBuilder` |
| `CYarp.Server/Configs/CYarpOptionsBinder.cs`（新增） | 手写配置绑定，替代反射绑定（`CYarpOptions.HttpTunnel` 会被源生成绑定器整段跳过） |
| `CYarp.Client/AOT.cs`、`CYarp.Client.h`、native 工程 | 移除：本方案只做单文件 AOT，不需要导出 C ABI |
| `src/CYarpServer/*`、`src/CYarpClient/*` | 本仓库新写的 Host（Kestrel 直连、多工作空间路由、ServiceSelf、内置日志、环境变量注入） |

### AOT 相关的三个坑（都已实测并处理）

1. **zig 默认 target 要求 glibc 2.29**，在 Ubuntu 18.04 上直接报 `GLIBC_2.29 not found`。
   项目里设了 `<GLibcVersion>2.17</GLibcVersion>`，二进制可在 CentOS 7 / Ubuntu 18.04 及以上运行。
2. **RDG 会把委托里的复杂参数当成 JSON body**：`app.Map(pattern, Delegate)` 里只要有一个非
   `HttpContext` 的复杂参数，首次请求就会抛
   `NotSupportedException: JsonTypeInfo metadata for type ... was not provided`。
   所以所有端点都用显式 `RequestDelegate`，依赖从 `HttpContext.RequestServices` 取。
3. **源生成配置绑定覆盖不到 `HttpTunnel`**：`HttpTunnelConfig.ActivityHeadersPropagator` 是抽象
   类型，绑定器会连带跳过整个 `HttpTunnel` 段（配置静默失效），因此改为手写绑定。

另外为了让 AOT 干净：去掉了 Serilog（改用 `Microsoft.Extensions.Logging` + 一个轻量按天文件
sink）、去掉了 Redis 状态存储（`StackExchange.Redis` 与 AOT 冲突，单 VPS 用不上，库里
`IClientStateStorage` 抽象保留）、去掉了 JWT（握手头里带的是 CNB token）、`/v1/models` 用
`JsonSerializerContext` 输出。

---

## 八、测试

```powershell
dotnet test cyarp-cnb.sln -c Release
```

集成测试在进程内起「假上游 + CYarpServer + 真实 CYarpClient」，断言：

| 用例 | 断言 |
|---|---|
| 单工作空间裸路径转发 | 上游收到 `/{org}/{repo}/-/ai/chat/completions`、`Authorization: Bearer <CNB token>`、query 与 body 保留、公网 key 未出现在任何请求头 |
| 双工作空间路径前缀 | 各自打到各自 TargetUri 与各自 token，不串台 |
| 双工作空间裸路径 | 返回 `502` |
| 未知前缀 | 返回 `502` |
| `X-CYarp-Client` 覆盖 | 按 header 路由 |
| `/v1/models` | 裸路径 `200` 且不触达上游 |
| `/{clientId}/v1/models` | 在线 `200`、离线 `503`，均不触达上游 |
| `PROXY_KEY` 错误/缺失 | `401` 且上游零请求 |
| `stream=true` | SSE 分帧透传：假上游写第一帧后阻塞，直到测试读到第一帧才写第二帧（整体缓冲的实现会卡死失败） |

另有 `scripts/verify-endpoint.sh`（在公网机器与工作空间内各跑一次对比），用于验证
「必须从工作空间内部发出请求」这个前提：

```bash
CNB_TOKEN=xxx CNB_REPO_SLUG=org/repo bash scripts/verify-endpoint.sh
```

实测结论：从外网带**真实** `CNB_TOKEN` 直连 `api.cnb.cool` 返回
`412 {"errcode":10412,"errmsg":"Blocked by network policy."}`（不带 token 是 `401`），说明鉴权已过、
卡在来源 IP 上；工作空间内部同样的命令返回正常 SSE 流。

---

## 九、已知取舍：现阶段没有 TLS

按当前决定，Kestrel 直接对外只提供 HTTP，因此：

- CNB token 在隧道握手头里、以及在 Server→Client 的隧道内注入时都是**明文**传输；
- 公网 API 内容也是明文。

请据此控制暴露面（只监听需要的网卡/端口、必要时用防火墙或 WireGuard 收敛来源）。
加 HTTPS 的改法见「三、服务端部署 → 以后要加 HTTPS」，不需要改代码。
