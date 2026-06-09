# AgentCall v5 Implementation Alignment Report

Date: 2026-06-09

## Scope

This report aligns the current implementation with the three accepted code plans:

- `docs/v5.0-code-plan.md`
- `docs/v5.1-code-plan.md`
- `docs/v5.2-code-plan.md`

The plan documents themselves are not changed by this report. This file records what has landed, what is partially landed, and what should remain as follow-up work.

## Implemented Alignment

### v5.0 Projection Fast Path + Codex Contract

Implemented:

- MCP bridge timeout handling in `crates/agentcall-mcp/src/daemon_client.rs`.
- Compact, capped tool response path and MCP timing record support in `crates/agentcall-mcp/src/protocol.rs`.
- `EventEnvelopeV1` with `global_seq` and `session_seq` in `crates/agentcall-daemon/src/events.rs`.
- Session projection model and reducer in `crates/agentcall-daemon/src/projection.rs`.
- Projection-backed board/session summaries in `crates/agentcall-daemon/src/summary.rs` and `crates/agentcall-daemon/src/mcp.rs`.
- `agentcall_session_send` idempotency and precondition fields in MCP schemas.
- Side-effect command idempotency checks in `crates/agentcall-daemon/src/commands.rs`.
- Codex supervisor protocol and generated skill in `docs/agentcall-protocol.md`, `docs/agentcall-supervisor-skill.md`, and `.codex/skills/agentcall-supervisor/SKILL.md`.

Current confidence: mostly implemented, with remaining audit needed around cold-path guarantees for every compact board/session branch.

2026-06-09 update:

- Added `scripts/agentcall_arch_audit.py` and wired it into `scripts/agentcall_dev.py release-check`.
- The audit now fails if:
  - generated/runtime build outputs are tracked by git,
  - non-actor modules reference `submit_raw_write`,
  - PTY writer ownership leaks outside session startup,
  - MCP default `agentcall_session` stops using `session_projection_summary`,
  - compact attention board stops using `board_attention_projection` before cold state reads.

### v5.1 SessionActor + Process Ownership

Implemented:

- `SessionActor` and actor command path in `crates/agentcall-daemon/src/actor.rs`.
- `PtyWriter` ownership split in `crates/agentcall-daemon/src/session.rs`.
- Command envelope and append-only command registry primitives in `crates/agentcall-daemon/src/commands.rs`.
- Owner/workspace lease model and canonical workspace conflict logic in `crates/agentcall-daemon/src/ownership.rs`.
- Windows Job Object process controller in `crates/agentcall-daemon/src/process.rs`.
- Route initial prompt path through the runtime/actor boundary in `crates/agentcall-daemon/src/routes.rs` and `crates/agentcall-daemon/src/runtime_pty.rs`.
- Interrupt/stop control semantics now distinguish sent control signals from observed completion.

Current confidence: implemented enough for first integration, but needs deeper real-worker verification for actor failure, writer-closed, output flood, and Windows child-process cleanup.

2026-06-09 update:

- `pty.session_started`, `pty.input_sent`, `pty.stop_requested`, and `pty.session_ended` now carry `session_id` for stable EventEnvelope/projection binding.
- Projection reducer now understands the real PTY/actor event names:
  - `pty.session_started` => working
  - `command.accepted` / `command.completed` / `pty.input_sent` => working progress
  - `command.awaiting_observation` / `pty.stop_requested` => awaiting observation
  - `pty.session_ended` => completed
- Added unit coverage for real PTY lifecycle and actor command projection events.
- Added deterministic fake PTY worker smoke:
  - `python scripts\agentcall_dev.py smoke real-worker`
  - starts a temporary daemon,
  - launches a real PTY route using `scripts/fake_pty_worker.py`,
  - verifies route prompt reaches the worker,
  - sends input through MCP `agentcall_session_send` and the actor command path,
  - verifies MCP `agentcall_session` default projection,
  - verifies stop returns `awaiting_observation`,
  - verifies compact attention board is projection-only.

### v5.2 Durable Runtime + Scheduler + Adapter Trait

Implemented:

- `RuntimeStore` trait and transaction-shaped API in `crates/agentcall-daemon/src/store.rs`.
- JSON-backed store adapter in `crates/agentcall-daemon/src/store_json.rs`.
- Store writer serialization wrapper in `crates/agentcall-daemon/src/store.rs`.
- SQLite store and migrations in `crates/agentcall-daemon/src/store_sqlite.rs`.
- Configurable store backend in `crates/agentcall-daemon/src/config.rs` and `config/agentcall.example.json`.
- Event cursor support through store-backed event queries.
- Worker scheduler and capacity/workspace rejection model in `crates/agentcall-daemon/src/scheduler.rs`.
- `AgentRuntime` trait, PTY runtime implementation, and gated experimental SDK runtime in:
  - `crates/agentcall-daemon/src/runtime.rs`
  - `crates/agentcall-daemon/src/runtime_pty.rs`
  - `crates/agentcall-daemon/src/runtime_sdk.rs`
- Deterministic `ConfidenceLedger` in `crates/agentcall-daemon/src/confidence.rs`.
- Skill/context generator in `scripts/generate_agentcall_skill.py`, covered by `tests/test_agentcall_skill_generator.py`.

Current confidence: broad first pass is implemented. The main remaining risk is not missing modules, but ensuring all live write paths have actually converged on the transaction-shaped store/actor boundaries.

2026-06-09 update:

- Real-worker smoke now supports both `json` and `sqlite` store backends:
  - `python scripts\agentcall_dev.py smoke real-worker --store-backend json`
  - `python scripts\agentcall_dev.py smoke real-worker --store-backend sqlite`
- Both backends now cover live route/session/projection and daemon restart recovery for:
  - durable route session record,
  - default MCP session projection,
  - compact attention board projection,
  - session event query,
  - command idempotency record.
  - command completed status after actor dispatch.
  - owner/workspace lease active and released state.

## Verification Already Run

Observed green runs before this report:

```text
python -m pytest -q
16 passed

cargo test -p agentcall-daemon -p agentcall-mcp
agentcall-daemon: 87 passed
agentcall-mcp: 7 passed
```

This commit should still be treated as an integration milestone, not final v5 sign-off.

Observed green runs after the v5 boundary audit and real-worker smoke update:

```text
python -m pytest -q
17 passed

cargo test -p agentcall-daemon -p agentcall-mcp
agentcall-daemon: 89 passed
agentcall-mcp: 7 passed

python scripts\agentcall_arch_audit.py
[OK] AgentCall architecture audit passed

python scripts\agentcall_dev.py smoke real-worker
[OK] real worker PTY smoke

python scripts\agentcall_dev.py smoke real-worker --store-backend sqlite
[OK] real worker PTY smoke
```

## Remaining Work Plan

### P0: Boundary Audit

- Verify every compact `agentcall_board` and default `agentcall_session` path is projection/index-only. **Covered by architecture audit for the current MCP defaults.**
- Verify no route/MCP/http path can still write PTY stdin without actor submission. **Covered for MCP, HTTP input/stop, route initial prompt, and WebSocket input by code path plus architecture audit.**
- Verify no hook/report/session path writes live state outside `RuntimeStore` or accepted legacy/debug-only wrappers.
- Add `rg`/architecture checks for forbidden direct calls once module names settle. **Initial script exists; extend it as more forbidden calls are identified.**

### P0: Real Worker Smoke

- Start one real PTY worker through `agentcall_route`. **Done via deterministic fake PTY worker.**
- Send a normal prompt through the actor command path with explicit `idempotency_key`. **Done via MCP `agentcall_session_send` real-worker smoke.**
- Verify board projection updates without reading raw terminal output. **Done for compact attention board and MCP default session projection.**
- Verify `interrupt` returns an interrupting/awaiting-observation state before final completion.
- Verify `stop` releases workspace lease and does not leave a healthy running projection. **Partially done: stop returns `awaiting_observation` and projection moves to stopping/completed; explicit lease-state assertion still open.**

### P1: Store Backend Hardening

- Run the same smoke with `store_backend=json` and `store_backend=sqlite`. **Done for live route/session/projection and restart recovery path.**
- Confirm daemon restart preserves:
  - events **covered by real-worker smoke**
  - projections **covered by real-worker smoke**
  - idempotency records **covered by real-worker smoke**
  - command records and completed status **covered by real-worker smoke**
  - leases **covered by real-worker smoke**
- Add corruption/rebuild tests for command index and projection snapshot.
  - command index corruption rebuild **covered for JSON store** from `commands.ndjson` plus `command-status.ndjson`.
  - projection snapshot corruption **covered for JSON store** as stale/missing rather than false-healthy; replay rebuild remains a future explicit recovery mode.
- Add SQLite transaction failure tests for command completion.
  - unknown command completion rejects without writing event/projection.
  - event uniqueness failure rolls back command status to `accepted`.
- Add SQLite transaction failure tests for route/session/lease creation.
  - injected workspace lease insert failure rolls back session row and owner lease row.

### P1: Scheduler and Lease Validation

- Test same-workspace exclusive conflict with differently spelled Windows paths.
- Test capacity rejection does not create hidden queued work.
- Test `scope=mine` / owner filtering after multiple owners exist.

### P1: Process Ownership Validation

- Verify Windows Job Object process cleanup with parent plus child process.
- If Job Object assignment fails in real Claude Code spawn, expose `portable_pty_best_effort` clearly in runtime health.
- Add real-world stop/kill evidence before calling kill-tree guaranteed.

### P2: Confidence and Report Review

- Add examples where Claude claims success but observed evidence is missing.
- Add examples where tests fail after a success report.
- Keep natural-language report claims low-confidence unless backed by structured evidence.

### P2: Skill Rollout

- Run `python scripts/generate_agentcall_skill.py --check` in release-check.
- Confirm the generated skill is the one installed/visible to Codex sessions.
- Add a short operator note explaining that Codex should inspect board/session projection before sending or interrupting.

## Commit Recommendation

Commit this as a v5 integration checkpoint after tests pass and build artifacts are excluded. Do not present it as final v5 completion until the P0 boundary audit and real-worker smoke are complete.
