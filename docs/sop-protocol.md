# SOP Protocol

AgentCall version 1 uses files as the collaboration contract.

## Task Artifact

`task.md` is written by the orchestra.

```yaml
task_id: task-0001
title: Build a minimal game loop
status: created
```

The body should describe:

- objective
- scope boundaries
- acceptance criteria
- report expectations

## Worker Report

`report.md` is written by the worker.

Required frontmatter keys:

```yaml
task_id: task-0001
run_id: run-0001
agent: worker-name
status: done
changed_files: []
tests: []
blockers: []
```

The body should summarize the work, known issues, and any requested next action.

## Review / Feedback

`review.md` is written by the orchestra or reviewer only when a task needs
revision, is blocked, or requires explicit feedback. A clean acceptance should
be represented by task status or an event, not by a ceremonial `review.md`.

```yaml
task_id: task-0001
decision: accepted
reviewer: cleverGPT
```

Allowed feedback decisions:

- `needs_revision`
- `blocked`

`accepted` can still appear in legacy artifacts, but v1.0 treats that as a
status transition rather than a required review document.

## Event Stream

`.agentcall/events.ndjson` is append-only. Each line is one JSON event:

```json
{"id":"evt-000001","task_id":"task-0001","type":"task.created","message":"Task created"}
```

Events are used for status, debugging, and later live-stream collaboration.

## Worker Registry

Externally launched agents are registered under `.agentcall/workers/`.

```json
{
  "id": "GLM1",
  "pid": 38168,
  "title": "GLM1",
  "kind": "claude-code",
  "source": "window-title"
}
```

The first version treats this registry as a supervised fact table. The model's self-report is not trusted for identity; the orchestra records the externally observed PID and window title.
