# v6.9.1 Runtime Identity And Cold Board Report

Date: 2026-06-26

## Summary

This patch closes the stale-daemon and board-read contention gaps in v6.9.1 without changing the public worker model.

- `runtime-release` writes a hot-read `agentcall-version.json` manifest into the versioned runtime directory. The manifest records the product version plus daemon, MCP, and hook binary paths.
- MCP start/status/proxy paths validate daemon `/api/runtime/health` against that manifest and the compiled MCP `SERVER_VERSION`. A daemon with the wrong version or binary path is rejected with `daemon_version_drift`.
- Compact board now reads only store-backed projection state for all compact filters. It no longer recomputes live worker summaries, sweeps PTYs, performs stale cleanup, or acquires the daemon state-writer lock during board reads.
- Runtime and scheduler health reads were kept read-only so control-panel polling does not contend with hook/store writes.
- `runtime-release` Windows cleanup now quotes repo paths with PowerShell string rules and materializes matched AgentCall processes as an array before stopping them, so versioned runtime binaries can be overwritten while an old daemon is live.

## Validation

- `cargo test -p agentcall-daemon compact_attention_board`
- `cargo test -p agentcall-mcp`
- `cargo test --workspace`
- `python -m pytest -q`
- `python scripts\agentcall_arch_audit.py`
- `python agentcall.py release-check`
- `python agentcall.py runtime-release --version 6.9.1`
- `git diff --check`
