# CYarp CNB AI 映射：NuGet 依赖 + 认证 handler 承载客户端信息

把 CNB 工作空间内网独享的 AI 端点，转成**固定域名 + OpenAI 兼容**的 API，不再依赖会漂移的
`https://<subdomain>-9001.cnb.run`。

```
公网客户端 ──Bearer ApiKey──▶ http://<你的域名>/<clientId>/v1/chat/completions
                                      │  (Kestrel 直接对外，无反向代理)
                                      ▼
                                 CYarpServer (VPS, 单文件自包含, ServiceSelf 常驻)
                                      │  长连接隧道(工作空间主动向外拨号)
                                      ▼
                          CYarpClient ← 跑在 CNB 工作空间里 (Native AOT 单文件)
                                      │  携带 Authorization: Bearer $CNB_TOKEN
                                      ▼
                 https://api.cnb.cool/$CNB_REPO_SLUG/-/ai/chat/completions
```

本仓库**不含任何 CYarp 上游源码**，两端都引用官方 NuGet 包（`CYarp.Server` / `CYarp.Client` 2.0.3）。
工作空间的身份与 CNB token 通过 CYarp 客户端握手头进入 Kestrel 的自定义 authentication scheme，
被解析成 claims 放进 `HttpContext.User`；CYarp 库会把该 principal 存到 `IClient.User`，服务端转发时
从那里取出 token 注入——因此 `IClient` 接口不需要任何扩展。

一个 VPS 可以同时接**多个工作空间**：每个工作空间有自己的 `clientId`（默认就是仓库 slug `<org>/<repo>`），
公网用路径前缀区分。

---

## 一、目录结构

```
cyarp-cnb.sln
global.json                SDK 固定到 .NET 10（rollForward latestFeature）
Directory.Build.props      net10.0 / Nullable / LangVersion 等公共属性
src/
  CYarpServer/             服务端进程：Kestrel + 认证 handler + 多工作空间路由 + ServiceSelf
  CYarpClient/             客户端进程：环境变量注入 + 自动重连 + 内置文件日志（Native AOT）
tests/CYarp.Cnb.Tests/     端到端集成测试（进程内起 server/client/假上游）
deploy/
  README.md                部署指南（VPS 服务端 + CNB 工作空间逐步操作）
  .cnb.yml                 CNB 流水线配置（含保活 cron）——本目录即 CNB 仓库根内容
  cyarp-start.sh           CNB 工作空间启动脚本（按自身目录找 cyarp/CYarpClient）
  cyarp/CYarpClient        已编译好的 linux-x64 Native AOT 客户端（入库）
  cyarp/appsettings.json   客户端非敏感默认配置
scripts/
  publish.ps1              一键发布客户端(AOT) + 服务端(自包含 trimmed)
  verify-endpoint.sh       验证「必须从工作空间内部发出请求」的前提
```

---

## 二、构建、测试、发布

前置：本机装 .NET 10 SDK（`dotnet --list-sdks` 能看到 10.x）。**不需要** Docker，也**不需要**
目标机器装 .NET。

```powershell
# 构建（零警告）
dotnet build cyarp-cnb.sln -c Release

# 端到端集成测试
dotnet test cyarp-cnb.sln -c Release

# 发布 linux-x64（Windows 上即可完成）
pwsh ./scripts/publish.ps1
```

| 产物 | 位置 | 形态 | 说明 |
|---|---|---|---|
| `CYarpClient` | `deploy/cyarp/CYarpClient` | **Native AOT** 单文件，约 8 MB | linux-x64，glibc ≥ 2.17，**入库**，CNB 工作空间直接用 |
| `CYarpServer` | `artifacts/server/CYarpServer` | **自包含 + trimmed** 单文件，约 17 MB | linux-x64，运行时内嵌，上传到 VPS 即可 |

> 服务端发布时会有一条 `IL2104: Assembly 'CYarp.Server' produced trim warnings`。这是 CYarp 包内部的
> 裁剪提示，实机验证过握手、隧道、转发、流式响应全部正常（见第七节），因此保留提示不静默屏蔽。
> 客户端 AOT 发布零警告；首次发布会联网下载 ILCompiler runtime pack 与 zig 工具链（进 NuGet 缓存）。

---

## 三、服务端部署（VPS，不用 Docker、不用 Caddy）

Kestrel 直接对外，默认监听 `http://+:80`（可用 `--urls`、`ASPNETCORE_URLS` 或 Kestrel 配置覆盖）。
进程用 [ServiceSelf](https://github.com/xljiulang/ServiceSelf) 自安装为系统服务：Linux 生成 systemd
unit，Windows 走 SCM，同一条命令。

```bash
# 1) 上传两个文件到 VPS 同一目录，例如 /opt/cyarp/
#    CYarpServer（来自 artifacts/server/）
#    appsettings.json（来自 artifacts/server/）

# 2) 改 appsettings.json 里的 ApiKey（公网调用方要带的 key）

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
  "ApiKey": "CHANGE-ME-public-api-key",     // 公网 chat 调用要带的 Bearer key；为空则不校验
  "ProxyModels": [ "deepseek-v4-flash", "glm-5.3-flash", "kimi-k3" ],
  "CYarpOptions": {
    "Node": "node0",
    "Client": { "KeepAlive": true, "KeepAliveInterval": "00:00:40" },
    "HttpTunnel": { "MaxTunnelsPerClient": 20, "CreationTimeout": "00:00:10" }
  }
}
```

> `CYarpOptions` 里的值会被服务端读取并在启动日志里回显，例如：
> `CYarp options in effect: KeepAlive=True, KeepAliveInterval=00:00:40, MaxTunnelsPerClient=20, CreationTimeout=00:00:10`。
> 升级 CYarp 或调整裁剪设置后，先看这一行确认配置确实生效（原因见第七节）。

### 以后要加 HTTPS

现在的取舍是**先不上 TLS**（见第九节风险）。要加时**代码不用动**：

1. 在 `appsettings.json` 增加 Kestrel 的 HTTPS 端点与证书（或再加一层你熟悉的 TLS 终结）；
2. 客户端 `CYARP_SERVER_URI` 从 `http://` 改成 `https://`；
3. `deploy/.cnb.yml` 的 `FIXED_URL` 从 `http://` 改成 `https://`。

---

## 四、CNB 工作空间侧接入

**第 1 步**：把 `deploy/` 的**内容**放到 CNB 仓库根目录（`.cnb.yml`、`cyarp-start.sh`、`cyarp/CYarpClient`
三样必带），例如 `rsync -a --delete deploy/ /path/to/cnb-some-repo/` 后提交推送。

> 要放进**你想暴露其 AI 端点**的那个仓库：隧道出口是 `https://api.cnb.cool/$CNB_REPO_SLUG/-/ai`，
> 而 `CNB_REPO_SLUG` 与 `CNB_TOKEN` 的作用域都是「工作空间所在仓库」。

**第 2 步**：改 `.cnb.yml` 里标了「改这里」的两处：`CYARP_SERVER_URI`（workspace）与
`FIXED_URL`（keepalive cron）。保活 cron 必须在 `main:` 键下（对应默认分支），否则永不触发。
`.cnb.yml` 里不需要任何 key：`v1/models` 免鉴权，cron 直接探活。

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
| `CYarpClientOptions__ServerUri` | `http://ai.example.com` | 你的 CYarpServer 地址（加 TLS 后改 `https://`） |
| `CYarpClientOptions__TargetUri` | `https://api.cnb.cool/$CNB_REPO_SLUG/-/ai` | 隧道出口指向 CNB 内网 AI |
| `CYarpClientOptions__ConnectHeaders__Authorization` | `Bearer $CNB_TOKEN` | 握手时把 CNB token 交给 Server |
| `CYarpClientOptions__ConnectHeaders__X-CYarp-Client` | `<org>/<repo>` | 工作空间 id，固定取 `CNB_REPO_SLUG`（脚本内写死，不可配置），同时是公网路径前缀 |
| `CYarpClientOptions__KeepAliveInterval` | `00:00:30` | 心跳，同时也是工作空间保活信号 |

> 两个坑已在脚本里处理：① `X-CYarp-Client` 这种**带连字符的变量名不能用 `export`**（bash 报
> `not a valid identifier`），脚本用 `env "NAME=value"` 注入；② CYarp 2.0.3 的就绪日志是中文
> 「连接到服务器…成功」，脚本的就绪判定同时匹配中英文。
>
> 客户端**不携带任何公网 key**：握手只发 CNB token。服务端隧道入站也不做鉴权（见第九节）。

---

## 五、公网调用

```bash
# 该工作空间的隧道状态（在线 200 / 离线 503；本地应答、免鉴权、不耗额度）
curl http://ai.example.com/org/repo/v1/models

# 对话（需要 ApiKey；OpenAI 客户端把 base_url 设成带 <org>/<repo> 前缀的地址即可）
curl http://ai.example.com/org/repo/v1/chat/completions \
  -H "Authorization: Bearer $ApiKey" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}],"stream":true}'
```

### 路由与鉴权规则

公网接口是一张**固定的路由表**，`clientId` 由路由参数 `{org}` 与 `{repo}` 拼成（即 `org/repo`）：

| 请求 | 行为 |
|---|---|
| `GET /{org}/{repo}/v1/models` | **免鉴权**。该工作空间在线 `200`（返回 `ProxyModels` 配置的列表），**离线 `503`**（隧道假死信号，不耗额度） |
| `POST /{org}/{repo}/v1/chat/completions` | 需要 `Authorization: Bearer <ApiKey>`；转发到该工作空间，上游收到 `/{org}/{repo}/-/ai/chat/completions`；离线 `502` |
| 其它路径（含裸 `/v1/models`、`/v1/chat/completions`、单段/三段前缀、`/v1/embeddings`…） | `404`（路由表即接口清单） |
| 方法不匹配（如 `POST .../v1/models`） | `405` |
| chat 的 `ApiKey` 缺失/错误 | `401`，请求不会到达任何工作空间 |
| `/cyarp`、`/cyarp/{tunnelId}`（隧道握手） | 不受 `ApiKey` 约束，握手头里带的是 CNB token；形如 `/cyarp/<repo>/v1/chat/completions` 的公网路径仍要求 key |

> 前缀固定为**两段** `{org}/{repo}`（大小写敏感）。`clientId` 恒等于工作空间的 `CNB_REPO_SLUG`
> （`cyarp-start.sh` 内写死，`.cnb.yml` 里没有可配项），所以公网前缀不可能与隧道注册的 id 不一致。

服务端收到请求后：**丢弃调用方的 `Authorization`（公网 key）**，换成该工作空间握手时报上来的
`Authorization: Bearer <CNB_TOKEN>` 再发往上游，因此上游鉴权成立，公网 key 不会泄漏。
实际效果（集成测试断言的行为）：

```
POST /orgA/repoA/v1/chat/completions
  → 上游收到 POST /orgA/repoA/-/ai/chat/completions
    Authorization: Bearer <CNB_TOKEN>
```

> 上游会改写 `model` 字段：请求 `deepseek-v4-flash` 时响应里的 `model` 是
> `deepseek-v4.1-flash`，`id` 是 32 位 hex（不是 `chatcmpl-` 前缀）。客户端若有严格校验需自行适配。
> CNB 官方 OpenAPI 里 AI 标签只有 `/{repo}/-/ai/chat/completions`，没有 `/v1/models`，
> 所以 `/{org}/{repo}/v1/models` 由 CYarpServer 本地返回（`ProxyModels` 数组配置驱动），不占上游额度。

---

## 六、保活与自愈

平台回收规则（官方 `docs.cnb.cool/zh/workspaces/workspace-recycling`）：页面关闭后 10 分钟无操作
回收（`keepAliveTimeout` 默认 600000ms）、单会话上限默认 18h、凌晨 4:00–6:00 强制回收。

| 层 | 手段 | 防的是什么 |
|---|---|---|
| L0 | `services.vscode.options.keepAliveTimeout: 18h` | 把「10 分钟无操作即回收」放宽到会话上限 |
| L1 | `main:` cron 每 5 分钟查 `workspace/list`，无 running → `workspace/start` | 被平台回收后自动拉起 |
| L2 | cron 探活 `$FIXED_URL/$CLIENT_ID/v1/models`（**不带 key**），连续两次非 200 → `stop + start` | 「假死」：workspace 显示 running 但隧道已断 |

L0 + 客户端 30s 心跳长连接共同把工作空间钉住；隧道断开由 CYarpClient 自己重连（5s/10s）；
被回收或假死由 cron 兜底。每次 cron 触发都是流水线新铸造的 `CNB_TOKEN`，工作空间重建后新 token
随新进程自动生效。

> L2 探的是 `/{org}/{repo}/v1/models`：本地应答、免鉴权（所以 cron 里不需要任何 key）、不耗 AI 额度，
> 同时**隧道断了会返回 503**，所以能真正识别假死。

---

## 七、依赖、版本与设计取舍

### 依赖

| 组件 | 版本 | 用途 |
|---|---|---|
| `CYarp.Client` | 2.0.3 | 客户端隧道库（Native AOT 下发布零警告） |
| `CYarp.Server` | 2.0.3 | 服务端隧道库 |
| `Yarp.ReverseProxy` | 2.3.0 | 只用其中的 `IHttpForwarder` + `HttpTransformer` |
| `ServiceSelf` | 1.0.8 | 自安装为 systemd / Windows 服务 |

### 客户端信息如何进入服务端（不扩展 CYarp API）

Kestrel 上注册了自定义 scheme `CYarpTunnel`（`app.UseAuthentication()` 在 `UseCYarp()` 之后）。
handshake 请求（带 `CYarp-TargetUri` 头，只有 CYarp 握手会带；普通公网请求返回 `NoResult`，不受影响）
会被解析成三个 claims：

| claim | 来源 | 用途 |
|---|---|---|
| `cnb:client-id` | `X-CYarp-Client`（缺省 `cnb`） | `MapCYarp(context => ...)` 的 clientId，最终进 `IClient.Id` |
| `cnb:upstream-authorization` | 握手 `Authorization`（`Bearer $CNB_TOKEN`） | 转发时注入上游 |
| `cnb:target-uri` | `CYarp-TargetUri` | 日志/诊断 |

CYarp 会把握手请求的 `ClaimsPrincipal` 存到 `IClient.User`，转发时 `HttpForwardHandler` 直接从
`client.User` 取 token。因此**不需要**给 `IClient` 加属性，也不需要改 CYarp 源码。

### 为什么 `CYarpOptions` 要在服务端项目里绑定

`CYarp.Server` 自己带的 `AddCYarp().Configure(section)` 内部是**反射绑定**，而配置绑定源生成器只能拦截
*它自己参与编译的调用点*——包内部那一行拦不到，于是它在 trimmed 发布下**静默失效**：

| | `Node` | `Protocols` | `KeepAlive` | `KeepAliveInterval` | `MaxTunnelsPerClient` |
|---|---|---|---|---|---|
| appsettings 期望 | `probe-node` | `WebSocket` | `False` | `00:00:07` | `42` |
| 用包自带重载（trimmed） | 空 | `All` | `True` | `00:00:50` | `10` |
| 在本项目里 `Configure<CYarpOptions>(section)`（trimmed） | `probe-node` | `WebSocket` | `False` | `00:00:07` | `42` |

所以在 `CYarpServerHost` 里显式写一句 `builder.Services.Configure<CYarpOptions>(...)`：调用点落在我们自己的
程序集里，源生成器就会生成绑定代码，`Node` / `Protocols` / `Client.*` / `HttpTunnel.*` 全部按配置生效。

代价是源生成器对这个调用点报 3 条警告（`SYSLIB1100` + 2×`SYSLIB1101`），原因是
`HttpTunnelConfig.ActivityHeadersPropagator` 是抽象类型 `DistributedContextPropagator`，无法生成构造代码，
`HttpTunnel` 子树因此走回退路径。我们从不配置该属性，所以在 CYarpServer.csproj 里定向 `NoWarn`，
并在启动日志回显生效值——万一将来升级 CYarp 或调整裁剪后回退路径失效，日志会立刻显示回落到库默认值
（`KeepAliveInterval=00:00:50`、`MaxTunnelsPerClient=10`），而不会静默出错。

### 为什么服务端不做 Native AOT（已实测）

`CYarp.Server` 2.0.3 的端点是这样映射的：`endpoints.Map("/{tunnelId}", TunnelHandler.HandleTunnelAsync)`
——handler 带 DI 参数，编译期只能选中 `Map(IEndpointRouteBuilder, string, Delegate)` 重载，
运行期由 `RequestDelegateFactory` 用表达式树构造请求委托。请求委托的源生成器（RDG）无法拦截
**预编译程序集内**的调用点，于是在 Native AOT 下：

```
System.NotSupportedException: JsonTypeInfo metadata for type '...' was not provided ...
   at Microsoft.AspNetCore.Http.RequestDelegateFactory.Create(...)
```

表现为**服务端首个请求即 500**，且发布时会有 `IL2026`/`IL3050`。用最小复现工程（预编译库
`Map(pattern, Delegate)` + `PublishAot` + PublishAotClang）验证过，确认与业务代码无关。

所以服务端改为 **self-contained 单文件 + trimmed**：约 17 MB、VPS 无需 .NET 运行时，冷启动比
AOT 略慢（JIT），代价换来零 CYarp 源码维护。一旦上游把端点映射改成 AOT 友好的写法并发包，
把 `PublishAot` 加回服务端即可。

### 客户端为什么可以 AOT

客户端只用 `HttpClient` / `ClientWebSocket`，没有端点映射，`CYarp.Client` 包本身也是按 AOT
友好的方式编译的，实测发布零警告。产物固定 glibc 2.17 目标（`<GLibcVersion>2.17</GLibcVersion>`），
可在 CentOS 7 / Ubuntu 18.04 及以上运行——zig 默认目标需要 glibc 2.29，会在老系统上报
`GLIBC_2.29 not found`。

---

## 八、测试

```powershell
dotnet test cyarp-cnb.sln -c Release
```

集成测试在进程内起「假上游 + CYarpServer + 真实 CYarpClient」，断言：

| 用例 | 断言 |
|---|---|
| 握手 claims | `IClient.User` 上有 `cnb:client-id` 与 `cnb:upstream-authorization` |
| 对话转发 | 上游收到 `/{org}/{repo}/-/ai/chat/completions`、`Authorization: Bearer <CNB token>`、query 与 body 保留、公网 key 未出现在任何请求头、Host 为目标地址 |
| 双工作空间 | 各自打到各自 TargetUri 与各自 token，不串台 |
| 离线工作空间对话 | 返回 `502` |
| `GET /{org}/{repo}/v1/models` | 在线 `200`（内容为 `ProxyModels` 数组）、离线 `503`、含尾斜杠 `200`，均不触达上游 |
| 裸 `/v1/...`、单段/三段前缀、其它 `/v1/*` | 返回 `404` |
| 方法不匹配（`POST .../v1/models`） | 返回 `405` |
| `ApiKey` 错误/缺失 | `401` 且上游零请求 |
| `stream=true` | SSE 分帧透传：假上游写第一帧后阻塞，直到测试读到第一帧才写第二帧（整体缓冲的实现会卡死失败） |

另有 `scripts/verify-endpoint.sh`（在公网机器与工作空间内各跑一次对比），验证「必须从工作空间内部
发出请求」这个前提：

```bash
CNB_TOKEN=xxx CNB_REPO_SLUG=org/repo bash scripts/verify-endpoint.sh
```

实测结论：从外网带**真实** `CNB_TOKEN` 直连 `api.cnb.cool` 返回
`412 {"errcode":10412,"errmsg":"Blocked by network policy."}`（不带 token 是 `401`），说明鉴权已过、
卡在来源 IP 上；工作空间内部同样的命令返回正常 SSE 流。

---

## 九、已知取舍：models 免鉴权、隧道入站无鉴权、现阶段没有 TLS

- **`GET /{org}/{repo}/v1/models` 免鉴权**：它是本地应答的静态模型列表（不耗额度、不触上游、不含敏感
  数据），免鉴权是为了让保活 cron 不带任何密钥探活。知道组织/仓库名的人可以读到模型列表与该工作空间的
  隧道在线状态。
- **隧道入站不做鉴权**：客户端只带 CNB token（不具备公网密钥语义），服务端不做准入拒绝，也没校验
  共享密钥。因此能访问到服务端端口的人都可以用任意 `X-CYarp-Client` 注册隧道，把该工作空间的请求引到
  自己那里（能看到请求体），并把真正的工作空间踢下线。要收紧只需在该 handler 里加一个共享密钥 claim
  校验并在失败时返回 `401`，改动局限在这一个文件。
- **没有 TLS**：Kestrel 直接对外只提供 HTTP，CNB token（握手头 + 隧道内注入）与 API 内容都是明文。
  请据此控制暴露面（只监听需要的网卡/端口、用防火墙或 WireGuard 收敛来源）。加 HTTPS 的改法见
  「三、服务端部署 → 以后要加 HTTPS」，不需要改代码。
