# AgentCall

AgentCall 是一个面向复杂工程协作的本地 Agent 编排底座[特别适配Codex调用Claude Code]。当前主线是 Rust daemon 单写模型：daemon 负责 PTY 会话、hook ingest、file claim、runtime binding、summary 和 HTTP API；Python 只保留 CLI 胶水、测试辅助和 legacy/manual 路径。

当前版本：`v0.7.1`

版本历史见 [CHANGELOG.md](CHANGELOG.md)，文档索引见 [docs/README.md](docs/README.md)。

## 当前原则

- Rust daemon 是 live 状态唯一写者：`events`、`file_claims`、`active_sessions`、`session_summary`、`runtime_binding`。
- Claude/Codex hooks 走 daemon-first，POST `/api/hooks/ingest`；daemon 不可达时 fail-open。
- Codex 默认读 `agentcall_board` / `agentcall_session` 的结构化 summary，不默认读大段 raw terminal。
- PTY wrapper 用于人类可视化、handoff 和 debug；状态判断优先 hook/report/validator，TUI 只做弱信号。
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

Claude Code hooks 默认写入项目本地 `.claude/settings.local.json`。Codex hooks 写入项目本地 `.codex/hooks.json`。

## MCP 默认工具流

推荐 Codex 默认使用：

```text
agentcall_board(view=compact, filter=attention)
agentcall_delegate
agentcall_session
agentcall_session_send
agentcall_report
agentcall_runtime_health
```

手动运行 MCP server：

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
POST /api/sessions/{name}/stop
POST /api/hooks/ingest
```

`session_summary` 关键字段：

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

## v0.7.1 重点修复

- Hook-aware summary binding：wrapper session 与 Claude hook session 通过 `AGENTCALL_WRAPPER_SESSION` 可靠绑定。
- Windows ConPTY DSR 修复：daemon PTY 会回答 `ESC[6n`，避免 headless PTY 启动卡在 4 bytes。
- Stop 修复：使用 `clone_killer()`，stop 不再被 waiter 持有 child 锁阻塞。
- Clean output 可读性改善：保留光标定位类 ANSI 的换行语义，降低 TUI 文本黏连。
- Compact attention 降噪：legacy Python PTY 不再默认污染 attention 列表。
- Hook UTF-8 修复：Claude/Codex hook 使用 `utf-8-sig` stdin 和 UTF-8 stdout/stderr。

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
      review.md             # 只有 drift/blocker/revision 时需要

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

`pytest` 使用 `.agentcall_pytest/` 作为临时目录，避免污染项目主状态。
