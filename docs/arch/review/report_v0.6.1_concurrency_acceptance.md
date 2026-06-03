# v0.6.1 Report: Concurrency acceptance

## Summary

Added real OS-process acceptance tests for daemon-first hook ingestion. The test starts a real daemon, launches independent Python hook processes, and validates daemon-owned claims/events.

## Scenarios

- Eight independent hook processes concurrently run `PreToolUse Write` for the same file.
- Exactly one active owner remains in daemon `file_claims`.
- The losing hook processes receive Claude-compatible `PreToolUse` deny output.
- `events.ndjson` is parsed line by line and event ids are checked for uniqueness.
- `PostToolUse Read` does not create a write claim.

## Verification

- `python -m pytest -q tests\test_v061_hook_daemon_ingest.py`
- Full suite: `python -m pytest -q`
