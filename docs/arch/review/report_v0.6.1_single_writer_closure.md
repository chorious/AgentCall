# v0.6.1 Report: Single-writer closure

## Summary

The critical hook write gap is closed: live hook/session/file-claim/event writes now enter the Rust daemon path. Python hook ingest remains legacy fallback only and is explicitly labelled as non-primary.

## Changes

- Hook scripts no longer default to `python -m agentcall hook ingest`.
- MCP `agentcall_hook_ingest`, file claims, board/session APIs continue to use daemon endpoints.
- Python CLI `hook ingest` help now marks the command as legacy fallback.
- No Python file lock, Python/Rust dual lock, or Python/Rust dual-writer scheme was added.

## Verification

- `rg` confirms MCP hook ingest calls daemon `/api/hooks/ingest`.
- `python -m pytest -q tests\test_v061_hook_daemon_ingest.py`
- `cargo test -p agentcall-daemon -p agentcall-mcp`
