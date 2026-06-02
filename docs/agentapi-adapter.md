# AgentAPI Adapter

This is a v1.0 archive note. AgentAPI was the most practical first control
plane for AgentCall version 1. It starts Claude Code inside a PTY and exposes
HTTP endpoints for sending messages, reading status, and observing terminal
output.

This avoids the failed route of injecting input into an already-open Windows
Terminal tab.

It is not the short-term v2 development direction. v2 now focuses on bounded
parent/child lifecycles, structured reports, and protocol-first drivers such as
Claude ACP/SDK or headless JSON.

## Install

Download the Windows release:

```powershell
New-Item -ItemType Directory -Force tools
Invoke-RestMethod `
  -Uri https://github.com/coder/agentapi/releases/download/v0.12.2/agentapi-windows-amd64.exe `
  -OutFile tools\agentapi.exe
```

## Start A Claude Worker

```powershell
New-Item -ItemType Directory -Force .agentcall\agentapi

$root = (Resolve-Path .).Path
$dir = Join-Path $root '.agentcall\agentapi'
$out = Join-Path $dir 'stdout.log'
$err = Join-Path $dir 'stderr.log'
$pidfile = Join-Path $dir 'agentapi.pid'

Start-Process `
  -FilePath (Resolve-Path .\tools\agentapi.exe).Path `
  -ArgumentList @(
    'server', 'claude',
    '--port', '3284',
    '--pid-file', $pidfile,
    '--term-height', '1000',
    '--term-width', '100'
  ) `
  -WorkingDirectory $root `
  -RedirectStandardOutput $out `
  -RedirectStandardError $err `
  -WindowStyle Hidden
```

Check status:

```powershell
Invoke-RestMethod -Uri http://localhost:3284/status
```

Expected shape:

```text
status: stable
agent_type: claude
transport: pty
```

## Start With Fewer Approval Stops

AgentAPI can run a full custom command after `--`. Use this to pass Claude
Code permission options:

```powershell
Start-Process `
  -FilePath (Resolve-Path .\tools\agentapi.exe).Path `
  -ArgumentList @(
    'server',
    '--type', 'claude',
    '--port', '3285',
    '--pid-file', '.agentcall\agentapi-acceptedits\agentapi.pid',
    '--term-height', '1000',
    '--term-width', '100',
    '--',
    'claude',
    '--permission-mode', 'acceptEdits'
  ) `
  -WorkingDirectory (Resolve-Path .).Path `
  -WindowStyle Hidden
```

`acceptEdits` is a useful middle ground for SOP tests: routine file edits are
less likely to stop for approval, while higher-risk operations can still pause.
For fully isolated sandboxes, `--permission-mode bypassPermissions` is faster
but much less conservative.

For SOP workers that should move without routine approval prompts, `auto` is a
strong default:

```powershell
Start-Process `
  -FilePath (Resolve-Path .\tools\agentapi.exe).Path `
  -ArgumentList @(
    'server',
    '--type', 'claude',
    '--port', '3286',
    '--pid-file', '.agentcall\agentapi-auto\agentapi.pid',
    '--term-height', '1000',
    '--term-width', '100',
    '--',
    'claude',
    '--permission-mode', 'auto'
  ) `
  -WorkingDirectory (Resolve-Path .).Path `
  -WindowStyle Hidden
```

## Visible Supervision

Do not rely on Codex polling `/messages` as the only view. Open a visible
AgentAPI attach terminal:

```powershell
.\tools\agentapi.exe attach --url http://localhost:3284
```

or for the lower-friction worker:

```powershell
.\tools\agentapi.exe attach --url http://localhost:3285
```

This lets the human supervisor watch the same PTY session and handle permission
prompts directly. AgentCall can still poll `/status`, `/messages`, and
`report.md` for automation.

## Auto Mode Test

`task-0007` proved the lower-friction workflow:

```text
worker: AgentAPIAuto
command: claude --permission-mode auto
transport: pty
result: report.md written without manual approval
review: accepted
```

AgentAPI still exposed the progress trail through `/messages`:

```text
Thought for 13s, read 1 file, ran 1 shell command
Write(.agentcall\tasks\task-0007\report.md)
Wrote 17 lines
Done. The report is written to ...
```

## Send A Task

Register the worker and create an SOP task:

```powershell
$env:PYTHONPATH = 'src'
python -m agentcall worker register AgentAPI1 --pid 8060 --title AgentAPI1 --kind claude-code-agentapi --source agentapi-pty
python -m agentcall task create "AgentAPI SOP test"
python -m agentcall task assign task-0006 AgentAPI1
```

Send the task over HTTP:

```powershell
$msg = @{
  type = 'user'
  content = 'AgentCall SOP task for AgentAPI1. Read E:\Project\AgentCall\.agentcall\workers\AgentAPI1\inbox\task-0006.md and complete it by writing E:\Project\AgentCall\.agentcall\tasks\task-0006\report.md exactly as requested.'
} | ConvertTo-Json

Invoke-RestMethod `
  -Uri http://localhost:3284/message `
  -Method Post `
  -ContentType 'application/json' `
  -Body $msg
```

## Observe

```powershell
Invoke-RestMethod -Uri http://localhost:3284/status
Invoke-RestMethod -Uri http://localhost:3284/messages
```

AgentAPI exposes Claude's terminal progress, including thinking text,
commands, file-write previews, and permission prompts.

## Permission Prompts

If Claude asks for approval, send raw keystrokes:

```powershell
$raw = @{ type = 'raw'; content = "y`r" } | ConvertTo-Json
Invoke-RestMethod `
  -Uri http://localhost:3284/message `
  -Method Post `
  -ContentType 'application/json' `
  -Body $raw
```

## Proven Result

The first AgentAPI run completed:

```text
task-0006
transport: pty
report.md: written
review.md: accepted
```

This proved the first-version SOP path:

```text
AgentCall task -> AgentAPI HTTP -> Claude PTY -> report.md -> review.md
```

For the active v2 direction, see `docs/v2.0-architecture.md`.
