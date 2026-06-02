# AgentCall

AgentCall is an orchestration layer for supervising CLI-based coding agents through task artifacts, feedback gates, and process-aware delegation.

The first version is deliberately small: it proves an SOP loop in one shared workspace. An orchestra creates a task, starts a supervised worker process, records PID and events, captures logs, waits for a standardized report, and records acceptance or feedback.

## v1.0 Scope

- File-based SOP artifacts under `.agentcall/`
- Append-only event log for auditability
- Task lifecycle tracking
- Worker run records with PID, command, stdout, stderr, and exit code
- Standard `task.md` and `report.md` handoff files
- `review.md` only when feedback, revision, or blocker handling is needed
- A local CLI that can run without a daemon
- Named PTY sessions for tmux-like stdin/output control

This version is not yet large-scale parent/child agent collaboration. That comes after the SOP and process-supervision loop is solid.

## Quick Start

```powershell
$env:PYTHONPATH='src'
python -m agentcall init
python -m agentcall task create "Build a minimal game loop"
python -m agentcall run start task-0001 -- python -c "from pathlib import Path; Path('.agentcall/tasks/task-0001/report.md').write_text('status: done\nsummary: ok\n', encoding='utf-8')"
python -m agentcall task status task-0001
```

For interactive agents, see `docs/session-supervisor.md`.
For the browser-based pane prototype, run `viewer/tmux_server.py`.
For the planned Rust runtime split, see `docs/rust-daemon-architecture.md`.

## Rust Daemon MVP

The current higher-fidelity path is the Rust daemon plus xterm.js UI:

```powershell
npm install
cargo build -p agentcall-daemon
target\debug\agentcall-daemon.exe --port 3293 --workspace .
```

Then open:

```text
http://localhost:3293
```

Start a PowerShell pane from the UI, or by API:

```powershell
$body = @{
  name = 'rps1'
  command = @('powershell.exe', '-NoLogo')
  cols = 100
  rows = 36
} | ConvertTo-Json -Depth 5

Invoke-RestMethod `
  -Uri http://localhost:3293/api/sessions `
  -Method Post `
  -ContentType 'application/json' `
  -Body $body
```

The Rust daemon streams PTY bytes over a WebSocket, and the browser renders
them with xterm.js. Keyboard input, terminal control responses, and resize
events use the same WebSocket, so the pane behaves like a real terminal instead
of a polling log viewer.

## Directory Layout

```text
.agentcall/
  events.ndjson
  tasks/
    task-0001/
      task.json
      task.md
      report.md
      review.md        # only when feedback is needed
      runs/
        run-0001/
          run.json
          stdout.log
          stderr.log
```

## Design Bias

AgentCall treats agents as supervised workers, not synchronous function calls. The SOP files are the contract; the process supervisor owns execution facts such as PID, command, exit code, and logs. This keeps the first version boring enough to trust.
