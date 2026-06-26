# v6.9.1 MCP Schema Alignment Report

Date: 2026-06-21

## Summary

v6.9.1 fixes the daemon/MCP schema split that left Codex unable to call `agentcall_session_send(action=approve_changed_dir)` even though the v6.9 daemon already implemented the action.

## Root Cause

The daemon schema in `crates/agentcall-daemon/src/mcp.rs` exposed `approve_changed_dir`, `dir`, and `reason`, but the stdio MCP bridge advertises a static fallback schema from `crates/agentcall-mcp/src/tools.rs`. That fallback action enum was still at the pre-v6.9 shape, so Codex rejected the call at tool-schema validation before it could reach the daemon.

## Changes

- `agentcall-mcp` now tries `GET /api/mcp/tools` with a short timeout during `tools/list` and uses the daemon schema when available.
- The bridge static fallback schema now includes `approve_changed_dir`, `dir`, and `reason`.
- `scripts/agentcall_arch_audit.py` now compares daemon and bridge fallback schemas for canonical proxy tools, including property names, required fields, and enum values.
- Release notes and docs were updated for v6.9.1.

## Validation

- `python scripts/agentcall_arch_audit.py`
- Full release validation is expected through `python agentcall.py runtime-release --version 6.9.1`.
