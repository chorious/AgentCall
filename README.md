# AgentCall

AgentCall is an orchestration layer for supervising coding agents through bounded child lifecycles, structured reports, feedback gates, hooks, and process-aware delegation.

第一版证明了 SOP：多个 agent 能在同一个 workspace 下通过标准任务、报告、审查文件协作。现在的主线是 v2：主程拥有项目上下文、阶段组织和验收；子 Agent 通过 ACP/SDK 或 PTY handoff 执行一段有边界的生命周期；完成后必须交付结构化 report；没有问题就直接接受，不机械写 review。

## v0.5.1 Codex Hooks 与架构收敛

v0.5.1 把 Codex 自己也接入 AgentCall 的 hook/preflight 体系：Codex hooks 负责在回合开始、用户输入、停止和 compact 前后记录状态并注入提醒；Rust MCP 提供 `agentcall_codex_preflight`，返回主程下一步应该检查的 board、route、claims、reports。

架构收敛为：

- `web/`：CSS/JS/HTML 前端与状态看板。
- `src/agentcall/`：Python 胶水层，保留 CLI、ACP adapter、workflow 测试入口。
- `crates/`：Rust 后端层，包含 MCP server、hook receiver、PTY daemon。

ACP 与 PTY 共享同一生命周期，只有 route/runtime adapter 不同。详见 `docs/v0.5.1-architecture.md`。

## v0.5 Handoff 可观察性

v0.5 增加 Claude Code hooks、file claim 冲突保护、transcript 索引、增强 route，以及可视化 board。

- `docs/v0.5-implementation.md`

## v0.4 编排控制面

v0.4 建立了 AgentCall 的控制面：MCP 暴露能力、route 区分 ACP agents-as-tools 与 Claude Code handoff、context packet 约束子任务输入。

- `docs/v0.4-orchestration-roadmap.md`
- `docs/v0.4-implementation.md`

## v2 Direction

- Parent owns context, orchestration, validation, and task state.
- Child agents are bounded lifecycle calls, not long-running project owners.
- Every child call must return a structured report.
- Reviews are delegated only when parent validation finds risk, drift, blockers, or low confidence.
- SOP behavior moves into code: report validation, scope checks, lifecycle limits, route policy, hooks, and file claims.
- ACP/SDK is the flagship child-call path.
- PTY/tmux is retained as a visible handoff/debug route and archived v1.0 prototype.

## Quick Start

```powershell
$env:PYTHONPATH='src'
python -m agentcall init
python -m agentcall task create "Build a minimal game loop"
python -m agentcall task status task-0001
```

Run the current bounded workflow simulation:

```powershell
$env:PYTHONPATH='src'
python -m agentcall --root .agentcall-demo workflow simulate --driver scripted
python -m agentcall --root .agentcall-demo workflow inspect task-0001
```

The default live child route is Claude ACP over stdio JSON-RPC. A deterministic `scripted` driver remains available for free smoke tests.

## Rust Backends

Build all Rust backends:

```powershell
cargo build
```

MCP server:

```powershell
target\debug\agentcall-mcp.exe --workspace E:\Project\AgentCall
```

Hook receiver:

```powershell
target\debug\agentcall-hook.exe --root E:\Project\AgentCall --event UserPromptSubmit --runtime codex
```

PTY daemon and board:

```powershell
target\debug\agentcall-daemon.exe --port 3293 --workspace .
```

Then open:

```text
http://localhost:3293/board
```

## Hooks

Install Codex hooks:

```powershell
cargo build -p agentcall-hook
.\scripts\install-codex-hooks.ps1 -Root E:\Project\AgentCall
```

Install Claude Code hooks:

```powershell
cargo build -p agentcall-hook
.\scripts\install-claude-hooks.ps1 -Root E:\Project\AgentCall
```

Claude Code hooks include `PreToolUse/PostToolUse` file claim protection. Codex hooks currently focus on state recording and context/preflight reminders.

## MCP Surface

AgentCall exposes a Rust MCP server so Codex or other parent processes can discover and call orchestration tools:

```text
agentcall_capabilities
agentcall_report_schema
agentcall_codex_preflight
agentcall_route_task
agentcall_context_packet_create
agentcall_workflow_simulate
agentcall_workflow_inspect
agentcall_board
agentcall_file_claims
agentcall_reports_list
agentcall_events_tail
agentcall_hook_ingest
agentcall_session_*
```

See `docs/v3.0-mcp.md`.

## Directory Layout

```text
.agentcall/
  events.ndjson
  state/
    active_sessions.json
    file_claims.json
    transcripts.json
  tasks/
    task-0001/
      task.json
      task.md
      reports/
      calls/
      review.md        # only when feedback is needed
```

## Tests

```powershell
python -m pytest -q
cargo test -p agentcall-mcp
cargo test -p agentcall-hook
cargo test -p agentcall-daemon
```

pytest temp output is configured under `.agentcall_pytest/` so root does not get flooded by `.pytest_tmp*` directories.
