# CNB Bridge

CNB Bridge 是一个 Windows 本机 AI 桥接客户端：通过 CNB WebView 扫码绑定，自动创建个人访问令牌并配置 SSH 公钥，在当前用户命名空间下创建/复用 `cnb-ai-bridge` 仓库，启动 CNB 云原生开发工作空间，经纯 Rust `russh` 建立 SSH 本地端口转发，把工作空间内的 OpenAI 兼容 agent 暴露到：

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

agent 是零 npm 依赖的 Node 单文件，由 CNB 云原生开发会话启动并常驻，监听 `127.0.0.1:11233`：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/healthz` | 健康检查 |
| GET | `/models`、`/v1/models` | 本地模型列表 |
| GET | `/models/{id}`、`/v1/models/{id}` | 模型详情 |
| POST | `/chat/completions`、`/v1/chat/completions` | 任意凭据/请求体透传到 CNB AI |
| OPTIONS | 上述路径 | CORS 预检 |

agent 从云会话继承 `CNB_TOKEN`，只监听 loopback。客户端侧传入的 API key 可以是任意值或留空；agent 会覆盖为工作空间 token。

## 构建

前置：

- Windows 10/11 x64
- Rust 1.97.1（`rust-toolchain.toml` 已固定）
- Visual Studio Build Tools（MSVC + CMake；`git2` 需要）
- Node.js 20+
- Microsoft Edge WebView2 Runtime（Tauri 界面）

```powershell
# 前端
npm --prefix ui install
npm --prefix ui run build

# Rust 核心与 Tauri 壳
cargo test
cargo build --release -p cnb-bridge-desktop --features custom-protocol

# 安装包（需要 Inno Setup 6）
iscc installer\cnb-bridge.iss
```

如果 CMake 只随 Visual Studio 安装，构建前把它加入 PATH：

```powershell
$env:PATH = "C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;$env:PATH"
```

## 运行流程

1. 首次启动点击“扫码绑定 CNB”，客户端 WebView 打开 `https://cnb.cool/?s_url=%2Fprofile%2Ftoken`。
2. 微信扫码成功后，客户端自动获取个人命名空间、上传专用 Ed25519 公钥并创建永久 PAT。
3. PAT 通过 DPAPI 加密保存，扫码会话随后清理，后续启动无需再次扫码。
4. 客户端启动后只运行本机网关，不会立即启动 CNB 工作空间。
5. 把任意 OpenAI 兼容客户端的 base URL 指向 `http://127.0.0.1:11233/v1`；首个请求会按需创建或连接工作空间。
6. 最后一次请求结束 60 秒后关闭 SSH 隧道，之后由 CNB 默认空闲规则回收工作空间。
7. 关闭窗口最小化到托盘；托盘“退出客户端”只关闭本地资源，云主机由 CNB 稍后回收。

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
