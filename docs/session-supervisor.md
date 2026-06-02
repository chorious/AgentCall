# Session Supervisor

AgentCall can now start and supervise a named PTY session, similar to a tiny
workspace-local tmux pane. The important difference from the failed window
injection route is ownership: AgentCall creates the PTY process, so it can write
stdin, capture output, and keep process metadata from the start.

This is the preferred first-version control plane for visible or semi-visible
CLI agents when `claude -p` is too opaque.

## Commands

```powershell
$env:PYTHONPATH = 'src'

python -m agentcall init
python -m agentcall session start demo -- cmd.exe
python -m agentcall session send demo "echo AGENTCALL_TMUX_OK"
python -m agentcall session tail demo --lines 80 --plain
python -m agentcall session status demo
python -m agentcall session stop demo
```

Session files live under:

```text
.agentcall/
  sessions/
    demo/
      state.json
      input.ndjson
      output.log
      worker.stdout.log
      worker.stderr.log
```

`state.json` tracks the supervisor worker pid and the child process pid.
`input.ndjson` is the command queue. `output.log` is the captured terminal
stream. `tail --plain` strips ANSI escape sequences for easier reading.

## Claude Worker Sketch

Once the operator is ready to handle any first-run auth or trust prompts, start
a Claude Code session like this:

```powershell
$env:PYTHONPATH = 'src'
python -m agentcall session start claude1 -- claude --permission-mode auto
python -m agentcall session send claude1 "Read .agentcall/workers/Kimi1/inbox/task-0001.md and write the requested report.md."
python -m agentcall session tail claude1 --lines 120 --plain
```

For the SOP workflow, the orchestrator still uses `.agentcall/tasks/*` for the
contract. The session supervisor only supplies the interactive transport.

## Current Proof

The first smoke test launched `cmd.exe` inside a PTY, sent:

```text
echo AGENTCALL_TMUX_OK
```

and captured the echoed output through `session tail`. That proves the core
mechanism: start, send stdin, capture stdout, and stop.
