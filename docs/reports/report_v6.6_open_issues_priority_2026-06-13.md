# AgentCall v6.6 未关闭 Issue 优先级整理

> 日期：2026-06-13  
> 来源报告：`docs/reports/report_code_reasonability_2026-06-13.md`  
> 对照闭口：`docs/reports/report_v6.6_code_optimization_closure_2026-06-13.md`

## Summary

这份清单只整理原代码优化报告中 **v6.6 尚未完全关闭** 的 issue。  

- `partial`：v6.6 已做第一阶段修复，但原 issue 还不能判定关闭。
- `open`：v6.6 未直接处理。
- “原优先级”保留原报告的 P0/P1/P2。
- “我的优先级”按 v6.6 后真实风险重排，用 `P0 / P1 / P2 / P3` 表示。

我的总体判断：

- 下一轮最值得优先做的是 **状态字段 enum 化、typed error 继续下沉、SQLite 查询下推、control token 指纹替换、hook policy 函数拆分**。
- HTTP/WebSocket 全量迁移、连接池、schema migration、magic number 集中化重要但不应抢主线。
- JSON 文件锁不应作为主方向；主方向应继续让 live 写入走 daemon + SQLite。

## Open / Partial Issues

| 原优先级 | 我的优先级 | 状态 | Issue | 原证据 | v6.6 后判断 |
|---|---:|---|---|---|---|
| P0 | **P0** | open | `SessionProjectionV1` 字符串状态字段 | `projection.rs:10-56` | 仍是最大可维护性风险。建议内部引入 `LivenessStatus`、`AttentionStatus`、`TurnStatus`、`PatienceStatus` enum，serde 输出继续保持 snake_case 字符串。 |
| P0 | **P1** | partial | `apply_event_to_projection` 巨型 reducer | `projection.rs:187-364` | v6.6 已拆出多个 reducer helper，但仍是一个中心 dispatch。后续应继续按事件族拆文件/模块，并让状态 enum 接管字符串分支。 |
| P0 | **P1** | partial | `worker_state_for_session` 手动状态机 | `worker_state.rs:119-320` | v6.6 已抽出 `decide_worker_state` / `decide_prompt_gate_state`，但还不是状态转换表。建议在状态 enum 完成后补 `validate_transition` 和 fixture 表。 |
| P1 | **P1** | open | `WorkerStateKind` 无转换校验 | `worker_state.rs:15-35` | 应和上一项合并推进：补合法/非法转换表，避免 report/prompt/terminal 状态互相覆盖。 |
| P1 | **P1** | open | action 名称字符串重复 | `control.rs:347-377` | 仍值得做。建议新增 `ControlAction` enum，并让 MCP schema、command type、allowed/debug actions 从同一处派生。 |
| P0 | **P1** | partial | `StoreWriterRuntimeStore` 单线程写瓶颈 | `store.rs:170-184`, `381-446` | v6.6 已支持 SQLite 6 writer；剩余问题是 JSON fallback 仍单写、默认迁移策略和真实压测证据。当前不再是 P0。 |
| P1 | **P2** | open | 大量 `.lock().unwrap()` 未处理 poisoning | 多处 `Mutex::lock().unwrap()` | 单机 daemon 下不是当前体验主痛点，但长期稳定性需要统一 `lock_or_recover` helper 并记录 poisoning event。 |
| P1 | **P1** | open | 锁顺序缺少全局约定 | `control.rs:85-180` 等 | 比 poisoning 更重要。建议写入代码注释/AGENTS，并对同时拿多把锁的函数做小步审查。 |
| P1 | **P1** | open | 持锁期间执行 I/O | `ownership.rs:57-69`, `87-98` | 可能直接造成 route/lease 卡顿。建议先 clone state、drop lock、再 persist。 |
| P2 | **P2** | open | `TerminalScreen` 线程安全契约不清 | `terminal_screen.rs:47-58` | 需要确认访问边界。除非继续出现 TUI snapshot 数据竞争，否则不抢优先级。 |
| P0 | **P0** | partial | 公共 API 仍大量 `Result<T, String>` | `store.rs`, `runtime.rs`, `commands.rs`, `mcp.rs` | v6.6 只把响应层和安全锁入口 enum 化。后续应定义 `AgentCallError` / `StoreError` / `RuntimeError` 并逐步替换核心 trait 签名。 |
| P1 | **P2** | partial | HTTP 状态映射/错误分类依赖字符串 | `errors.rs` | v6.6 已转成 `ErrorCode` enum，但 `classify_message` 仍保留 legacy string fallback。继续替换 `Result<T, String>` 后才能真正关闭。 |
| P1 | **P1** | open | MCP 错误日志与响应结构不一致 | `mcp.rs:135-165` | 真实排障价值高。建议 MCP tool error event 直接记录同一个 structured error object，而不是记录字符串再响应 JSON。 |
| P0 | **P2** | open | JSON rotating NDJSON 非原子旋转 | `store_json.rs:366-392` | 因 v6.6 live 推荐 SQLite，此项从 P0 降为 P2。若 JSON 只做 fallback/debug，不值得先大修。 |
| P0 | **P0** | open | SQLite `get_events` 过滤/limit 未下推 SQL | `store_sqlite.rs:60-117` | v6.6 已把 live 切到 SQLite，这项重要性上升。应尽快把 `event_types` 和 `LIMIT` 下推 SQL，避免 board/events 查询随日志增长变慢。 |
| P1 | **P2** | open | JSON `get_events` 全量读取 | `store_json.rs:38-74` | 同 JSON fallback 逻辑，建议不再投入复杂索引，除非决定继续支持 JSON live。 |
| P1 | **P1** | open | SQLite 每次新建连接 | `store_sqlite.rs:524-529` | 6 writer 后连接创建成本更明显。可以先做线程本地 connection，再考虑连接池。 |
| P1 | **P3** | open | JSON 索引文件 read-modify-write 无锁 | `store_json.rs:394-418`, `hooks.rs` | 不建议用加锁作为主路线；应继续把 live 写入收敛到 daemon + SQLite。仅保留最低限度保护。 |
| P2 | **P3** | open | SQLite schema 内联字符串 | `store_sqlite.rs:406-522` | 需要做，但不影响当前控制面体验。等 schema 稳定后再迁移到版本化 migration。 |
| P0 | **P0** | open | control token 指纹使用 FNV-1a | `control.rs:217-224` | 成本低、收益明确，应尽快改为 SHA-256/BLAKE3 指纹。 |
| P0 | **P2** | open | HTTP 层手写 SHA1/base64 | `http.rs:637-731` | 安全债真实存在，但本地 loopback 场景下不如 control token 指纹紧急。可用 crate 替换，避免全量 HTTP 重写。 |
| P1 | **P2** | open | 自定义 HTTP parser | `http.rs:87-139` | 不建议立刻迁移 hyper/axum。先补连接数、header/body limit、错误码和测试即可。 |
| P1 | **P2** | open | 自定义 WebSocket parser | `http.rs:567-606` | 同上。若 viewer/stream 成为主链路，再迁移 `tokio-tungstenite`。 |
| P1 | **P2** | open | MCP 每次新建 TCP 连接 | `daemon_client.rs:33-79` | 若 MCP 仍出现几十秒延迟，需要量化是不是连接成本。当前更像 payload/日志问题，暂列 P2。 |
| P2 | **P3** | open | URL decode 只处理少数编码 | `http.rs:836-842` | 小债，低成本但低风险。可顺手换 `percent-encoding`。 |
| P0 | **P1** | open | Windows `kill_tree` fallback 不真正杀父进程 | `process.rs:51-85` | 和真实 stop/ghost session 问题相关，建议排 P1。若 stop 问题复现，升 P0。 |
| P1 | **P3** | open | `WindowsJobHandle` 缺 SAFETY 注释 | `process.rs:99-112` | 文档/审计债，建议随 process.rs 修改顺手补。 |
| P1 | **P2** | open | `spawn_reader` 持 replay 锁做 extend/drain | `session.rs:185-251` | 如果 TUI/session 查询继续慢，应升 P1。当前先观察。 |
| P1 | **P1** | open | actor panic 后没有重启/清理策略 | `actor.rs:57-75` | 和 worker 卡死/命令永久失败相关。建议至少把 session 标 terminal failed 并释放 lease。 |
| P2 | **P3** | open | SDK runtime stub capability 不一致 | `runtime_sdk.rs:33-38` | ACP/SDK 已非主线，低优先级；可改 `supports_sdk=false` 作为小修。 |
| P0 | **P0** | open | `ingest_hook` 巨型函数 | `hooks.rs:177-319` | hook 是当前 AgentCall 状态权威入口之一。应拆 parse/policy/binding/projection/event append，优先级高。 |
| P0 | **P0** | open | `pre_tool_use_claim_locked` 混合策略/IO/JSON | `hooks.rs:881-989` | policy deny、claim、report ready、bounded write 都依赖这里。应尽快拆纯策略函数，方便测试和修权限体验。 |
| P1 | **P1** | open | Bash 只读黑名单容易绕过 | `hooks.rs:2011-2058`, `agentcall-hook/src/main.rs` | 由于 AgentCall 当前依赖 bounded write policy，这项不能太低。建议明确它不是强安全边界，收敛成小白名单或禁写 shell。 |
| P1 | **P2** | open | `WRITE_TOOLS` 硬编码 | `agentcall-hook/src/main.rs:8` | 会随 Claude 工具名 drift。可以集中到共享配置或生成常量。 |
| P1 | **P2** | open | hook 二进制 append event 无文件锁 | `agentcall-hook/src/main.rs:134-152` | 如果 hook 主路径都 POST daemon，则优先级下降；应确认 legacy fallback 不再是 live 路线。 |
| P1 | **P2** | open | magic number 分散 | 多处常量 | 不要一次性全搬 config。建议只把影响真实体验的 ack deadline、patience、replay limit、store writer 这类先配置化。 |
| P1 | **P3** | open | `configured_claude_workspace` 错误字符串过长 | `session.rs:321-328` | 可读性债，低风险。 |
| P1 | **P3** | open | `check_or_record_idempotency` 仅测试存在 | `commands.rs:343-409` | v6.x 已有 RuntimeStore command registry，除非需要生产重建命令索引，否则不急。 |
| P2 | **P2** | open | prompt gate JSON builder 重复 | `routes.rs:1309-1334` | v6.6 动过 prompt gate，后续可以顺手提 builder，减少 contract 漂移。 |
| P2 | **P1** | open | MCP budget 修剪反复序列化 | `mcp.rs:833-886` | 如果 MCP 慢继续复现，这项应升 P1；它直接关系 Codex 工具调用体积和延迟。 |

## 我建议的下一轮排序

### Next P0

1. `SessionProjectionV1` 内部 enum 化，外部 JSON 保持字符串。
2. `AgentCallError` typed error 下沉到核心 trait / route / command。
3. SQLite `get_events` SQL 下推过滤和 limit。
4. control token 指纹改 SHA-256/BLAKE3。
5. `hooks.rs` 的 ingest / PreToolUse policy 拆纯函数。

### Next P1

1. worker state 转换表和 `validate_transition`。
2. action enum 统一 MCP / control / command。
3. lease 持锁 I/O 拆分。
4. SQLite connection 复用。
5. actor panic 后 session terminalization + lease cleanup。
6. Windows kill fallback 真正 kill parent。
7. MCP error event 与响应结构统一。
8. MCP budget 修剪优化。

### 暂不主推

- JSON 后端复杂锁和索引：除非重新决定支持 JSON live，否则只做保底。
- HTTP/WebSocket 全量迁移：先补局部 limit 和标准 crate，等 viewer/stream 成为主链路再迁。
- SDK runtime：非当前 PTY-first 主线。
