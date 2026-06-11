---
name: agentcall
description: Use AgentCall when coordinating daemon-backed Claude Code PTY workers, checking AgentCall board/session/report state, or delegating bounded utility work through AgentCall.
---

# AgentCall

Use AgentCall as the coordination layer for Codex-directed Claude Code utility workers.

## Default Workflow

1. Verify availability by directly calling `agentcall_daemon(action="status")`; do not use `tool_search agentcall` as the availability check.
2. Inspect `agentcall_board(view="compact", filter="attention")` before delegating or declaring a worker stuck.
3. Use `agentcall_route(objective=..., workspace=..., write_paths=..., reference_paths=...)` to start bounded PTY utility work.
4. Use `agentcall_session(name=...)` for compact worker state, why, `primary_action`, available/debug actions, report info, workspace projection, and fresh control token.
5. Follow `primary_action` for normal flow; use `debug_actions` only for recovery.
6. Use `agentcall_session_send(action="request_report")` when the worker should close.
7. Use `agentcall_report` to inspect or accept reports.

## Operating Rules

- Do not use HTTP fallback unless the user explicitly asks.
- `tool_search` may return zero AgentCall tools even when AgentCall MCP is callable; treat direct MCP calls as the source of truth.
- Treat workers as slower utility collaborators; wait for board/report evidence before calling them failed.
- Respect the 60s patience contract: while `patience_status` is `inside_patience_window`, avoid repeated continue/status nudges unless attention is active.
- Prefer board/session summaries over raw logs. AgentCall uses recent-first logs and artifact files; open raw artifacts only for debugging.
- Prefer small, clearly owned handoff tasks with write paths and optional reference paths.
- `agentcall_route` defaults to PTY; do not ask Codex to choose `runtime`, estimate task size, or pass `mode` in normal flow.
- `report_path` is optional in normal flow. When omitted, the daemon mints a unique per-route report path under the target workspace and includes it in route/session/report projections.
- Distinguish `workspace.daemon_workspace`, `workspace.target_workspace`, `workspace.claude_cwd`, and `report.path`; route `workspace` is the target project, not Claude Code cwd.
- Do not provide `idempotency_key`, `owner_lease_id`, `lease_generation`, or `precondition`; the daemon handles them.
- Treat default PTY workers as bounded-write workers: write tools may use route `write_paths`, `report_path`, and the session scratch, while Bash remains readonly-only unless a future policy says otherwise.
- `reference_paths` are read/context recommendations, not permission boundaries.
- If `attention_status` is `blocked_by_policy`, do not wait inside the patience window or resend the same prompt. Inspect `policy_block`, then adjust write paths/task, add reference paths as context if useful, interrupt for a blocker report, or stop the worker.
- Do not let two workers write the same files.
- Require a concise report or exact change summary at lifecycle end.
- Write a review only for drift, blockers, failed validation, or revision.
- When a session summary shows `prompt_pending`, `prompt_missing`, or `prompt_commit_unacknowledged`, do not queue natural-language input. Follow `primary_action`; `submit_pending_prompt` is a debug/recovery action, not the normal path.
- Treat a successful `submit_pending_prompt` response as a commit signal only. It returns `prompt_commit_signal_sent` and `not_completed=true`; refresh `agentcall_session` until `prompt_submitted`, tool progress, report evidence, or explicit failure appears.
- Treat `request_report` as a finite state transition. After it returns `report_requested`, refresh `agentcall_session` until `report_drafting`, `report_ready`, or `report_overdue`; do not keep sending closure prompts.
- If the worker enters `report_overdue`, inspect, interrupt, or stop instead of waiting inside the ordinary patience window.
- When a session summary shows `needs_permission` or a numeric menu, use `agentcall_session_send(action="select_option", choice="1")` or the intended option number. Do not send natural-language prompts into permission menus.
- Use `agentcall_session_send(action="interrupt")` only when a worker is clearly on the wrong path or must be reclaimed immediately.
- For report acceptance, read `confidence.overall`, `confidence.artifact`, `confidence.daemon_write`, and `confidence.route_match`. `overall=high` requires daemon-observed write/equivalent evidence; an existing report file alone is at most medium.
