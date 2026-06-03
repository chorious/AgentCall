# v0.6.1 Report: Hook daemon ingest

## Summary

Claude/Codex hook clients now use daemon-first ingestion. The live hook write path posts to `/api/hooks/ingest`; legacy Python `agentcall hook ingest` is kept only as fail-open fallback when the daemon is unavailable.

## Changes

- `scripts/agentcall-claude-hook.py` posts hook payloads to `AGENTCALL_DAEMON_URL` or `http://127.0.0.1:3293`.
- `scripts/agentcall-codex-hook.py` uses the same endpoint and marks `runtime` as `codex`.
- Both scripts use only Python standard library `urllib.request`.
- Fallback to legacy Python ingest logs to stderr and preserves Claude/Codex hook output compatibility.

## Verification

- `python -m compileall src scripts tests`
- `python -m pytest -q`
- `cargo test -p agentcall-daemon -p agentcall-mcp`
