# AgentCall

AgentCall is an orchestration layer for supervising coding agents through bounded child lifecycles, structured reports, feedback gates, and process-aware delegation.

The first version proved an SOP loop in one shared workspace. The v2 direction is protocol-first: a parent process owns project context, calls bounded child agents through drivers such as Claude ACP/SDK, requires a report at the end of every child lifecycle, and accepts clean work without ceremonial reviews.

## v2.0 Direction

- Parent owns context, orchestration, validation, and task state.
- Child agents are bounded lifecycle calls, not long-running project owners.
- Every child call must return a structured report.
- Reviews are delegated only when parent validation finds risk, drift, blockers, or low confidence.
- SOP behavior moves into code: report validation, scope checks, lifecycle limits, and acceptance policy.
- ACP/SDK is the flagship driver path.
- PTY/tmux is parked as a v1.0 archive and fallback, not the short-term development direction.

Run the current v2 simulation:

```powershell
$env:PYTHONPATH='src'
python -m agentcall --root .agentcall-demo workflow simulate
python -m agentcall --root .agentcall-demo workflow inspect task-0001
```

The simulation creates a tiny calculator project under `.agentcall-demo/.agentcall/simulations/`, runs a planner child, runs an executor child, validates the report and allowed scope, then records parent acceptance without writing `review.md`.

The ACP path is now represented by a tested stdio JSON-RPC driver boundary. The
simulation uses a deterministic child driver; live Claude ACP execution is the
next integration step.

Driver choices:

```powershell
python -m agentcall --root .agentcall-demo workflow simulate --driver scripted
python -m agentcall --root .agentcall-demo workflow simulate --driver headless-json
python -m agentcall --root .agentcall-demo workflow simulate --driver acp
```

`scripted` is deterministic and free. `headless-json` and `acp` are live Claude
paths and should be used only when you intend to spend a bounded child lifecycle.

## v3.0 MCP Surface

AgentCall now includes a Rust MCP server so other processes can discover and
call the v2 runtime:

```powershell
cargo build -p agentcall-mcp
target\debug\agentcall-mcp.exe --workspace E:\Project\AgentCall
```

It exposes:

```text
agentcall_capabilities
agentcall_report_schema
agentcall_workflow_simulate
agentcall_workflow_inspect
```

The local Codex app config is registered with `[mcp_servers.agentcall]`. New
Codex sessions can load the server and call these tools through MCP. See
`docs/v3.0-mcp.md`.

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

For the archived PTY/tmux prototype, see `docs/v1.0-tmux-pty-archive.md`.

## v1.0 PTY/tmux Archive

The Rust daemon plus xterm.js UI was the v1.0 high-fidelity prototype. It is
kept as an archive and fallback, but it is no longer the short-term development
direction.

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

The Rust daemon streams PTY bytes over a WebSocket, and the browser renders them
with xterm.js. This remains useful for attach/debug/fallback work. The v2 runtime
focus is protocol-first child orchestration through ACP/SDK/headless drivers.

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
