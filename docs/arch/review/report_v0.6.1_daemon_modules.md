# v0.6.1 Report: Daemon modules

## Summary

The daemon monolith was split by subsystem while preserving behavior and public API shape.

## Modules

- `main.rs`: argument parsing, `AppState` construction, bind/run loop.
- `state.rs`: daemon state, event id allocation, JSON state IO, event append.
- `hooks.rs`: hook ingest, file claim policy, unmatched hooks, event append endpoint payloads.
- `summary.rs`: board, runtime health, project state, session summary.
- `session.rs`: PTY session lifecycle, input, resize, stop, broadcast.
- `http.rs`: HTTP routing, SSE, WebSocket, static files, JSON responses.
- `util.rs`: path/name/time helpers.

## Verification

- `cargo fmt -p agentcall-daemon`
- `cargo test -p agentcall-daemon -p agentcall-mcp`
