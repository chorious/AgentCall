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

- Current product version: `6.9.2`.
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

`agentcall_route` defaults to a daemon-owned PTY worker. Do not ask Codex to choose `runtime`, estimate task size, or hand-build lease/precondition/idempotency fields in the normal flow. `report_path` is optional; the daemon mints a unique report path when it is omitted. Route containment exposes `workspace_contract.v1`; prefer that contract for task root, artifact root, writable/readonly roots, and Bash side-effect mode instead of exposing daemon cwd internals.

AgentCall has only two normal worker kinds:

- `coding`: pass implementation `write_paths`; the worker receives an exclusive target workspace lease, must target a dedicated git worktree branch for real Claude coding routes, and may write only those paths plus scratch/report.
- `report`: omit implementation `write_paths` or restrict them to report scope; the worker receives a shared report workspace lease and may create/edit only its own report/scratch artifacts, not pre-existing project files.

Do not pass or reintroduce `read_only`. A worker that should inspect and then write a report is a `report` worker, not a pure read-only worker.

Use `agentcall_daemon(action=status)` as the real availability check. `tool_search agentcall` may be stale or false-negative inside Codex.

## Worker Discipline

- Ask for a report or exact change summary at lifecycle end.
- Do not write outside the assigned workspace contract. For report/review workers, do not modify pre-existing project files; create the requested report/scratch artifacts and then edit only files claimed by the same session.
- Do not use raw PTY output as the primary status source when projection is available.
- Review only when there is drift, blocker, failed validation, low confidence, or requested review.
- Do not mechanically review a clean report.
- If a worker repeats a denied action or folder heartbeat audit reports an unapproved changed folder, treat it as `blocked_by_policy`; do not keep waiting.
- If `policy_block.category=workspace_audit_changed_dir`, Codex may approve an expected folder with `agentcall_session_send(action=approve_changed_dir, dir=..., reason=...)`; approvals are session-scoped and must not be treated as global write permission.
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

## Current v6.9 Status

See `docs/v6.2-code-plan.md` for the frozen implementation baseline. Do not edit that plan during implementation.

- v6.2 keeps the default Codex loop slim: compact board, route start, normalized worker summary, explicit next actions, report request, and structured report acceptance.
- v6.3 adds structured safety-lock errors and version alignment over that baseline.
- v6.5 removes the `read_only` route line and leaves only `coding` and `report` worker kinds.
- v6.6 makes daemon safety-lock errors enum-backed, introduced SQLite store writer fanout, and lets daemon auto-commit stale prompt-pending PTY handoffs before Codex has to use debug recovery. The fanout line is superseded by v6.8.3's SQLite single-writer policy.
- v6.7 hardens the control plane internals; v6.7.1 fixes bounded daemon start waiting, SQLite sequence recovery, and full board store-backed event reads; v6.7.2 scopes worker capacity to the current Codex session/thread owner instead of enforcing a daemon-global six-worker cap.
- v6.8 keeps the owner-scoped model and reduces MCP observation cost: compact board is owner-filtered by default, session summary uses a projection-first snapshot path, and control tokens are minted only for explicit `include=["control"]` reads.
- v6.8.1 closes the remaining global visibility leak: ordinary MCP board/status calls are owner-safe, `scope=all` is ignored for non-debug compact board views, and fallback MCP owner ids are per MCP process instead of global `codex`.
- v6.8.2 closes the accepted-report cleanup gap: destructive control tokens last 5 minutes, live accepted workers project as `accepted_live`, and daemon auto-closes accepted live PTY workers after a 5 minute grace period if Codex does not stop them first.
- v6.8.3 closes the SQLite writer-contention regression: SQLite uses a single daemon store writer with WAL/`synchronous=NORMAL`, compact board stale-runtime cleanup is throttled, and `store_writer_threads>1` is ignored for SQLite to keep board observation responsive during hook-heavy worker bursts.
- v6.9 replaces Bash readonly-only preemption with monitored execution: PTY route startup records a lightweight folder-audit baseline, hook turns update changed-folder heartbeats, and daemon policy-blocks only when target-workspace folders change outside scratch/report/write boundaries.
- v6.9 adds `approve_changed_dir` as a session-scoped Codex judgment path for expected folder changes and exposes blocked folder details in `policy_block.path_diagnosis`.
- v6.9 makes `accepted_live` cleanup easier: default session summary includes a fresh stop control token when owner-bound Codex inspects an accepted live worker, and the primary stop action carries that token in `args.control_token`.
- v6.9.1 keeps MCP bridge tool metadata aligned with daemon metadata: the bridge prefers live `/api/mcp/tools`, static fallback exposes `approve_changed_dir`, and release-check compares daemon/bridge schemas for canonical tools.
- v6.9.1 injects `agentcall-version.json` during `runtime-release`; MCP hot-reads it and rejects daemons whose `/api/runtime/health` version or binary path does not match the manifest and compiled MCP `SERVER_VERSION`.
- v6.9.1 makes compact board a cold store-projection read for all compact filters; it must not sweep live PTYs, run stale-runtime cleanup, or acquire the daemon state-writer lock on the board read path.
- v6.9.2 keeps compact board cold but gates projection rows through the daemon current live-session index. Historical `needs_attention` projections belong only in debug/raw views and must not inflate ordinary compact board payloads.
- v6.9.2 introduces `workspace_contract.v1` in PTY containment so Codex/UI can reason in task-root/artifact-root/writable/readonly/Bash-effect terms while daemon cwd, Claude cwd, route, prompt gate, hooks, claims, and audit remain implementation/debug details.
- v6.9.2 makes report/review policy created-artifact based: report/review workers may create the requested report or scratch artifacts and keep editing files claimed by the same session; they must not edit pre-existing project files or use non-readonly Bash.
- v6.9.2 requires real Claude coding routes to target a linked git worktree on a non-main branch and projects `coding_requires_worktree`, `worktree_path`, `branch`, `merge_requires_pr`, and `pr_report_path` for Codex.
- v6.9.2 brings `agentcall-flow` under repository management and makes `runtime-release` require an explicit skill update decision through prompt, `--update-skills`, or `--skip-skill-update`.
- `workspace_busy`, `owner_lease_exists`, `capacity_exceeded`, and control-precondition failures must surface structured error codes and details instead of a bare `400`.
- New safety-lock codes must be added as `ErrorCode` enum variants first; do not introduce ad hoc string-only error codes in daemon live paths.
- SQLite is the recommended RuntimeStore backend for live multi-worker use. It intentionally uses one daemon store writer; `store_writer_threads>1` is ignored for SQLite to avoid busy writer contention. JSON remains a single-writer safety fallback even if a larger writer count is configured.
- Worker start capacity is owner-scoped: MCP derives owner identity from `AGENTCALL_OWNER_ID` or `CODEX_THREAD_ID`, and `per_owner_max_sessions=6` is the enforced quota. `max_sessions` is advisory health data, not a global hard cap across unrelated Codex sessions.
- report workers may share a target workspace lease and write only report/scratch artifacts; coding workers that write implementation paths still require exclusive workspace ownership.
- `submit_pending_prompt` is a finite debug/recovery prompt commit signal, not a normal completion state; daemon should auto-commit stale `prompt_pending_ack` routes and converge to `prompt_submitted` or `prompt_commit_unacknowledged`.
- `agentcall_route` mints a unique per-route report path under the target workspace when `report_path` is omitted.
- `agentcall_session_send(action=request_report)` is a finite report state transition; observe `report_requested`, `report_drafting`, `report_ready`, or `report_overdue`.
- Report acceptance confidence is split into `overall`, `artifact`, `daemon_write`, and `route_match`; `overall=high` requires daemon-observed evidence.
- `agentcall_session` default summary must expose `state`, `why`, `can_wait`, `primary_action`, `available_actions`, `debug_actions`, report info, workspace projection, active policy block details, and a short-lived control token if available.
- Coding/edit Bash is `monitored`, not a filesystem sandbox. Report/review Bash is readonly in pre-policy. Do not claim monitored Bash proves command safety; it observes folder-level side effects and blocks on unapproved changed target folders.
- Compact board must list current live workers and attention only; historical projections belong in debug/raw views.
- Board/session must distinguish daemon workspace, target workspace, Claude cwd, and report workspace.
- `/api/*` requires daemon token unless `dev_open_loopback=true` is explicitly set in local config.
- `config/agentcall.local.json` is local-only; do not commit daemon tokens.
- Keep `docs/v6.2-code-plan.md` frozen as the authoritative baseline unless the user explicitly lifts the freeze.
