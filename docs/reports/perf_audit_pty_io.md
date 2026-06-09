# AgentCall PTY / Session I/O 性能审查报告

> 审查范围：`crates/agentcall-daemon/src/session.rs`、`terminal.rs`、`hooks.rs`、`mcp.rs` 及相关路径。  
> 原则：只读审查，未修改源码。  
> 重点：PTY 输出缓存、clean_output、session_send、queued instruction、权限菜单、PostToolBatch 注入、stop/interrupt/select_option 对性能和交互延迟的影响。

---

## 1) PTY I/O 慢路径

### 1.1 `spawn_reader` 中对 DSR 控制序列的扫描随 buffer 增长而变长
- **位置**：`crates/agentcall-daemon/src/session.rs:190-192`，函数 `spawn_reader`。  
- **现象**：每次 `read` 返回后，代码把上一次的 `control_tail` 与新读到的 `bytes` 拼接成 `control_scan`，然后对整个 `control_scan.windows(4)` 扫描寻找 `\x1b[6n`：
  ```rust
  let mut control_scan = control_tail.clone();
  control_scan.extend_from_slice(bytes);
  for _ in control_scan.windows(4).filter(|window| *window == b"\x1b[6n") { ... }
  ```
- **性能影响**：虽然 `control_tail` 被截断为最后 3 字节，但 `control_scan` 仍包含本次全部 `bytes`；当 PTY 产生大量输出时，每次都要对新增字节做一次 O(n) 扫描，高频 read 时 CPU 会显著上升。
- **建议**：仅扫描 `control_tail + bytes` 中新增部分（例如从 `control_tail.len().saturating_sub(3)` 开始），避免整段重复遍历。

### 1.2 `decode_utf8_stream` 对同一 `pending` 反复从头解码
- **位置**：`crates/agentcall-daemon/src/terminal.rs:13-54`，函数 `decode_utf8_stream`。  
- **现象**：每次收到新 chunk 都会 `pending.extend_from_slice(bytes)`，然后在循环里反复调用 `std::str::from_utf8(pending)`。对于跨 chunk 的不完整 UTF-8 序列，`pending` 会保留到下一次，而每次都要从 0 开始重新验证整个 `pending`。
- **性能影响**：当 pending 较长（例如包含多字节字符的边界残片）时，反复 full-scan 接近 O(n²)；高频输出下 decoder 是 reader 线程的主要 CPU 消耗点之一。
- **建议**：保留 `valid_up_to` 索引，仅对新增 bytes 做验证；或改为使用 `String::from_utf8_lossy` 增量切片配合已知有效前缀长度。

### 1.3 `append_limited_text` 每次超限都全串扫描字符边界
- **位置**：`crates/agentcall-daemon/src/terminal.rs:56-67`，函数 `append_limited_text`。  
- **现象**：`clean_replay` 超过 `CLEAN_LIMIT`（512 KB）后，每次追加都要执行 `target.char_indices().find(|(index, _)| *index >= drop)` 来找到安全截断点。
- **性能影响**：对 512 KB 的字符串做 `char_indices()` 线性扫描，每 read 一次就可能发生一次，reader 线程会被拖慢；且截断后 `drain(..keep_from)` 还会移动剩余字符。
- **建议**：使用字节/字符 ring buffer，或记录上一次截断位置，避免每次全量扫描。

### 1.4 `broadcast` 无差别克隆事件并持有 `clients` 锁
- **位置**：`crates/agentcall-daemon/src/session.rs:490-493`，函数 `broadcast`。  
- **现象**：每个 output chunk 都会锁住 `session.clients`，生成 `StreamEvent` 的完整克隆（含 `data: String`），然后对所有 sender 调用 `tx.send(event.clone())`。`mpsc` channel 是无界的，慢消费端会导致内存无限增长。
- **性能影响**：
  - 数据量大时每个 chunk 都会克隆一份 String，若 WebSocket/SSE 客户端较多，内存拷贝成倍增加。
  -  retain 里的 `send` 若因对端缓慢而隐式阻塞，会拉长 `clients` 锁持有时间，拖慢后续广播。
- **建议**：考虑把 `data` 改为 `Arc<str>` 共享；给 channel 加容量或超时策略，避免无界积压。

### 1.5 SSE / WebSocket 建立连接时一次性克隆整个 `clean_replay`
- **位置**：`crates/agentcall-daemon/src/http.rs:288`（SSE）、`:331`（WebSocket），函数 `write_sse` / `write_ws`。  
- **现象**：连接建立时直接 `session.clean_replay.lock().unwrap().clone()`，把最大 512 KB 的字符串完整复制一次作为 replay。
- **性能影响**：对于频繁刷新页面的 TUI 或监控端，每次连接都产生一次大内存拷贝；在 TUI 轮询场景下会被反复触发。
- **建议**：对 `clean_replay` 使用 `Arc<String>` 或快照机制，让 replay 发送共享只读数据。

---

## 2) Session 交互延迟原因

### 2.1 `write_input` 在锁内 `thread::sleep(80ms)` 才发送 `\r`
- **位置**：`crates/agentcall-daemon/src/session.rs:343`，函数 `write_input`。  
- **现象**：发送完文本后，若 `enter=true`，代码会 `thread::sleep(Duration::from_millis(80))`，然后才在 `writer` 锁内写入 `\r`。
- **性能影响**：`writer` 互斥锁被占用 80 ms，期间其他输入（如紧急 interrupt、resize）必须排队等待；MCP `session_send` 调用的响应延迟也包含这 80 ms。
- **建议**：把 sleep 移到锁外，或用独立线程/异步定时器延迟写入 `\r`，不要阻塞 `writer`。

### 2.2 `interrupt_session` 同步等待 250ms 才写 redirect_text
- **位置**：`crates/agentcall-daemon/src/session.rs:377`，函数 `interrupt_session`。  
- **现象**：发送 ESC 后，若携带 `redirect_text`，会在当前线程 sleep 250 ms，然后调用 `write_input`（再次可能 sleep 80ms）。
- **性能影响**：MCP 调用 `action=interrupt` 的返回会被延迟 250+ ms；interrupt 本意是快速 reclaim，却被同步等待抵消。
- **建议**：在独立线程中完成延迟写入，API 立即返回 `interrupt_sent` 与 `redirect_scheduled`。

### 2.3 `stop_session` 与 `spawn_waiter` 中的 cleanup 在全局 `state_writer` 锁内做大量文件 I/O
- **位置**：`crates/agentcall-daemon/src/session.rs:256-263`（`spawn_waiter` 调用 cleanup），`hooks.rs:78-147`（`cleanup_wrapper_session`）。  
- **现象**：子进程退出后，`spawn_waiter` 会调用 `cleanup_wrapper_session`，后者先获取 `state.state_writer.lock().unwrap()`，然后读写 `runtime_binding.json`、`file_claims.json`、`pending_supervisor_instructions.json`、`routes.json`，最后 append event。
- **性能影响**：`state_writer` 是 daemon 级全局锁；cleanup 期间任何其他 session 的 hook、event、route 更新都被阻塞。文件系统较慢或 state 文件较大时，阻塞时间可达数十到数百毫秒。
- **建议**：把 cleanup 拆分为“释放进程资源”（快路径）和“异步/批量写 state”（慢路径），或在锁内只写内存快照，后台线程刷盘。

### 2.4 `mcp_session_send` 在关键分支反复调用 `session_summary`
- **位置**：`crates/agentcall-daemon/src/mcp.rs:237`（`select_option`）、`:301`（普通 send 的 working 判断）。  
- **现象**：无论最终是 `select_option`、queued 还是直接 `write_input`，都会先调一次 `session_summary(state, &session)`，以读取 `attention_status`、`liveness_status` 等。
- **性能影响**：`session_summary` 本身会触发大量文件读取与字符串清洗（见第 3 节），导致一次 MCP 调用内部可能花 50–200 ms，严重影响 TUI / 自动化脚本对 session 的频繁控制。
- **建议**：
  - 对 `session_summary` 增加轻量入口，只返回控制所需的几个字段。
  - 在 MCP 层使用缓存的 `attention_status` 快照，避免每次 send 都全量清洗输出。

### 2.5 `session_summary` 内部多次重复读取并解析事件/状态文件
- **位置**：`crates/agentcall-daemon/src/summary.rs:436-731`，函数 `session_summary`。  
- **具体重复点**：
  - `routes_state(state)` 每次读取 `routes.json`。
  - `runtime_bindings_state(state)` 每次读取 `runtime_binding.json`。
  - `policy_denials_state(state)` 每次读取 `policy_denials.json`。
  - `pending_supervisor_instructions_state(state)` 每次读取 `pending_supervisor_instructions.json`。
  - `session_has_seen_hook_event` 与 `last_supervisor_instruction_injected_at` 分别调用 `read_events`，每次读取 `events.ndjson` 尾部 2 MB 并解析最多 80 条事件（`state.rs:90-103`）。
- **性能影响**：单次 `session_summary` 可能产生 5+ 次文件读取和 JSON 解析；在 board / MCP 高频调用时，磁盘 I/O 和 JSON 反序列化会成为瓶颈。
- **建议**：在 `AppState` 中维护基于 `mtime` 的解析缓存，或改用 `RwLock` 缓存最近一次读取结果，并在写入时使缓存失效。

---

## 3) TUI 提取/清洗成本

### 3.1 `clean_session_output` 每次全量 clone + 全量清洗
- **位置**：`crates/agentcall-daemon/src/summary.rs:798-800`，函数 `clean_session_output`；`terminal.rs:132-139`，`clean_terminal_text`。  
- **现象**：
  ```rust
  let text = session.clean_replay.lock().unwrap().clone();
  tail_lines(&clean_terminal_text(&text), 120)
  ```
  先克隆最大 512 KB 的 `clean_replay`，再对整个字符串执行 `strip_ansi`（逐字符 `Peekable` 遍历）、按行 `trim_end`、过滤空行、收集为 `Vec`、join；最后 `tail_lines` 再做一次 `lines().collect()`。
- **性能影响**：`session_summary`、`/api/sessions/{name}/output/clean`、`mcp_session` 的 `clean_tail` 都会触发这一整串计算。频繁刷新 board 时，CPU 会花在重复的字符遍历与内存分配上。
- **建议**：
  - 在 reader 线程增量维护一份“最近 120 行 clean tail”缓存，API 直接返回缓存字符串。
  - 若必须即时计算，使用 `Arc<str>` 或切片共享，避免全量 clone。

### 3.2 `strip_ansi` 是逐字符状态机，未使用 SIMD/正则等加速
- **位置**：`crates/agentcall-daemon/src/terminal.rs:69-115`，函数 `strip_ansi`。  
- **现象**：对每一个字符调用 `chars.next()` 和 `chars.peek()`，遇到 `\x1b` 再进入 CSI / OSC 分支循环。
- **性能影响**：实现正确但纯串行；512 KB × 高频请求时，清洗本身会消耗可观 CPU。
- **建议**：使用成熟库（如 `ansi-to-tui` 或 `strip-ansi-escapes`），或至少把清洗结果缓存到按行失效的 buffer 中。

### 3.3 `looks_like_menu_prompt` 做双重字符反转
- **位置**：`crates/agentcall-daemon/src/mcp.rs:385-399`，函数 `looks_like_menu_prompt`。  
- **现象**：
  ```rust
  let tail = clean_output.chars().rev().take(4000).collect::<String>()
      .chars().rev().collect::<String>()
  ```
  为取尾部 4000 字符，先把整个字符串反转、收集、再反转、收集。
- **性能影响**：虽然 4000 字符不大，但 `clean_output` 最大可达 512 KB，每次 `select_option` 都要两次全字符遍历 + 两次 String 分配。
- **建议**：使用 `clean_output.char_indices()` 从后向前找到第 4000 个字符的安全边界，然后直接 slice。

### 3.4 board / attention 计算重复调用 `session_summary`
- **位置**：`crates/agentcall-daemon/src/summary.rs:899-934`，函数 `attention_items`。  
- **现象**：`board_state` 构建 attention 视图时，会对每个 live session 调用一次完整的 `session_summary`，而后者又会重复读取所有 state 文件并清洗输出。
- **性能影响**：session 数量多或 board 被频繁轮询时，总体工作量随 session 数线性放大。
- **建议**：
  - `attention_items` 只需要 `attention_status`、`liveness_status`、`patience_status` 等少量字段；应提供轻量函数，避免全量 `session_summary`。
  - 对 `board_state` 整体增加短时间缓存（例如 1 秒），避免 TUI 轮询打爆 daemon。

### 3.5 `context_injection` 在 `PostToolBatch` 时递归统计 reports
- **位置**：`crates/agentcall-daemon/src/hooks.rs:294-358`，`context_injection`；`:360-387`，`count_reports` / `count_report_files`。  
- **现象**：每次 `PostToolBatch` hook 都会递归扫描 `.agentcall/tasks` 与 `.agentcall/reports` 目录，按文件名规则计数。
- **性能影响**：当 reports / tasks 目录下文件很多时，单次 hook 可能增加数十到数百毫秒；`PostToolBatch` 又是 Claude Code 高频事件，延迟会叠加。
- **建议**：
  - 在 daemon 启动时做一次性扫描，维护计数器；后续通过 route/report 创建事件增量更新。
  - 或在 `AppState` 内加 `Arc<RwLock<ReportCounter>>` 缓存，定时失效。

---

## 4) P0 / P1 / P2 修复建议

### P0（影响大、风险低，建议优先）

| # | 问题 | 建议修改点 |
|---|------|-----------|
| 1 | `spawn_reader` 控制序列扫描范围过大 | `session.rs:190-192`：仅扫描新增字节，避免整段 `control_scan.windows(4)`。 |
| 2 | `decode_utf8_stream` 反复从头解码 | `terminal.rs:13-54`：保留有效前缀长度，增量验证新增 bytes。 |
| 3 | `append_limited_text` 每次 512KB 扫描 | `terminal.rs:56-67`：改用 ring buffer 或记录字符边界，避免 `char_indices()` 全扫。 |
| 4 | `session_summary` 反复读盘 | `summary.rs:436-731`：对 routes/bindings/denials/events 加基于 mtime 的解析缓存。 |
| 5 | `write_input` / `interrupt_session` 锁内 sleep | `session.rs:343`、`session.rs:377`：把 sleep 与延迟写入移到独立线程/锁外。 |

### P1（扩展性与体验，建议次优先）

| # | 问题 | 建议修改点 |
|---|------|-----------|
| 1 | `broadcast` 克隆大 String | `session.rs:490-493`：`StreamEvent.data` 改为 `Arc<str>`；channel 考虑有界。 |
| 2 | SSE/WS replay 全量 clone | `http.rs:288`、`http.rs:331`：返回 `clean_replay` 的 `Arc<String>` 快照。 |
| 3 | `clean_session_output` 全量清洗 | `summary.rs:798-800` + `terminal.rs:132-139`：reader 线程增量维护 clean tail 缓存。 |
| 4 | `looks_like_menu_prompt` 双重反转 | `mcp.rs:385-399`：用 `char_indices()` 从后找边界，避免两次全串遍历。 |
| 5 | `context_injection` 递归扫描 reports | `hooks.rs:360-387`：维护增量计数缓存，避免每次 `PostToolBatch` 都扫目录。 |
| 6 | board / attention 调用全量 summary | `summary.rs:899-934`：提供只读 attention 所需字段的轻量函数。 |

### P2（观测与长期架构）

| # | 问题 | 建议修改点 |
|---|------|-----------|
| 1 | 缺少 reader/summary 延迟观测 | 在 `spawn_reader`、`session_summary`、`mcp_session_send` 增加 `tracing` / histogram 指标。 |
| 2 | MCP `session_send` P99 未量化 | 增加按 action 分位的延迟日志（`stop` / `interrupt` / `select_option` / `send`）。 |
| 3 | PTY 读线程为同步模型 | 评估迁到 `tokio-pty-process` 或类似 async PTY，减少锁与线程切换开销。 |
| 4 | `state_writer` 全局锁可能成为长期瓶颈 | 拆分“内存状态锁”与“磁盘序列化锁”，关键路径只争用内存锁。 |

---

## 5) 验收 smoke 建议

以下 smoke 测试用于验证修复是否生效，均可在不改动业务语义的前提下进行。

1. **高流量 PTY 输出压力测试**
   - 启动一个 session，命令例如 `cmd /c "for /L %i in (1,1,100000) do @echo hello world %i"`（Linux 可用 `yes | head -n 100000`）。
   - 同时持续调用 `/api/sessions/{name}/output/clean` 与 `/api/board?section=sessions`，观察 daemon CPU 占用与响应延迟。
   - **通过标准**：在高输出阶段，API P99 延迟不应明显高于空闲阶段；CPU 不应因 `decode_utf8_stream` 或 `append_limited_text` 而单核跑满。

2. **MCP `session_send` 连续调用延迟测试**
   - 对同一 running session 连续调用 `agentcall_session_send(action=continue)` 10 次，测量每次返回耗时。
   - **通过标准**：P99 < 50 ms（当前预期因 `session_summary` 文件 I/O 可能 > 100 ms）。

3. **权限菜单 `select_option` 延迟测试**
   - 让 PTY 进入 Claude Code 权限菜单（或动态 workflow 菜单），调用 `agentcall_session_send(action=select_option, text=1)`。
   - **通过标准**：从 MCP 调用到返回 `menu_option_selected` 应在 30 ms 内完成（不含 PTY 自身渲染时间）。

4. **PostToolBatch hook 延迟测试**
   - 在 worker 执行多轮 tool batch 时，持续触发 Claude Code 的 `PostToolBatch` hook，测量 daemon 的 `/api/hooks/ingest` 响应时间。
   - 在 `.agentcall/tasks` 下人为创建大量 report 文件（例如 1000 个空 JSON），复测。
   - **通过标准**：文件增多前后，hook 响应时间差异 < 20 ms。

5. **stop / cleanup 不阻塞并发测试**
   - 启动两个 session A 与 B，让 A 产生大量输出，B 保持空闲。
   - 对 A 调用 `stop_session`，在 A 退出期间持续向 B 发送 `agentcall_session_send(action=continue)` 并测量延迟。
   - **通过标准**：B 的 send 延迟不应因 A 的 cleanup 出现 > 200 ms 的毛刺。

6. **interrupt 快速返回测试**
   - 在 worker 运行时调用 `agentcall_session_send(action=interrupt, text="请写报告")`。
   - **通过标准**：MCP 响应应在 20 ms 内返回，redirect 文本的写入可异步完成。

---

## 结论

当前 `agentcall-daemon` 的 PTY/session I/O 路径在**功能上正确**，但在高输出、高并发查询、频繁 MCP 控制的场景下存在以下主要性能风险：

1. **Reader 线程内部多项 O(n) / O(n²) 操作**：控制序列扫描、UTF-8 解码、clean_replay 截断。
2. **session_summary 与 board 路径存在大量重复磁盘 I/O 与字符串清洗**：每次调用都重新读取 state 文件并清洗 512 KB 输出。
3. **交互 API（send / interrupt / stop）包含同步 sleep 或重量级 cleanup**，直接拉长 MCP 调用延迟并阻塞其他 session。
4. **PostToolBatch 的 context_injection 递归扫描 reports**，在文件多时会放大 hook 延迟。

建议按 **P0 → P1 → P2** 的顺序逐步优化，优先解决 reader 线程的解码/截断/扫描问题，以及 `session_summary` 的缓存问题，可快速获得最大的延迟与 CPU 收益。
