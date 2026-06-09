# AgentCall MCP 控制面性能审查报告

> **审查范围**: `crates/agentcall-mcp/src` + `crates/agentcall-daemon/src/mcp.rs/http.rs/routes.rs` 及相关支撑模块（session.rs, state.rs, summary.rs, hooks.rs, terminal.rs）
>
> **审查目标**: MCP 工具调用慢、排队、超时、巨大响应、stdio backpressure
>
> **审查性质**: 只读审查，未修改源码
>
> **审查日期**: 2026-06-09

---

## 一、慢路径分类总览

| 类别 | 问题数 | 严重程度分布 | 核心表现 |
|---|---|---|---|
| **A. MCP 传输链同步阻塞** | 5 | P0×2, P1×2, P2×1 | stdio ↔ TCP 全链路同步，无连接复用，无超时 |
| **B. 巨大响应体与序列化开销** | 4 | P0×1, P1×2, P2×1 | `agentcall_board` / `session_summary` 返回数 MB 级 JSON |
| **C. 全局锁竞争与串行化** | 4 | P0×2, P1×1, P2×1 | `state_writer` 全局写锁 + session 多 Mutex 竞争 |
| **D. 重复文件 I/O 与日志扫描** | 5 | P1×4, P2×1 | 每次 API 调用重复读取/解析多份 JSON 状态文件 |
| **E. SSE/WS 背压与广播瓶颈** | 3 | P1×2, P2×1 | 无界 channel、无消费者反压、慢客户端拖垮整体 |

---

## 二、P0 级问题（阻塞/高影响）

### P0-1: MCP 全链路同步阻塞，无连接复用

**证据:**

- `crates/agentcall-mcp/src/protocol.rs:10-33`
  ```rust
  pub(crate) fn serve(config: Config) -> io::Result<()> {
      let stdin = io::stdin();
      let mut stdout = io::stdout();
      for line in stdin.lock().lines() {   // ← 同步逐行阻塞读取
          // ...
          writeln!(stdout, "...")?;        // ← 同步写入
          stdout.flush()?;                   // ← 每消息 flush
      }
  }
  ```
- `crates/agentcall-mcp/src/daemon_client.rs:28-56`
  ```rust
  fn daemon_request(...) -> Result<Value, String> {
      let mut stream = TcpStream::connect(...)?; // ← 每次工具调用新建 TCP 连接
      stream.write_all(request.as_bytes())?;
      stream.flush()?;
      read_http_json(stream)                       // ← 同步阻塞等待完整响应
  }
  ```

**问题分析:**

Claude Code 的 MCP Host 通过 stdio 与 agentcall-mcp 通信，而 agentcall-mcp 每次工具调用都新建一个到 daemon 的 TCP 连接。该设计存在三重瓶颈：

1. **连接建立开销**: 每次 `TcpStream::connect` + HTTP 1.1 请求头往返，在高频调用（如 board 轮询、session 查询）下累积延迟显著
2. **无超时保护**: `TcpStream` 没有设置 `read_timeout` / `write_timeout`，daemon 卡住时 MCP 调用会永久挂起
3. **stdio 背压**: 当 daemon 响应巨大（见 P0-2）时，`read_http_json` 必须等待完整 body 读取后才能返回，`stdout` 在此期间无法输出，MCP Host 的后续请求被阻塞在 stdin

**预计收益**: 引入连接池后可降低单次 MCP 调用延迟 10~50ms（本地）/ 100~300ms（高负载），消除因连接建立导致的抖动。

---

### P0-2: `agentcall_board` 可能返回数 MB 级 JSON 响应

**证据:**

- `crates/agentcall-daemon/src/summary.rs:20-110` — `board_state()` 函数
  - 无条件读取并聚合以下全部数据：
    - `events.ndjson` / `events/recent.ndjson`（最多读取尾部 2MB，`READ_TAIL_BYTES = 2*1024*1024`）
    - `state/active_sessions.json`
    - `state/file_claims.json`
    - `state/transcripts.json`
    - `state/routes.json`（通过 `routes_state()` 全量读取并排序）
    - `state/project.json`
    - `tasks/` 目录下所有报告（`read_reports()` 递归遍历）
    - `sessions/` 遗留会话（`legacy_detached_sessions()`）
    - 实时 PTY sessions（`list_sessions()`）
  - 即使请求 `view=compact`，仍然先执行 `cleanup_stale_runtime_state` 和 `attention_items`（后者遍历所有 session 并调用 `session_summary`）
- `crates/agentcall-daemon/src/http.rs:614-621`
  ```rust
  pub(crate) fn json_response<T: Serialize>(value: &T) -> Response {
      let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
      // ↑ 整个 board_state 结果一次性序列化到内存 Vec
  }
  ```
- `crates/agentcall-daemon/src/http.rs:29`
  ```rust
  const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024; // 1MB
  ```
  该限制仅作用于**请求体**（`read_request` 中检查 `content_length`），**响应体无任何限制**。

**问题分析:**

在活跃工作负载下（多个 PTY session + 大量事件 + 多份报告），`board_state()` 的 JSON 输出可轻松超过 1MB，极端情况下超过 5MB。这会导致：

1. **serde_json 序列化 CPU 峰值**: 大型 JSON 结构 `to_vec` 是 O(n) 内存拷贝 + 格式化开销
2. **TCP 发送阻塞**: 大 body 在 `write_fixed()` 中通过 `stream.write_all()` 同步发送，占用 daemon 的 HTTP handler 线程
3. **MCP 层反压**: `daemon_client.rs:read_http_json` 使用 `reader.read_exact(&mut body)` 读取完整 Content-Length，大响应会导致 agentcall-mcp 长时间阻塞，stdio 无响应
4. **Claude Code token 压力**: 如果 board 结果被注入到上下文，数 MB 的文本会消耗大量上下文窗口

**预计收益**: 增加 board 响应分页/截断后，响应大小可控制在 50~100KB 以内，序列化时间从 50ms+ 降至 <5ms。

---

### P0-3: `state_writer` 全局写锁串行化所有状态变更

**证据:**

- `crates/agentcall-daemon/src/state.rs:16-24`
  ```rust
  pub(crate) struct AppState {
      // ...
      pub(crate) state_writer: Mutex<()>,  // ← 全局粗粒度写锁
  }
  ```
- 被 `state_writer` 保护的慢操作（不完全统计）：
  - `append_agent_event()` — 每次 hook 触发、每次 session 输入、每次状态变更都写入事件日志
  - `patch_route_record()` / `upsert_route_record()` — route 状态更新
  - `queue_supervisor_instruction()` — supervisor 指令排队
  - `checkpoint_session()` — session checkpoint
  - `ingest_hook()` — 整个 hook 处理（包含 claim 检查、policy 决策、binding 更新、事件追加）
  - `cleanup_stale_runtime_state()` — 在 `board_state()` / `runtime_health()` 内部也持有此锁

**问题分析:**

这是一个**伪装的串行化瓶颈**。所有涉及磁盘状态写入的操作都被迫排队：

- **并发 hook 注入时**: 多个 Claude Code session 同时产生 `PreToolUse`/`PostToolBatch`，这些 hook 通过 HTTP POST 到达 daemon，但全部被 `state_writer` 串行化。在高频工具调用场景下（如 Claude 连续执行 10+ 个工具），hook 处理队列会累积延迟
- **board 查询与写入互斥**: `board_state()` 调用 `cleanup_stale_runtime_state()` 需要获取 `state_writer`，这意味着 board 查询会阻塞所有事件写入，反之亦然
- **无读写分离**: 读操作（如 `read_routes()`）不需要写锁，但 `cleanup_stale_runtime_state` 等混合操作同时持有写锁进行读改写

**预计收益**: 将事件追加（append-only）与状态文件（routes/claims/bindings）更新解耦，或改用分片锁，可减少并发冲突 50% 以上。

---

### P0-4: `session_summary()` 在单条 MCP 调用链中被重复计算

**证据:**

- `crates/agentcall-daemon/src/mcp.rs:188-367` — `mcp_session_send()`
  - 第 237 行: `let summary = session_summary(state, &session);`
  - 第 301 行: `let summary = session_summary(state, &session);` ← 对同一个 session 重复计算
- `crates/agentcall-daemon/src/mcp.rs:161-186` — `mcp_session()`
  - 第 164 行: `let summary = session_summary(...);`
  - 第 176 行: `session.decode_health.lock().unwrap().clone()` 再次获取锁
- `crates/agentcall-daemon/src/summary.rs:436-731` — `session_summary()` 内部
  - 调用 `clean_session_output()` → `clean_terminal_text()` → `tail_lines()`，涉及大量字符串分配和 ANSI 剥离
  - 调用 `route_result_for_session()` → `routes_state()` → 全量读取 `routes.json` 并排序
  - 调用 `runtime_bindings_state()` → 读取 `runtime_binding.json`
  - 调用 `policy_denials_state()` → 读取 `policy_denials.json`
  - 调用 `read_json_file(&agent_dir.join("state/file_claims.json"), ...)`
  - 调用 `pending_supervisor_instruction_count()` → 读取 `pending_supervisor_instructions.json`
  - 调用 `last_supervisor_instruction_injected_at()` → 扫描事件日志（`read_events()`）
  - 调用 `session_has_seen_hook_event()` → 再次扫描事件日志

**问题分析:**

`session_summary()` 是一个**重型聚合函数**，每次调用触发 5+ 次文件 I/O + 多次 JSON 解析 + 字符串处理。在 `mcp_session_send` 中它被无缓存地调用了两次。在 board 查询中，每个 running session 都会触发一次 `session_summary()`（通过 `attention_items()`），N 个 session = N 次重复计算。

**预计收益**: 引入 `session_summary` 结果缓存（TTL 1~2 秒）后，`agentcall_session` 和 `agentcall_session_send` 响应时间可从 50~200ms 降至 <10ms。

---

## 三、P1 级问题（显著影响）

### P1-1: `read_events()` 每次全量扫描 2MB 日志文件

**证据:**

- `crates/agentcall-daemon/src/state.rs:90-103`
  ```rust
  pub(crate) fn read_events(path: &Path) -> Vec<serde_json::Value> {
      let event_path = recent_events_path_for(path).unwrap_or_else(|| path.to_path_buf());
      let Some(text) = read_tail_text(&event_path, READ_TAIL_BYTES) else {  // ← 读取尾部 2MB
          return vec![];
      };
      let mut events: Vec<serde_json::Value> = text
          .lines()
          .filter_map(|line| serde_json::from_str(line).ok())  // ← 逐行 JSON 解析
          .collect();
      if events.len() > RECENT_EVENT_LIMIT {  // ← 超过 80 条截断
          events = events.split_off(events.len() - RECENT_EVENT_LIMIT);
      }
      events
  }
  ```
- `READ_TAIL_BYTES = 2 * 1024 * 1024`（state.rs:12）
- `RECENT_EVENT_LIMIT = 80`（state.rs:13）
- 调用方（不完全统计）：
  - `board_state()` — 获取 recent_events
  - `session_has_seen_hook_event()` — mcp.rs:402-415, summary.rs:772-785
  - `last_supervisor_instruction_injected_at()` — summary.rs:745-770
  - `cleanup_stale_runtime_state()` — summary.rs:338-427（间接通过 timestamp_age_ms 等）

**问题分析:**

每次调用 `read_events()` 都执行：文件 seek → 读取 2MB → 按行 split → 逐行 JSON parse → 取最后 80 条。当事件日志较大（如 4MB+）时，即使只取尾部 2MB，I/O + 解析开销仍然显著。

更严重的是，`session_summary()` 内部同时调用了 `last_supervisor_instruction_injected_at()` 和 `session_has_seen_hook_event()`，这意味着**单次 session 查询会三次独立读取并解析同一份事件日志**。

**预计收益**: 事件日志改为内存缓存（如 `VecDeque` 保留最近 200 条）后，事件查询延迟从 5~20ms 降至 <0.1ms。

---

### P1-2: `routes.json` / `file_claims.json` 等状态文件全量读写

**证据:**

- `crates/agentcall-daemon/src/routes.rs:590-599`
  ```rust
  fn read_routes(state: &AppState) -> Value {
      read_json_file(
          &state.workspace.join(".agentcall").join("state").join("routes.json"),
          json!({}),
      )
  }
  ```
- `crates/agentcall-daemon/src/routes.rs:488-519` — `route_for_wrapper_session()`
  - 被 `mcp_session_send()`（is_plan_then_auto_session、update_pty_workflow_route）、`mcp_board()`、`session_summary()` 多次调用
  - 每次调用都通过 `read_routes()` 全量读取文件
- `crates/agentcall-daemon/src/hooks.rs:23-76` — 所有 `*_state()` 函数都是直接 `read_json_file()`

**问题分析:**

JSON 状态文件（routes.json、file_claims.json、runtime_binding.json、policy_denials.json、pending_supervisor_instructions.json）全部采用**全量读写**模式。即使只更新一条 route 或一个 claim，也要：

1. 读取整个 JSON 文件到内存
2. 修改一个字段
3. `serde_json::to_string_pretty` 序列化整个文件
4. 写入临时文件
5. `fs::rename` 原子替换

随着 routes 和 claims 数量增加，单次写入的 I/O 和 CPU 开销线性增长。在高并发 hook 场景下，多个 `PreToolUse` 需要串行化更新 claims，每次都要重写整个 claims 文件。

**预计收益**: 改用增量写入（如 ndjson append + compaction）或内存缓存 + 异步 flush，可将状态更新延迟降低 80% 以上。

---

### P1-3: SSE/WS `broadcast()` 使用无界 channel，无背压保护

**证据:**

- `crates/agentcall-daemon/src/session.rs:490-494`
  ```rust
  pub(crate) fn broadcast(session: &Arc<Session>, event: StreamEvent) {
      let mut clients = session.clients.lock().unwrap();
      clients.retain(|tx| tx.send(event.clone()).is_ok());
      // ↑ mpsc::channel 默认无界（实际上是 bounded 但 capacity 很大）
  }
  ```
- `crates/agentcall-daemon/src/http.rs:284-309` — `write_sse()`
  ```rust
  let (tx, rx) = mpsc::channel::<StreamEvent>();
  session.clients.lock().unwrap().push(tx);
  for event in rx {  // ← 如果客户端接收慢，这里会累积事件
      if write_event(&mut stream, &event).is_err() {
          break;
      }
  }
  ```
- `crates/agentcall-daemon/src/http.rs:342-354` — WebSocket writer thread
  - 同样在 `for event in rx` 中同步写入

**问题分析:**

PTY 输出通过 `broadcast()` 推送到所有 SSE/WS 客户端。`mpsc::channel` 虽然内部有缓冲区，但没有明确的容量限制。当：

1. **客户端接收慢**: 浏览器 tab 在后台或网络拥塞时，TCP 发送缓冲区填满，`write_event` 阻塞，导致 channel 中事件累积
2. **大量输出场景**: Claude Code 执行 `cat` 大文件或大量工具输出时，PTY reader 线程产生事件速率远超客户端消费速率
3. **内存泄漏风险**: channel 中的 `StreamEvent` 持有 `String` 数据（clean output），累积事件会消耗大量内存

**预计收益**: 引入有界 channel（如 `sync::mpsc::sync_channel(128)`）并丢弃溢出事件后，可避免内存暴涨和慢客户端拖垮 daemon。

---

### P1-4: `write_input()` 中硬编码 `thread::sleep(Duration::from_millis(80))`

**证据:**

- `crates/agentcall-daemon/src/session.rs:332-353`
  ```rust
  pub(crate) fn write_input(state: &AppState, name: &str, req: InputRequest) -> Result<(), String> {
      // ...
      if enter {
          thread::sleep(Duration::from_millis(80));  // ← 硬编码延迟
          writer.write_all(b"\r").map_err(|err| err.to_string())?;
      }
      // ...
  }
  ```

**问题分析:**

每次通过 MCP `agentcall_session_send` 发送输入时，如果 `enter=true`（默认），会强制睡眠 80ms。这 80ms 被计入整个 MCP 调用延迟：

```
Claude Code → stdio → agentcall-mcp → TCP → daemon → write_input(80ms sleep) → TCP → mcp → stdio → Claude Code
```

单次 80ms 在交互场景下可感知，如果 supervisor 连续发送多条指令（如先 `continue` 再 `request_report`），累积延迟显著。

**预计收益**: 将固定 sleep 替换为 PTY 就绪检测或降低到 10~20ms，可减少 MCP 调用延迟 60ms+。

---

## 四、P2 级问题（中等影响/技术债）

### P2-1: `looks_like_menu_prompt()` 中 O(n²) 字符反转操作

**证据:**

- `crates/agentcall-daemon/src/mcp.rs:385-400`
  ```rust
  fn looks_like_menu_prompt(clean_output: &str) -> bool {
      let tail = clean_output
          .chars().rev().take(4000)   // ← 创建迭代器
          .collect::<String>()        // ← 分配 String
          .chars().rev()              // ← 再反转一次
          .collect::<String>()        // ← 再分配 String
          .to_ascii_lowercase();      // ← 第三次分配
      // ...
  }
  ```

**问题分析:**

该函数意图取输出尾部 4000 字符，但实现方式导致 3 次 String 分配和 2 次完整字符遍历。虽然单次开销不大，但它在 `mcp_session_send()`（select_option action）中被调用，且 `clean_output` 可能很大（512KB clean_replay 上限）。

**修复建议**: 使用 `clean_output.chars().rev().take(4000)` 配合 `as_str()` 比较，避免分配。

---

### P2-2: `plan_artifact_from_binding()` 逐行解析整个 transcript JSONL

**证据:**

- `crates/agentcall-daemon/src/summary.rs:1151-1283` — `plan_artifact_from_binding()`
  - 如果 binding 中有 `transcript_path`，逐行读取并 `serde_json::from_str::<Value>()` 解析整个 transcript 文件
  - transcript 文件可能达到数 MB（长时间 session）
  - 该函数在 `session_plan_artifact()` 中被调用，后者在 `mcp_session()`（`include=plan`）中被调用

**问题分析:**

`agentcall_session` with `include=plan` 会触发对整个 transcript JSONL 的逐行解析，即使只需要最后几条包含 plan 的记录。没有索引、没有反向搜索、没有提前终止。

**修复建议**: 从文件尾部反向搜索，或维护一个 plan 提取索引。

---

### P2-3: `count_reports()` 递归遍历任务目录

**证据:**

- `crates/agentcall-daemon/src/hooks.rs:360-387` — `count_reports()` / `count_report_files()`
  - 每次 hook 的 `context_injection()`（`SessionStart`、`UserPromptSubmit`、`PostToolBatch`）都递归遍历 `tasks/` 和 `reports/` 目录
  - 随着任务数量增加，遍历开销线性增长

**问题分析:**

context injection 是高频路径（每次 UserPromptSubmit 和 PostToolBatch 都触发），但 `structured_reports` 计数却很少变化。重复遍历是浪费。

**修复建议**: 缓存报告数量（如维护一个计数器文件或内存缓存）。

---

### P2-4: `agentcall-mcp` 无日志/无指标

**证据:**

- `crates/agentcall-mcp/src/protocol.rs` — 无任何日志输出或性能指标收集
- `crates/agentcall-mcp/src/daemon_client.rs` — 无请求耗时记录、无重试、无连接状态监控

**问题分析:**

当 MCP 调用变慢时，无法定位瓶颈在 stdio 层、TCP 层还是 daemon 处理层。缺乏可观测性使性能问题难以排查。

---

## 五、预计修复收益汇总

| 修复项 | 当前延迟估算 | 优化后估算 | 收益 |
|---|---|---|---|
| MCP TCP 连接复用 | 10~50ms/调用 | <1ms/调用 | 消除连接抖动 |
| board 响应截断/分页 | 50~500ms + 数 MB | <10ms + <100KB | 消除巨型响应 |
| state_writer 分片/解耦 | 串行排队 N×5ms | 并行化 | 并发吞吐量 ×N |
| session_summary 缓存 | 50~200ms/次 | <5ms/次 | 查询延迟 -90% |
| read_events 内存缓存 | 5~20ms/次 | <0.1ms/次 | 日志查询 -99% |
| 状态文件增量写入 | 5~50ms/写 | <1ms/写 | 状态更新 -90% |
| SSE/WS 有界 channel | 内存泄漏风险 | 可控内存 | 稳定性提升 |
| write_input sleep 优化 | 80ms/次 | 10ms/次 | 交互延迟 -70ms |

---

## 六、需要父层确认的问题

1. **MCP 超时策略**: 当前 daemon 的 HTTP handler 和 agentcall-mcp 的 TCP 客户端都没有超时。是否需要：
   - 在 daemon 侧为 `/api/mcp/call` 设置最大处理时间（如 30s）？
   - 在 agentcall-mcp 侧设置 TCP 连接/读取超时？

2. **board 响应大小上限**: `board_state()` 的 `full` 视图在某些场景下可能超过 5MB。是否：
   - 对所有 MCP 工具响应增加硬性的 JSON 大小截断（如 256KB）？
   - 还是仅在 `agentcall_board` 中实现分页/增量查询？

3. **状态持久化模型**: 当前所有状态以 JSON 文件存储，每次全量读写。是否考虑：
   - 短期：内存缓存 + 异步批量 flush
   - 长期：嵌入式 KV（如 sled/rocksdb）或 SQLite

4. **session_summary 缓存 TTL**: 缓存可以显著提升性能，但会引入一致性问题（如 hook 刚更新了 binding，但缓存返回旧值）。可接受的 TTL 是多少？（建议 1~2 秒，因为 session 状态变化频率远低于查询频率）

5. **agentcall-mcp 进程模型**: 当前 agentcall-mcp 是一个独立进程，通过 stdio 与 Claude Code 通信。是否有计划改为：
   - 内嵌式（in-process）MCP server 以减少进程间通信开销？
   - 或保持进程隔离但引入持久 TCP 连接到 daemon？

---

## 七、附录：关键慢函数路径速查

| 函数 | 文件 | 调用热点 | 主要开销 |
|---|---|---|---|
| `serve()` | `protocol.rs:10` | MCP 每条消息 | stdio 同步读写 |
| `daemon_request()` | `daemon_client.rs:28` | 每次工具调用 | TCP 连接新建 + 同步 HTTP |
| `mcp_call()` | `mcp.rs:114` | `/api/mcp/call` | 分发到各工具 handler |
| `board_state()` | `summary.rs:20` | `agentcall_board` | 多文件聚合 + 全量序列化 |
| `session_summary()` | `summary.rs:436` | `agentcall_session`, `board` | 5+ 文件 I/O + 字符串处理 |
| `mcp_session_send()` | `mcp.rs:188` | `agentcall_session_send` | 2× session_summary + route 查询 |
| `read_events()` | `state.rs:90` | board, summary, cleanup | 2MB 文件 tail + JSON 解析 |
| `read_routes()` | `routes.rs:590` | session_send, summary, board | 全量 routes.json 读取 |
| `ingest_hook()` | `hooks.rs:175` | `/api/hooks/ingest` | state_writer 锁 + 多文件读写 |
| `broadcast()` | `session.rs:490` | PTY 每 8KB 输出 | 无界 channel + 锁竞争 |
| `write_input()` | `session.rs:332` | `session_send` | 80ms 硬编码 sleep |
| `spawn_reader()` | `session.rs:166` | PTY 持续输出 | 8KB buf + replay 锁竞争 |

---

*报告结束。等待 supervisor 审阅与优先级确认。*
