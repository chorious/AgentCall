# AgentCall

AgentCall 是一个面向复杂工程协作的本地 Agent 编排底座，特别适配 Codex 调用 Claude Code。当前主线是 Rust daemon 单写模型：daemon 负责 PTY 会话、hook ingest、file claim、runtime binding、route、summary、board 和 HTTP API；Python 只保留薄脚本、installer、测试辅助和显式 legacy/debug 路径。

当前版本：`v0.8a`

版本历史见 [CHANGELOG.md](CHANGELOG.md)，文档索引见 [docs/README.md](docs/README.md)。

## 当前原则

- Rust daemon 是 live 状态唯一写者：`events`、`file_claims`、`active_sessions`、`runtime_binding`、`routes` 和 `session_summary`。
- MCP 正常路径简并为 `agentcall_board -> agentcall_route -> agentcall_session/agentcall_report`。
- `agentcall_route` 是唯一高层调度入口；ACP 和 PTY 是 route 的 runtime 参数。
- Claude/Codex hooks 走 daemon-first：POST `/api/hooks/ingest`。
- Codex 默认读 board/session summary，不默认读 raw terminal。
- PTY wrapper 用于人类可视化、handoff 和 debug；状态判断优先 hook/report/daemon 结构化事件。
- `D:\guKimi` 只能通过 `AGENTCALL_CLAUDE_WORKSPACE` 注入，不作为发布硬编码默认。
- 更新后必须重启 daemon、MCP/viewer 和旧 Claude PTY；旧 PID 不假定能吃到新配置。

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

## MCP 默认工具

`tools/list` 默认只暴露正常控制路径：

```text
agentcall_daemon
agentcall_board
agentcall_route
agentcall_session
agentcall_session_send
agentcall_report
```

`agentcall_daemon(action=start)` 是 bootstrap 入口：daemon 未运行时由 MCP 拉起 daemon；已运行时返回 `already_running`。它不负责自动 kill/restart。

旧 `agentcall_delegate` / `agentcall_delegate_acp` 已从默认工具面移除；隐藏兼容 handler 只返回 deprecated 提示，不再执行 Python workflow。

## Daemon API

常用接口：

```text
GET  /api/runtime/health
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

`agentcall_route` 支持：

```text
mode: recommend | start
runtime: auto | pty | acp
```

`runtime=auto` 必须提供估计字段：`estimated_minutes`，以及 `estimated_files` 或 `estimated_loc`。

## 项目结构

```text
.agentcall/
  events.ndjson
  state/
    active_sessions.json
    file_claims.json
    runtime_binding.json
    routes.json
    transcripts.json
  sessions/                 # legacy detached Python PTY，仅 debug/manual
  tasks/
    task-0001/
      calls/
      reports/
      review.md             # 只有 drift/blocker/revision 时需要

crates/
  agentcall-daemon/          # Rust daemon、HTTP API、PTY、hooks、route、summary
  agentcall-mcp/             # Rust MCP server
  agentcall-hook/            # legacy standalone hook receiver

scripts/
  agentcall-claude-hook.py
  agentcall-codex-hook.py
  install_claude_hooks.py
  install_codex_hooks.py
```

## 测试

```powershell
cargo test --workspace
python -m pytest -q
python -m compileall scripts src
```
