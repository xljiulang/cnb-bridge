# CYarp 取代 `<subdomain>-9001.cnb.run`：CNB 工作空间侧启动与常驻

把 CNB 工作空间内网独享的 AI 端点转成固定域名 OpenAI 兼容 API，**不再依赖会漂移的
`https://<subdomain>-9001.cnb.run`**。

```
公网客户端  ──Bearer PROXY_KEY──▶  https://ai.example.com/v1/chat/completions
                                        │  (Caddy 自动 HTTPS)
                                        ▼
                                   CYarpServer (VPS, 已改源码)
                                        │  长连接隧道(出站建立, 固定域名)
                                        ▼
                            CYarpClient  ← 跑在 CNB 工作空间里(本文重点)
                                        │  携带 Authorization: Bearer $CNB_TOKEN
                                        ▼
                   https://api.cnb.cool/$CNB_REPO_SLUG/-/ai/chat/completions
```

为什么不再需要 `-9001.cnb.run`：CYarp 是**内网主动向外拨号**建立长连接（frp 同类），
公网入口是你自己的固定域名 VPS。工作空间子域名怎么变都无所谓，隧道重连即可。

---

## 一、核心问题：`CNB_TOKEN` 从哪来

**平台自动注入，不需要你申请。** CNB 在每次流水线运行时铸一把临时凭证放进环境变量
`CNB_TOKEN`，作用域覆盖 `api.cnb.cool` 的团队 API 与 `/-/ai/chat/completions`。
它随工作空间回收而作废，所以**任何一端都不要落盘缓存**——本方案全程只走环境变量。

---

## 二、工作空间侧怎么启动 CYarpClient

### 2.1 原理

CYarpClient 是个普通常驻进程，用 `Host.CreateDefaultBuilder` 启动，所以**所有配置都能用
环境变量注入**（双下划线 `__` 表示嵌套层级）。这意味着 `CNB_TOKEN` 可以只存在于环境变量里，
一个字节都不写盘。已在真实环境实测：环境变量注入的 token 被正确送达上游。

对应关系：

| 环境变量 | 值 | 说明 |
|---|---|---|
| `CYarpClientOptions__ServerUri` | `wss://ai.example.com` | 你的 CYarpServer 固定域名 |
| `CYarpClientOptions__TargetUri` | `https://api.cnb.cool/$CNB_REPO_SLUG/-/ai` | 隧道出口指向 CNB 内网 AI |
| `CYarpClientOptions__ConnectHeaders__Authorization` | `Bearer $CNB_TOKEN` | **握手时把 CNB token 交给 Server** |
| `CYarpClientOptions__ConnectHeaders__X-CYarp-Client` | `cnb` | 客户端 id，单工作空间用默认值即可 |
| `CYarpClientOptions__KeepAliveInterval` | `00:00:30` | 心跳，同时也是工作空间保活信号 |

Server 端收到握手后把 `Authorization` 存进 `IClient.UpstreamAuthorization`，转发时**丢弃调用方
的 PROXY_KEY、换成这个 CNB token**，因此内网上游鉴权成立，公网 key 也不会泄漏到上游。

### 2.2 三步接入

**第 1 步：把客户端二进制放进仓库**

工作空间是 Linux 容器，用自包含单文件发布（无需预装 .NET，约 35 MB）：

```bash
# 在开发机上（已对 CYarp 源码打过 patches/cyarp-cnb-token.patch）
dotnet publish Host/CYarpClient/CYarpClient.csproj \
  -c Release -r linux-x64 --self-contained true \
  -p:PublishSingleFile=true -p:EnableCompressionInSingleFile=true \
  -o ./out
# 把产物放到仓库的 deploy/cyarp/ 下
mkdir -p deploy/cyarp && cp ./out/CYarpClient deploy/cyarp/
```

> 本目录 `deploy/cyarp/CYarpClient` 已随附一个编译好的 linux-x64 单文件产物，可直接
> 拷进你的仓库，跳过这一步。

**第 2 步：放启动脚本** —— `deploy/cyarp-start.sh`（本目录已提供）

它做的事：校验 `CYARP_SERVER_URI` / `CNB_TOKEN` 非空 → 幂等杀掉旧进程 → 把配置全部导出为
环境变量 → `nohup` 拉起 → 轮询日志确认 `Connected to server`，成功才让构建变绿。

**第 3 步：在 `.cnb.yml` 里调用** —— 见 `deploy/cnb.yml.example`

```yaml
$:
  vscode:
    - docker:
        image: cnbcool/default-dev-env:latest
      env:
        CYARP_SERVER_URI: "wss://ai.example.com"
      runner:
        cpus: 2
      services:
        - name: vscode
          options:
            keepAliveTimeout: 18h      # 隧道长连接即心跳，别用默认 10 分钟
        - docker
      stages:
        - name: start-cyarp-client
          script: |
            set -euo pipefail
            bash "$(pwd)/deploy/cyarp-start.sh"
```

在仓库页面点「云开发」启动工作空间，构建日志会出现：

```
===== cyarp client started, pid=123 =====
===== cyarp client: tunnel established =====
```

看到第二行即隧道已通，此时公网域名就能调用了。

### 2.3 保活与自愈

平台回收机制（官方 `docs.cnb.cool/zh/workspaces/workspace-recycling.md`）：

- 创建后 **10 分钟未进入 VS Code 页面** → 回收；
- 页面关闭后超过 **10 分钟无操作** → 回收（对应 `keepAliveTimeout` 默认 `600000ms`，
  官方定义为「检测不到开发环境内的 http/ssh 连接就关闭」）；
- **单会话上限默认 18h**（持续心跳可维持到上限）；
- **凌晨 4:00–6:00 强制回收**（使用超过 8h 时），这条绕不过，只能靠重建。

对应到本方案是三层保活：

| 层 | 手段 | 防的是什么 |
|---|---|---|
| L0 | `services.vscode.options.keepAliveTimeout: 18h` | 把「10 分钟无操作即回收」放宽到会话上限 |
| L1 | `main:` cron 每 5 分钟查 `workspace/list`，无 running → `workspace/start` | 被平台回收后自动拉起 |
| L2 | cron 探活你的固定域名 `/v1/models`，连续两次失败 → `stop + start` | 「假死」：workspace 显示 running 但隧道/客户端实际已死 |

- **工作空间存活**：`keepAliveTimeout: 18h` + 客户端 30s 心跳长连接，两者共同把工作空间钉住。
- **客户端重连**：CYarpClient 自带重连循环（断开后 5s/10s 重试），无需额外守护。
- **被回收/假死**：由上面 cron 兜底。每次 cron 触发都是流水线新铸造的 `CNB_TOKEN`，
  工作空间重建后新 token 随新进程自动生效。

> 注意：cron 必须放在 `main:` 键下（对应默认分支名），否则永不触发。
> L2 探活打的是 CYarpServer 本地提供的 `/v1/models`（不耗 AI 额度）；若用
> `/v1/chat/completions` 探活每次会真实计费。`FIXED_URL` 留空则只做状态检查。

---

## 三、VPS 侧（前置条件）

见 `server/`：`Dockerfile` + `docker-compose.yml` + `Caddyfile`。

```bash
# 1) 准备源码：克隆 CYarp，打补丁
git clone https://github.com/xljiulang/CYarp.git && cd CYarp
git apply --ignore-whitespace /path/to/patches/cyarp-cnb-token.patch
# 补丁基于上游 commit a88f9ec (2026-06-09) 生成；--ignore-whitespace 兼容行尾差异

# 2) 把 server/ 里的文件放到源码根目录，改 appsettings.json 的 ProxyKey 和 Caddyfile 的域名
#    确认 ai.example.com 的 DNS A 记录已指向本机

# 3) 启动
docker compose up -d
```

Caddy 自动申请并续期 HTTPS 证书。`RedisClientStateStorageOptions.ConnectionString` 留空即可
——单节点不需要 Redis（补丁已把 Redis 改为可选）。

公网调用：

```bash
# /v1/models（本地返回，不耗额度）
curl https://ai.example.com/v1/models -H "Authorization: Bearer <你的 PROXY_KEY>"

# 对话
curl https://ai.example.com/v1/chat/completions \
  -H "Authorization: Bearer <你的 PROXY_KEY>" \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}]}'
```

`/v1` 前缀会被 Server 剥掉再拼到 TargetUri 上，因此 `/v1/chat/completions` 最终命中
`https://api.cnb.cool/<org>/<repo>/-/ai/chat/completions`。

---

## 四、源码改动清单

补丁 `patches/cyarp-cnb-token.patch`，共 6 个文件：

| 文件 | 改动 |
|---|---|
| `CYarp.Server/IClient.cs` | 新增 `string? UpstreamAuthorization { get; }` |
| `CYarp.Server/Clients/Client.cs` | 实现该属性 + 构造参数 |
| `CYarp.Server/Clients/ClientStateChannel.cs` | `ReadOnlyClient` 同步实现该属性 |
| `CYarp.Server/Middlewares/ClientHandler.cs` | 握手时保存 `Authorization` 头 |
| `Host/CYarpServer/HttpForwardHandler.cs` | 新增 `CnbAiTransformer`：剥离 `/v1` + 注入 CNB token |
| `Host/CYarpServer/Program.cs` | 移除 JWT 鉴权；clientId 改为握手头；Redis 可选；可选 PROXY_KEY 入口校验；本地提供 `/v1/models` |

已实测通过：`/v1/chat/completions` → 上游收到 `/org/repo/-/ai/chat/completions`
与 `Authorization: Bearer <CNB_TOKEN>`，外部 PROXY_KEY 未泄漏。

---

## 五、两个实测结论（会直接影响你的部署）

> 可复现脚本：`scripts/verify-endpoint.sh`。在**公网机器**和**CNB 工作空间**里各跑一次，
> 对比输出即可自行验证下面全部结论。
>
> ```bash
> CNB_TOKEN=xxx CNB_REPO_SLUG=xljiulang/cyarp bash scripts/verify-endpoint.sh
> ```

### 1. 为什么必须走隧道：`412 Blocked by network policy`

用**真实 `CNB_TOKEN`** 从外网直连 `api.cnb.cool`，实测结果：

| 请求 | 结果 |
|---|---|
| 不带 token | `401` `{"errcode":16,...not logged in}` |
| **带真实 CNB_TOKEN** | **`412` `{"errcode":10412,"errmsg":"Blocked by network policy."}`** |
| 带伪造 token | `401` |

关键点：**带真 token 时错误从 401 变成 412**——说明鉴权已通过，卡在**网络策略**上。
而且 412 是全站性的（`/user/repos`、`/workspace/list`、`/-/ai/chat/completions` 全都 412），
换美国出口 IP 也一样。

→ 所以限制的是**来源 IP**，token 只是必要条件。**请求必须从 CNB 工作空间内部发出**，
这正是 CYarp 隧道存在的意义。（在工作空间内用同样命令实测返回正常的 SSE 流。）

### 2. CNB 没有 `/v1/models`，由 Server 本地提供

官方 OpenAPI（`https://api.cnb.cool/swagger.json`）中 `AI` 标签下**只有一条路径**：
`/{repo}/-/ai/chat/completions`。实测 `/-/ai/models`、`/-/ai/v1/models` 均返回
`404 Resource not found`（路由不存在，与策略拦截的 412 不同）。

但 OpenAI 客户端启动时普遍会探测 `/v1/models`，因此补丁让 Server **本地返回**该列表，
由 `ProxyModels` 配置（默认 `deepseek-v4-flash,glm-5.3-flash,kimi-k3`）驱动，不占用上游额度。

> 注意：上游会改写 `model` 字段。请求 `deepseek-v4-flash` 时，响应里的 `model` 是
> `deepseek-v4.1-flash`，`id` 是 32 位 hex（不是 `chatcmpl-` 前缀）。若你的客户端对
> 这两处有严格校验，需要自行适配。
