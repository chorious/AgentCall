---
name: agentcall
description: Use AgentCall when coordinating daemon-backed Claude Code PTY workers, checking AgentCall board/session/report state, or delegating bounded utility work through AgentCall.
---

# AgentCall

Use AgentCall as the coordination layer for Codex-directed Claude Code utility workers.

## Default Workflow

1. Verify availability by directly calling `agentcall_daemon(action="status")`; do not use `tool_search agentcall` as the availability check.
2. Inspect `agentcall_board(view="compact", filter="attention")` before delegating or declaring a worker stuck.
3. Use `agentcall_route` to start bounded PTY utility work.
4. Use `agentcall_session` for compact session summaries.
5. Use `agentcall_session_send` for nudge, continue, stop, report-request, or plan/auto mode actions.
6. Use `agentcall_report` to inspect or accept reports.

## Operating Rules

- Do not use HTTP fallback unless the user explicitly asks.
- `tool_search` may return zero AgentCall tools even when AgentCall MCP is callable; treat direct MCP calls as the source of truth.
- Treat workers as slower utility collaborators; wait for board/report evidence before calling them failed.
- Prefer small, clearly owned handoff tasks with allowed paths.
- Do not let two workers write the same files.
- Require a concise report or exact change summary at lifecycle end.
- Write a review only for drift, blockers, failed validation, or revision.
