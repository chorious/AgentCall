# AgentCall Architecture

## Version 1: SOP Orchestra

Version 1 proves a local orchestration loop:

```text
task created
-> worker process started
-> PID/logs/events captured
-> worker writes report.md
-> orchestra writes review.md
-> task accepted or marked for revision
```

The orchestra and worker share a workspace, but coordination happens through explicit artifacts under `.agentcall/`.

## Core Components

### SOP Artifact Protocol

The protocol defines stable files:

- `task.md`: assignment, scope, and acceptance criteria
- `report.md`: worker result, changed files, tests, blockers, and next request
- `review.md`: reviewer decision and notes
- `events.ndjson`: append-only audit stream

### Task Store

The store owns the `.agentcall` directory, allocates task and run IDs, writes JSON state, and appends events.

### Process Supervisor

The supervisor starts a command as a worker process, records its PID, captures stdout/stderr, waits for completion, and stores exit information. It does not assume the worker is cooperative.

### Review Gate

The review command writes a standardized review artifact and updates task state. In the first version this is manual or scripted; later it can be performed by a reviewer agent.

## State Model

```text
CREATED
-> RUNNING
-> REPORT_READY
-> REVIEWING
-> ACCEPTED

RUNNING -> FAILED
REPORT_READY -> NEEDS_REVISION
```

## Version 2 Direction

Version 2 expands from one supervised SOP loop into parent/child agent collaboration:

- multiple workers and leases
- heartbeats
- interrupt/kill/fallback policy
- worktree isolation
- child task spawning
- reviewer agents
- durable background queues
