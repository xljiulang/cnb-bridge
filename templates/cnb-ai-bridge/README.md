# CNB Bridge workspace

这个仓库由 CNB Bridge 自动创建并维护，用于在 CNB 云原生开发环境中运行本机 AI 桥接 agent。

- `.cnb.yml`：启动 `agent/supervisor.sh`，不使用 `keepAliveTimeout` 覆盖 CNB 默认回收规则。
- `agent/agent.mjs`：监听 `127.0.0.1:11233`，提供 OpenAI 兼容端点。
- `agent/models.json`：本地模型目录。
- `.cnb-ai-bridge/workspace.json`：产品标记，请勿删除。

请通过 CNB Bridge 桌面客户端管理此仓库；手动修改可能导致客户端拒绝接管。
