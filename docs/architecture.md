# AgentCall Architecture

## One Sentence

AgentCall is a local control plane where Codex supervises daemon-owned Claude Code PTY utility workers through compact MCP tools, hook-aware state projection, and bounded write policy.

## Current Runtime Circuit

```text
Codex thread
  -> AgentCall MCP bridge (stdio, compact tool JSON)
  -> Rust daemon (HTTP, single writer)
  -> route/session/report APIs
  -> SessionActor
  -> daemon-owned PTY
  -> Claude Code worker
  -> Claude hooks in configured claude_workspace
  -> POST /api/hooks/ingest
  -> event store + runtime bindings + projections
  -> compact board/session summary back to Codex
```

## Authority Boundaries

| Layer | Owns | Must Not Own |
|---|---|---|
| Codex | task decomposition, supervision, report acceptance, integration | raw PTY scraping as default state source |
| MCP bridge | stable tool schemas, compact JSON transport, daemon bootstrap | live state authority or workflow orchestration |
| Rust daemon | events, claims, sessions, routes, bindings, projections, health | user-level Codex plugin lifecycle |
| SessionActor | ordered PTY input, stop/interrupt dispatch, command completion events | hook policy, report validation, global scheduling |
| Hooks | structured Claude/Codex runtime observations | arbitrary state writes outside daemon ingest |
| Claude Code worker | bounded implementation/review/report work | supervisor decisions, cross-session ownership |

## Core Modules

```text
crates/agentcall-daemon/src
  actor.rs        PTY command actor and command completion
  commands.rs     command envelopes, idempotency, safety preconditions
  hooks.rs        hook ingest, binding, policy, claims, report-ready detection
  mcp.rs          daemon-side MCP tool handlers
  prompt_gate.rs  route prompt delivery gate and UserPromptSubmit ack state
  projection.rs   session projection reducer and compact board items
  routes.rs       route creation, PTY prompt/context, containment
  session.rs      PTY spawn/read/wait/stop, cwd policy
  state.rs        event append, runtime state helpers, log artifacting
  summary.rs      board/session/runtime health projection
  worker_state.rs normalized Codex-facing worker state and next actions
  store*.rs       JSON/SQLite runtime store implementations
```

```text
crates/agentcall-mcp/src
  protocol.rs       MCP stdio loop and compact response caps
  tools.rs          canonical tool schemas
  daemon_client.rs  HTTP client and timeout classification
  bootstrap.rs      agentcall_daemon status/start helper
```

## Data Surfaces

| Surface | Purpose |
|---|---|
| `.agentcall/events/recent.ndjson` | recent event hot log |
| `.agentcall/logs/hooks/<event>/recent.ndjson` | per-hook hot logs |
| `.agentcall/artifacts/hooks/...` | large hook payload artifacts |
| `.agentcall/state/runtime_binding.json` | wrapper/session hook binding and hot hook flags |
| `.agentcall/state/routes.json` | route state and context packet projection |
| `.agentcall/state/file_claims.json` | active/stale/released file ownership claims |
| `.agentcall/state/projections/sessions/*.json` | compact session projection snapshots |
| `.agentcall/state/pending_supervisor_instructions.json` | queued hook-injected supervisor instructions |

The long-term direction is projection-first reads: board and session summaries should avoid scanning large raw event files.

## Worker Lifecycle

1. Codex calls `agentcall_route`.
2. Daemon validates route fields, reserves leases, creates context packet, and starts a PTY worker.
3. Daemon writes the full handoff to a file and sends Claude a short prompt referencing that file.
4. Claude Code starts in configured `claude_workspace`, not necessarily the task workspace.
5. Daemon injects `AGENTCALL_WRAPPER_SESSION`.
6. Claude hooks bind Claude session id/transcript to wrapper session.
7. `UserPromptSubmit` closes the prompt gate and marks the task as truly started.
8. Hook events update runtime bindings, file claims, policy denials, and projections.
9. Worker writes `report_path`.
10. PostToolUse hook marks route/session as `report_ready`.
11. Codex reads compact board/session summary and accepts or requests revision.
12. Stop/cleanup releases claims and leases.

## Current Guarantees

- Claude cwd is forced by daemon local config `claude_workspace`.
- Hook install target is `<claude_workspace>\.claude\settings.local.json`.
- Normal MCP callers do not provide lease ids, preconditions, or idempotency keys; daemon mints command envelopes.
- Destructive or phase-changing actions require a fresh daemon-minted control token.
- `SessionStart`, `pty.input_sent`, and command completion are not treated as task start; `UserPromptSubmit` is the task-start ack.
- Missing prompt ack becomes `prompt_missing` / `submit_pending_prompt`, not silent `working/none`.
- Repeated policy denial becomes attention, not healthy working.
- Compact board lists live workers, not historical projections.
- Default session summary exposes normalized `state`, `why`, `can_wait`, `next_actions`, report info, control token, and debug refs.
- Writing route `report_path` marks the worker as `report_ready`.
- Read-only PTY routes conservatively deny `TaskCreate`.
- Writer/reader PTY failures emit projection-visible failure events.

## Known Open Gates

Tracked against the frozen [v6.0 code plan](v6.0-code-plan.md) and current implementation reports. Do not reopen ACP/SDK as the mainline unless the user explicitly asks.

## Design Direction

AgentCall deliberately favors a simple, visible PTY worker model over opaque SDK/ACP delegation. The goal is not to make Claude Code disappear behind a tool call; the goal is to make Codex a calmer supervisor with compact, trustworthy state and enough control to coordinate multiple workers without reading raw terminal walls.
