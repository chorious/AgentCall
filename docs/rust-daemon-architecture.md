# Rust Daemon Architecture

The Python PTY worker proves the control plane, but it should not be the final
runtime. The production AgentCall daemon should be Rust-first.

## Split

```text
Browser / desktop UI
  -> HTTP/WebSocket API
    -> Rust daemon
      -> PTY sessions
      -> event log
      -> SOP task files
      -> process registry
```

The web UI should talk to a stable API, not to Python internals. That lets us
replace the prototype worker without changing the operator experience.

## Rust Responsibilities

- Own PTY creation and lifecycle from process start.
- Stream terminal frames over WebSocket.
- Accept stdin writes, resize events, stop/kill, and metadata updates.
- Keep a session registry with `session_id`, `worker_pid`, `child_pid`, cwd,
  command, status, started time, and output cursor.
- Persist the SOP contract under `.agentcall/`.
- Emit append-only lifecycle events.
- Apply backpressure so a noisy agent cannot block the whole orchestra.

## Suggested Crate Layout

```text
crates/
  agentcall-daemon/
    src/
      main.rs
      api.rs
      pty.rs
      session.rs
      store.rs
      protocol.rs
      windows.rs
```

On Windows, the PTY layer should target ConPTY/winpty through a Rust crate such
as `portable-pty` or a direct ConPTY wrapper. The rest of the daemon should stay
platform-neutral.

## API Shape

```text
GET    /api/sessions
POST   /api/sessions
GET    /api/sessions/{id}
POST   /api/sessions/{id}/input
POST   /api/sessions/{id}/resize
POST   /api/sessions/{id}/stop
WS     /api/sessions/{id}/stream
```

Terminal stream events should be structured:

```json
{"type":"output","seq":42,"bytes":"...base64..."}
{"type":"status","status":"running","child_pid":1234}
{"type":"exit","code":0}
```

The browser can then render a real terminal emulator while the daemon remains
responsible for throughput, pid truth, and session durability.
