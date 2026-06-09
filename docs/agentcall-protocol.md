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
2. Route work: agentcall_route(mode=recommend|start, runtime=auto|pty).
3. Inspect worker: agentcall_session(name, include=["summary"]).
4. Send only when summary says it is safe or attention requires intervention.
5. Accept report only after reading report confidence and evidence.
```

`runtime=auto` selects PTY. `runtime=sdk` is experimental and disabled unless local daemon config explicitly enables it. ACP is not the default runtime.

## Action Matrix

| State / signal | Codex action |
| --- | --- |
| `projection_stale=true` | Do not send new work. Inspect session events or debug projection first. |
| `attention_status=none` and `patience_status=inside_patience_window` | Wait. Do not retry the same prompt. |
| `attention_status=needs_permission` | Use `agentcall_session_send(action=select_option, text="1|2|3")` only after reading the structured interaction. |
| `attention_status=waiting_input` | Send the minimal missing answer with a fresh `idempotency_key`. |
| `attention_status=blocked_by_policy` or `policy_blocked` | Do not repeat the denied command. Interrupt only when the worker is clearly stuck or wrong; otherwise change allowed paths or ask for blocker report. |
| `report_ready=true` | Call `agentcall_report(action=accept)` and inspect `confidence.band`, `evidence`, and `contradictions`. |
| `confidence.band=low` | Treat the report as unproven. Inspect evidence or request a revised report. |
| `confidence.contradictions` non-empty | Review before accepting; do not mark task completed. |

## Session Send Rules

Every side-effecting `agentcall_session_send` call must include an explicit `idempotency_key`.

Destructive actions such as `interrupt`, `stop`, or future `kill` require a safety precondition when available:

```json
{
  "idempotency_key": "stable-operation-key",
  "precondition": {
    "projection_last_session_seq": 123,
    "turn_state": "Idle"
  }
}
```

Never rely on a timeout retry to resend natural language. Reuse the same `idempotency_key` only for the same exact command payload.

## Permission And Menu Rules

Permission menus are structured interactions, not free-form chat prompts.

Use `select_option` for numbered menus. Do not send natural language into a permission menu unless the wrapper explicitly classifies the interaction as a question.

When a worker is still running, ordinary `send` may be queued and not heard immediately by Claude Code. Use queued supervisor instructions for gentle guidance; use `interrupt` only when the worker is clearly wrong, unsafe, or must be recovered immediately.

## Runtime Rules

PTY is the default and production runtime. It is human-visible and hook-aware.

SDK runtime is experimental, gated by `experimental_sdk_runtime=true`, and must still emit the same EventEnvelope / Projection contract. It must not bypass SessionActor or write raw PTY stdin.

## Report Acceptance Rules

AgentCall report acceptance is evidence-based:

```text
high confidence:
  structured success report + daemon-observed file write or test pass

medium confidence:
  report artifact exists but deterministic backing evidence is incomplete

low confidence:
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
