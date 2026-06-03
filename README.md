# AgentCall

AgentCall 是一个面向复杂工程的多 Agent 协作底座。它把 Claude Code / Codex / 未来 ACP 或 SDK 子 Agent 的工作统一收敛到同一个 daemon、同一套事件与状态 API，并让主程通过结构化 summary、report 和 file claim 来组织协作，而不是反复读取原始终端画面。

当前主线版本：`v0.7.1 Hook-Aware Summary Binding`

版本演进请看 [CHANGELOG.md](CHANGELOG.md)；完整文档索引请看 [docs/README.md](docs/README.md)。README 只记录当前可用入口和环境刷新方式。

## 当前原则

- Rust daemon 是 live 状态的唯一写者：`events`、`file_claims`、`active_sessions`、`session_summary`、`runtime_binding`。
- Python 只保留 CLI 胶水、测试辅助和 legacy/manual 路径，不作为 live state writer。
- Codex 默认读取 `agentcall_board` / `agentcall_session` 的结构化 summary，不默认读 raw terminal。
- Claude Code hooks daemon-first：hook 脚本 POST `/api/hooks/ingest`，daemon 不可达时 fail-open。
- PTY wrapper 负责人类可视化和 handoff；hook/report/validator 才是状态权威。
- `D:\guKimi` 只通过 `AGENTCALL_CLAUDE_WORKSPACE` 注入，不作为发布硬编码默认。

## 快速启动

构建 Rust 后端：

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

刷新 hook 配置：

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall
python scripts\install_codex_hooks.py  --root E:\Project\AgentCall
```

Claude Code hooks 默认写入项目本地 `.claude/settings.local.json`；Codex hooks 写入项目本地 `.codex/hooks.json`。更新 daemon、MCP 或 hook 脚本后，需要重启 daemon、viewer 和旧 Claude PTY 会话；旧 PID 不保证吃到新配置。

## MCP 默认工具流

AgentCall 暴露 Rust MCP server，供 Codex 或其他主进程低负担调用：

```text
agentcall_board(view=compact, filter=attention)
agentcall_delegate
agentcall_session
agentcall_session_send
agentcall_report
agentcall_runtime_health
```

默认用法是先看 board，再进入具体 session：

```powershell
target\debug\agentcall-mcp.exe --workspace E:\Project\AgentCall --daemon-url http://127.0.0.1:3293
```

## Daemon API

常用接口：

```text
GET  /api/runtime/health
GET  /api/board?view=compact&filter=attention
GET  /api/sessions
GET  /api/sessions/{name}/summary
GET  /api/sessions/{name}/output/clean
POST /api/sessions
POST /api/sessions/{name}/input
POST /api/hooks/ingest
```

`session_summary` 当前包含：

```text
liveness_status
attention_status
report_ready
report_source
status_source
binding_source
hook_session_id
last_hook_status
decode_health
confidence
```

## 目录结构

```text
.agentcall/
  events.ndjson
  state/
    active_sessions.json
    file_claims.json
    runtime_binding.json
    transcripts.json
  sessions/                 # legacy detached Python PTY，仅 debug/manual
  tasks/
    task-0001/
      task.json
      task.md
      reports/
      calls/
      review.md             # 只有需要反馈时才写 review

crates/
  agentcall-daemon/          # PTY daemon、HTTP API、hook ingest、summary
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

`pytest` 临时输出固定在 `.agentcall_pytest/`，避免污染项目根目录。
