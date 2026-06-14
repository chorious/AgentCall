# AgentCall

当前版本 / Current version: `v6.8.0`

AgentCall is a local coordination plane that lets **Codex supervise Claude Code PTY utility workers** through a daemon-backed MCP interface. Codex stays the parent agent: it reads a compact board, starts bounded workers, sends safe commands, waits with patience hints, asks for reports, and accepts or revises deliverables. Claude Code workers do the visible PTY work under hook-aware policy and file ownership.

AgentCall 是一个本地多 Agent 协作控制面：让 **Codex 指挥 Claude Code PTY worker 集群**。Codex 负责拆分、监督、验收和整合；Claude Code worker 负责执行边界明确的实现、审查、证据检查和报告任务。

v6.8.0 延续 v6.2 worker closure 主线，并聚焦 MCP 响应速度和 owner 隔离：compact board 默认按当前 Codex session/thread owner 过滤，`agentcall_session` 默认走 projection-first summary 快路径且不默认铸造 control token，显式 `include=["control"]` 才返回短期控制令牌。v6.8 还补充了 batch-state/latency 分析报告，减少 Codex 为查看多个 worker 状态而串行读取多个 session 的上下文负担。冻结实现基线仍见 [v6.2 code plan](docs/v6.2-code-plan.md)。

## Product Shape / 产品特点

- **Codex parent, Claude workers**: Codex coordinates; Claude Code executes bounded PTY utility work.
- **PTY-first**: ACP is no longer the default path. PTY workers preserve human visibility and handoff.
- **Rust daemon authority**: runtime events, claims, sessions, bindings, routes, summaries, and board projection are daemon-owned.
- **Hook-aware state**: Claude/Codex hooks provide structured liveness, attention, report, permission, and policy signals.
- **Projection-first MCP**: Codex should read compact board/session projections, not raw PTY logs by default.
- **Bounded write policy**: write tools are constrained by route containment; Bash remains readonly-only in the default policy.
- **Two worker kinds**: `coding` workers modify implementation paths under exclusive workspace lease; `report` workers share the workspace and write only report/scratch artifacts.
- **Typed error codes**: safety-lock errors are produced from Rust `ErrorCode` variants and serialized as stable snake_case JSON codes.
- **SQLite writer fanout**: SQLite/WAL store writes can fan out to six daemon writer threads; JSON remains safety-capped to one writer.
- **Report-ready closure**: route can mint a unique report path, `request_report` is a state transition, and daemon-observed report writes update route/session projection to `report_ready`.
- **Prompt commit contract**: daemon auto-commits stale `prompt_pending_ack`; manual `submit_pending_prompt` is only a debug/recovery signal and must converge to `prompt_submitted` or `prompt_commit_unacknowledged`.
- **Readable wrapper**: raw output, clean output, and LLM summary are separate surfaces.
- **Recent-first logs**: large hook payloads are stored as artifacts; hot logs rotate by hook type.
- **Plugin-provided MCP**: the repo ships a Codex plugin so tool metadata and usage guidance travel together.

## Current Architecture / 当前架构

```text
Codex
  -> AgentCall MCP bridge (stdio)
  -> Rust daemon HTTP API
  -> SessionActor / PTY runtime
  -> Claude Code worker in configured claude_workspace
  -> Claude/Codex hooks POST /api/hooks/ingest
  -> projections, board, claims, reports, runtime health
```

Core crates:

- `crates/agentcall-daemon`: daemon, HTTP API, PTY runtime, hooks, routes, projections, ownership, log hygiene.
- `crates/agentcall-mcp`: stable MCP stdio bridge and compact tool protocol.
- `crates/agentcall-hook`: hook helper crate.

Supporting paths:

- `scripts/`: hook installers, diagnostics, release checks, cleanup helpers.
- `plugins/agentcall/`: repo-local Codex plugin and supervisor skill.
- `config/agentcall.example.json`: local daemon config template.
- `docs/`: current and archived design docs.

## Quick Start / 快速开始

Build:

```powershell
cargo build --workspace
```

Create local config:

```powershell
Copy-Item config\agentcall.example.json config\agentcall.local.json
```

Set `claude_workspace`:

```json
{
  "claude_workspace": "D:\\guKimi",
  "store_backend": "sqlite",
  "store_writer_threads": 6,
  "max_sessions": 6,
  "per_owner_max_sessions": 6,
  "experimental_sdk_runtime": false,
  "dev_open_loopback": false,
  "daemon_token": "replace-with-local-token"
}
```

Start daemon:

```powershell
target\debug\agentcall-daemon.exe --workspace E:\Project\AgentCall
```

Open board:

```text
http://127.0.0.1:3293/board
```

## Scripts / 脚本入口

Use the repo entrypoint for routine diagnostics:

```powershell
python agentcall.py doctor
python agentcall.py install-hooks
python agentcall.py daemon-health
python agentcall.py verify-runtime-build
python agentcall.py paths
python agentcall.py logs doctor
python agentcall.py sessions cleanup --stale-after 5m
python agentcall.py release-check
python agentcall.py runtime-release --version 6.8.0
```

The scripts are intentionally loud: missing Cargo, stale hooks, daemon health timeout, plugin validation failure, pytest failure, or whitespace diff errors should point to the failing subsystem.

`runtime-release` is the version/runtime alignment path. It updates version files, builds the workspace, stops stale AgentCall daemon/MCP processes, starts the daemon as a Windows breakaway process, and verifies `/api/runtime/health` reports the requested live version. Use `--dry-run`, `--skip-tests`, or `--no-restart` for narrower maintenance runs.

## Hooks And cwd / Hooks 与 cwd

`claude_workspace` is the authoritative Claude Code runtime directory.

It controls:

- the forced cwd for Claude Code PTY workers;
- where Claude Code reads `.claude/settings.local.json`;
- where AgentCall installs Claude hooks;
- stable hook binding through `AGENTCALL_WRAPPER_SESSION`.

The route `workspace` field is the task target directory. It does **not** decide Claude Code process cwd.

Example:

```json
{
  "claude_workspace": "D:\\guKimi",
  "dev_open_loopback": false,
  "daemon_token": "replace-with-local-token"
}
```

`daemon_token` is local-only and must not be committed. MCP and hook scripts read it from `AGENTCALL_DAEMON_TOKEN` or `config\agentcall.local.json`, then send it as `X-AgentCall-Token`.

AgentCall starts Claude Code in `D:\guKimi` and expects hooks at:

```text
D:\guKimi\.claude\settings.local.json
```

Install or refresh Claude hooks:

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall
```

The installer reads `config\agentcall.local.json` and writes `<claude_workspace>\.claude\settings.local.json`. `--root` is the AgentCall repo root, not the Claude hook settings root.

Explicit settings root:

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall --settings-root D:\guKimi
```

Running Claude PTY workers do not hot-reload hook config. Restart workers after hook changes.

Install or refresh Codex hooks:

```powershell
python scripts\install_codex_hooks.py --root E:\Project\AgentCall
```

## MCP / Plugin

Repo-local Codex plugin:

```text
plugins/agentcall/
  .codex-plugin/plugin.json
  .mcp.json
  skills/agentcall/SKILL.md
```

Install from the local marketplace:

```powershell
codex plugin marketplace add E:\Project\AgentCall
codex plugin add agentcall@personal
```

Fully restart Codex Desktop after plugin or MCP binary changes. A new thread inside an already-running host may keep an old closed MCP transport.

Recommended Codex flow:

```text
agentcall_daemon(action=start)
agentcall_board(view=compact, filter=attention)
agentcall_route(objective=..., workspace=..., write_paths=..., reference_paths=...)
agentcall_session(name=...)
agentcall_session_send(action=<primary_action.kind when applicable>)
agentcall_report(action=request|accept)
```

`agentcall_route` defaults to a daemon-owned Claude Code PTY worker. `runtime`, `mode`, SDK/ACP, estimates, lease ids, preconditions, and idempotency keys are debug/compatibility internals, not the normal Codex loop.

AgentCall has exactly two normal worker kinds:

- `coding`: pass implementation `write_paths`; the worker receives an exclusive target workspace lease and may only write those paths plus scratch/report.
- `report`: omit implementation `write_paths` or point them only at the report scope; the worker receives a shared report lease and may write only scratch/report artifacts.

Do not pass `read_only`; it is no longer a route parameter. A worker that must produce a report is a `report` worker, not a pure read-only worker.

`report_path` is optional. If omitted, the daemon generates a unique path under the route target workspace:

```text
<target_workspace>\.agents\agentcall\<route_id>-<session_name>.md
```

Route/session/report projections distinguish `daemon_workspace`, `target_workspace`, `claude_cwd`, and `report_workspace`. The route `workspace` is the task target; it does not override Claude Code cwd.

`write_paths` define where Write/Edit/MultiEdit may modify files. `reference_paths` are read/context recommendations for the worker, not daemon-enforced read permissions.

If `agentcall_session` returns `state=prompt_pending`, `state=prompt_missing`, or `state=prompt_commit_unacknowledged`, follow `primary_action`. `submit_pending_prompt` is a debug/recovery action, not the default path. A successful call means only that the commit signal was sent; it returns `not_completed=true` and must be followed by `agentcall_session(name=...)` until `UserPromptSubmit`, tool progress, report evidence, or explicit failure appears.

Use `agentcall_session_send(action=request_report)` when the worker should close. It returns `report_requested` with a request id/deadline. Then refresh `agentcall_session` until `report_drafting`, `report_ready`, or `report_overdue`; do not keep sending closure prompts.

`agentcall_report(action=accept, session_id=...)` splits confidence into `overall`, `artifact`, `daemon_write`, and `route_match`. `overall=high` requires daemon-observed report/write evidence; an existing report file without daemon evidence is at most `medium`.

Use `agentcall_daemon(action="status")` as the smoke test. `tool_search agentcall` can return false negatives depending on Codex session state.

## Public API / 常用 API

Static pages such as `/board` can load without a token. `/api/*`, session WebSocket, and mutating endpoints require `X-AgentCall-Token` unless local config explicitly sets `dev_open_loopback=true`.

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

## Tests / 测试

```powershell
cargo test --workspace
python -m pytest -q
python agentcall.py verify-runtime-build
python -m compileall scripts src
python C:\Users\MUSHI\.codex\skills\.system\plugin-creator\scripts\validate_plugin.py E:\Project\AgentCall\plugins\agentcall
```

## Docs / 文档

- [CHANGELOG](CHANGELOG.md)
- [docs/README.md](docs/README.md)
- [Architecture](docs/architecture.md)
- [About AgentCall](docs/about.md)
- [AGENTS.md](AGENTS.md)
- [v5.4 implementation closure](docs/reports/v5.4-implementation-closure.md)
- [v5.4 code plan](docs/v5.4-code-plan.md)
- [v6.0 code plan](docs/v6.0-code-plan.md)
- [v6.1 code plan](docs/v6.1-code-plan.md)
- [MCP transport recovery](docs/mcp-transport-recovery.md)
