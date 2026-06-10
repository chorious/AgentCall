# AgentCall v5.2 代码健壮性审查报告

> 审查范围：Rust daemon/MCP 请求路径、Windows PTY/process cleanup、Python 脚本/hook installer、测试覆盖
> 审查日期：2026-06-09
> 审查模式：只读，未修改生产代码

---

## 目录

1. [最值得先修的 5 项（优先级排序）](#1-最值得先修的-5-项)
2. [高风险 Bug](#2-高风险-bug)
3. [健壮性风险](#3-健壮性风险)
4. [性能风险](#4-性能风险)
5. [测试覆盖缺口](#5-测试覆盖缺口)
6. [最可能导致 MCP 慢 / transport closed / worker 卡住的代码点](#6-最可能导致-mcp-慢--transport-closed--worker-卡住的代码点)

---

## 1. 最值得先修的 5 项

| 优先级 | 问题 | 证据路径 | 影响 |
|---|---|---|---|
| **P0** | MCP `READ_TIMEOUT = 10s` 对 board full / 大量 event 请求极易超时 | `crates/agentcall-mcp/src/daemon_client.rs:8` | Codex/Claude 将 MCP transport 标记为 closed，工具完全不可用 |
| **P0** | Actor `write_input` 中 80ms `thread::sleep` 阻塞单线程 actor | `crates/agentcall-daemon/src/actor.rs:29` | Session 命令串行排队，supervisor 连续操作累计延迟显著，worker 感觉"卡住" |
| **P1** | `ingest_hook` 使用全局 `state_writer` Mutex 串行所有 hook | `crates/agentcall-daemon/src/hooks.rs:180` | 高频 hook（PreToolUse/PostToolBatch）并发时锁竞争严重，吞吐瓶颈 |
| **P1** | `spawn_reader` 隐藏所有 I/O 错误，不记录诊断信息 | `crates/agentcall-daemon/src/session.rs:230` | PTY 断连时无法定位根因，只能猜测是进程退出或管道断裂 |
| **P1** | `session_has_seen_hook_event` 每次线性扫描 events 文件 | `crates/agentcall-daemon/src/mcp.rs:462` | 活跃 session 的 events 累积后，每次 session_send 都 O(n) 扫描，越来越慢 |

---

## 2. 高风险 Bug

### 2.1 `spawn_reader` 静默吞掉所有 I/O 错误（高）

**位置**：`crates/agentcall-daemon/src/session.rs:230`
```rust
Err(_) => break,
```

`reader.read()` 返回的任何 `Err`（包括 `EAGAIN`、`EWOULDBLOCK`、`BrokenPipe`、`TimedOut`）都被 `_` 模式匹配忽略，直接退出 reader loop。没有日志、没有事件记录、没有状态更新。

**后果**：PTY reader 意外退出后，session 的 `status` 仍为 `"running"`（因为没有进入 `spawn_waiter` 的 `child.wait()` 路径），supervisor 看到 session "running" 但没有任何新输出，误判为 worker 卡住。

**复验**：构造一个让 `portable_pty` reader 返回 `TimedOut` 的场景（如系统负载高），观察 reader 线程静默死亡但 session 状态不变。

### 2.2 `agentcall-mcp` stdin JSON 解析无大小限制（中高）

**位置**：`crates/agentcall-mcp/src/protocol.rs:23`
```rust
let response = match serde_json::from_str::<Value>(line) {
```

`stdin.lock().lines()` 读取的每行 JSON 没有预检查长度。如果 MCP host 发送超大请求（如包含大型文件内容的 tool call），`serde_json::from_str` 会在堆上分配与输入大小成正比的内存，可能导致 OOM。

**后果**：agentcall-mcp 进程被 OS kill，Codex/Claude 侧看到 "transport closed"。

**复验**：向 agentcall-mcp stdin 发送一行 >100MB 的 JSON，观察内存使用。

### 2.3 `actor.rs` 中 `reply_rx.recv_timeout(Duration::from_secs(5))` 对慢命令可能超时（中）

**位置**：`crates/agentcall-daemon/src/actor.rs:105`
```rust
reply_rx
    .recv_timeout(Duration::from_secs(5))
    .map_err(|err| format!("session actor command timeout: {err}"))?
```

`stop_session` 命令在 Windows 上需要遍历进程树（`tasklist`），可能超过 5 秒。如果超时，调用方收到 `session actor command timeout` 错误，但 actor 实际上仍在执行 stop，导致状态不一致。

**后果**：supervisor 看到 stop 失败，可能重试 stop，但第一次 stop 已经发出，第二次对已经停止的 session 操作产生混乱错误。

**复验**：在 Windows 上启动一个深层子进程树（如 `powershell` 启动 `node` 启动 `claude`），然后调用 stop session。

### 2.4 `http.rs` `write_fixed` 在 response 后 `shutdown(Write)` 但 SSE/WebSocket 路径不关闭（中）

**位置**：`crates/agentcall-daemon/src/http.rs:693`
```rust
let _ = stream.shutdown(Shutdown::Write);
```

`write_fixed`（普通 HTTP JSON 响应）在写完后调用 `shutdown(Write)`，但 SSE (`write_sse`) 和 WebSocket (`write_ws`) 路径没有这个调用。虽然 SSE/WS 是长连接，但在异常退出时可能导致半开连接（half-open connections）累积。

**后果**：大量 SSE/WS 客户端断连但未正确关闭 TCP 连接，文件描述符泄漏。

### 2.5 `store_json.rs` `append_rotating_ndjson` 的 rotate 逻辑可能切断多字节 UTF-8（中）

**位置**：`crates/agentcall-daemon/src/store_json.rs:366-392`
```rust
existing.seek(SeekFrom::End(-(keep as i64)));
// ... 找第一个换行符截断
```

`seek(End(-keep))` 从末尾反向 `keep` 字节处开始读取。如果恰好落在一个多字节 UTF-8 字符中间，`find('\n')` 可能找不到换行符（因为中间字节可能碰巧是 `0x0A` 或不是），导致截断后的文件首行包含损坏的 UTF-8。

**后果**：`read_events` 解析该文件时，`serde_json::from_str` 在损坏行上失败，导致事件丢失。

---

## 3. 健壮性风险

### 3.1 Windows PTY/Process Cleanup

#### 3.1.1 `ProcessHandle::create` 在 `child_pid = None` 时完全退化为 best_effort

**位置**：`crates/agentcall-daemon/src/process.rs:31-49`

如果 `portable_pty` 的 `process_id()` 返回 `None`（在 Windows 上某些配置下可能发生），则无法创建 Windows Job Object，子进程清理退化为 "best_effort_parent_kill_only"。子进程的子进程（grandchildren）将无法被清理。

**风险**：Claude Code 本身可能启动子进程（如 `git`、`npm`），这些子进程在 PTY kill 后成为僵尸。

#### 3.1.2 `WindowsJobHandle::Drop` 的 `handle != 0` 判断不严谨

**位置**：`crates/agentcall-daemon/src/process.rs:181-189`
```rust
impl Drop for WindowsJobHandle {
    fn drop(&mut self) {
        unsafe {
            if self.handle != 0 {
                CloseHandle(self.handle as _);
            }
        }
    }
}
```

`handle` 类型是 `isize`。`CreateJobObjectW` 失败时返回 `null`（即 `0`），但合法的 handle 也可能是负数（因为 `isize` 的 `null` 是 `0`）。条件 `self.handle != 0` 对负数 handle 是正确的，但如果某次 `assign` 错误后 handle 被设为 `0` 以外的值（如 `-1`），这里会尝试关闭无效 handle。

当前代码中 `assign` 只在成功时创建 `WindowsJobHandle`，所以实际上安全，但这是隐式约定，没有显式防御。

#### 3.1.3 `spawn_waiter` 的 `child.wait()` 在 Windows 上可能被 defunct 子进程阻塞

**位置**：`crates/agentcall-daemon/src/session.rs:236-288`

`spawn_waiter` 线程调用 `child.wait()` 等待进程退出。在 Windows 上，如果子进程有未处理的句柄引用（如被另一个进程通过 `OpenProcess` 持有），进程对象可能保持 "zombie" 状态，`wait()` 返回但资源未释放。更严重的是，如果子进程本身通过 `CreateProcess` 创建了孙进程且孙进程未退出，`wait()` 实际上只等待直接子进程。

**风险**：`WindowsJob` 的 `TerminateJobObject` 会终止 Job 中所有进程，但如果 assign 失败，或者子进程在 assign 之前就已经创建了孙进程，清理可能不彻底。

#### 3.1.4 `start_session` 中 `safe_name` 不拒绝空字符串但后续使用不安全

**位置**：`crates/agentcall-daemon/src/util.rs:20-25`
```rust
pub(crate) fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}
```

`safe_name` 检查了 `!name.is_empty()`，这是正确的。但 `start_session` 在 `resolve_session_cwd` 中使用 `req.name` 构造路径时没有进一步 sanitize（如防止 `".."` 或 `"."`）。虽然 `safe_name` 已经过滤了这些字符，但如果将来放宽 `safe_name`，可能引入路径遍历风险。

**当前状态**：安全，因为 `safe_name` 足够严格。但建议保留防御性检查。

#### 3.1.5 `canonical_workspace_key` 在路径不存在时的 fallback 不一致

**位置**：`crates/agentcall-daemon/src/ownership.rs:320-331`
```rust
pub(crate) fn canonical_workspace_key(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    });
    normalize_workspace_key(&canonical)
}
```

如果 `path.canonicalize()` 失败（路径不存在），fallback 使用原始路径。但在 Windows 上，`C:\Project` 和 `c:\project` 是同一目录但字符串不同。`normalize_workspace_key` 做了 `to_ascii_lowercase()`，这是好的，但如果路径包含 `\\?\` 前缀（Windows 长路径），`strip_prefix` 处理了这种情况。

**风险**：如果 workspace 路径在 lease 创建时存在（canonicalize 成功），但后续被删除再重新创建（canonicalize 在另一个路径上成功），可能产生不同的 `workspace_key`，导致 lease 冲突检查失效。

### 3.2 Python 脚本和 Hook Installer

#### 3.2.1 `agentcall-claude-hook.py` 的 daemon ingest 超时固定为 5 秒

**位置**：`scripts/agentcall-claude-hook.py:84`
```python
with urllib.request.urlopen(request, timeout=5) as response:
```

Claude 的 hook timeout 配置为 30 秒（`install_claude_hooks.py:68`），但 hook 脚本内部对 daemon 的请求只有 5 秒超时。在 daemon 负载高或 `state_writer` 锁竞争激烈时，5 秒可能不足。

**后果**：hook 超时后 Claude 侧看到 hook 执行失败，但 daemon 实际上可能仍在处理该请求，导致重复 ingest 或状态不一致。

**建议**：将 hook 脚本超时增加到 25 秒，或使其可配置。

#### 3.2.2 `agentcall-codex-hook.py` 缺少 `PostToolBatch` 事件支持

**位置**：`scripts/agentcall-codex-hook.py:30`

Codex hook 只注册了 `SessionStart`, `UserPromptSubmit`, `Stop`, `PreCompact`, `PostCompact`，没有 `PostToolBatch`。这意味着 Codex 会话无法接收 queued supervisor instructions（这些指令依赖于 `PostToolBatch` hook 来触发 context injection）。

**后果**：通过 `agentcall_session_send` 的 `QueueSupervisorInstruction` 对 Codex 会话永远不会被投递。

#### 3.2.3 `agentcall-codex-hook.py` 的 `ingest` 函数没有区分 daemon 不可达和 daemon 返回错误

**位置**：`scripts/agentcall-codex-hook.py:66-71`
```python
except (OSError, urllib.error.URLError, urllib.error.HTTPError) as exc:
    print(f"AgentCall daemon ingest failed: {exc}", file=sys.stderr)
    return {}
```

所有网络错误都被捕获并返回 `{}`，调用方无法区分 daemon 未启动 vs daemon 拒绝请求 vs 超时。

**后果**：supervisor 看到 hook 输出正常但 context injection 为空，无法判断是 daemon 问题还是真的没有待注入内容。

#### 3.2.4 `install_codex_hooks.py` 使用 `shell_token` 拼接命令字符串而非列表

**位置**：`scripts/install_codex_hooks.py:34-41`
```python
command = " ".join([
    shell_token(args.python),
    shell_token(str(hook_script)),
    ...
])
```

Codex 的 hook JSON 支持 `args` 数组格式（`install_claude_hooks.py` 就使用了），但 Codex installer 却将命令拼接为字符串。在 Windows 上，包含空格的路径（如 `C:\Program Files\Python\python.exe`）可能导致解析错误。

**后果**：如果 Python 安装路径包含空格，hook 命令可能解析失败。

#### 3.2.5 `codex_mcp_transport_recovery.py` 的 `terminate_process` 在 Windows 上使用 `Stop-Process -Force`

**位置**：`scripts/codex_mcp_transport_recovery.py:251-258`
```python
subprocess.check_call(
    ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", f"Stop-Process -Id {pid} -Force"]
)
```

`-Force` 会强制终止进程且不等待子进程清理。如果 Codex 进程正在写入文件，可能导致文件损坏。

**风险**：低，因为这是 recovery 脚本的显式 opt-in 操作（需要 `--yes`）。

### 3.3 配置读取与兼容风险

#### 3.3.1 `LocalConfig::load` 在 `config/agentcall.local.json` 不存在时使用 `LocalConfig::default()`

**位置**：`crates/agentcall-daemon/src/config.rs:14-24`

如果配置文件缺失，`claude_workspace` 为 `None`，后续 `configured_claude_workspace` 返回错误。但 daemon 仍然启动，只是所有需要 `claude_workspace` 的操作（如启动 claude PTY）会失败。

**风险**：用户可能误以为 daemon "正常运行"，但所有 route start 都返回配置错误。

#### 3.3.2 `agentcall-mcp` 的 `Config::from_args` 对 `--python` 参数只消费但不使用

**位置**：`crates/agentcall-mcp/src/config.rs:25-28`
```rust
"--python" => {
    index += 1;
    let _ = args.get(index).ok_or("missing --python value")?;
}
```

`--python` 参数被解析但值被丢弃。如果 MCP server 配置中包含 `--python`，不会报错但也不起作用，用户可能误以为指定了 Python 路径。

---

## 4. 性能风险

### 4.1 `read_events` 每次调用读取 2MB 并逐行解析 JSON

**位置**：`crates/agentcall-daemon/src/state.rs:128-141`
```rust
pub(crate) fn read_events(path: &Path) -> Vec<serde_json::Value> {
    let event_path = recent_events_path_for(path).unwrap_or_else(|| path.to_path_buf());
    let Some(text) = read_tail_text(&event_path, READ_TAIL_BYTES) else {  // 2MB
        return vec![];
    };
    let mut events: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if events.len() > RECENT_EVENT_LIMIT {  // 80
        events = events.split_off(events.len() - RECENT_EVENT_LIMIT);
    }
    events
}
```

`board_state` 的 full view 调用 `read_events`，每次请求都读取 2MB、逐行 `serde_json::from_str`、保留最后 80 条。如果 event 密度高（如每条几百字节），2MB 可能包含数千条 event，解析开销大。

**证据**：`RECENT_EVENT_LIMIT = 80`，但 `READ_TAIL_BYTES = 2MB`。如果平均 event 大小为 200 字节，2MB 包含约 10000 条 event，解析 10000 条 JSON 只保留 80 条，浪费严重。

### 4.2 `board_attention_projection` 使用 SQLite 时做全表扫描

**位置**：`crates/agentcall-daemon/src/store_sqlite.rs:137-158`
```rust
let sql = if query.attention_only {
    "SELECT projection_json FROM projections WHERE needs_attention = 1 ORDER BY updated_at DESC"
} else {
    "SELECT projection_json FROM projections ORDER BY updated_at DESC"
};
```

`needs_attention = 1` 的 WHERE 条件虽然减少了行数，但如果 projections 表没有索引（schema 中确实没有 `needs_attention` 的索引），这仍是全表扫描。SQLite 的 `WAL` 模式缓解了并发写入问题，但读取仍可能慢。

**建议**：在 `projections(needs_attention, updated_at)` 上创建复合索引。

### 4.3 `session_has_seen_hook_event` 和 `last_supervisor_instruction_injected_at` 线性扫描 events

**位置**：
- `crates/agentcall-daemon/src/mcp.rs:462-475`
- `crates/agentcall-daemon/src/summary.rs:797-822`

两者都调用 `read_events`（读取 2MB），然后在内存中反向扫描。对于活跃 session，events 文件不断增长，每次调用的 O(n) 开销线性增长。

**计算**：如果 session 运行 1 小时，每秒 2 个 hook event，则 7200 条 event。每次 `session_send` 都要扫描这 7200 条。如果 supervisor 每分钟发送 10 次 session_send，每小时扫描 72000 条。

### 4.4 `count_reports` 递归遍历目录树

**位置**：`crates/agentcall-daemon/src/hooks.rs:361-388`

`context_injection` 每次 `SessionStart` / `UserPromptSubmit` / `PostToolBatch` hook 都调用 `count_reports`，递归遍历 `.agentcall/tasks/` 下的所有目录和文件。如果任务数量多（如数十个），每次 hook 都有数十次目录枚举。

**建议**：缓存报告计数，或在 project.json 中维护计数器。

### 4.5 `plan_artifact_from_binding` 逐行解析整个 transcript JSONL 文件

**位置**：`crates/agentcall-daemon/src/summary.rs:1221-1285`

`session_plan_artifact` 读取整个 transcript 文件（可能数 MB 的 JSONL），逐行解析 `serde_json::from_str`，只查找 `plan_mode` 和 `ExitPlanMode` 相关的记录。没有限制读取行数或提前退出。

**建议**：从文件末尾反向读取，或限制扫描行数（plan 相关事件通常出现在最近的几百行内）。

### 4.6 `agentcall-mcp` `tool_text` 对大响应做双重 JSON 序列化

**位置**：`crates/agentcall-mcp/src/protocol.rs:111-138`
```rust
let text = serde_json::to_string(&value).unwrap();  // 第一次
let original_bytes = text.len();
let (text, truncated) = if original_bytes > TOOL_TEXT_CAP_BYTES {
    (
        serde_json::to_string(&json!({"truncated": true, ...}))  // 第二次
        .unwrap(),
        true,
    )
} else { (text, false) };
```

对 >128KB 的响应，先完整序列化一次获取大小，再序列化 truncated wrapper。这意味着 10MB 的响应会被序列化两次，产生 20MB 的临时字符串。

**建议**：使用 `serde_json::to_vec` 获取字节数，或实现自定义 `Serialize` 来计算大小而不实际分配。

### 4.7 `broadcast` 每次 clone StreamEvent 到所有 clients

**位置**：`crates/agentcall-daemon/src/session.rs:481-484`
```rust
pub(crate) fn broadcast(session: &Arc<Session>, event: StreamEvent) {
    let mut clients = session.clients.lock().unwrap();
    clients.retain(|tx| tx.send(event.clone()).is_ok());
}
```

`StreamEvent` 包含 `data: String`，每次 broadcast 都 clone 该 String 到所有 clients。如果有 10 个 SSE 连接，每行 PTY 输出都要 clone 10 次。PTY 输出频率可能很高（如编译大量文件时每秒数百行）。

**建议**：使用 `Arc<str>` 或 `Arc<String>` 共享 event data。

---

## 5. 测试覆盖缺口

### 5.1 并发/压力测试

| 缺失场景 | 影响 | 建议 |
|---|---|---|
| 并发 `agentcall_session_send` 到同一 session | actor 命令队列顺序和去重未经验证 | 写 test：10 个线程同时 send，验证只保留最后一个 |
| 并发 `ingest_hook` 对同一文件 claim | 虽然有 `test_v061_hook_daemon_ingest.py` 但只有一个并发场景 | 扩展为 50+ 并发进程竞争同一文件 |
| SQLite WAL 模式并发读写 | `busy_timeout = 5000ms` 的 retry 行为未测试 | 写 test：一个线程持续 append event，另一个线程持续 query |

### 5.2 边界条件测试

| 缺失场景 | 影响 | 建议 |
|---|---|---|
| `stop_session` 后资源完全清理（leases, claims, bindings） | 当前测试只验证 lease 文件更新，不验证内存状态 | 写 test：stop 后检查 `state.owner_leases` 和 `state.workspace_leases` 为空 |
| `read_ws_frame` 对 payload len = 126/127 的处理 | WebSocket 协议边界未测试 | 写 test：构造 126 字节和 65536 字节的 frame |
| `write_fixed` 的 `MAX_HTTP_BODY_BYTES = 1MB` 拒绝 | 超大请求直接拒绝未测试 | 写 test：POST 1.1MB body，验证 413 |
| `runtime_lock` 在进程崩溃后的自动释放 | `pid_is_live` 对不存在进程的判断未测试 | 写 test：模拟 lock 文件中的旧 PID，验证新实例可以获取 lock |

### 5.3 错误恢复测试

| 缺失场景 | 影响 | 建议 |
|---|---|---|
| `StoreWriterRuntimeStore` writer thread panic | writer thread 死亡后所有写操作 hang 在 `rx.recv()` | 写 test：模拟 inner store 返回 Err，验证 writer 线程继续运行 |
| `session_actor_loop` 在 `receiver.recv()` 返回 Err 时 | actor 死亡后新命令的 `submit_session_command` 行为 | 写 test：drop actor sender，验证 submit 返回 "actor registry mismatch" |
| `JsonRuntimeStore` events 文件损坏时的行为 | `append_rotating_ndjson` 可能产生损坏 UTF-8 | 写 test：注入损坏的 UTF-8 到 events 文件，验证 `get_events` 优雅跳过 |
| daemon 在 `state_writer` lock 被持有的同时收到 SIGTERM | 锁未释放导致重启后状态文件损坏 | 写 test：模拟 lock 持有者被 kill，验证 tmp 文件被清理 |

### 5.4 跨平台测试

| 缺失场景 | 影响 | 建议 |
|---|---|---|
| Windows Job Object 的进程树清理 | `process.rs` 的 `#[cfg(windows)]` test 只在 Windows 上运行 | 在 CI 中增加 Windows runner |
| `ownership.rs` `canonical_workspace_key` 的大小写敏感行为 | Windows 路径大小写不敏感，Linux 敏感 | 写 cross-platform test：验证 `"E:\\Project"` 和 `"e:\\project"` 产生相同 key |

### 5.5 E2E / 集成测试

| 缺失场景 | 影响 | 建议 |
|---|---|---|
| MCP 工具在 daemon 重启后的恢复 | `agentcall_daemon` 的 `start` action 可能启动旧版本 binary | 写 e2e test：daemon 被杀，MCP call `agentcall_daemon` start，验证新 daemon 响应健康检查 |
| `agentcall_session` 的 `include=plan` 在无 transcript 时 | `plan_artifact_from_binding` 可能 panic 或返回意外结果 | 写 e2e test：启动 session 但不产生 hook，查询 plan |
| `select_option` 在 session 非 running 状态时的行为 | `mcp_session_send` 中 status 检查 | 写 e2e test：stop session 后立即 select_option，验证返回 `session_not_accepting_input` |

---

## 6. 最可能导致 MCP 慢 / transport closed / worker 卡住的代码点

### 6.1 MCP "transport closed" 根因分析

**最直接原因**：`crates/agentcall-mcp/src/daemon_client.rs:8`
```rust
const READ_TIMEOUT: Duration = Duration::from_secs(10);
```

当 Codex/Claude 调用 MCP tool（如 `agentcall_board` full view）时，agentcall-mcp 通过 TCP 向 daemon 发送 HTTP 请求。daemon 处理请求时：

1. `board_state` → `read_events` → 读取 2MB events 文件 → 逐行解析 JSON → 过滤 → 保留 80 条
2. 如果 events 文件很大（如 100MB，因为未 rotate），`read_tail_text` 读取 2MB 仍然包含大量 event
3. `read_events` 的 `serde_json::from_str` 对每条 event 都要分配和解析

在高负载下，这很容易超过 10 秒。一旦超时，`daemon_request` 返回 `daemon_query_timeout`，agentcall-mcp 将该错误作为 tool error 返回给 MCP host。但 MCP host（Codex/Claude）对超时有自己的阈值（通常 10-30 秒），如果 agentcall-mcp 的 10 秒超时先于 host 超时触发，host 看到的可能是空响应或错误响应，导致 "transport closed"。

**更深层原因**：`crates/agentcall-daemon/src/mcp.rs:462` 的 `session_has_seen_hook_event`

`mcp_session_send` 在 `action = "continue"` 且 `liveness_status = "working"` 时，调用 `session_has_seen_hook_event` 检查是否见过 `PostToolBatch`。这个函数：
1. 调用 `read_events`（读取 2MB）
2. 反向扫描所有 event，比较 `type` 和 `wrapper_session`

如果 session 已经运行了很久（如 1 小时，7200 条 event），每次 `session_send` 都要扫描 7200 条。如果 supervisor 连续发送多个命令，累计延迟可能数秒。

### 6.2 Worker "卡住" 的感觉

**位置 1**：`crates/agentcall-daemon/src/actor.rs:29`
```rust
fn write_input(&mut self, text: &str, enter: bool) -> Result<(), String> {
    if !text.is_empty() {
        self.inner.write_all(text.as_bytes())?;
    }
    if enter {
        thread::sleep(Duration::from_millis(80));  // 阻塞 actor 线程！
        self.inner.write_all(b"\r")?;
    }
    self.inner.flush()
}
```

这 80ms 的 sleep 发生在 actor 单线程中。如果 supervisor 发送：
1. `select_option`（"1" + Enter = 80ms）
2. 紧接着另一个 `send`（又 80ms）
3. 再一个 `continue`（又 80ms）

三个命令串行执行，总延迟 240ms + 其他开销。虽然 240ms 对用户不明显，但如果 actor 队列中有 10 个命令（如批量 approve plan + start_auto），总延迟可能 1 秒以上。

**更严重的场景**：`InterruptTurn` 命令在发送 `\x1b` 后 sleep 250ms：
```rust
writer.write_raw(b"\x1b")?;
if let Some(text) = redirect_text.filter(|value| !value.trim().is_empty()) {
    thread::sleep(Duration::from_millis(250));  // 阻塞 250ms
    actor_write_input(state, session_id, writer, &text, true)?;
}
```

中断命令的 250ms sleep 加上后续 `write_input` 的 80ms，一个 interrupt 命令阻塞 actor 330ms。在 interrupt 期间，任何新命令（如再次检查 session 状态）都要排队等待。

**位置 2**：`crates/agentcall-daemon/src/hooks.rs:180`
```rust
pub(crate) fn ingest_hook(state: &AppState, req: HookIngestRequest) -> Result<serde_json::Value, String> {
    let _guard = state.state_writer.lock().unwrap();
    // ... 大量文件 I/O
}
```

Claude Code 的 hook 频率：
- `PreToolUse`：每个 tool call 前触发
- `PostToolUse`：每个 tool call 后触发
- `PostToolBatch`：每批 tool 后触发
- `UserPromptSubmit`：每次用户输入后触发

在一个活跃的 Claude 会话中，每秒可能有数次 hook。如果多个 session 同时活跃，hook 并发高。`state_writer` 锁将所有 hook 串行化，每个 hook 的处理时间（包括文件读写、JSON 解析）都会累积到后续 hook 的等待时间中。

如果 hook 处理时间平均 50ms，并发 4 个 session，每个 session 每秒 2 个 hook，则总 hook 频率 8/s。锁竞争导致部分 hook 等待数百毫秒。当等待时间超过 Claude 的 hook timeout（30 秒看起来很充裕，但如果 daemon 本身也在处理 board 请求，锁可能被 board 请求持有），hook 可能超时。

### 6.3 "MCP 慢" 的其他因素

**位置**：`crates/agentcall-mcp/src/protocol.rs:111-138`（双重序列化）

如果 `agentcall_board` full view 返回大量数据（如数十个 session、数百条 event），`tool_text` 先完整序列化一次（可能数 MB），发现超过 128KB cap，再序列化 truncated wrapper。两次序列化产生数 MB 的临时分配，垃圾回收压力可能导致短暂卡顿。

**位置**：`crates/agentcall-daemon/src/summary.rs:1221-1285`（transcript 扫描）

`agentcall_session` with `include=plan` 触发 `plan_artifact_from_binding`，读取并逐行解析整个 transcript 文件。transcript 文件可能数 MB（长会话的完整 JSONL），解析开销大。

---

## 附录：关键代码位置速查

| 文件 | 行号 | 问题类型 |
|---|---|---|
| `crates/agentcall-daemon/src/session.rs` | 230 | 高风险：静默吞 I/O 错误 |
| `crates/agentcall-daemon/src/session.rs` | 175-234 | 性能：PTY reader 无错误诊断 |
| `crates/agentcall-daemon/src/actor.rs` | 29, 184 | 性能：80ms/250ms sleep 阻塞 actor |
| `crates/agentcall-daemon/src/actor.rs` | 99-107 | 健壮性：5 秒 actor command timeout |
| `crates/agentcall-daemon/src/hooks.rs` | 180 | 性能/健壮性：全局 state_writer 锁 |
| `crates/agentcall-daemon/src/hooks.rs` | 361-388 | 性能：递归目录扫描 count_reports |
| `crates/agentcall-daemon/src/mcp.rs` | 462-475 | 性能：线性扫描 events |
| `crates/agentcall-daemon/src/state.rs` | 128-141 | 性能：2MB events 读取 + JSON 解析 |
| `crates/agentcall-daemon/src/summary.rs` | 797-822 | 性能：线性扫描 events |
| `crates/agentcall-daemon/src/summary.rs` | 1221-1285 | 性能：全量 transcript 扫描 |
| `crates/agentcall-daemon/src/store_json.rs` | 366-392 | 健壮性：rotate 可能切断 UTF-8 |
| `crates/agentcall-daemon/src/process.rs` | 181-189 | 健壮性：Drop handle 判断 |
| `crates/agentcall-daemon/src/http.rs` | 33-35, 693 | 健壮性：HTTP body cap / connection shutdown |
| `crates/agentcall-daemon/src/ownership.rs` | 320-331 | 健壮性：canonical workspace key 不一致 |
| `crates/agentcall-mcp/src/daemon_client.rs` | 8 | 高风险：10s read timeout |
| `crates/agentcall-mcp/src/protocol.rs` | 23 | 高风险：无大小限制的 JSON 解析 |
| `crates/agentcall-mcp/src/protocol.rs` | 111-138 | 性能：双重 JSON 序列化 |
| `scripts/agentcall-claude-hook.py` | 84 | 健壮性：5s daemon ingest 超时 |
| `scripts/agentcall-codex-hook.py` | 30 | 功能缺失：无 PostToolBatch |
| `scripts/install_codex_hooks.py` | 34-41 | 健壮性：shell 字符串拼接命令 |
