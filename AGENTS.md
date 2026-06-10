# AGENTS.md

This file is the working guide for Codex, Claude Code workers, and any other agent touching this repository.

## Project Mission

AgentCall lets Codex supervise Claude Code PTY utility workers through a local Rust daemon and MCP bridge. The product goal is not generic background automation; it is reliable multi-agent engineering coordination with compact state, bounded writes, hook-aware status, and explicit report/review closure.

## Current Mainline

- Runtime: PTY-first Claude Code utility workers.
- Parent: Codex coordinates, verifies, accepts reports, and integrates changes.
- Worker: Claude Code does bounded implementation/review/report work inside daemon-owned PTY sessions.
- State authority: Rust daemon.
- Default state source for Codex: compact board/session projection, not raw terminal output.
- Historical ACP/SDK plans are archived; do not revive them unless the user explicitly asks.

## Important Paths

- `crates/agentcall-daemon/`: daemon, HTTP API, PTY runtime, hooks, routes, projections.
- `crates/agentcall-mcp/`: MCP stdio bridge and tool schemas.
- `crates/agentcall-hook/`: hook helper binary.
- `scripts/`: hook installers, diagnostics, cleanup, release checks.
- `plugins/agentcall/`: Codex plugin and supervisor skill.
- `config/agentcall.example.json`: committed config template.
- `config/agentcall.local.json`: local-only config, not for commit.
- `docs/`: current docs, plans, reports, and archives.
- `.agentcall/`: runtime state/logs/artifacts, not for commit.
- `target/`, `node_modules/`, `.agentcall_*`: build/runtime artifacts, not for commit.

## Local Config Rule

Claude Code worker cwd is controlled by daemon config:

```json
{
  "claude_workspace": "D:\\guKimi"
}
```

Route `workspace` is the task target. It does not override Claude Code process cwd.

Claude hooks must be installed into:

```text
<claude_workspace>\.claude\settings.local.json
```

Use:

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall
python scripts\install_codex_hooks.py --root E:\Project\AgentCall
```

Restart Claude PTY workers after hook changes.

## AgentCall MCP Usage

Preferred flow:

```text
agentcall_daemon(action=start)
agentcall_board(view=compact, filter=attention)
agentcall_route(mode=start, runtime=auto|pty, objective=..., workspace=...)
agentcall_session(name=..., include=["summary"])
agentcall_session_send(action=continue|request_report|select_option|interrupt|stop)
agentcall_report(action=request|accept)
```

Use `agentcall_daemon(action=status)` as the real availability check. `tool_search agentcall` may be stale or false-negative inside Codex.

## Worker Discipline

- Ask for a report or exact change summary at lifecycle end.
- Do not write outside assigned `allowed_paths`, scratch, or `report_path`.
- Do not use raw PTY output as the primary status source when projection is available.
- Review only when there is drift, blocker, failed validation, low confidence, or requested review.
- Do not mechanically review a clean report.
- If a worker repeats a denied action, treat it as `blocked_by_policy`; do not keep waiting.
- Use `interrupt` only when the worker is drifting, doing the wrong thing, or must be reclaimed immediately.

## Coding Rules

- Prefer Rust daemon changes for live control/state behavior.
- Python can remain for thin scripts, hook installers, diagnostics, tests, and explicit legacy/debug tools.
- Do not add Python live writers for events, claims, routes, sessions, projections, or bindings.
- Keep MCP canonical tool surface small; do not add a tool when an existing board/route/session/report shape can carry the behavior.
- Avoid giant `main.rs` files; split by function area.
- Keep docs honest: if a hard gate is open, mark it open.

## Validation

Run before committing meaningful changes:

```powershell
cargo test --workspace
python -m pytest -q
python agentcall.py release-check
```

When only docs change, at least run:

```powershell
git diff --check
```

## Git Hygiene

- Do not commit `.agentcall/`, local config, target directories, runtime logs, or local router/fork files.
- Put current reports under `docs/reports/`.
- Put old plans under `docs/arch/plan/`.
- Put old reviews under `docs/arch/review/`.
- Root directory should stay focused on source entrypoints, README, CHANGELOG, config template, and build manifests.

## Current v5.3 Open Gates

See `docs/reports/v5.3-closure-status.md`. As of this checkpoint, these remain open:

- actor panic guard;
- control/output channel isolation;
- stop/interrupt priority queues;
- graceful stop vs hard kill split;
- orphan detection after daemon restart;
- report accept releasing worker leases.
