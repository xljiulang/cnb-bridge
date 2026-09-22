# 部署指南

把 CNB 工作空间里的 AI 端点，发布成固定域名的 OpenAI 兼容 API。部署分三步：

1. 在开发机构建两个产物（客户端 AOT / 服务端自包含单文件）
2. 在 VPS 上把服务端装成系统服务（ServiceSelf，不用 Docker、不用 Nginx/Caddy）
3. 把本目录（`deploy/`）的内容作为 CNB 仓库根放好，启动工作空间建立隧道

架构与设计取舍见仓库根目录 [`README.md`](../README.md)。

```
公网客户端 ──Bearer PROXY_KEY──▶ http://<你的域名>/<clientId>/v1/chat/completions
                                      │  Kestrel 直接对外
                                      ▼
                              CYarpServer (VPS 系统服务)
                                      │  长连接隧道（工作空间主动向外拨号）
                                      ▼
                       CYarpClient ← 跑在 CNB 工作空间（本目录 cyarp/CYarpClient）
                                      │  Authorization: Bearer $CNB_TOKEN
                                      ▼
                https://api.cnb.cool/$CNB_REPO_SLUG/-/ai/chat/completions
```

---

## 0. 需要准备什么

| 项 | 说明 | 是否必需 |
|---|---|---|
| VPS | Linux x64（glibc ≥ 2.17，即 CentOS 7 / Ubuntu 18.04 及以上）或 Windows；有公网 IP | 必需 |
| 域名 | 指向该 VPS 的 A 记录；现在只跑 HTTP，所以不必配证书 | 可选（也可以直接用 IP） |
| `PROXY_KEY` | 公网调用方要带的密钥，自己编一个强随机串 | 必需 |
| CNB 仓库 | **把本目录的内容放到它的根目录**；需要能开「云开发」工作空间 | 必需 |
| `CNB_TOKEN` | **平台自动注入**，每次流水线运行新铸一把，不需要申请、不要写进仓库 | 不用准备 |

### 本目录就是「CNB 仓库根内容」

把这个目录里的东西原样放到 CNB 仓库根，即得到：

```
<CNB 仓库>/
  .cnb.yml               CNB 流水线配置（含保活 cron）
  cyarp-start.sh         工作空间启动脚本（与 cyarp/CYarpClient 同级同目录）
  cyarp/CYarpClient      linux-x64 Native AOT 客户端（约 8 MB）
  cyarp/appsettings.json 客户端非敏感默认配置
  README.md              本文件
```

> 注意：要放进**你想暴露其 AI 端点**的那个仓库。客户端隧道的出口是
> `https://api.cnb.cool/$CNB_REPO_SLUG/-/ai`，而 `CNB_REPO_SLUG`、`CNB_TOKEN` 的作用域都是
> 「工作空间所在的那个仓库」，所以载荷仓库与 AI 仓库必须是同一个。

文件清单：

| 文件 | 作用 |
|---|---|
| `.cnb.yml` | 流水线配置：启动工作空间的 stage + 保活 cron，需要改 3 处（见 3.2） |
| `cyarp/CYarpClient` | 已编译好的 linux-x64 Native AOT 客户端（约 8 MB），必须随仓库提交 |
| `cyarp/appsettings.json` | 客户端日志级别等非敏感默认配置（敏感项全走环境变量） |
| `cyarp-start.sh` | 工作空间启动脚本：注入环境变量、拉起客户端、轮询日志确认隧道建立 |
| `README.md` | 本文件 |

---

## 1. 构建产物（开发机）

前置：安装 .NET 10 SDK（`dotnet --list-sdks` 能看到 10.x）。构建机需要联网一次（下载 ILCompiler 与 zig 工具链，之后进 NuGet 缓存可离线复现）。

```powershell
pwsh ./scripts/publish.ps1          # 客户端(linux-x64 AOT) + 服务端(linux-x64 自包含 trimmed)

# 只发服务端，并且是 Windows 版
pwsh ./scripts/publish.ps1 -SkipClient -ServerRuntimeIdentifier win-x64
```

产出：

| 产物 | 位置 | 形态 | 大小 |
|---|---|---|---|
| `CYarpClient` | `deploy/cyarp/CYarpClient`（同步后是载荷里的 `cyarp/CYarpClient`） | Native AOT 单文件（glibc 2.17） | 约 8 MB |
| `CYarpServer.exe` | `artifacts/server-win-x64/` | Windows 自包含 + trimmed 单文件 | 约 15 MB |
| `CYarpServer` | `artifacts/server/CYarpServer` | 自包含 + trimmed 单文件 | 约 17 MB |

两个产物都**不需要目标机器装 .NET 运行时**。

> `pwsh ./scripts/publish.ps1 -RuntimeIdentifier linux-arm64` 可发布到其他 RID。
> 服务端发布时会出现一条 `IL2104: Assembly 'CYarp.Server' produced trim warnings`，这是 CYarp 包内部的裁剪提示，属已知情况（见根 README 第七节）。

---

## 2. 部署服务端（VPS）

### 2.1 上传

只需两个文件，放在同一个目录（下文以 `/opt/cyarp/` 为例）：

```bash
sudo mkdir -p /opt/cyarp && cd /opt/cyarp

# 在开发机上
scp artifacts/server/CYarpServer  root@<VPS_IP>:/opt/cyarp/
scp artifacts/server/appsettings.json root@<VPS_IP>:/opt/cyarp/

# 在 VPS 上
chmod +x /opt/cyarp/CYarpServer
```

### 2.2 配置

编辑 `/opt/cyarp/appsettings.json`，**至少改掉 `ProxyKey`**：

```jsonc
{
  // 公网调用方必须带这个 key；留空表示不校验（只建议内网调试时）
  "ProxyKey": "换成你自己的强随机串",

  // /v1/models 本地返回的模型列表，不占上游额度
  "ProxyModels": "deepseek-v4-flash,glm-5.3-flash,kimi-k3",

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

`CYarpOptions` 的生效值会在启动日志里回显（`CYarp options in effect: ...`）；如果哪天这里与文件不一致，说明配置没被读到。

### 2.3 安装并启动为系统服务

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

### 2.4 验证服务端

```bash
curl -i http://<你的域名或IP>/v1/models -H "Authorization: Bearer <PROXY_KEY>"
```

期望 `200` 且返回模型列表。此时**还没有工作空间接入**，属于正常状态：

| 请求 | 期望 |
|---|---|
| `GET /v1/models` + 正确 key | `200`（本地应答，不需要隧道） |
| `GET /v1/models` + 错误/缺失 key | `401`（若 key 为空则不校验） |
| `GET /v1/chat/completions` 之类 | `502`（没有在线工作空间） |

### 2.5 升级与迁移

```bash
# 覆盖二进制后重启（unit 已存在，用 systemctl 即可）
scp artifacts/server/CYarpServer root@<VPS_IP>:/opt/cyarp/
chmod +x /opt/cyarp/CYarpServer
sudo systemctl restart CYarpServer
```

注意两点：

- **换目录**：unit 里记录的是绝对路径，且 ServiceSelf 会检查「同名但不同路径」并报错。要搬到别的目录，先 `sudo ./CYarpServer stop` 再在新目录 `sudo ./CYarpServer start`。
- **改端口**：在 `appsettings.json` 里加 `"Urls": "http://+:8080"` 后 `sudo systemctl restart CYarpServer`；systemd 服务不继承你 shell 里的 `ASPNETCORE_URLS`。

### 2.6 防火墙

放行服务端监听端口（默认 80）：

```bash
# ufw
sudo ufw allow 80/tcp
# firewalld
sudo firewall-cmd --permanent --add-port=80/tcp && sudo firewall-cmd --reload
```

### 2.7 Windows 部署（可选）

服务端也能跑在 Windows 上，ServiceSelf 会把它装成 Windows 服务（同样不需要装 .NET 运行时）：

```powershell
# 在开发机发布 Windows 版（产物在 artifacts/server-win-x64/）
pwsh ./scripts/publish.ps1 -SkipClient -ServerRuntimeIdentifier win-x64

# 把 CYarpServer.exe 与 appsettings.json 拷到目标机器同一目录，例如 C:\cyarp\
# 改 appsettings.json 里的 ProxyKey，然后以管理员身份：
C:\cyarp\CYarpServer.exe start     # 安装 + 启动 + 开机自启（SCM 服务）
C:\cyarp\CYarpServer.exe logs      # 跟随服务日志
C:\cyarp\CYarpServer.exe logs filter=ERROR
C:\cyarp\CYarpServer.exe stop      # 停止并删除服务
C:\cyarp\CYarpServer.exe           # 前台运行，方便调试
```

改端口同样在 `appsettings.json` 里加 `"Urls": "http://+:8080"`（默认 `http://+:80` 需要管理员权限才能绑定）。

---

## 3. 部署 CNB 工作空间侧

### 3.1 把本目录的内容放到 CNB 仓库根

两种落地方式，任选其一：

```bash
# 方式一：在本仓库里开发，然后把载荷同步到 CNB 仓库的工作副本
rsync -a --delete deploy/ /path/to/cnb-some-repo/
cd /path/to/cnb-some-repo && git add -A && git commit -m "update cyarp payload" && git push

# 方式二：直接在 cnb.cool 上新建仓库，把这几个文件按上面的结构推上去
```

如果希望载荷里只有运行必需的文件，同步时加 `--exclude README.md`；留着也无妨，它就是那个仓库的说明文档。

> 客户端运行时会把自己的文件日志写到 `cyarp/logs/`（按天分文件）。不想让它出现在 `git status` 里，就在**载荷仓库**的 `.gitignore` 加一条 `cyarp/logs/`。

### 3.2 改 `.cnb.yml` 里的三处配置

`.cnb.yml` 已经可用，只需要改三处（文件里都标了「改这里」）：

```yaml
# 1/3 workspace：你的 CYarpServer 地址（加 TLS 后改 wss://）
CYARP_SERVER_URI: "ws://ai.example.com"

# 2/3 keepalive cron：公网调用 key，与服务端 appsettings.json 的 ProxyKey 一致
PROXY_KEY: "CHANGE-ME-your-public-api-key"

# 3/3 keepalive cron：服务端公网基址；留空则只做工作空间状态检查
FIXED_URL: "http://ai.example.com"
```

保活 cron（L1/L2）在同一个文件的 `main:` 键下，**必须是 `main:`（对应默认分支名），否则永不触发**；一般不用动。

没改域名就开工作空间时，启动脚本会立刻打印 `CYARP_SERVER_URI is required, e.g. ws://ai.example.com` 并以退出码 1 结束（构建变红），不会静默不建隧道。

### 3.3 启动并验证

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

`clientId` 默认就是 `CNB_REPO_SLUG` 原样（含 `/`）；要自定义就设 `CYARP_CLIENT_ID`（需与服务端路由的前缀一致）。

### 3.4 客户端产物升级

替换载荷里的 `cyarp/CYarpClient` 并同步到 CNB 仓库后，重启工作空间（或在工作空间里重跑该 stage）即可；启动脚本会先 `pkill` 掉旧进程再拉起新的。

---

## 4. 端到端验收清单

假设工作空间 `org/repo` 已在线，`<base>` = `http://你的域名`：

```bash
# 1) 服务存活（本地应答，不耗额度，也不需要隧道）
curl -s -o /dev/null -w '%{http_code}\n' $BASE/v1/models -H "Authorization: Bearer $PROXY_KEY"
# 期望 200

# 2) 该工作空间的隧道状态（在线 200 / 隧道假死 503，不耗额度）
curl -s -o /dev/null -w '%{http_code}\n' $BASE/org/repo/v1/models -H "Authorization: Bearer $PROXY_KEY"
# 期望 200

# 3) 真实调用
curl -s $BASE/org/repo/v1/chat/completions \
  -H "Authorization: Bearer $PROXY_KEY" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}]}'
# 期望 200 + JSON

# 4) 流式
curl -sN $BASE/org/repo/v1/chat/completions \
  -H "Authorization: Bearer $PROXY_KEY" -H "Content-Type: application/json" \
  -d '{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}],"stream":true}'
# 期望 SSE 分帧返回

# 5) 鉴权与路由边界
curl -s -o /dev/null -w '%{http_code}\n' $BASE/v1/models -H "Authorization: Bearer wrong"   # 401
curl -s -o /dev/null -w '%{http_code}\n' $BASE/ghost/repo/v1/models -H "Authorization: Bearer $PROXY_KEY"  # 503
curl -s -o /dev/null -w '%{http_code}\n' $BASE/ghost/repo/v1/chat/completions -H "Authorization: Bearer $PROXY_KEY" -H 'Content-Type: application/json' -d '{}'  # 502
```

OpenAI 客户端接入时：`base_url = <base>/org/repo/v1`，`api_key = <PROXY_KEY>`。
单工作空间在线时也可以省掉前缀，直接 `base_url = <base>/v1`。

前缀规则（细节见根 README「路由与鉴权规则」）：

- `{clientId}` 就是仓库 slug，所以公网地址形如 `/<org>/<repo>/v1/...`；与**已连接**的工作空间做最长匹配，大小写敏感。
- 多工作空间同时在线时，请始终带前缀调用；带前缀的 `/v1/models` 在线 200、隧道断了 503（本地应答，不耗额度）。
- 组织名恰好叫 `cyarp` 也没问题：`/cyarp/<repo>/v1/...` 这类公网路径照样要求 `PROXY_KEY`，只有真正的 `/cyarp` 握手端点免鉴权。
- 旧形态（把 `/` 换成 `~` 的 id，如 `org~repo`）仍可作为普通 id 使用，但它与 `org/repo` 是两个不同的工作空间。

---

## 5. 配置参考

### 服务端（`appsettings.json` 或环境变量）

| 键 | 默认 | 说明 |
|---|---|---|
| `ProxyKey` | 空 | 公网调用方的 Bearer key；为空则不校验 |
| `ProxyModels` | `deepseek-v4-flash,glm-5.3-flash,kimi-k3` | `/v1/models` 本地返回的列表 |
| `Urls` | `http://+:80` | Kestrel 监听地址；也可用 `--urls` 启动参数 |
| `CYarpOptions:Client:KeepAliveInterval` | `00:00:40` | 服务端心跳周期 |
| `CYarpOptions:HttpTunnel:MaxTunnelsPerClient` | `20` | 每个工作空间并发 HTTP 隧道（≈并发请求）上限 |
| `CYarpOptions:HttpTunnel:CreationTimeout` | `00:00:10` | 隧道建立超时 |
| `Logging:LogLevel:*` | Information | 日志级别 |

环境变量写法：`ProxyKey=xxx`、`CYarpOptions__HttpTunnel__MaxTunnelsPerClient=50`（服务由 systemd 拉起时需写进 unit 的 `Environment=`，见 2.5）。

### 客户端（全部走环境变量，由 `cyarp-start.sh` 注入）

| 变量 | 值 | 说明 |
|---|---|---|
| `CYARP_SERVER_URI` | `ws://你的域名` | 服务端地址；加 TLS 后改 `wss://` |
| `CYARP_CLIENT_ID` | `<org>/<repo>` | 工作空间 id（公网路径前缀；默认取 `CNB_REPO_SLUG` 原样），可选 |
| `CYARP_RUN_DIR` | `/workspace/cyarp` | 客户端工作目录（`cyarp.log` 落在这里），可选 |
| `CYARP_BIN` | `<载荷根>/cyarp/CYarpClient` | 客户端二进制路径（默认按脚本自身目录推导），可选 |

脚本内部会导出这些配置（token 只存在环境变量里，不落盘）：

| 环境变量 | 值 |
|---|---|
| `CYarpClientOptions__ServerUri` | `$CYARP_SERVER_URI` |
| `CYarpClientOptions__TargetUri` | `https://api.cnb.cool/$CNB_REPO_SLUG/-/ai` |
| `CYarpClientOptions__ConnectHeaders__Authorization` | `Bearer $CNB_TOKEN` |
| `CYarpClientOptions__ConnectHeaders__X-CYarp-Client` | `$CYARP_CLIENT_ID` |
| `CYarpClientOptions__KeepAliveInterval` | `00:00:30` |
| `CYarpClientOptions__ConnectTimeout` | `00:00:10` |

---

## 6. 保活与自愈

| 层 | 手段 | 防的是什么 |
|---|---|---|
| L0 | `services.vscode.options.keepAliveTimeout: 18h` + 客户端 30s 心跳长连接 | 页面关闭 10 分钟即回收 |
| L1 | `main:` cron 每 5 分钟查 `workspace/list`，无 running → `workspace/start` | 被平台回收后自动拉起 |
| L2 | cron 探活 `$FIXED_URL/$CLIENT_ID/v1/models`，连续两次非 200 → `stop + start` | 「假死」：workspace 显示 running 但隧道已断 |

两个已在脚本里处理过的坑，自己改脚本时别踩回去：

1. `CYarpClientOptions__ConnectHeaders__X-CYarp-Client` 这种**带连字符的变量名不能用 `export`**（bash 报 `not a valid identifier`），必须用 `env "NAME=value"`。
2. CYarp 2.0.3 的就绪日志是中文「连接到服务器…成功」，判定要同时匹配中英文，否则构建永远不会变绿。

---

## 7. 故障排查

| 症状 | 可能原因 | 处理 |
|---|---|---|
| 一开工作空间就失败，日志里是 `CYARP_SERVER_URI is required, e.g. ws://ai.example.com` | `.cnb.yml` 里的域名还没改（或没同步到 CNB 仓库） | 按 3.2 改 `CYARP_SERVER_URI`；确认改动已推到 CNB 仓库的默认分支 |
| 一开工作空间就失败，日志里是 `client binary not found or not executable: .../cyarp/CYarpClient` | `cyarp-start.sh` 与 `cyarp/CYarpClient` 没在同一目录（载荷结构被改动），或二进制没随仓库提交 | 保持载荷结构不变；确认 `cyarp/CYarpClient` 已提交（注意 `.gitignore` 别把它排除） |
| 构建日志停在 `cyarp client: no tunnel after 30s` | 域名/端口不通、`CYARP_SERVER_URI` 写错、服务端没起 | 看脚本打印的 `cyarp.log` 尾部；在 VPS 上 `curl -i http://127.0.0.1/v1/models -H "Authorization: Bearer <key>"` |
| 客户端日志 `Connect failure ... Connection refused` | 服务端未监听该地址/端口，或被防火墙拦 | 确认 `sudo systemctl status CYarpServer` 与 `Now listening on: ...`；放行端口 |
| Windows 上启动即报 `SocketException (10013) ... 访问权限`（WSAEACCES） | 端口落在 Windows 保留段（Hyper-V/WSL 预留，如 8081-8180），或用了 <1024 的端口但没提权 | `netsh interface ipv4 show excludedportrange protocol=tcp` 查保留段，换一个端口：在 `appsettings.json` 加 `"Urls": "http://+:18089"` |
| 公网全部请求 `401` | `PROXY_KEY` 与 `appsettings.json` 里的 `ProxyKey` 不一致 | 重新 `curl` 带正确 key；或把 `ProxyKey` 清空（仅调试） |
| 公网请求 `502` | 没有该 `clientId` 的在线工作空间（前缀写错、大小写不符），或多工作空间下用了裸 `/v1/...` | 用脚本打印的 `clientId`（形如 `org/repo`）对齐 URL 前缀；单工作空间在线时裸路径才可用 |
| 裸路径 `502` 但带前缀正常 | 同时有多个工作空间在线 | 始终带前缀调用 |
| `/{clientId}/v1/models` 返回 `503` | 服务端活着但该工作空间的隧道断了（假死） | cron 会自动 `stop + start`；也可手动重启工作空间 |
| 上游返回 401/412 之类 | 上游鉴权/网络策略 | 412 表示请求不是从 CNB 工作空间内发出；401 多半是 `CNB_TOKEN` 过期，重启工作空间让平台重铸 |
| 启动日志里 `CYarp options in effect` 显示的是库默认值（`00:00:50` / `10`） | appsettings 没被读到，或升级 CYarp 后配置绑定回退路径失效 | 确认 `appsettings.json` 在二进制同目录；见根 README 第七节「为什么 CYarpOptions 要在服务端项目里绑定」 |
| `CYarpServer start` 报「系统已存在同名但不同路径的服务」 | 之前从别的目录安装过 | 在旧目录执行 `sudo ./CYarpServer stop`，或 `sudo systemctl disable --now CYarpServer && sudo rm /etc/systemd/system/CYarpServer.service` |
| `start` 报「无法操作服务：没有root权限」 | 没用 root | 加 `sudo` |
| 客户端日志出现 `连接到服务器...成功` 但公网仍 502 | 公网 URL 的 `clientId` 与握手头的 `X-CYarp-Client` 不一致 | 核对 `clientId`（`<org>/<repo>`） |

常用观察点：

```bash
sudo journalctl -u CYarpServer -n 100 --no-pager    # 服务端
tail -f /workspace/cyarp/cyarp.log                  # 客户端控制台输出（工作空间内）
tail -f cyarp/logs/log$(date +%Y%m%d).txt          # 客户端文件日志（工作空间内，相对载荷根）
```

---

## 8. 安全须知与后续可选项

当前部署形态有两处已知取舍，按需自行加固：

| 项 | 现状 | 加固方式 |
|---|---|---|
| 传输加密 | 只有 HTTP：CNB token（握手头 + 隧道内注入）与 API 内容都是明文 | 在 `appsettings.json` 加 Kestrel HTTPS 端点/证书（或前面挂一层你自己的 TLS 终结），客户端 `CYARP_SERVER_URI` 改 `wss://`，`FIXED_URL` 改 `https://`；代码不用动 |
| 隧道握手鉴权 | 未做准入校验：能访问到服务端端口的人都能注册任意 `clientId`，把该工作空间的请求引走 | 在 `CYarpTunnelAuthenticationHandler` 里加一个共享密钥 claim 校验（服务端配 `TunnelKey`、工作空间注入对应环境变量），不匹配返回 `401`；改动局限在该文件 |

`PROXY_KEY` 只保护公网 API 入口，不保护隧道端点；隧道端点在 `PROXY_KEY` 校验之外（握手头里带的是 CNB token）。
