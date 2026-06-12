# AgentCall 代码合理性与优化审查报告

> 审查目标：`E:\Project\AgentCall` Rust daemon / MCP / PTY / hooks / control-loop 代码
> 审查日期：2026-06-13
> 审查性质：只读静态审查，未修改源码
> 报告文件：`docs/reports/report_code_reasonability_2026-06-13.md`

---

## 1. 代码合理性总体评价

### 1.1 架构层面（合理）

- **状态权威集中在 Rust daemon**：`AppState` 作为唯一可变状态源，MCP bridge、HTTP API、PTY runtime、hooks 都围绕它工作，符合 v6.2/6.3 的规划。
- **运行时边界清晰**：`AgentRuntime` trait（`crates/agentcall-daemon/src/runtime.rs:40-55`）把 PTY/SDK 启动抽象得很好；`ClaudeCodePtyRuntime`（`runtime_pty.rs`）只是薄适配层，职责单一。
- **命令信封模型合理**：`CommandEnvelopeV1`（`commands.rs:14-32`）携带 owner lease、idempotency、control token、precondition，能够实现幂等和并发控制。
- **投影（projection）驱动的 API**：session summary、TUI view、board 都基于 `SessionProjectionV1`，避免直接解析原始 terminal 输出，符合项目目标。
- **钩子策略可落地**：`agentcall-hook` 二进制独立运行，PreToolUse 做文件 claim 冲突检查，PostToolUse 释放/更新 claim，逻辑简单直接。

### 1.2 实现层面（存在明显债务）

- **字符串类型状态机泛滥**：`liveness_status`、`attention_status`、`turn_status`、`event_type` 等大量本应是枚举的字段使用 `String`，导致无法利用 Rust 的 exhaustiveness check，重构风险高。
- **巨型 reducer/分发函数**：`apply_event_to_projection`（177 行）、`worker_state_for_session`（201 行）、`ingest_hook`（142 行）、`session_summary`（289 行）混合了业务规则、副作用、JSON 拼接，单测困难。
- **错误类型退化为 `Result<T, String>`**：从 store 到 runtime 到 MCP bridge，几乎所有公共 API 都返回字符串错误，丢失了错误码、重试语义和调用链。
- **单线程写瓶颈**：`store_writer_loop`（`store.rs`）把所有写操作串行到一个阻塞线程，即使不同 session 的事件写入也排队。
- **文件索引无锁**：`store_json.rs`、`hooks.rs`、`agentcall-hook` 都直接 read-modify-write JSON 文件，没有 advisory lock，并发下可能损坏。
- **手摇编码/哈希**：HTTP 层自己实现 SHA1、base64、WebSocket frame parser（`http.rs:637-731`）；控制 token 用 FNV-1a 做指纹（`control.rs:217-224`），不适合安全场景。
- **HTTP 解析缺少防护**：自定义 HTTP parser 和 WebSocket parser 没有请求速率限制、连接数限制、frame 数量限制，存在 DoS 风险。

### 1.3 已验证事实 vs 推断 vs 建议

- **已验证事实**：代码中确实存在上述函数/文件/行号；`cargo test --workspace` 未在本轮运行，但所有结论均来自源码静态阅读。
- **推断**：部分并发问题（如文件索引竞争、mutex 顺序死锁）在单用户本地场景可能未触发，但在高并发或长期运行后会暴露。
- **建议**：下文按 P0/P1/P2 给出可执行优化项，均附带具体文件路径和函数名作为证据。

---

## 2. 应当优化的代码报告

### 2.1 状态机与类型系统

#### P0 | `projection.rs:10-56` `SessionProjectionV1` 46 字段结构体且大量字段为字符串状态
- **问题**：`liveness_status`、`attention_status`、`turn_status`、`patience_status`、`next_recommended_action` 等应为枚举的字段使用 `String`。
- **影响**：无法在编译期检查状态转换合法性；`compact_event_kind`、`allowed_actions_for_projection` 等处出现大量字符串匹配，容易因拼写或新增状态遗漏分支。
- **建议**：定义 `LivenessStatus`、`AttentionStatus` 等枚举，使用 `#[serde(rename_all = "snake_case")]` 保持 JSON 兼容。

#### P0 | `projection.rs:187-364` `apply_event_to_projection` 177 行巨型 reducer
- **问题**：一个函数内按 `event_type` 字符串做巨大 match，混合状态变更、control_epoch 更新、terminal 事件处理。
- **影响**：新增事件类型需要改这里，容易破坏已有逻辑；单测只能端到端测，难以针对单个事件写单元测试。
- **建议**：拆成 `reduce_user_prompt_submit`、`reduce_pre_tool_use` 等纯函数，顶层只负责 dispatch。

#### P0 | `worker_state.rs:119-320` `worker_state_for_session` 201 行 42 分支手动状态机
- **问题**：用 42 个 if/else 分支从 projection/route/prompt_gate 推导出 `WorkerStateKind`，没有状态转换表。
- **影响**：任何投影字段变化都可能导致推导结果漂移；无法证明所有 `WorkerStateKind` 变体都是可达的。
- **建议**：使用状态转换表 `(current_state, input) -> next_state`，或改用状态机 derive 宏。

#### P1 | `worker_state.rs:15-35` `WorkerStateKind` 17 变体但无转换校验
- **问题**：枚举定义了状态，但没有 `validate_transition(prev, next)`。
- **建议**：添加转换校验函数并补充单元测试覆盖所有合法/非法转换。

#### P1 | `control.rs:347-377` `allowed_actions_for_projection` 返回字符串动作列表
- **问题**：动作名（`"interrupt"`、`"stop"` 等）与控制 token 验证、MCP schema、actor 命令类型多处硬编码重复。
- **建议**：定义 `Action` 枚举，统一转换到字符串。

### 2.2 并发与同步

#### P0 | `store.rs:170-184` / `store.rs:381-446` `StoreWriterRuntimeStore` + `store_writer_loop` 单线程写瓶颈
- **问题**：每个 store 操作通过新创建的 `mpsc` 通道发送给单个后台线程串行执行。
- **影响**：所有 session 的事件写入、command 完成、lease 持久化都排队；一个慢 IO 会阻塞整个 daemon。
- **建议**：如果 backend 是线程安全的（如 SQLite + WAL），直接使用 `tokio::sync::RwLock` 或 `parking_lot::RwLock`；否则按 session 分片 writer。

#### P1 | 全代码大量 `.lock().unwrap()` 未处理 mutex poisoning
- **位置**：`state.rs:28-38`、`control.rs:91/112/175/191/295/381`、`session.rs:85/149/310` 等。
- **问题**：任何线程在持有锁时 panic，锁会中毒，后续所有操作 panic，daemon 不可用。
- **建议**：对可恢复场景使用 `lock().unwrap_or_else(|poisoned| poisoned.into_inner())`，并记录中毒事件。

#### P1 | `control.rs:85-180` `mint_control_token` 多次获取锁且无全局顺序
- **问题**：依次获取 `sessions`、`owner_leases`、`control_tokens`；其他函数获取顺序可能不同，存在死锁风险。
- **建议**：定义全局锁层级（如 sessions → owner_leases → workspace_leases → control_tokens），所有函数按同一顺序获取。

#### P1 | `ownership.rs:57-69` / `87-98` 持锁期间执行 I/O
- **问题**：`prune_expired_leases` 在持有 `owner_leases`/`workspace_leases` 锁时调用 `persist_*_leases` 写磁盘。
- **影响**：所有 lease 操作被阻塞。
- **建议**：clone 数据后先 drop 锁，再持久化。

#### P2 | `terminal_screen.rs:47-58` `TerminalScreen` 跨线程使用但内部状态同步不明
- **问题**：`vt100::Parser` 的线程安全性未文档化；`process` 在 reader 线程调用，而 `snapshot` 在 API 线程调用。
- **建议**：明确 `TerminalScreen` 的线程安全契约，或在访问处加 `Mutex`。

### 2.3 错误处理与日志

#### P0 | 几乎所有公共 trait/函数返回 `Result<T, String>`
- **位置**：`store.rs:89-134`、`runtime.rs:40-55`、`commands.rs:74`、`mcp.rs:135` 等。
- **问题**：错误码、重试提示、源错误链全部丢失；调用方只能通过字符串前缀判断。
- **建议**：定义 `AgentCallError` enum（如 `StoreError`、`ControlError`、`RuntimeError`），实现 `std::error::Error` 和结构化序列化。

#### P1 | `errors.rs:40-57` / `errors.rs:59-90` HTTP 状态映射与错误分类依赖字符串前缀
- **问题**：`status_for_error` 和 `classify_message` 靠字符串前缀匹配决定 HTTP 状态码和错误类别。
- **影响**：新增错误码容易互相覆盖；无法保证 exhaustive。
- **建议**：使用结构化 `AgentCallError` 直接携带 `http_status` 和 `error_code`。

#### P1 | `mcp.rs:135-165` MCP 调用错误被记录后原样返回，但日志与响应结构不一致
- **问题**：`append_agent_event` 记录的 `error` 是字符串，而返回给 MCP 客户端的可能是字符串或 JSON；排查时需对照两种格式。
- **建议**：统一错误值类型，日志和响应使用同一结构化错误对象。

### 2.4 存储与持久化

#### P0 | `store_json.rs:366-392` `append_rotating_ndjson` 非原子旋转
- **问题**：文件旋转通过读 metadata、rename、再 append 实现；失败时可能丢失或重复 recent events。
- **建议**：使用 write-temp-rename 模式，或迁移到 SQLite 后端作为默认。

#### P0 | `store_sqlite.rs:60-117` `get_events` 过度取数
- **问题**：`scan_limit = requested_limit * 10`（最高 5000 行），然后在 Rust 里过滤 event_types 再截断。
- **影响**：浪费数据库 IO 和内存。
- **建议**：把 `event_types` 过滤和 `LIMIT` 直接放到 SQL 中参数化查询。

#### P1 | `store_json.rs:38-74` `get_events` 全量加载事件文件
- **问题**：每次查询都读取整个 ndjson 文件，逐行解析过滤，O(n)。
- **建议**：维护按 session 的 offset 索引，或默认使用 SQLite 后端。

#### P1 | `store_sqlite.rs:524-529` `open_connection` 每次新建连接
- **问题**：没有连接池，也没有复用 prepared statement。
- **建议**：使用 `r2d2`/`deadpool-sqlite` 或至少线程本地缓存。

#### P1 | `store_json.rs:394-418` / `hooks.rs` JSON 索引文件 read-modify-write 无锁
- **问题**：多进程（daemon + hook 二进制 + 其他工具）可能同时写 `file_claims.json`、`routes.json` 等。
- **建议**：使用 advisory file lock（`fs2` crate）或迁移到 SQLite。

#### P2 | `store_sqlite.rs:406-522` 116 行内联 schema 字符串
- **问题**：难以版本控制、diff 和增量迁移。
- **建议**：拆分编号迁移文件，使用 `refinery` 或自定义 schema_version 表。

### 2.5 安全与密码学

#### P0 | `control.rs:217-224` 控制 token 使用 FNV-1a 做指纹
- **问题**：FNV-1a 是 64 位非加密哈希，collision resistance 不足；token 虽为 256-bit 随机，但指纹用于查找和验证。
- **建议**：改用 SHA-256 或 BLAKE2b 指纹。

#### P0 | `http.rs:637-731` 手摇实现 SHA1 和 base64
- **问题**：WebSocket accept key 需要 SHA1+base64，代码里手动实现了完整算法；容易有 subtle bug 且未经过充分审查。
- **建议**：使用 `sha1` / `base64` crate。

#### P1 | `http.rs:87-139` 自定义 HTTP parser
- **问题**：自己解析请求行和 header；Content-Length 解析失败时默认 0； oversized body 通过伪造 method `__payload_too_large` 传递，虽然最终返回 413，但设计丑陋。
- **影响**：缺少 chunked transfer、keep-alive、HTTP/2、请求速率限制、连接数限制。
- **建议**：迁移到 `hyper` 或 `axum`；至少增加请求速率限制和最大 header 数量限制。

#### P1 | `http.rs:567-606` 自定义 WebSocket parser
- **问题**：未处理所有 WebSocket 控制帧和扩展；MAX_WS_FRAME_BYTES 只限制单帧大小，不限制帧数量。
- **建议**：使用 `tungstenite` 或 `tokio-tungstenite`。

#### P1 | `daemon_client.rs:33-79` 每个 MCP 调用新建 TCP 连接
- **问题**：`daemon_request` 每次都 `TcpStream::connect_timeout`、写请求、读响应、关闭连接。
- **影响**：高调用频率下连接开销大。
- **建议**：使用连接池或 HTTP client（如 `reqwest`）。

#### P2 | `http.rs:836-842` `url_decode` 只处理四个固定编码
- **问题**：只替换了 `%20`、`%2F`、`%5C`、`%3A`，其他 percent-encoding 未解码。
- **建议**：使用 `percent-encoding` crate。

### 2.6 PTY / 进程 / 平台相关

#### P0 | `process.rs:51-85` `kill_tree` Windows fallback 不真正杀父进程
- **问题**：如果 `WindowsJobHandle::terminate` 失败，fallback 返回 "best_effort_parent_kill_only" 但实际上没有调用 `kill()`。
- **建议**：在 fallback 路径中真正尝试 `kill()` 父 PID。

#### P1 | `process.rs:99-112` `WindowsJobHandle` 的 `Send`/`Sync` impl 缺少 SAFETY 注释
- **问题**：原始 `isize` 句柄被标记为 `Send` + `Sync`，但没有解释为何安全。
- **建议**：添加 SAFETY 注释，说明 Windows job object handle 是内核句柄、可跨线程使用。

#### P1 | `session.rs:185-251` `spawn_reader` 中持续持有 `replay` 锁做 extend/drain
- **问题**：每次读取 8192 字节都锁定 `replay` 做 extend 和可能的 drain，reader 线程与 API 线程争用。
- **建议**：使用 ring buffer 或无锁结构（如 `crossbeam` channel）批量传递数据后再归档。

#### P1 | `actor.rs:57-75` session actor 线程 panic 后仅记录事件
- **问题**：`run_session_actor_with_panic_guard` 捕获 panic 后没有重启 actor，后续对该 session 的命令会永久失败。
- **建议**：actor panic 后清理 session 并标记为 failed，或实现 supervisor 重启策略。

#### P2 | `runtime_sdk.rs:33-38` SDK runtime 是未实现 stub 但 capability 声明支持
- **问题**：`start` 返回 `experimental_stub` 错误，但 `capabilities().supports_sdk == true`。
- **建议**：在实现前将 `supports_sdk` 设为 `false`，避免 MCP schema 与实际行为不符。

### 2.7 Hooks / 策略

#### P0 | `hooks.rs:177-319` `ingest_hook` 142 行巨型函数
- **问题**：包含 payload 清洗、claim 处理、route patch、event append、policy denial 等多种副作用。
- **影响**：难以单测；新增 hook 事件会进一步膨胀。
- **建议**：拆分为 `parse_hook_event`、`apply_policy`、`update_projection`、`append_event` 等步骤。

#### P0 | `hooks.rs:881-989` `pre_tool_use_claim_locked` 108 行混合策略/IO/JSON 变更
- **问题**：文件 claim 冲突检测、claim 更新、denial 构建都在一个函数里。
- **建议**：拆分为纯策略函数和 effectful writer。

#### P1 | `hooks.rs:2011-2058` / `agentcall-hook/src/main.rs` bash 只读黑名单容易被绕过
- **问题**：通过字符串黑名单判断 bash 是否安全（如检查 `>`、`>>`、`tee`），但 `echo hello | tee file`、反引号、subshell 等都可绕过。
- **建议**：如要强制只读，使用命令 AST 白名单或沙箱；否则不要把它当作安全边界。

#### P1 | `agentcall-hook/src/main.rs:8` `WRITE_TOOLS` 硬编码列表
- **问题**：`Edit`、`MultiEdit`、`Write`、`NotebookEdit` 手动维护，容易与实际工具集 drift。
- **建议**：从 Claude/Codex 工具 schema 生成或集中配置。

#### P1 | `agentcall-hook/src/main.rs:134-152` `append_event` 无文件锁
- **问题**：hook 二进制与 daemon 可能并发写 `events.ndjson`。
- **建议**：使用 advisory lock 或改为通过 HTTP `/api/events` 提交给 daemon 统一写入。

### 2.8 配置与常量

#### P1 | 全代码大量 magic number 和硬编码配置
- 例：`CONTROL_TOKEN_TTL_SECONDS = 60`（`control.rs:11`）、`DEFAULT_ACK_DEADLINE_MS = 15000`（`prompt_gate.rs:8`）、`DEFAULT_COMMIT_ACK_DEADLINE_MS = 8000`、`DEFAULT_MAX_SESSIONS = 6`（`scheduler.rs:9`）、`SCREEN_SCROLLBACK_ROWS = 2000`（`terminal_screen.rs:6`）、`REPLAY_LIMIT = 512KB`（`session.rs:18`）等。
- **建议**：集中到 `LocalConfig` 并允许 `config/agentcall.local.json` 覆盖，文档化默认值 rationale。

#### P1 | `session.rs:321-328` `configured_claude_workspace` 缺少时的错误信息过长且重复
- **问题**：错误字符串作为默认值内嵌在代码中，不便国际化/调整。
- **建议**：拆分为常量或配置项。

### 2.9 测试与可维护性

#### P1 | `commands.rs:343-409` `check_or_record_idempotency` 只在 `#[cfg(test)]` 中存在
- **问题**：生产代码的幂等性逻辑在 `store.rs` 中，但测试辅助函数和 `rebuild_commands_index_from_log` 只在测试编译；实际生产路径的幂等重建能力未暴露。
- **建议**：将重建逻辑提升到生产代码或明确说明为何不需要。

#### P2 | `routes.rs:1309-1334` `submit_pty_prompt_with_ack` / `submit_pty_prompt_without_hook_ack` 大量重复 JSON 结构
- **问题**：两个函数构造几乎相同的 prompt_gate JSON，只是 `status` 和 `awaiting_hook` 不同。
- **建议**：提取 `prompt_gate_value` builder。

#### P2 | `mcp.rs:833-886` `attach_budget` / `insert_budget` / `enforce_json_budget` 预算修剪循环多次序列化
- **问题**：`json_size` 每次都 `serde_json::to_string`，预算收紧循环可能反复序列化大对象。
- **建议**：预算修剪使用 streaming size estimator 或一次性序列化后按 token 截断。

---

## 3. 十大可执行优化项（按 P0/P1/P2 排序）

| 优先级 | 优化项 | 证据文件/函数 | 预期收益 |
|--------|--------|---------------|----------|
| **P0** | 将 `SessionProjectionV1` 的字符串状态字段改为枚举 | `crates/agentcall-daemon/src/projection.rs:10-56` | 编译期保证状态合法性，减少 match 遗漏 |
| **P0** | 拆分 `apply_event_to_projection` 为按事件类型的纯 reducer | `crates/agentcall-daemon/src/projection.rs:187-364` | 可单测、易维护、降低回归风险 |
| **P0** | 将 `worker_state_for_session` 42 分支改为状态转换表 | `crates/agentcall-daemon/src/worker_state.rs:119-320` | 明确可达状态，减少推导漂移 |
| **P0** | 将 `StoreWriterRuntimeStore` 单线程写改为并发或按 session 分片 | `crates/agentcall-daemon/src/store.rs:170-184` / `381-446` | 消除全局写瓶颈，提升并发能力 |
| **P0** | 控制 token 指纹改用 SHA-256 | `crates/agentcall-daemon/src/control.rs:217-224` | 提升安全性，避免哈希碰撞风险 |
| **P0** | HTTP/WebSocket 解析迁移到 `hyper`/`tungstenite` | `crates/agentcall-daemon/src/http.rs:637-731` / `567-606` | 减少自研解析 bug，获得限速/keep-alive |
| **P1** | 定义 `AgentCallError` enum 替代全代码 `Result<T, String>` | `crates/agentcall-daemon/src/store.rs`、`commands.rs`、`runtime.rs` 等 | 保留错误码、重试语义、调用链 |
| **P1** | 为 JSON/ndjson 索引文件添加 advisory lock | `crates/agentcall-daemon/src/store_json.rs`、`hooks.rs`；`crates/agentcall-hook/src/main.rs:134-152` | 防止多进程并发写损坏 |
| **P1** | `store_sqlite.rs:get_events` 将过滤和 limit 下推到 SQL | `crates/agentcall-daemon/src/store_sqlite.rs:60-117` | 减少数据库 IO 和内存占用 |
| **P2** | 集中 magic number 到 `LocalConfig` 并允许本地覆盖 | `control.rs:11`、`prompt_gate.rs:8-9`、`scheduler.rs:9-10`、`terminal_screen.rs:6` 等 | 提高可配置性，便于调优 |

---

## 4. 风险与建议优先级

- **P0 风险**：字符串状态机和巨型 reducer 是当前最大维护风险；新增 hook 事件或状态很容易在多处遗漏。
- **P0 风险**：单线程 store writer 在高频事件场景会成为整个 daemon 的瓶颈。
- **P0 风险**：FNV-1a 用于控制 token、手摇 SHA1/base64 属于安全债，应尽快替换为标准库/crate。
- **P1 风险**：文件索引无锁和 `Result<T, String>` 广泛存在，会在长期运行和高并发下暴露。
- **P2 风险**：magic number、重复 JSON builder、预算序列化性能属于可逐步偿还的代码异味。

---

*报告结束。本报告未修改任何源码，仅基于 2026-06-13 的代码状态进行静态审查。*
