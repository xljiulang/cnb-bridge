# CNB Bridge

CNB Bridge 是一个 Windows 本机 AI 桥接客户端：通过 CNB WebView 扫码绑定，自动创建个人访问令牌并配置 SSH 公钥。新账号没有可建仓的 Owner 组织时，客户端会使用账号名自动创建一个根组织；随后在该组织下创建/复用 `cnb-ai-bridge` 仓库，启动 CNB 云原生开发工作空间，经纯 Rust `russh` 建立 SSH 本地端口转发，把工作空间内的 OpenAI 兼容 agent 暴露到：

```
http://127.0.0.1:11233/v1
```

不使用公网中转服务端，不依赖系统 `ssh.exe`，也不读取用户已有的 `~/.ssh/id_rsa` 或 `id_ed25519`。

## 结构

| 目录 | 作用 |
|---|---|
| `src/` | Rust 核心：WebView PAT 授权、DPAPI 凭据、CNB API、建仓、工作空间、russh 隧道、状态机 |
| `src-tauri/` | Tauri 2 桌面壳：托盘、单实例、自动启动、命令层 |
| `ui/` | Vue 3 + Vite 界面 |
| `templates/cnb-ai-bridge/` | 自动推送到用户 CNB 仓库的工作空间模板与 Node agent |
| `installer/` | Inno Setup 安装脚本 |
| `tests/` | 跨模块测试 |

## 工作空间 Agent

agent 是零 npm 依赖的 Node 单文件，由 CNB 云原生开发会话启动并常驻，监听 `127.0.0.1:11233`。远端 agent 只保留两个内部接口：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/healthz` | 健康检查 |
| POST | `/chat/completions` | 任意凭据/请求体透传到 CNB AI |

agent 从云会话继承 `CNB_TOKEN`，只监听 loopback。客户端侧传入的 API key 可以是任意值或留空；agent 会覆盖为工作空间 token。

桌面客户端只提供带 `/v1` 前缀的公开接口：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/v1/healthz` | 本机健康检查，不启动工作空间 |
| GET | `/v1/models`、`/v1/models/{id}` | 客户端内置模型目录，包含 `deepseek-v4.1-flash` |
| POST | `/v1/chat/completions` | PromptGate 兼容处理后转发 Chat Completions；支持流式与非流式客户端 |
| POST | `/v1/responses` | Responses 入、Chat 出并回译 |
| OPTIONS | 已知 `/v1` 公开路径 | CORS 预检 |

无 `/v1` 前缀的旧路径直接返回 404，不做重定向。

CNB Chat 上游当前只接受流式请求。桌面网关会在发送前把合法 JSON 请求统一改为 `stream:true`：客户端使用 `stream:true` 时原样回传 SSE；使用 `stream:false` 或省略 `stream` 时，网关读取上游 SSE 并聚合为标准 `chat.completion` JSON。非流式响应会合并多个 choice、文本、`reasoning_content`、工具调用分片和 usage，并保留上游实际模型 ID。

模型目录通过编译内置的 `resources/models.json` 提供。`deepseek-v4.1-flash` 声明文本/图像输入、流式输出、工具调用、reasoning summary 和 1,048,576 token 上下文；推理请求仍允许显式传入其它模型 ID。

## PromptGate 兼容层

客户端的 Chat/Responses 请求在发往 workspace agent 前会解析原始 `User-Agent`，并按产品选择编译内置门禁：

- `harness`：仅当 `User-Agent` 的产品 token 为 `deepseek-harness` 时启用，将 `DeepSeek Harness` 品牌表达改为 `Harness of DeepSeek`，并处理已确认会触发上游 `11128` 的 `@` 路径提示句。产品 token 取首个 `/` 前的内容并精确匹配，ASCII 大小写不敏感。
- `gate/developer-role`：把消息里的 `developer` 角色改写为 `system`。实测携带 `developer` 的 Chat 请求会被上游以 `11128` 拒绝，改成 `system` 后可以通过。

多个匹配 profile 按注册顺序执行；未匹配时不修改品牌或路径文本。`developer` 角色改写保持全局生效。新增品牌门禁时只需注册目标 UA 产品和规则，无需修改 Chat/Responses 调用链。规则幂等，可安全重复执行。

发往上游的 User-Agent 会统一覆盖为 `WorkBuddy/<客户端版本>`，当前版本为 `WorkBuddy/1.0.0`。Authorization、Cookie、会话头和其他客户端头保持不变。

如果上游仍返回 `11128`，客户端不会自动重试，而是返回稳定的 `prompt_gate_blocked` 错误，并在运行日志记录命中的 profile/rule 和上游 request ID。

## HTTP 日志

客户端只为实际获得上游 HTTP 响应的 Chat/Responses 请求写入 `%AppData%\cnb-bridge\logs\http-trace`：

设置页提供“HTTP 日志”策略：默认“仅异常”，成功请求不会写盘，只保留失败、流中断或客户端断开的记录；长流异常最多保留前 4 MiB 和后 4 MiB，并标记为截断。选择“所有”会保留最多 64 MiB 的完整报文。修改后立即对后续请求生效。

- 每个请求生成一对同前缀的 `*.request.http` 和 `*.response.http` 文件，不创建请求目录或 `meta.json`。
- 请求文件以 `POST /chat/completions... HTTP/1.1` 开始，记录实际发往上游 agent 的请求头与正文。
- 响应文件以 `HTTP/1.1 <status> <reason>` 开始，记录上游 agent 返回的响应头与正文。
- 头字段每行一个 `header-name: header-value`，空行后是原始正文。

本地健康检查、模型、CORS、错误路由及尚未发往上游的失败不会生成 HTTP trace。日志包含 Authorization、Cookie、API Key 和用户正文，不会在桌面页面加载全文，请勿直接分享。

单次请求上限为 64 MiB，单个请求/响应日志最多保留前 64 MiB；日志目录保留 7 天、总量上限 1 GiB。

## 构建

前置：

- Windows 10/11 x64
- Rust 1.97.1（`rust-toolchain.toml` 已固定）
- Visual Studio Build Tools（MSVC + CMake；`git2` 需要）
- Node.js 20+
- Microsoft Edge WebView2 Runtime（Tauri 界面）

```powershell
# 依赖
npm --prefix ui install

# Rust 核心与 Tauri 壳
cargo test

# 独立 debug 客户端（先构建最新前端，再编译 Tauri 壳）
pwsh -File scripts/build-debug.ps1

# release 客户端
npm --prefix ui run build
cargo build --release -p cnb-bridge-desktop --features custom-protocol

# 安装包（需要 Inno Setup 6.7.3+）
iscc installer\cnb-bridge.iss
```

`cargo build` 不会执行 `tauri.conf.json` 中的 `beforeBuildCommand`，因此直接编译 Tauri 客户端会内嵌现有的 `ui/dist`。本地调试请使用 `scripts/build-debug.ps1`；它固定先执行前端类型检查和 Vite 构建，再执行 Rust debug 编译，避免运行到旧版界面。

仓库内固定包含安装包所需的 `installer\runtime\vcruntime140.dll`、`installer\languages\ChineseSimplified.isl` 和 `installer\assets\cnb-bridge.ico`。本地与 CI 打包都不依赖系统 Inno Setup 语言目录，安装器、快捷方式和卸载信息统一使用项目图标。新安装默认创建桌面快捷方式。

## GitHub Actions 手动打包

在 GitHub Actions 中打开 `Package Windows Installer` 工作流并点击 `Run workflow`。工作流会执行前端与 Rust 测试、构建 release 客户端、编译简体中文 Inno Setup 安装包，并验证安装目录包含 `cnb-bridge.exe`、`vcruntime140.dll` 和 `app.ico`。

安装和升级前会通过 `--shutdown-for-update` 优雅停止正在运行的客户端，替换完成后保持关闭；完成页可通过默认不勾选的“运行cnb-bridge”立即启动。升级时会为已有开机启动注册项补充 `--autostart` 参数，卸载前也会停止客户端并清理开机启动注册项。安装器不再检测 WebView2，也不会显示语言选择，只使用项目内简体中文。

成功后可下载名为 `cnb-bridge-v<version>-windows-x64-setup` 的 Artifact，其中只包含 `cnb-bridge-v<version>-windows-x64-setup.exe`。

如果 CMake 只随 Visual Studio 安装，构建前把它加入 PATH：

```powershell
$env:PATH = "C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;$env:PATH"
```

## 运行流程

1. 登录页可选择已保存的 CNB 账号，或点击“扫码绑定 CNB”打开 `https://cnb.cool/?s_url=%2Fprofile%2Ftoken`。
2. 微信扫码成功后，客户端上传专用 Ed25519 公钥、创建包含组织管理权限的永久 PAT，并检查 Owner 组织。公钥名称固定为 `cnb-bridge-{XXH3-64(公钥)}`；PAT 名称固定为 `cnb-bridge-{公钥hash XOR AUTH_SCOPES hash}`。两个 hash 都使用无符号十进制表示，应用界面不展示这些内部标识。名称正确且可用的公钥和本机 PAT 会直接复用；同名公钥内容冲突或 PAT 无法在本机复用时，会删除同名资源后重建。`AUTH_SCOPES` 变化会生成新的 PAT 名称。
3. 若账号没有 Owner 组织，客户端使用账号名自动创建根组织，无需再手动启动一次请求来触发建仓。
4. 多账号凭据和账号级仓库设置通过 DPAPI 加密保存；同一 username 再次扫码会更新最近凭据，退出登录只断开当前账号，不删除本机凭据。界面优先显示 CNB 昵称，没有昵称时降级显示账号，并从 `https://cnb.cool/users/{username}/avatar/l` 加载可选头像。同一 CNB 账号可在多台机器登录，每台机器只使用自己的公钥派生名称，其他机器及旧名称的资源不会被主动删除。
5. 客户端启动后只运行本机网关，不会立即启动 CNB 工作空间。
6. 把任意 OpenAI 兼容客户端的 base URL 指向 `http://127.0.0.1:11233/v1`；首个请求会按需创建或连接工作空间。
7. 最后一次请求结束 5 分钟后关闭 SSH 隧道，之后由 CNB 默认空闲规则回收工作空间。
8. 登录 Windows 自动启动时保持隐藏并驻留托盘；手动启动显示主窗口，关闭窗口固定最小化到托盘。托盘“退出客户端”只关闭本地资源，云主机由 CNB 稍后回收。

## 配置

通过环境变量可覆盖 CNB 平台/API 地址，便于企业版或测试：

- `CNB_BRIDGE_PLATFORM_URL`：默认 `https://cnb.cool`
- `CNB_BRIDGE_API_URL`：默认 `https://api.cnb.cool`
- `CNB_BRIDGE_DATA_DIR`：覆盖本地数据目录（测试/便携模式）

## 已知边界

- 需要 WebView2 Runtime 承载 Tauri UI 和 CNB 扫码授权页。
- PAT 与公钥自动化依赖 CNB 网页内部接口，CNB 前端升级后可能需要适配。
- 云原生开发环境有 18 小时会话上限和凌晨 4–6 点强制回收；下一次本地请求会自动重新拉活工作空间。
- 客户端空闲时不轮询 CNB，也不发送 SSH keepalive。
- v1 单工作空间，不做多环境额度优选。

## 测试

```powershell
cargo test
npm --prefix ui test
node --test tests/agent.test.mjs
```
