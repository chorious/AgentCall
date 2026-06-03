# AgentCall Codex 运行状态诊断报告

## Summary

本轮检查的结论是：Codex 本体、AgentCall MCP、Rust daemon 当前都没有整体卡死；真正的问题集中在 AgentCall 对 Codex/PTY 的观测链路。

当前 MCP 到 daemon 的连接已经不像上一轮那样保持卡死的 `ESTABLISHED` 状态，3293 端口只有 daemon 在监听和若干 `TIME_WAIT`。但是 Codex hook 输入发生编码损坏，新 PTY session 没有和 Claude hook 形成 binding，PTY clean output 也为空，因此 wrapper 层对当前 Claude 工作状态基本失明。

## Evidence

- `agentcall-daemon.exe` PID `29688` 正常运行，`/api/runtime/health` 可快速返回。
- `agentcall-mcp.exe` PID `23436` 是 Codex PID `25284` 的子进程，CPU 为 0，进程响应正常。
- `netstat` 显示 `127.0.0.1:3293` 只有 `LISTENING` 和 `TIME_WAIT`，没有长期卡住的 `ESTABLISHED`。
- daemon 当前有 3 个 live PTY session：
  - `androidpet-v26-gpu`
  - `androidpet-v26-gpu-dgukimi`
  - `androidpet-v26-gpu-kimi`
- 这 3 个 live session 全部是 `binding_source=unbound`，summary 只有低置信度的 `working/unbound`。
- `androidpet-v26-gpu-kimi` 的 child `claude.exe` 存在，但 `clean_output` 为空，`replay_bytes=4`，没有有效 TUI 输出。
- `.agentcall/state/runtime_binding.json` 只有旧的 `androidpet-v25` binding，没有 v26 binding。
- Codex hook 已经进入 daemon events，但 `UserPromptSubmit` 和 `Stop.last_assistant_message` 中文文本已变成 mojibake。

## Root Causes

### 1. Codex hook UTF-8 入口损坏

`.codex/hooks.json` 当前调用：

```text
C:/ProgramData/anaconda3/python.exe E:/Project/AgentCall/scripts/agentcall-codex-hook.py ...
```

该 Python 运行时当前编码状态为：

```text
defaultencoding = utf-8
preferredencoding = cp932
stdin.encoding = cp932
stdout.encoding = cp932
PYTHONUTF8 = None
PYTHONIOENCODING = None
```

所以 `scripts/agentcall-codex-hook.py` 中的 `sys.stdin.read()` 会在入口处按 Windows 默认编码读取 hook JSON，中文在进入 daemon 前已经损坏。后续 `sanitize()` 只能替换非法字符，不能恢复原文。

### 2. v26 PTY 没有形成 hook-aware binding

daemon 已经为 PTY 注入 `AGENTCALL_WRAPPER_SESSION`，但当前 v26 session 没有出现在 `runtime_binding.json` 中。这说明新 session 没有产生可绑定的 Claude hook，或者 Claude 没有真正进入会触发 hook 的工作流阶段。

结果是 `session_summary` 无法使用 hook-derived 状态，只能显示 `unbound` 和低置信度状态。

### 3. PTY session 活着但不可观测

`androidpet-v26-gpu-kimi` 的 Claude 进程存在，daemon 也记录了 `pty.input_sent`，但 PTY 输出为空、没有 hook binding、没有 report-ready 信号。对 Codex 来说，这不是一个可验收的 handoff 状态。

### 4. board attention 噪声过高

compact board 里同时展示：

- 3 个 unbound live daemon session。
- 8 个 legacy detached session。

这会让 Codex 的默认读取成本变高，也会掩盖真正需要处理的 live session 问题。

### 5. stop 接口存在锁等待问题

已确认 `spawn_waiter()` 持有 `session.child` 锁调用 `child.wait()`，而 `stop_session()` 也需要同一把锁才能 `kill()`。因此旧错误 session 可能无法及时 stop，进一步造成旧 session 残留和 board 噪声。

## Current Assessment

当前 AgentCall 的实际状态：

- MCP 通。
- daemon 通。
- Codex hook 能触发，但中文输入已乱码。
- PTY session 能启动，但当前 v26 没有有效输出和 binding。
- summary-first 的读取链路存在，但当前数据源质量不足。

因此这不是 Codex 本体崩溃，而是 AgentCall wrapper/observability 闭环没有真正成立。

## Recommended Next Steps

1. 修 Codex hook UTF-8：确保 hook 子进程以 UTF-8 读取 stdin/stdout，尤其是 `PYTHONUTF8=1` 或显式重包 `sys.stdin.buffer`。
2. 修 stop：使用 `portable-pty` 的 `clone_killer()` 或独立 killer handle，避免 waiter 持有 child 锁阻塞 stop。
3. 清理旧 legacy detached session 噪声，compact board 默认不展示 legacy。
4. 重启全套：daemon、MCP、viewer、旧 Claude PTY。
5. 重新拉一个干净 PTY session，验证：
   - clean output 非空。
   - hook event 有 readable 中文。
   - runtime binding 写入新 wrapper session。
   - summary 能从 hook-derived 状态读取。

