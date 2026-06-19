# AgentCall v6.8.3 SQLite Single-writer Observation Report

## Summary

v6.8.3 fixes a control-plane latency regression observed during hook-heavy multi-worker runs. The failure mode was not a daemon crash: compact board calls queued behind frequent hook writes and sometimes exceeded the Codex/MCP 10 second tool-call budget.

## Evidence

- Runtime SQLite had WAL enabled, but the daemon allowed up to six SQLite store writer threads.
- During the incident window, the event store recorded roughly 90-178 events per minute, mostly `hook.*` events from active Claude PTY workers.
- `agentcall_board(view=compact)` also ran stale runtime cleanup on every call, which could take the global `state_writer` mutex and perform maintenance writes before returning an observation.
- SQLite only admits one writer at a time, so multi-writer fanout increased busy-lock contention instead of improving throughput.

## Changes

- SQLite RuntimeStore now reports `supports_parallel_writes=false`, so `StoreWriterRuntimeStore` uses a single writer thread even if `store_writer_threads` is configured above 1.
- SQLite connections set WAL, `synchronous=NORMAL`, `busy_timeout=5000`, and `wal_autocheckpoint=1000`.
- Compact board stale-runtime cleanup is throttled to once every 30 seconds, keeping normal board refreshes off the maintenance-write path.
- README, AGENTS, docs index, about page, changelog, and the example config now document SQLite as a single-writer WAL backend.

## Follow-up

If hook ingest still becomes a bottleneck under larger worker bursts, the next step is a deeper CQRS split: update in-memory projections synchronously, enqueue durable SQLite writes asynchronously, and move cleanup/orphan release to a background ticker instead of board observation.
