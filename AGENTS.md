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

## Frozen Plans

- Current authoritative implementation baseline: `docs/v6.2-code-plan.md`.
- `docs/v6.1-code-plan.md` remains a frozen historical plan and must not be edited.
- The v6.2 plan was created from the v6.1 post-release evidence and remains the frozen implementation baseline for the v6.x control-loop work.
- Agents must not edit, split, rename, or replace the v6.2 plan.
- If implementation finds new evidence, write it to `docs/reports/` or an implementation report, then fix code/tests within the frozen plan.
- If new evidence conflicts with the frozen plan, report a blocker instead of rewriting the plan.
- Plan changes are user-owned during the freeze; agents may only modify the plan after the user explicitly lifts the freeze.

## Version Discipline

- Current product version: `6.6.0`.
- Product version is the single public version source. Keep these in lockstep: README/CHANGELOG, Rust crate versions, `pyproject.toml`, MCP `SERVER_VERSION`, Codex plugin manifest, `Cargo.lock`, and the live daemon build version.
- Do not claim a version bump is complete after only editing source files. Rebuild and restart daemon/MCP where applicable, then verify `agentcall_daemon(action=status)` reports the same build version.
- If source version and live daemon version differ, report version drift explicitly and rebuild/restart before continuing live validation.
- Patch releases over the frozen v6.2 baseline may update code/docs/tests, but must not rewrite the frozen plan unless the user explicitly lifts the freeze.
- Use `python agentcall.py runtime-release --version <x.y.z>` for version alignment, build, stale daemon/MCP cleanup, breakaway daemon restart, and live version verification. If it fails, report the script's structured error code instead of replaying the steps manually.

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
agentcall_route(objective=..., workspace=..., write_paths=..., reference_paths=...)
agentcall_session(name=...)
agentcall_session_send(action=<primary_action.kind when applicable>)
agentcall_report(action=request|accept)
```

`agentcall_route` defaults to a daemon-owned PTY worker. Do not ask Codex to choose `runtime`, estimate task size, or hand-build lease/precondition/idempotency fields in the normal flow. `report_path` is optional; the daemon mints a unique report path when it is omitted.

AgentCall has only two normal worker kinds:

- `coding`: pass implementation `write_paths`; the worker receives an exclusive target workspace lease and may write only those paths plus scratch/report.
- `report`: omit implementation `write_paths` or restrict them to report scope; the worker receives a shared report workspace lease and may write only scratch/report artifacts.

Do not pass or reintroduce `read_only`. A worker that should inspect and then write a report is a `report` worker, not a pure read-only worker.

Use `agentcall_daemon(action=status)` as the real availability check. `tool_search agentcall` may be stale or false-negative inside Codex.

## Worker Discipline

- Ask for a report or exact change summary at lifecycle end.
- Do not write outside assigned `write_paths`, scratch, or `report_path`.
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

## Current v6.6 Status

See `docs/v6.2-code-plan.md` for the frozen implementation baseline. Do not edit that plan during implementation.

- v6.2 keeps the default Codex loop slim: compact board, route start, normalized worker summary, explicit next actions, report request, and structured report acceptance.
- v6.3 adds structured safety-lock errors and version alignment over that baseline.
- v6.5 removes the `read_only` route line and leaves only `coding` and `report` worker kinds.
- v6.6 makes daemon safety-lock errors enum-backed, enables SQLite store writer fanout up to the configured six-worker concurrency limit, and lets daemon auto-commit stale prompt-pending PTY handoffs before Codex has to use debug recovery.
- `workspace_busy`, `owner_lease_exists`, `capacity_exceeded`, and control-precondition failures must surface structured error codes and details instead of a bare `400`.
- New safety-lock codes must be added as `ErrorCode` enum variants first; do not introduce ad hoc string-only error codes in daemon live paths.
- SQLite is the recommended RuntimeStore backend for live multi-worker use. It may use `store_writer_threads=6`; JSON remains a single-writer safety fallback even if a larger writer count is configured.
- report workers may share a target workspace lease and write only report/scratch artifacts; coding workers that write implementation paths still require exclusive workspace ownership.
- `submit_pending_prompt` is a finite debug/recovery prompt commit signal, not a normal completion state; daemon should auto-commit stale `prompt_pending_ack` routes and converge to `prompt_submitted` or `prompt_commit_unacknowledged`.
- `agentcall_route` mints a unique per-route report path under the target workspace when `report_path` is omitted.
- `agentcall_session_send(action=request_report)` is a finite report state transition; observe `report_requested`, `report_drafting`, `report_ready`, or `report_overdue`.
- Report acceptance confidence is split into `overall`, `artifact`, `daemon_write`, and `route_match`; `overall=high` requires daemon-observed evidence.
- `agentcall_session` default summary must expose `state`, `why`, `can_wait`, `primary_action`, `available_actions`, `debug_actions`, report info, workspace projection, and a short-lived control token if available.
- Compact board must list current live workers and attention only; historical projections belong in debug/raw views.
- Board/session must distinguish daemon workspace, target workspace, Claude cwd, and report workspace.
- `/api/*` requires daemon token unless `dev_open_loopback=true` is explicitly set in local config.
- `config/agentcall.local.json` is local-only; do not commit daemon tokens.
- Keep `docs/v6.2-code-plan.md` frozen as the authoritative baseline unless the user explicitly lifts the freeze.
