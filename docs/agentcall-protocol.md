# AgentCall Codex Supervisor Protocol

This document is the source text for the generated Codex skill at `.codex/skills/agentcall-supervisor/SKILL.md`.

## Purpose

AgentCall lets Codex supervise Claude Code PTY workers through a small MCP control plane. Codex should read compact projections first, route work through PTY utility workers, and only expand raw/clean terminal output when projection confidence or attention state requires it.

## Canonical Tools

Use only these default AgentCall MCP tools:

```text
agentcall_board
agentcall_route
agentcall_session
agentcall_session_send
agentcall_report
```

Do not call deprecated delegate/workflow tools. Do not use raw terminal output as the default state source.

## Default Workflow

```text
1. Inspect board: agentcall_board(view=compact, filter=attention).
2. Route work: agentcall_route(objective, workspace, write_paths/reference_paths).
3. Inspect worker: agentcall_session(name).
4. Follow `primary_action` for the normal path.
5. Use request_report only when the worker should close.
6. Accept report only after reading report confidence and evidence.
```

`agentcall_route` defaults to a daemon-owned Claude Code PTY worker. Runtime selection, task-size estimation, SDK/ACP knobs, lease ids, preconditions, and idempotency are daemon/debug internals, not the normal Codex loop. `report_path` is optional in normal use; if omitted, the daemon mints a unique per-route report path under the target workspace.

Board and session projections are project-aware. Read `workspace.daemon_workspace`, `workspace.target_workspace`, `workspace.claude_cwd`, and `report.path` before judging where a worker is operating. `workspace` on `agentcall_route` is the target project; Claude Code process cwd still comes from daemon local config.

## Action Matrix

| Worker state | Codex action |
| --- | --- |
| `starting` / `prompt_pending` with `can_wait=true` | Follow `primary_action=wait`, then refresh `agentcall_session`. |
| `prompt_missing` | Follow `primary_action`; `submit_pending_prompt` is debug/recovery only. Do not queue natural language. |
| `prompt_commit_unacknowledged` | Inspect screen or use debug recovery; do not treat prompt commit as the normal path. |
| `prompt_submitted` | Wait for hook/tool/report progress; do not send more text yet. |
| `working` | Follow `primary_action=wait`. `request_report` requires explicit closure intent after the patience window or `user_explicit_close=true`. |
| `idle_after_turn` | Request a report or inspect progress before sending more text. |
| `report_requested` / `report_drafting` | Wait inside the deadline; refresh `agentcall_session` rather than sending more prompts. |
| `report_overdue` | Inspect, interrupt, or stop; do not keep waiting as if it were ordinary working. |
| `needs_permission` | Use `agentcall_session_send(action=select_option, choice="1|2|3")` only after reading the structured interaction. |
| `blocked_by_policy` | Do not repeat the denied command. Adjust write paths/task, add reference paths as context if useful, request a blocker report, interrupt, or stop. |
| `report_ready` | Call `agentcall_report(action=accept, session_id=...)` and inspect validation/confidence. |
| `confidence.overall=medium` | Report artifact exists, but daemon evidence is incomplete; inspect before final closure. |
| `confidence.overall=low` | Treat the report as unproven. Inspect evidence or request a revised report. |

## Session Send Rules

Codex sends intent; the daemon generates idempotency, attaches leases, checks projection freshness, and returns structured refusals.

Use `control_token` only for destructive or phase-changing actions such as `interrupt`, `stop`, `kill`, `approve_plan`, and `start_auto`. Fetch a fresh token from `agentcall_session(name)` before using those actions.

Do not provide `idempotency_key`, `owner_lease_id`, `lease_generation`, or `precondition` in normal flow.

`agentcall_session_send(action=request_report)` is a state transition, not just natural language. It records `report_requested`, returns a request id/deadline, and expects later daemon-observed report write evidence. After requesting a report, wait for `report_drafting`, `report_ready`, or `report_overdue`.

## Permission And Menu Rules

Permission menus are structured interactions, not free-form chat prompts.

Use `select_option` with `choice` for numbered menus. Do not send natural language into a permission menu unless the wrapper explicitly classifies the interaction as a question.

When a worker is still running, ordinary `send` may be queued and not heard immediately by Claude Code. If the worker is in `prompt_pending`, `prompt_missing`, or `prompt_commit_unacknowledged`, do not queue text; follow `primary_action`. `submit_pending_prompt` is only a debug/recovery action and returns `prompt_commit_signal_sent` with `not_completed=true`; keep observing until `prompt_submitted`, tool progress, report evidence, or explicit failure appears.

## Runtime Rules

PTY is the default and production runtime. It is human-visible and hook-aware. Historical ACP/SDK paths are not part of the normal Codex control loop.

## Report Acceptance Rules

AgentCall report acceptance is evidence-based:

```text
overall=high:
  report artifact exists and daemon_observed_write/equivalent deterministic evidence is true

overall=medium:
  report artifact exists but deterministic backing evidence is incomplete

overall=low:
  natural-language-only report, missing evidence, policy block, permission denial, test failure, or contradiction
```

Do not treat Claude natural language as final truth when daemon evidence contradicts it.

## Forbidden Defaults

```text
- Do not default to raw terminal / transcript reading.
- Do not retry prompts while patience window says wait.
- Do not treat quiet Claude Code reading/thinking as failure.
- Do not use Python as live state writer.
- Do not let TUI hints override hook/report/daemon structured state.
- Do not auto-kill visible PTY workers.
```
