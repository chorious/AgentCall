# AgentCall

当前版本 / Current version: `v5.3.0 checkpoint`

AgentCall is a local coordination plane that lets **Codex supervise Claude Code PTY utility workers** through a daemon-backed MCP interface. Codex stays the parent agent: it reads a compact board, starts bounded workers, sends safe commands, waits with patience hints, asks for reports, and accepts or revises deliverables. Claude Code workers do the visible PTY work under hook-aware policy and file ownership.

AgentCall 是一个本地多 Agent 协作控制面：让 **Codex 指挥 Claude Code PTY worker 集群**。Codex 负责拆分、监督、验收和整合；Claude Code worker 负责执行边界明确的实现、审查、证据检查和报告任务。

v5.3 是 hard-gate closure checkpoint：已经闭合 report-ready projection、stale command precondition、read-only worker drift、writer/reader failure projection 等真实运行暴露的问题；仍保留 actor control/output 隔离、stop/kill 语义拆分等 open gates，详见 [v5.3 closure status](docs/reports/v5.3-closure-status.md)。

## Product Shape / 产品特点

- **Codex parent, Claude workers**: Codex coordinates; Claude Code executes bounded PTY utility work.
- **PTY-first**: ACP is no longer the default path. PTY workers preserve human visibility and handoff.
- **Rust daemon authority**: runtime events, claims, sessions, bindings, routes, summaries, and board projection are daemon-owned.
- **Hook-aware state**: Claude/Codex hooks provide structured liveness, attention, report, permission, and policy signals.
- **Projection-first MCP**: Codex should read compact board/session projections, not raw PTY logs by default.
- **Bounded write policy**: write tools are constrained by route containment; Bash remains readonly-only in the default policy.
- **Report-ready closure**: writing the route `report_path` updates route/session projection to `report_ready`.
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
  "store_backend": "json",
  "max_sessions": 6,
  "per_owner_max_sessions": 3,
  "experimental_sdk_runtime": false
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
python agentcall.py paths
python agentcall.py logs doctor
python agentcall.py sessions cleanup --stale-after 5m
python agentcall.py release-check
```

The scripts are intentionally loud: missing Cargo, stale hooks, daemon health timeout, plugin validation failure, pytest failure, or whitespace diff errors should point to the failing subsystem.

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
  "claude_workspace": "D:\\guKimi"
}
```

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
agentcall_route(mode=start, runtime=auto|pty, objective=..., workspace=...)
agentcall_session(name=..., include=["summary"])
agentcall_session_send(action=continue|request_report|select_option|interrupt|stop)
agentcall_report(action=request|accept)
```

Use `agentcall_daemon(action="status")` as the smoke test. `tool_search agentcall` can return false negatives depending on Codex session state.

## Public API / 常用 API

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
python -m compileall scripts src
python C:\Users\MUSHI\.codex\skills\.system\plugin-creator\scripts\validate_plugin.py E:\Project\AgentCall\plugins\agentcall
```

## Docs / 文档

- [CHANGELOG](CHANGELOG.md)
- [docs/README.md](docs/README.md)
- [Architecture](docs/architecture.md)
- [About AgentCall](docs/about.md)
- [AGENTS.md](AGENTS.md)
- [v5.3 closure status](docs/reports/v5.3-closure-status.md)
- [v5.3 code plan](docs/v5.3-code-plan.md)
- [MCP transport recovery](docs/mcp-transport-recovery.md)
