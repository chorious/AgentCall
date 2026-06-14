# AgentCall v6.8 Owner-scoped Batch Snapshot Implementation Report

Date: 2026-06-14

## Summary

Implemented the first v6.8 control-plane pass without restarting the live daemon or MCP service.

This change focuses on the code path that made Codex pay for repeated `agentcall_session` calls and risk cross-owner state leakage:

- compact board now uses a pure-read worker snapshot instead of the prompt-gate-refreshing worker state path;
- default session summary now uses the same pure-read snapshot and no longer includes a control token unless `include=["control"]` is requested;
- MCP client owner is threaded into board/session/session_send/report entrypoints instead of only route;
- route first-prompt submission and daemon prompt-gate auto-submit no longer hardcode `owner_id="codex"`;
- `owner_unbound` is completed in the structured error enum metadata.

## Files Changed

- `crates/agentcall-daemon/src/worker_state.rs`
  - Added `worker_snapshot_for_session`.
  - Kept `worker_state_for_session` as the refresh-capable path.
  - Shared decision logic through an internal `worker_state_for_session_with_gate`.

- `crates/agentcall-daemon/src/summary.rs`
  - Compact board now uses `worker_snapshot_for_session`.
  - Compact board applies owner filtering before building worker projections.
  - Compact board adds no-token control metadata and owner visibility metadata.

- `crates/agentcall-daemon/src/mcp.rs`
  - `agentcall_board` default schema scope is now `mine`.
  - `agentcall_session` accepts explicit `include=["control"]`.
  - MCP client owner is passed into board/session/session_send/report handlers.
  - Default session summary returns no control token.
  - Session send fills `owner_id` from MCP client context when caller did not pass one.
  - Tests updated for pure-read/no-token defaults.

- `crates/agentcall-daemon/src/routes.rs`
  - First prompt submission helpers now take the route owner id.
  - Removed hardcoded `"owner_id": "codex"` from route prompt submit paths.

- `crates/agentcall-daemon/src/prompt_gate.rs`
  - Daemon auto-submit resolves owner id from owner lease or route record.
  - Removed hardcoded `"owner_id": "codex"` from prompt-gate auto-submit.

- `crates/agentcall-daemon/src/control.rs`
  - `control_summary_for_session(..., None)` no longer mints a token.
  - Token minting now requires an explicit owner.

- `crates/agentcall-daemon/src/errors.rs`
  - Completed `OwnerUnbound` status/metadata handling.

## Validation

Passed:

```powershell
cargo-1.95.0-msvc.cmd test --workspace
```

Result:

- `agentcall-daemon`: 177 passed
- `agentcall-hook`: 2 passed
- `agentcall-mcp`: 10 passed

Also passed earlier during iteration:

```powershell
cargo test -p agentcall-daemon
cargo test -p agentcall-mcp
```

## Important Behavior Changes

- `agentcall_session(include=["summary"])` should no longer include `control.token`.
- `agentcall_session(include=["summary", "control"])` is now the explicit token-bearing path.
- `agentcall_board(view=compact, scope=mine)` should be the default batch state path.
- Compact board no longer advances prompt-gate timeout maintenance as a read side effect.

## Remaining Risks

- The live daemon was intentionally not restarted, so current Codex sessions still see the old behavior until rebuild/restart.
- The screenshot-observed concurrency problem is not fully closed by this change alone: already accepted/stale workers can still appear in attention or occupy projections in the currently running daemon. The current patch reduces cross-owner batch leakage and default token minting, but stale accepted worker cleanup/attention suppression should be handled as a follow-up if it persists after restart.
- `scope=all` debug/read-only semantics are partially represented through no-token control metadata, but this pass does not yet harden every non-board debug surface.
- `mcp_report` now receives client context but does not yet enforce owner ownership on report accept; this remains a follow-up for full v6.8 acceptance.

## No Restart Performed

Per request, this implementation was tested from source only. No live daemon or MCP restart was performed.
