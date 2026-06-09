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

## Remaining Work Plan

### P0: Boundary Audit

- Verify every compact `agentcall_board` and default `agentcall_session` path is projection/index-only.
- Verify no route/MCP/http path can still write PTY stdin without actor submission.
- Verify no hook/report/session path writes live state outside `RuntimeStore` or accepted legacy/debug-only wrappers.
- Add `rg`/architecture checks for forbidden direct calls once module names settle.

### P0: Real Worker Smoke

- Start one real PTY worker through `agentcall_route`.
- Send a normal prompt through `agentcall_session_send` with explicit `idempotency_key`.
- Verify board projection updates without reading raw terminal output.
- Verify `interrupt` returns an interrupting/awaiting-observation state before final completion.
- Verify `stop` releases workspace lease and does not leave a healthy running projection.

### P1: Store Backend Hardening

- Run the same smoke with `store_backend=json` and `store_backend=sqlite`.
- Confirm daemon restart preserves:
  - events
  - projections
  - idempotency records
  - command records
  - leases
- Add corruption/rebuild tests for command index and projection snapshot.

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
