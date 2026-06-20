# v6.9.0 Lightweight Folder Audit Implementation Report

Date: 2026-06-20

## Summary

v6.9.0 replaces the over-conservative readonly-only Bash policy with monitored Bash execution backed by a lightweight folder heartbeat audit.

The implementation intentionally avoids a full filesystem sandbox and avoids file-level change logs. It records directory-level signatures at PTY route start, updates a folder heartbeat after hook-observed tool turns, and blocks only when changed target-workspace folders are outside the session's scratch/report/write boundaries.

## Implemented

- Added `workspace_audit.rs` for session-scoped folder baseline, heartbeat diffing, policy block creation, and `approve_changed_dir` approval.
- PTY route startup now initializes a workspace audit baseline and records it under route result metadata.
- `PostToolUse` hook observation now emits a `workspace_audit` heartbeat even when the tool reports no explicit file paths, covering Bash and helper scripts.
- Non-readonly Bash is no longer rejected solely because it may write; obvious destructive commands and `git clone` remain preflight-denied.
- Active folder-audit blocks stop subsequent tool calls until Codex approves the folder or interrupts/stops the worker.
- `agentcall_session_send(action=approve_changed_dir, dir=...)` approves a changed folder for the current session only and clears the audit block.
- Worker summary output includes active `policy_block` details so Codex can see blocked folders and ready-to-send approval actions.
- `accepted_live` summaries now auto-include a stop control token for owner-bound callers and inject it into the primary stop action.

## Boundaries

- This is not a sandbox and does not claim command-level filesystem isolation.
- The audit stores changed directories, not changed files.
- Folder approval is session-scoped and does not widen global write policy.
- Directory scan limits intentionally degrade to `overflow_attention` instead of producing huge reports.

## Validation

- `cargo test -p agentcall-daemon` passed locally with 184 tests.
- Full release validation is expected through `python agentcall.py runtime-release --version 6.9.0`.
