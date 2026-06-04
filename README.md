# AgentCall

AgentCall 是一个本地多 Agent 协作控制面。当前主线是 Rust daemon 单写模型：daemon 负责 PTY 会话、hook ingest、file claim、runtime binding、route、summary、board 和 HTTP API；Python 只保留薄脚本、installer、测试辅助与显式 legacy/debug 路线。

当前版本：`v0.8b`

- 版本历史：[CHANGELOG.md](CHANGELOG.md)
- 文档索引：[docs/README.md](docs/README.md)

## 当前原则

- Rust daemon 是 live 状态唯一写者。
- MCP 正常路径是 `agentcall_board -> agentcall_route -> agentcall_session/agentcall_report`。
- `agentcall_route` 是唯一高层调度入口；ACP 和 PTY 是 route 的 runtime 参数。
- ACP 默认控制逻辑在 Rust daemon 内；Python ACP driver 只保留为 reference/legacy/debug。
- MCP stdio 进程保持稳定，只做 bootstrap 与 daemon bridge。
- Claude/Codex hooks 走 daemon-first：POST `/api/hooks/ingest`。
- Codex 默认读 board/session summary，不默认读 raw terminal。
- `D:\guKimi` 只能通过 `AGENTCALL_CLAUDE_WORKSPACE` 等本机配置注入，不作为发布硬编码默认。
- 更新 daemon 后需要重启 daemon、viewer 和旧 Claude PTY；但业务工具面更新不应再要求重启 MCP transport。

## 快速启动

构建 Rust 组件：

```powershell
cargo build -p agentcall-daemon -p agentcall-mcp
```

启动 daemon：

```powershell
$env:AGENTCALL_CLAUDE_WORKSPACE='D:\guKimi'
target\debug\agentcall-daemon.exe --workspace E:\Project\AgentCall
```

打开 board：

```text
http://127.0.0.1:3293/board
```

刷新 hooks 配置：

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall
python scripts\install_codex_hooks.py  --root E:\Project\AgentCall
```

启动 MCP server：

```powershell
target\debug\agentcall-mcp.exe --workspace E:\Project\AgentCall --daemon-url http://127.0.0.1:3293
```

## MCP 工具

本地 MCP 只固定暴露 `agentcall_daemon`。其余业务工具由 daemon 动态提供：

```text
agentcall_board
agentcall_route
agentcall_session
agentcall_session_send
agentcall_report
```

`agentcall_daemon(action=start)` 是 bootstrap 入口：daemon 未运行时由 MCP 拉起 daemon；daemon 已运行时返回 `already_running`。

## Daemon API

常用接口：

```text
GET  /api/runtime/health
GET  /api/mcp/tools
POST /api/mcp/call
GET  /api/board?view=compact&filter=attention
GET  /api/sessions
GET  /api/sessions/{name}/summary
GET  /api/sessions/{name}/output/clean
POST /api/routes
GET  /api/routes/{id}
POST /api/sessions
POST /api/sessions/{name}/input
POST /api/sessions/{name}/checkpoint
POST /api/sessions/{name}/stop
POST /api/context
POST /api/transcripts/index
POST /api/hooks/ingest
```

ACP route 需要显式传入 `adapter_command` 或设置 `AGENTCALL_ACP_COMMAND`，例如：

```json
{
  "objective": "bounded review",
  "mode": "start",
  "runtime": "acp",
  "adapter_command": ["npx", "-y", "@agentclientprotocol/claude-agent-acp"],
  "timeout_seconds": 120
}
```

## 测试

```powershell
cargo test --workspace
python -m pytest -q
python -m compileall scripts src
```
