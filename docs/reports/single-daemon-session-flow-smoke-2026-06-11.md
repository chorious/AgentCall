# AgentCall Daemon PTY 压力与单次闭环报告

日期：2026-06-11

## 结论摘要

这轮测试分成两部分：

1. **6 并发压力测试**：同时拉起 6 个 daemon-owned Claude Code PTY worker，让它们执行有负担的只读审计任务。
2. **单 session 强行闭环测试**：在正常 route prompt 卡住后，绕过 MCP `agentcall_session_send`，直接调用 daemon HTTP input，把一次任务完整跑到 report 落地和 session 回收。

结论很明确：

- daemon 的并发容量、session ownership、hook env binding、lease 获取与释放基本能工作。
- v5.5 control token 的正常 stop 路径能用，stale token 也能正确拒绝。
- 真正的 P0 问题不是“Claude Code 不能跑”，而是 **route prompt 提交流程不可靠**。
- 当前系统会把“PTY bytes 已写入”误当成“Claude 已接受 user turn”；当 prompt 卡在输入框时，summary/board 仍会显示 `working / attention=none`，误导 Codex 等待。
- MCP `agentcall_session_send` 在 `working + attention=none` 时会转为 hook queue，不会直接敲 PTY，因此无法修复“输入框里有未提交 prompt”的特殊状态。
- 直接调用 daemon HTTP `/api/sessions/{name}/input` 可以绕过这个限制，并成功把卡住的输入提交出去。

一句话：**AgentCall 单 worker 闭环是可行的，但 route prompt gate 和 MCP 控制抽象目前不可信。**

## 测试环境

AgentCall workspace：

```text
E:\Project\AgentCall
```

Claude Code process cwd：

```text
D:\guKimi
```

daemon 关键状态：

```text
runtime=agentcall-daemon
process_controller=WindowsJob
max_sessions=6
per_owner_max_sessions=6
queue_policy=reject_when_full
claude hooks path=D:\guKimi\.claude\settings.local.json
PostToolBatch=true
```

测试开始前 board：

```text
live_daemon_sessions=0
owner_leases.active=0
workspace_leases.active=0
active_file_claims=0
```

## 第一部分：6 并发压力测试

### 测试目标

同时拉起 6 个 daemon PTY worker，让它们分别审计 AgentCall 的不同代码区域，验证：

- 并发 session 上限是否生效；
- route 是否能同时启动多个 PTY；
- hook binding 是否能区分 6 个 session；
- prompt 是否能真正提交；
- board/summary 是否能暴露异常；
- stop/control token/lease cleanup 是否能并发回收。

### 并发 worker 列表

| Session | 任务区域 |
|---|---|
| `stress-v55-control` | control token / actor / command safety |
| `stress-v55-route-lease` | route / lease / scheduler / PTY runtime |
| `stress-v55-hooks-proj` | hooks / projection / summary |
| `stress-v55-terminal` | terminal screen / TUI cleaner / session output |
| `stress-v55-store` | store / idempotency / persistence |
| `stress-v55-mcp-http` | MCP schema / daemon client / HTTP auth |

6 个 route 都使用：

```text
runtime=pty
read_only=true
workspace=E:\Project\AgentCall
```

### 并发启动结果

6 个 `agentcall_route(mode=start, runtime=pty)` 全部返回成功。

共同特征：

```text
status=started_pending_prompt_ack
actor_result.status=input_sent_by_actor
binding_gate.status=pending_hook
workspace lease=SharedReadonly
cwd=D:\guKimi
```

这说明 daemon 层成功做到了：

- 同时启动 6 个 PTY child；
- 6 个 session 都进入 live daemon session 列表；
- scheduler 上限 6 生效，没有误拒；
- read-only workspace lease 可以并发共享；
- prompt dispatch actor 都返回 `input_sent_by_actor`。

### 等待后的 board/summary 结果

等待约 65 秒后：

```text
active_pty_sessions=6
live_daemon_sessions=6
owner_leases.active=6
workspace_leases.active=6
attention=[]
```

6 个 summary 都显示：

```text
liveness=working
attention=none
needs_attention=false
binding_source=env
last_progress=hook.SessionStart
projection_stale=false
runtime=unknown
workspace=D:\guKimi
warnings=["projection missing; default path did not scan cold logs"]
```

但事件列表只包含：

```text
pty.session_started
command.accepted
pty.input_sent
command.completed
hook.SessionStart
```

没有任何：

```text
hook.UserPromptSubmit
hook.PreToolUse
hook.PostToolUse
hook.PostToolBatch
```

### 并发压力暴露的 P0 问题

抽查 `stress-v55-control` 和 `stress-v55-mcp-http` 的 debug screen 后，结论明确：

**route prompt 文本停在 Claude Code 输入框里，没有被提交。**

也就是说：

```text
pty.input_sent != Claude accepted prompt
command.completed != UserPromptSubmit
input_sent_by_actor != task started
```

当前 route start 只证明 daemon 把字节写进了 PTY，不证明 Claude Code 接受了 user turn。

### MCP continue/send 的恢复失败

对 `stress-v55-control` 试了一次：

```text
agentcall_session_send(action=continue)
```

返回：

```json
{
  "ok": true,
  "status": "queued_until_next_hook_injection",
  "delivery": "PostToolBatch_or_next_context_hook",
  "post_tool_batch_seen": false
}
```

这暴露了第二个核心问题：

- MCP 看到 `liveness=working` 且 `attention=none`，就把 supervisor 输入转成 hook queue；
- 但 session 还没有真正进入 Claude turn，也没有 PostToolBatch；
- 因此 queued instruction 没有可靠注入机会。

这个设计适合“Claude 正在执行中，不要强塞新 prompt”，但不适合“输入框已有未提交 route prompt”的 pre-turn 卡住状态。

### 并发 stop 与资源回收

刷新 6 个 session summary 后，分别拿到新 control token，并发调用：

```text
agentcall_session_send(action=stop)
```

6 个 stop 全部返回：

```json
{
  "ok": true,
  "status": "stop_signal_sent",
  "lease_release": "pending_process_exit"
}
```

等待后 board 显示：

```text
live_daemon_sessions=0
owner_leases.active=0
workspace_leases.active=0
```

并发回收成功。

### 并发测试额外发现

#### 1. command-status 有残留 accepted

压力测试后，`command-status.ndjson` 中 6 个 stop command 仍停在：

```text
status=accepted
```

board 已经显示 session exited 和 lease released，所以资源释放不受影响；但 command 追踪状态没有完成闭环，会污染后续排查。

#### 2. board route 状态不一致

部分旧 route 仍显示：

```text
status=started_pending_prompt_ack
workflow_status=running
```

但对应 live daemon session 已不存在。

这说明 route projection 对 orphan/stale route 的收敛仍不够。

#### 3. projection 字段漂移

并发 session 的 summary 里：

```text
runtime=unknown
workspace=D:\guKimi
```

但 route 实际 runtime 是 PTY，target workspace 是：

```text
E:\Project\AgentCall
```

`D:\guKimi` 是 Claude process cwd，不应该覆盖 task workspace。

#### 4. 日志安全仍有风险

最近 hook 日志中出现了其他 unbound Claude session 的 PowerShell command，其中包含敏感 env/API key 文本。

这不是本轮 stress worker 产生的，但它在同一次日志检查中暴露了一个红线：

```text
tool_input.command 也必须脱敏，不能只 artifact 化 stdout/stderr。
```

## 第二部分：单 session 强行闭环测试

### 测试目标

在不追求优雅的前提下，验证一个 daemon-owned Claude Code PTY worker 是否能完成完整生命周期：

```text
route start
prompt submit
UserPromptSubmit hook
Read files
Write report
PostToolBatch
Stop/SubagentStop
report_ready
accept/report check
control-token stop
lease cleanup
```

### Session 信息

```text
session=single-flow-smoke-v1
route_id=route-1489058
workspace=E:\Project\AgentCall
claude_cwd=D:\guKimi
report_path=E:\Project\AgentCall\.agentcall\reports\single-flow-smoke-v1.md
```

任务内容：

```text
Read README.md and AGENTS.md only.
Create exactly one report file at:
E:\Project\AgentCall\.agentcall\reports\single-flow-smoke-v1.md
with sections: status, files_read, findings, completion.
```

Allowed paths：

```text
E:\Project\AgentCall\.agentcall\reports
E:\Project\AgentCall\README.md
E:\Project\AgentCall\AGENTS.md
```

### 正常 route 启动

`agentcall_route(mode=start, runtime=pty)` 成功启动 session。

route 返回：

```json
{
  "status": "started_pending_prompt_ack",
  "prompt": {
    "status": "started_pending_prompt_ack",
    "expected_hook": "UserPromptSubmit",
    "actor_result": {
      "ok": true,
      "status": "input_sent_by_actor"
    }
  }
}
```

初始事件：

```text
evt-040293 pty.session_started
evt-040294 command.accepted
evt-040295 pty.input_sent
evt-040296 command.completed
evt-040300 hook.SessionStart
```

### 再次复现 prompt 卡输入框

等待后 debug screen 仍显示 route prompt 在 Claude Code 输入框内。

没有 `UserPromptSubmit`。

这与 6 并发压力测试的失败模式完全一致。

### MCP send 仍无法救场

尝试：

```text
agentcall_session_send(action=send, text=" ", enter=true)
```

返回 queued supervisor instruction：

```json
{
  "ok": true,
  "status": "queued_until_next_hook_injection",
  "post_tool_batch_seen": false
}
```

这说明 MCP 控制层仍把该 session 当作“正在工作”，而不是“待提交输入框”。

### 直接 daemon HTTP input 强行提交

绕过 MCP 后，直接调用：

```powershell
POST http://127.0.0.1:3293/api/sessions/single-flow-smoke-v1/input
body: {
  "text": " ",
  "enter": true,
  "idempotency_key": "single-flow-smoke-v1-http-space-enter-1"
}
```

daemon 返回：

```json
{
  "ok": true,
  "status": "input_sent_by_actor"
}
```

随后 session 立刻进入正常 Claude Code 工作流。

### 成功事件链

HTTP 强行提交后，出现完整事件链：

```text
evt-040312 hook.UserPromptSubmit
evt-040313 hook.PreToolUse Read
evt-040314 hook.PreToolUse Read
evt-040315 hook.PostToolUse Read README.md
evt-040316 hook.PostToolUse Read AGENTS.md
evt-040317 hook.PostToolBatch
evt-040318 hook.PreToolUse Write .agentcall/reports/single-flow-smoke-v1.md
evt-040320 hook.PostToolUse Write .agentcall/reports/single-flow-smoke-v1.md
evt-040321 hook.PostToolBatch
evt-040322 hook.Stop
evt-040323 hook.SubagentStop
```

这证明 daemon session 一旦 prompt 被真正提交，后续 hook、permission、report、summary 链路可以跑通。

### 报告产出

报告文件成功创建：

```text
E:\Project\AgentCall\.agentcall\reports\single-flow-smoke-v1.md
```

内容摘要：

```text
status: SUCCESS

files_read:
- E:\Project\AgentCall\README.md
- E:\Project\AgentCall\AGENTS.md

findings:
- AgentCall v5.3.0 checkpoint
- PTY-first workers
- Rust daemon authority
- Codex parent / Claude Code worker split
- v5.4 plan frozen
- hooks installed to D:\guKimi\.claude\settings.local.json
- no source files modified

completion:
Report written. Worker idle.
```

### report accept 与 stop

`agentcall_report(action=accept)` 返回：

```json
[]
```

这个返回值太弱，但没有阻断。

第一次 stop 使用旧 token，被正确拒绝：

```json
{
  "ok": false,
  "status": "stale_control_token",
  "reason": "control_epoch changed from 1 to 2"
}
```

刷新 summary 后，使用新 token stop 成功：

```json
{
  "ok": true,
  "status": "stop_signal_sent",
  "lease_release": "pending_process_exit"
}
```

最终 board：

```text
live_daemon_sessions=0
owner_leases.active=0
workspace_leases.active=0
route single-flow-smoke-v1=session_exited
```

## 综合判断

### 已经证明可行的部分

| 能力 | 结论 |
|---|---|
| daemon 启动 PTY | 可用 |
| 6 并发 session 上限 | 可用 |
| read-only shared lease | 可用 |
| exclusive workspace lease | 单 session 可用 |
| env hook binding | 可用 |
| hook event ingest | prompt 提交后可用 |
| allowed paths report write | 可用 |
| report_ready | 可用 |
| control token stale rejection | 可用 |
| stop 后 lease cleanup | 可用 |

### 当前最关键的问题

| 优先级 | 问题 | 影响 |
|---|---|---|
| P0 | route prompt 写入 PTY 后没有可靠提交 | worker 看似启动，实际没开始 |
| P0 | `prompt_ack_missing` 没进入 attention | Codex 被误导为继续等待 |
| P0 | MCP `send/continue` 在 pre-turn stuck 状态下进入 hook queue | 无法恢复卡住的输入框 |
| P1 | HTTP input 能直接写 PTY，MCP 却不能表达同样动作 | 控制能力在接口层割裂 |
| P1 | route/session workspace/cwd 投影混淆 | Codex 判断任务归属困难 |
| P1 | command-status 对 stop 命令有 accepted 残留 | 命令追踪不干净 |
| P1 | `agentcall_report accept` 返回空数组 | 验收体验不闭合 |
| P2 | raw hook command 可能泄露敏感 env | 日志安全风险 |

## 根因分析

### 根因 1：prompt delivery contract 太弱

现在系统隐含把下面三件事混在一起：

```text
1. daemon 写 PTY bytes 成功
2. Claude TUI 输入框接受并提交
3. Claude Code 触发 UserPromptSubmit hook
```

但真实世界里这三层会分裂。

本轮并发和单 session 都证明：

```text
1 成功，不代表 2 成功；
2 不成功，就永远没有 3。
```

### 根因 2：summary 把 pre-turn stuck 误判成 working

session 只有 `SessionStart`，没有 `UserPromptSubmit`，但是 summary 显示：

```text
liveness=working
attention=none
```

这导致 MCP 控制层进一步做错选择。

### 根因 3：MCP send 策略没有区分“真 mid-turn”和“假 working”

`crates/agentcall-daemon/src/mcp.rs` 当前逻辑：

```rust
if liveness_status == "working" && attention_status == "none" {
    command.command_type = CommandType::QueueSupervisorInstruction;
}
```

这个逻辑保护了 mid-turn Claude，不让 Codex 乱塞 prompt。

但在 prompt gate pending 且输入框未提交时，它会导致恢复路径失效。

### 根因 4：HTTP 和 MCP 的控制语义不一致

HTTP：

```text
POST /api/sessions/{name}/input
```

可以直接进入 SendInput actor。

MCP：

```text
agentcall_session_send(action=send)
```

会根据 summary 状态改成 queue。

这不是纯粹 bug，但目前没有给 Codex 一个安全、明确、低负担的“提交当前输入框”动作。

## 建议修复路线

### Fix 1：新增 `prompt_ack_missing` / `prompt_stuck_in_input` 一等状态

当 route prompt gate pending，并且没有 `UserPromptSubmit`，且 screen 显示输入框仍有 route prompt 时：

```json
{
  "liveness_status": "waiting_input",
  "attention_status": "prompt_ack_missing",
  "needs_attention": true,
  "recommended_action": "submit_pending_prompt"
}
```

不要再显示：

```text
working / none
```

### Fix 2：新增受限 MCP action：`submit_pending_prompt`

这个 action 只做一件事：

```text
提交当前 Claude Code 输入框里的已有内容
```

它应该绕过 hook queue，直接走 SendInput actor，但必须有严格 gate：

- 当前 session 是 daemon PTY；
- route prompt gate pending；
- 无 `UserPromptSubmit`；
- screen/output 证明处于 input prompt；
- action 有 control token 或 route-owned command 身份；
- 每个 route prompt 最多自动/手动提交一次。

### Fix 3：route start 自愈一次

route 启动后，如果 N 秒内没有 `UserPromptSubmit`，daemon 可以做一次 bounded self-heal：

```text
send Enter 或 " " + Enter
```

并记录：

```text
prompt_gate.self_heal_submit_attempted
```

如果仍失败，则 route 进入：

```text
prompt_ack_missing
```

而不是继续假装 running。

### Fix 4：board route projection 以 prompt gate 为准

board 中 route 不应该只显示：

```text
started_pending_prompt_ack
```

还应明确：

```text
ack_deadline_exceeded=true
attention=prompt_ack_missing
suggested_action=submit_pending_prompt_or_restart
```

### Fix 5：清理 workspace/cwd/runtime 投影

summary 应拆开：

```text
target_workspace=E:\Project\AgentCall
claude_cwd=D:\guKimi
runtime=pty
```

不要让 `hook.SessionStart.cwd` 覆盖 route workspace。

### Fix 6：命令状态闭环

对于 stop/kill 这类 awaiting observation 的命令：

- 进程退出后应把 command-status 从 `accepted` 推进到 `observed_completed` 或类似状态；
- 否则后续排查会看到一堆“已 accepted 但不知道结局”的控制命令。

### Fix 7：报告验收返回结构化结果

`agentcall_report(action=accept)` 至少返回：

```json
{
  "ok": true,
  "status": "accepted",
  "session_id": "...",
  "report_path": "...",
  "validation": "passed"
}
```

空数组对 Codex 和人类都没有解释力。

### Fix 8：hook command 脱敏

对 hook raw payload 中的：

```text
tool_input.command
env assignment
API key / token patterns
```

必须统一 redaction。

这应该优先于继续扩大日志内容。

## 可接受的临时操作规程

在修复前，如果必须让单个 daemon session 落地，可采用临时手法：

1. `agentcall_route(mode=start, runtime=pty)` 启动；
2. 等待 5-15 秒；
3. 如果没有 `UserPromptSubmit`，查看 debug screen；
4. 如果 prompt 卡在输入框，调用 daemon HTTP：

```powershell
$cfg = Get-Content -Raw config\agentcall.local.json | ConvertFrom-Json
$headers = @{ 'x-agentcall-token' = $cfg.daemon_token }
$body = @{
  text = ' '
  enter = $true
  idempotency_key = '<unique-key>'
} | ConvertTo-Json -Compress
Invoke-RestMethod `
  -Uri 'http://127.0.0.1:3293/api/sessions/<session>/input' `
  -Method Post `
  -Headers $headers `
  -ContentType 'application/json' `
  -Body $body
```

5. 观察 `UserPromptSubmit`；
6. 等 report；
7. 刷新 summary 拿新 control token；
8. `agentcall_session_send(action=stop)` 回收。

这不是产品方案，只是当前的救火流程。

## 最终判断

AgentCall 的核心 PTY worker 路线没有死，反而这次测试证明它是能跑通的。

真正要修的是控制面语义：

```text
route prompt gate
MCP send/recovery action
attention projection
workspace/cwd projection
command/report closure
```

如果只看结果：

- **6 并发能启动和回收，但 6 个都没有真正开始任务。**
- **单 session 能完整落地，但需要绕过 MCP，直接打 daemon HTTP input。**

所以优先级应该是：

```text
P0: prompt_ack_missing 状态 + submit_pending_prompt 动作
P1: route prompt 自愈 + board attention
P1: projection 字段修正 + command/report closure
P2: hook raw command 脱敏
```
