# AgentCall Daemon 性能审查报告：状态 / 日志 / Summary

**审查范围：** `crates/agentcall-daemon/src/summary.rs`、`state.rs`、`hooks.rs` 及其上下游调用链  
**审查方式：** 只读源码审查，不涉及修改  
**日期：** 2026/06/09

---

## 一、巨大文件 / 目录读写风险

### 1.1 事件日志：`events.ndjson` / `events/recent.ndjson`

| 位置 | 风险等级 | 说明 |
|------|----------|------|
| `state.rs:90-102` `read_events()` | **P1** | 每次调用从磁盘 `seek` 到末尾，读取最多 **2 MB** 尾部，逐行 JSON parse。`board_state()``、`runtime_health()`、`session_summary()` 都会直接或间接调用。 |
| `state.rs:434-447` `read_tail_text()` | **P1** | 使用 `SeekFrom::Start(len - 2MB)` + `read_to_end()`，大文件下 seek 开销可接受，但 **String::from_utf8_lossy** 会分配完整 2MB 字符串。 |
| `state.rs:419-432` `recent_events_path_for()` | **P2** | 旧路径 `events.ndjson` 与新路径 `events/recent.ndjson` 并存，旧文件不会被自动清理，长期运行后形成 **双份存储** 浪费。 |

**量化评估：** 若 board 前端每秒轮询 1 次，一天内 `read_events()` 会读取约 **2 MB × 86,400 = 165 GB** 的累计 I/O 量（全从 page cache 则降为内存拷贝，但仍有解析 CPU 开销）。

### 1.2 Transcript 文件：完整全量读取

| 位置 | 风险等级 | 说明 |
|------|----------|------|
| `summary.rs:1169-1233` `plan_artifact_from_binding()` | **P0** | 对 `transcript_path` 指向的 `.jsonl` 文件执行 **`fs::read_to_string()` 全量读取**，然后逐行 `serde_json::from_str()` 解析。长时间会话的 transcript 可达 **数百 MB 至数 GB**，每次 `session_summary` 触发 plan 检测时都会完整扫描。 |

**关键代码：**
```rust
// summary.rs:1171
if let Ok(text) = fs::read_to_string(transcript) {
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue; };
        // ... 逐行匹配 plan_mode / ExitPlanMode
    }
}
```

**影响：** `GET /api/sessions/{name}/summary` 和 board 中的 `plan_ready` 字段都依赖此路径。这是本审查中 **最严重的单点性能风险**。

### 1.3 Tasks / Reports 目录：递归遍历

| 位置 | 风险等级 | 说明 |
|------|----------|------|
| `summary.rs:1388-1406` `read_reports()` | **P1** | 遍历 `.agentcall/tasks/` 下每个 task 子目录的 `reports/*.json`。task 和 report 数量随时间线性增长，**无深度限制、无分页**。 |
| `hooks.rs:360-387` `count_reports()` | **P2** | 每次 `context_injection()`（即每次 `SessionStart`/`UserPromptSubmit`/`PostToolBatch` hook）都递归遍历 `.agentcall/tasks` 和 `.agentcall/reports` 统计文件数。hook 调用频率远高于 board 查询。 |

### 1.4 Legacy Sessions 目录遍历

| 位置 | 风险等级 | 说明 |
|------|----------|------|
| `summary.rs:867-897` `legacy_detached_sessions()` | **P2** | 遍历 `.agentcall/sessions/` 下所有子目录并读取 `state.json`。session 数量增长时线性变慢，且每次 `board_state()` 和 `runtime_health()` 都会调用。 |

### 1.5 Artifact 文件：只写不清理

| 位置 | 风险等级 | 说明 |
|------|----------|------|
| `state.rs:300-325` `write_text_artifact()` | **P1** | 当 tool 输出超过 `TOOL_OUTPUT_INLINE_LIMIT` (4096 字节) 时，将完整内容写入 `artifacts/hooks/{hook_name}/{event_id}-{label}.txt`。**无 TTL、无清理策略、无上限制**。大量 PostToolUse/PostToolBatch 会产生海量小文件。 |

---

## 二、Board / Summary 构造成本

### 2.1 `board_state()`：N 次重复磁盘读取，无缓存

**位置：** `summary.rs:20-110`

每次 `GET /api/board` 调用 `board_state()` 时，串行执行以下 I/O：

```
1. list_sessions(state)                          → HashMap lock
2. cleanup_stale_runtime_state(state, ...)       → 读 active_sessions.json + pending_supervisor_instructions.json + 写回
3. attention_items(state)                        → 对每个 live session 调用 session_summary()
4. read_events(&events.ndjson)                   → 读 2MB tail
5. read_json_file(state/project.json)            → 读 + parse
6. read_json_file(state/active_sessions.json)    → 读 + parse
7. read_json_file(state/file_claims.json)        → 读 + parse
8. read_json_file(state/transcripts.json)        → 读 + parse
9. read_reports(&tasks/)                         → 递归遍历所有 reports
10. routes_state(state)                          → 读 routes.json
11. legacy_detached_sessions(&sessions/)          → 遍历所有 legacy session
12. runtime_health(state)                        → 再次 list_sessions + cleanup_stale_runtime_state + 读 file_claims.json
```

**重复读取问题：** 同一次 `board_state()` 调用中：
- `file_claims.json` 被读取 **2 次**（board 自身 + runtime_health 内的 stale_claim_count）
- `active_sessions.json` 被读取 **2 次**（board 自身 + cleanup_stale_runtime_state 内部）
- `list_sessions()` 被调用 **3 次**（line 27, line 65, runtime_health 内）

**级联放大：** `attention_items()` 对每个 live session 调用 `session_summary()`，而 `session_summary()` 内部又会：
- 读取 `file_claims.json` 一次
- 调用 `routes_state()`（读取 `routes.json`）
- 调用 `runtime_bindings_state()`（读取 `runtime_binding.json`）
- 调用 `policy_denials_state()`（读取 `policy_denials.json`）
- 调用 `last_supervisor_instruction_injected_at()`（**重新读取 events！**）
- 调用 `session_has_seen_hook_event()`（**重新读取 events！**）

**结论：** 若有 M 个 live session，一次 board 请求的总 I/O 量约为：
> `events × (2 + 2M) + file_claims × (2 + M) + routes × (1 + M) + ...`

### 2.2 `session_summary()`：单会话 Summary 的重负载

**位置：** `summary.rs:436-731`

| 操作 | 开销 | 说明 |
|------|------|------|
| `session.status.lock()` | 低 | 但频繁的锁竞争在并发 board 请求时累积 |
| `clean_session_output()` | **中** | `clean_replay.lock()` + `clone()` + `tail_lines(120)`。`clean_replay` 上限 512KB，clone 成本固定。 |
| `routes_state(state)` | **高** | 每次重新读取 `routes.json` 并 parse |
| `runtime_bindings_state(state)` | **高** | 每次重新读取 `runtime_binding.json` 并 parse |
| `policy_block_for_wrapper()` | **高** | 读取 `policy_denials.json` |
| `read_json_file(file_claims.json)` | **高** | 每次重新读取 |
| `last_supervisor_instruction_injected_at()` | **极高** | **重新读取全部 events**（2MB），然后反向遍历匹配 |
| `session_has_seen_hook_event()` | **极高** | **再次重新读取全部 events**（2MB），反向遍历匹配 |

**关键问题：** `last_supervisor_instruction_injected_at()` 和 `session_has_seen_hook_event()` 这两个函数在 `session_summary()` 内被调用，意味着 **每个 live session 都会独立触发一次 2MB events 读取**。3 个 live session = 3 × 2MB = 6MB events 读取，用于回答一次 board 查询。

### 2.3 `runtime_health()`：GET 请求触发写操作

**位置：** `summary.rs:112-157`

`runtime_health()` 内部调用 `cleanup_stale_runtime_state()`，后者：
1. 获取 `state.state_writer.lock()`（全局串行写锁）
2. 读取 `active_sessions.json`
3. 过滤 stale keys，修改内存对象
4. 若变更则 **写回** `active_sessions.json`
5. 读取 `pending_supervisor_instructions.json`
6. 过滤 stale wrappers，若变更则 **写回**

**这意味着：** 对 `/api/runtime/health` 的纯 GET 请求可能触发文件写入，并在高并发 board 轮询时造成 **全局 state_writer 锁竞争**。

---

## 三、Hook 日志膨胀路径

### 3.1 事件双写

**位置：** `state.rs:142-150` `append_agent_event_locked()`

每个 hook 事件被写入两个位置：
1. `.agentcall/events/recent.ndjson`（上限 2MB，按日期归档）
2. `.agentcall/logs/hooks/{HookType}/recent.ndjson`（上限 4MB，按日期归档）

**膨胀分析：** 一个活跃的 Claude Code 会话每秒可能产生数条 hook 事件（PreToolUse、PostToolUse、PostToolBatch 等）。假设平均 2 条/秒：
- 每天 events 写入量：~2 × 86400 = 17.3 万条
- 每条 JSON 平均 ~2KB
- **单日双写总量：~692 MB**
- 归档文件按日期存放在 `archive/YYYY-MM-DD/` 下，**永久保留**

### 3.2 大工具输出 Artifact 写入

**位置：** `state.rs:205-298` `sanitize_tool_response()` 系列

当 `PostToolUse` / `PostToolBatch` 中的 tool_response 超过 4096 字节时：
1. 完整内容写入 `artifacts/hooks/{hook_name}/{event_id}-{label}.txt`
2. 在事件 JSON 中保留截断版本 + artifact 路径

**典型场景：** `Bash` 工具执行 `git log`、`find`、`grep -r` 等命令，stdout 可达数十 MB。每次这样的调用都产生一个 artifact 文件。

**风险：** 无总量控制、无过期清理。运行数周后，`artifacts/` 目录可能膨胀到 **数十 GB**。

### 3.3 Hook Index 归档无上限

**位置：** `state.rs:378-405` `append_rotating_ndjson()`

```rust
if metadata.len().saturating_add(line_bytes) > max_bytes {
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let archive_dir = dir.join("archive").join(date);
    // ... rename current to archive, then continue appending
}
```

归档按日存放，但：
- **无归档总大小限制**
- **无归档文件数量限制**
- **无自动清理旧归档策略**

---

## 四、投影缓存相关慢路径

### 4.1 全局串行写锁：`state_writer`

**位置：** `state.rs:23` `Mutex<()>`

所有状态修改操作都通过 `state.state_writer.lock()` 串行化：

| 调用方 | 场景 | 持有锁时长 |
|--------|------|------------|
| `append_agent_event()` / `append_agent_event_locked()` | 每次 hook 调用 | 写 events + hook index |
| `cleanup_stale_runtime_state()` | 每次 board / health 查询 | 读 + 可能写 active_sessions + pending_instructions |
| `ingest_hook()` | 每次 hook 调用 | 读 + 写 runtime_binding + file_claims + active_sessions + policy_denials + unmatched_hooks |
| `patch_route_record_locked()` | route 更新 | 读 + 写 routes.json |
| `upsert_route_record()` | 新 route | 读 + 写 routes.json |

**瓶颈：** 在高频 hook 调用（每秒多次）与高频 board 轮询并发时，所有操作排队等待单一互斥锁。锁内还包含文件 I/O，进一步放大延迟。

### 4.2 零内存缓存：所有状态函数都是纯磁盘读取

当前架构中没有任何内存缓存层。以下函数每次调用都完整走磁盘：

- `read_json_file()` — 总是 `fs::read_to_string()` + `serde_json::from_str()`
- `read_events()` — 总是 `read_tail_text()` + 逐行 parse
- `routes_state()` — 总是 `read_routes()`
- `runtime_bindings_state()` — 总是读 `runtime_binding.json`
- `file_claims_state()` — 总是读 `file_claims.json`
- `policy_denials_state()` — 总是读 `policy_denials.json`

**这意味着：** 即使同一毫秒内连续 10 次 board 查询，也会执行 10 次完全相同的文件读取和 JSON 解析。

### 4.3 `next_event_number_from_log()` / `next_runtime_seq_from_state()`

**位置：** `state.rs:449-495`

Daemon 启动时：
1. 遍历 `event_log_candidates()`（2 个路径）
2. 对每个路径读取 2MB tail
3. 逐行 parse JSON，提取 `evt-XXXXXX` 格式的 ID
4. 同时扫描 `routes.json` 递归收集 route number

**启动延迟：** 若 events 文件已积累数月归档，`recent.ndjson` 本身可能接近 2MB 上限，启动扫描的解析开销在 **10-100ms** 量级（取决于 CPU）。

---

## 五、P0 / P1 / P2 修复建议

### 🔴 P0 — 必须修复（单点可导致严重延迟或资源耗尽）

| # | 问题 | 建议修复 | 涉及文件/函数 |
|---|------|----------|---------------|
| P0-1 | Transcript 全量读取 | **增量索引 + 缓存**：首次读取 transcript 时构建内存索引（plan_mode 位置、ExitPlanMode 位置），后续只读取新增行。或者维护一个 `transcript_index.json` 旁路文件。 | `summary.rs:plan_artifact_from_binding()` |
| P0-2 | 每个 session 独立读 events | **在 session_summary 调用方预加载 events**：`board_state()` 和 `attention_items()` 应只读一次 events，作为参数传递给 `session_summary()`，避免 N 次重复读取。 | `summary.rs:session_summary()` |
| P0-3 | board 请求触发写操作 | **移出读路径**：`cleanup_stale_runtime_state()` 不应在 `board_state()` / `runtime_health()` 中同步调用。改为后台定时任务（如每 30 秒）或单独的 POST `/api/maintenance/cleanup` 端点。 | `summary.rs:cleanup_stale_runtime_state()` |

### 🟡 P1 — 强烈建议修复（显著影响吞吐或资源使用）

| # | 问题 | 建议修复 | 涉及文件/函数 |
|---|------|----------|---------------|
| P1-1 | 无内存缓存 | **在 AppState 中加入 `RwLock<HashMap<PathBuf, (mtime, Value)>>` 缓存**：`read_json_file()` 先检查缓存，命中则直接返回。写操作更新缓存。适用于 routes.json、file_claims.json、runtime_binding.json 等小型状态文件。 | `state.rs:read_json_file()` `write_json_file()` |
| P1-2 | Artifact 文件无清理 | **添加 TTL 清理**：启动时扫描 `artifacts/` 和 `archive/`，删除超过 7 天的文件。或在 `append_rotating_ndjson()` 中限制每个 hook 类型的归档总数。 | `state.rs:write_text_artifact()` `append_rotating_ndjson()` |
| P1-3 | read_reports 线性遍历 | **添加缓存或限制**：只读取最近 N 个 task 的 reports，或维护一个 `reports_index.json` 增量索引。 | `summary.rs:read_reports()` |
| P1-4 | events 双写 | **评估是否保留双写**：`events/recent.ndjson` 已包含所有事件，`logs/hooks/{type}/recent.ndjson` 可按需重建。若保留，建议 hook index 不写完整事件 payload（只写 id + type 的轻量索引）。 | `state.rs:append_agent_event_locked()` `append_hook_index()` |
| P1-5 | `read_tail_text` 全量分配 | **改为流式读取**：用 `BufReader` 从 seek 位置流式读取，只保留最后 N 行，避免分配完整 2MB 字符串。 | `state.rs:read_tail_text()` |

### 🟢 P2 — 建议优化（改善长期可维护性和边缘场景）

| # | 问题 | 建议修复 | 涉及文件/函数 |
|---|------|----------|---------------|
| P2-1 | `list_sessions` 多次调用 | **在 board_state 内只调用一次**，将结果复用。 | `summary.rs:board_state()` |
| P2-2 | `context_injection` 递归遍历报告 | **缓存报告计数**：在 AppState 中维护 `AtomicUsize` 计数器，在报告创建/删除时更新，避免每次 hook 都遍历目录。 | `hooks.rs:count_reports()` |
| P2-3 | `legacy_detached_sessions` 遍历 | **异步后台刷新**：改为定时扫描，结果缓存到 `AppState` 中。 | `summary.rs:legacy_detached_sessions()` |
| P2-4 | `events.ndjson` 遗留文件 | **启动时迁移并删除旧文件**，或在文档中明确指导用户手动清理。 | `state.rs:recent_events_path_for()` |
| P2-5 | `clean_session_output` 全量 clone | **返回 `&str` 或 Cow**：`tail_lines` 操作不需要 clone 完整 512KB 字符串，可以直接在原地切片。 | `summary.rs:clean_session_output()` `terminal.rs:tail_lines()` |
| P2-6 | `state_writer` 粗粒度锁 | **按文件分锁**：不同状态文件使用独立的 `Mutex`，减少锁竞争。或使用 `parking_lot::RwLock` 实现读并发。 | `state.rs:AppState.state_writer` |

---

## 六、可测指标

以下指标可用于量化验证修复效果：

### 6.1 Board 查询延迟

```
指标名：board_state_latency_ms
测量方式：在 board_state() 入口和出口打点计时
基准值（当前预期）：
  - 0 live sessions, 0 tasks: ~5-20 ms
  - 3 live sessions, 10 tasks: ~50-200 ms
  - 3 live sessions, 100 tasks: ~200-1000 ms
目标值（P0+P1 修复后）：
  - 所有场景: < 20 ms（95th percentile）
```

### 6.2 Session Summary 延迟

```
指标名：session_summary_latency_ms
测量方式：在 session_summary() 入口和出口打点
基准值（当前预期）：
  - 无 events: ~5-10 ms
  - 2MB events 文件: ~30-80 ms
目标值（P0-2 修复后）：
  - 所有场景: < 10 ms
```

### 6.3 Events 读取放大

```
指标名：events_bytes_read_per_board_query
测量方式：统计单次 board_state() 调用链中 read_tail_text() 读取的总字节数
基准值（当前）：
  - 0 sessions: ~2 MB
  - N sessions: ~2 × (1 + N) MB
目标值（P0-2 修复后）：
  - 所有场景: ~2 MB
```

### 6.4 Hook 处理延迟

```
指标名：ingest_hook_latency_ms
测量方式：在 ingest_hook() 入口和出口打点
基准值（当前预期）：
  - 简单 hook (Stop): ~5-15 ms
  - PreToolUse (含 file claim): ~10-30 ms
  - 含 context injection: ~20-50 ms（因 count_reports 遍历）
目标值（P1-1 缓存 + P2-2 计数缓存后）：
  - 所有场景: < 10 ms
```

### 6.5 磁盘使用增长

```
指标名：daily_disk_growth_mb
测量方式：监控 .agentcall/ 目录 24 小时增量
基准值（当前预期）：
  - 活跃会话 1 个: ~200-500 MB/天（含 artifacts）
  - 活跃会话 3 个: ~600 MB-1.5 GB/天
目标值（P1-2 artifact 清理 + P1-4 双写优化后）：
  - 稳定态: < 100 MB/天（7 天滚动窗口）
```

### 6.6 State Writer 锁竞争

```
指标名：state_writer_wait_time_ms
测量方式：在 lock() 前后打点，记录等待时间
基准值（当前预期）：
  - 低并发: ~0 ms
  - 10 req/s board + 5 hook/s: ~5-50 ms
目标值（P0-3 + P2-6 修复后）：
  - 所有场景: < 1 ms
```

---

## 七、风险矩阵

| 风险 | 影响 | 可能性 | 当前缓解措施 | 修复优先级 |
|------|------|--------|--------------|------------|
| Transcript 全量读取导致 summary API 超时 | 高 | 高 | 无 | **P0-1** |
| N 个 session 各读一次 events，board 查询 O(N) 放大 | 高 | 高 | 无 | **P0-2** |
| GET board 触发写文件 + 全局锁 | 高 | 中 | 无 | **P0-3** |
| 零缓存导致并发请求各自重复 I/O | 中 | 高 | 无 | **P1-1** |
| Artifact 文件无限制增长，耗尽磁盘 | 高 | 中 | 无 | **P1-2** |
| Hook index 双写 + 归档无上限 | 中 | 高 | 2MB/4MB 单文件上限 | **P1-3 / P1-4** |
| 全局 state_writer 锁成为并发瓶颈 | 中 | 中 | 无 | **P1-6 / P2-6** |

---

## 八、审查结论

AgentCall daemon 的状态和日志子系统当前采用**纯磁盘状态 + 零缓存 + 粗粒度全局锁**的架构，在单用户、低并发场景下运行良好，但存在以下结构性性能风险：

1. **最严重**：`plan_artifact_from_binding()` 对 transcript 的全量读取，以及 `session_summary()` 中对 events 的 N 次重复读取，是 **O(文件大小 × 会话数)** 的放大器。
2. **次严重**：`board_state()` 在读路径上同步触发写操作（`cleanup_stale_runtime_state`），将 GET 请求转变为持有全局锁的写事务。
3. **长期风险**：日志双写 + artifact 无清理 + 归档无上限的组合，会导致磁盘使用量随运行时间线性增长，最终耗尽存储。

**建议修复顺序：** P0-2（减少 events 读取放大）→ P0-3（分离读写路径）→ P0-1（transcript 增量索引）→ P1-1（内存缓存）→ P1-2（artifact 清理）。
