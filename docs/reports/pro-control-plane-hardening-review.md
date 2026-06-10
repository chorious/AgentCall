我整体看了一轮。先说结论：**这个项目不是“写烂了”，而是典型的 Codex 快速堆出架构后，控制面契约、状态机、安全边界、错误语义没有同步硬化。** 当前 `main` 里的 README 已经标成 `v5.3.0 checkpoint`，不是纯 v5.2；项目文档自己也承认 v5.3 仍没完全闭合，actor 队列、stop/kill 语义、actor/output 隔离、orphan 检测、report accept 释放 lease 都还是 open gate。  

我没有成功把仓库 clone 到本地跑 `cargo test`，因为当前执行环境无法解析 `github.com`。所以这份是基于 `main` 分支源码和项目内审查报告做的静态审查，不冒充运行验证结果。

## 最重要的判断

AgentCall 当前已经有清晰产品形状：Codex 通过 MCP bridge 调 Rust daemon，daemon 管 PTY session、hooks、projection、board、routes、reports。README 里描述的架构路径是 Codex → MCP bridge → daemon HTTP API → SessionActor/PTY runtime → Claude Code worker → hooks/projections。

但实现层面现在最大的问题是：**上层像“严肃控制面”，底层仍像“本地实验 daemon”。** 它能跑 happy path，但遇到慢 hook、MCP timeout、worker 卡住、stop/kill、hook 丢失、daemon 重启、恶意/异常输入时，状态一致性和安全语义会明显破。

---

## P0：最先修的功能实现问题

### 1. `agentcall_route(mode=start)` 很容易把 MCP 自己拖超时

这是我认为最可能造成你说的“问题很多 / transport closed / worker 像卡住”的点。

`submit_pty_prompt_with_ack` 会尝试 2 次发送 prompt，每次最多等 8 秒等 `UserPromptSubmit` hook ack。也就是说单次 route start 可能同步等到 16 秒以上。 但 MCP bridge 到 daemon 的 read timeout 是 10 秒。

这两个数字是硬冲突。结果就是：daemon 可能还在正常启动 worker、等 hook，MCP bridge 已经读超时，Codex 侧看到工具失败或 transport 关闭。这个问题不是调大一点 timeout 就彻底解决，根因是 **route start 不应该同步等待 hook ack**。

修法：`route start` 应该在 PTY spawn + prompt command accepted 后立即返回 `started_pending_prompt_ack`，hook ack 走 projection/board 后续观察。MCP 工具调用应该是短事务，不要在里面等 worker 生命周期事件。

---

### 2. `stop` 和 `kill` 语义现在是混的，而且 lease 释放太早

Actor 里 `StopSession | KillSession` 走同一个 `stop_session`，返回也是 `stop_signal_sent`。 但 `stop_session` 实际先 `kill_tree()`，再 `killer.kill()`，然后立刻把 status 设为 `stopping`，并释放 owner/workspace lease。

这会导致两个问题：

第一，API 名字叫 stop，但行为接近 kill，没有 graceful stop、timeout escalation、kill fallback 的分层。第二，lease 在进程真实退出前就释放了，理论上 supervisor 可以启动另一个 worker 抢同一 workspace，而旧进程还没完全退出。

修法：拆成三态：`stop_requested` 只发 graceful signal，不释放 lease；`kill_requested` 才 kill tree；只有 waiter 观察到 `process_exited` 后释放 lease。现在的 release 应该从 `stop_session` 移到 `spawn_waiter` 的退出确认路径。

---

### 3. precondition / lease 防陈旧命令还不够硬

当前 v5.3 文档说 precondition seq validation 已关闭，但代码里只是“如果传了 `projection_last_session_seq` 就校验”。`validate_projection_precondition` 在没有 seq 时直接 Ok。 而 `stop` / `interrupt` 只要求有一个 `precondition` object，不强制里面必须包含 seq、owner lease、generation。

更糟的是，`attach_or_validate_owner_lease` 会把缺失的 `owner_lease_id` / `lease_generation` 自动填进 precondition，而不是拒绝缺失字段。 同时 owner lease ID 永远是 `lease-{session_id}-1`，generation 永远是 `1`。

这意味着“有 precondition”不等于“防陈旧”。一个空 `{}` precondition 对破坏性命令来说不应该过。

修法：对 `stop`、`kill`、`interrupt`、`approve_plan/start_auto` 强制要求：

```text
precondition.projection_last_session_seq
precondition.owner_lease_id
precondition.lease_generation
```

缺一个都拒绝。lease ID 也应该是随机/单调唯一，不要复用 `lease-session-1`。

---

### 4. PTY path policy 不是可靠安全边界

现在的 route containment 主要是写进 prompt 和 hook policy。`pty_path_policy_for_wrapper` 只有在能根据 `wrapper_session` 找到 route 时才会拿到 containment。 如果 hook 没绑定 wrapper，策略就退化到普通 file claim；而普通 file claim 更像协作锁，不是权限边界。`pre_tool_use_claim_locked` 里也是先看 wrapper policy，拿不到 wrapper 后才走普通 write claim。

Bash readonly 也只是字符串黑名单/白名单：检查 `>`, `rm`, `mkdir`, `echo` 等，再允许 `pwd`, `ls`, `cat`, `rg`, `git diff` 等前缀。 这不是 sandbox，绕过方式很多，比如 `python -c`、`node -e`、`sed -i`、`git checkout`、PowerShell 复杂命令、间接脚本等。

修法：把 hook policy 定位为“协作纪律”和“可观测阻断”，不要把它当强安全。真正的硬边界要么依赖 Claude Code 权限模式和 hook 正确安装，要么引入 OS 级 sandbox / workspace copy / allowlisted tool wrapper。至少要做到：wrapper_session 缺失时，对 route worker 默认 fail closed，而不是继续允许。

---

### 5. `agentcall_report(action=accept)` 没有完成生命周期闭环

v5.3 status 自己列了：`Report accept releases worker lease` 仍 open。 代码也印证了：`mcp_report` 的 `accept` 只是读取 board 的 reports，然后加 confidence 返回；没有 mark accepted、没有 release lease、没有 stop/retire worker。

这会让 supervisor 以为“报告已验收”，但 worker/route/lease 状态没有闭合。久了就会出现脏状态、worker 残留、board 上已完成任务还需要 attention 的错觉。

修法：`accept` 至少应该做四件事：标记 report accepted、route status → accepted/done、release owner/workspace lease、可选发送 stop/retire worker。

---

## P1：功能漂移和产品契约问题

### 6. SDK runtime 暴露了，但实际上是 stub

daemon 端 MCP schema 允许 `runtime: sdk`。 但 MCP bridge 端 schema 只暴露 `auto | pty`。 真正的 `ClaudeCodeSdkRuntime` 里 `start` 和 `submit_command` 都直接返回 experimental stub error。

这会造成“文档/daemon/API/bridge 四套说法不一致”。建议现在直接从 daemon schema 移除 `sdk`，或者明确标成 `unsupported_stub`，不要让调用方以为这是半可用能力。

---

### 7. route decision 现在是“PTY-only”，估算字段基本是死参数

`route_decision` 对 `runtime=auto` 直接返回 PTY，并把 `estimated_minutes/files/loc/needs_continuity/risk` 放进 `legacy_estimates_ignored`。

这不是 bug，但产品语义上要诚实。现在叫 “route decision” 容易让人以为有调度/策略选择，其实只是 “start PTY worker”。如果短期不会实现多 runtime routing，就把字段从外部 schema 降级或标为 telemetry，不要让 Codex 以为这些参数影响决策。

---

### 8. `workspace` 和 `claude_workspace` 的语义容易让 worker 写错地方

README 明确说 `claude_workspace` 是 Claude Code PTY 的真实 cwd，而 route 的 `workspace` 只是任务目标目录，不决定 Claude process cwd。 代码也是这样：如果 command 是 Claude，就强制用 configured `claude_workspace`。

这个设计可以成立，但风险很高：worker 的 shell cwd 和任务 workspace 不一致，所有路径约束都要靠 prompt/hook/allowed_paths 传达。只要 hook binding 失败或 prompt 理解偏了，就会出现“看起来在处理 A 项目，实际 cwd 在 B 项目”的问题。

修法：board/session summary 里要非常醒目地显示 `process_cwd` 和 `target_workspace`，并在 prompt 开头强制要求 `cd`/使用绝对路径，或者直接把 Claude process cwd 设成 target workspace，再把 hook settings 问题单独解决。

---

### 9. route 启动失败后的状态回滚不完整

`start_pty_route` 先 `acquire_route_leases_and_create_session`，然后才启动 runtime。runtime start 失败时只看到释放 owner/workspace lease 的补偿动作。 但 session record / route record 的失败态是否完全一致回滚不明显。

修法：把 “reserve lease + create session + spawn worker” 做成明确状态机：`reserved → spawning → running`，失败要写 `spawn_failed` projection，并清理 session record 或标成 terminal failed，而不是只释放 lease。

---

### 10. direct `/api/sessions` 可以绕过 route policy

`start_session` 只做 safe name、command、cwd、duplicate 检查，然后启动 PTY，并默认 `ensure_owner_lease(..., "codex")`。  它没有 route 里的 workspace lease、allowed_paths、read_only、report_path 语义。

如果 `/api/sessions` 是内部调试 API，应该明确标记 debug-only；如果是 public API，就会绕过 AgentCall 自己的 route 纪律。

---

## 代码层面的主要问题

### 11. HTTP / WebSocket 是手写的，风险偏大

daemon 每个 TCP connection 都直接 `thread::spawn`，没有线程池、连接超时、并发上限。 WebSocket handshake 只要有 `sec-websocket-key` 就接受，没有完整校验 Upgrade/Connection/Version/Origin。 WebSocket frame 读取也没有拒绝 unmasked client frame。

普通 HTTP 响应还统一加了 `Access-Control-Allow-Origin: *`，而 daemon 没有 auth。 虽然只绑定 `127.0.0.1`，但本地恶意网页仍可能尝试从浏览器打 localhost API。对本地控制面来说，最少也要有随机 token 或 Origin 限制。

修法：中期换 `axum/hyper/tokio` + `tokio-tungstenite`。短期至少加 read timeout、连接数限制、auth token、严格 WS handshake、CORS allowlist。

---

### 12. 静态文件路径依赖 daemon 启动时的进程 cwd

HTTP route 返回 `web/index.html`, `web/board.html`, `web/app.js` 等。 但 `static_file` 是直接 `fs::read(path)`，没有用 `state.workspace` 拼路径。

如果用户不是在仓库根目录启动 daemon，`/board` 很可能 404。README 的启动命令传了 `--workspace`，但代码并不会因此把 static file root 切到 workspace。

修法：`static_file` 接收 `state`，读取 `state.workspace.join(path)`，或者把 web assets embed 到 binary。

---

### 13. MCP bridge stdin 没有输入大小限制

MCP bridge 用 `stdin.lock().lines()` 读整行，然后直接 `serde_json::from_str`。 这意味着 host 传一个超大 JSON line 时，bridge 会先分配整行，再 parse，可能 OOM，然后 Codex 侧就是 transport closed。

虽然 tool response 做了 128KB cap，这是输出侧；输入侧还没 cap。

修法：改成 bounded reader，超过例如 1MB 直接 JSON-RPC error，不 parse。

---

### 14. Actor 仍是单队列、无优先级、无 panic guard

`ActorControlCommand` 里同时有 `Submit` 和 `RawWrite(Vec<u8>)`。 actor loop 是裸 `for command in receiver`，没有 `catch_unwind`，没有高优先级 stop/kill channel。 调用方还固定等 5 秒回复。

这几个组合起来会让慢命令、store 卡顿、panic、stop 排队都变成“supervisor 看到 session 不可信”。

修法：actor 至少拆两个 channel：`control_rx` 高优先级，只放 small command；`aux_writer_rx` 或直接 writer task 处理 RawWrite。所有命令先快速 ack `accepted`，不要让 MCP 同步等执行完成。actor loop 外包 `catch_unwind`，panic 写 `session.actor_failed` projection。

---

### 15. Hook ingest 和 store writer 都是全局串行，吞吐会被锁放大

`ingest_hook` 一进入就拿 `state_writer` 全局 mutex。 store 又有单独的 `StoreWriterRuntimeStore`，所有写请求都串行通过一个 writer thread，并且每个调用方同步 `rx.recv()` 等结果。 

这能降低 JSON store 并发损坏概率，但会放大 hook 高峰、MCP 调用、event append 的尾延迟。更危险的是 `rx.recv()` 没 timeout；store writer 如果卡死，调用线程也会无限卡。

修法：保留单 writer 可以，但要有 timeout、panic recovery、队列深度指标、backpressure。hook ingest 热路径要尽量只 append compact event，大 payload 和报告扫描放异步。

---

### 16. 有路径遍历 / 任意文件读取风险

`create_context` 只检查 `task_id/call_id/objective` 非空，然后把 `task_id` 和 `call_id` 直接拼到 `.agentcall/tasks/{task_id}/calls/{call_id}`。 如果调用方传 `../..` 这类路径组件，可能写出预期目录。

`index_transcript` 更直接：请求里给一个 path，daemon 就 `read_to_string`。 在无 auth + CORS `*` 的前提下，这个 API 不应该允许任意路径。

修法：所有 path component 用 `safe_name` 或单独的 `safe_path_component`；所有文件路径 canonicalize 后必须落在 workspace 或允许目录内。`transcripts/index` 只允许 `.agentcall`、Claude transcript dir 或显式配置目录。

---

### 17. 日志和状态文件有增长/泄露风险

`mcp_call` 会把 tool arguments 原样写入 event data。 MCP timing log 只是 append，没有 rotation。 unmatched hooks 是 JSON array，不断 push 后整文件重写，也没有 cap。

这会带来两类问题：隐私上，objective/text/路径甚至 token 可能进日志；性能上，JSON 状态越大，hook ingest 越慢，越容易触发前面 MCP timeout。

修法：MCP arguments 默认只记录 schema-safe 摘要；text/objective 超长截断或 hash；unmatched hooks 改 rotating ndjson；timing log 也走 rotating ndjson。

---

### 18. session start 有 check-then-insert 竞态

`start_session` 先检查 `state.sessions` 是否已有同名 session，然后后面 spawn PTY，再 insert。  两个并发 start 同名 session 时，有窗口同时通过检查。

修法：先在锁内插入 `Starting` placeholder 或 reservation，spawn 失败再移除。route 层和 direct session API 都应共用这个 reservation。

---

## 我建议的修复顺序

**第一阶段：先止血，不要继续加 v5.x 功能。**

1. 把 `agentcall_route(mode=start)` 改成短事务，不在 MCP call 内等 hook ack。
2. 拆 `stop` / `kill`：stop graceful，kill force；lease 只在 observed exit 后释放。
3. destructive commands 强制完整 precondition；空 `{}` 直接拒绝。
4. lease ID/generation 改成真正唯一，不能复用 `lease-session-1`。
5. actor 加 panic guard、priority control channel，移除或隔离 `RawWrite(Vec<u8>)`。
6. wrapper binding 缺失时 route policy fail closed。

**第二阶段：把本地 daemon 从实验服务变成可靠服务。**

1. HTTP/WS 换成熟库，或至少加 token、CORS allowlist、timeout、连接上限。
2. MCP stdin 加 input cap。
3. 所有 path 参数 canonicalize + 限制在 workspace/配置目录。
4. 日志统一 rotation + redaction。
5. store writer 加 timeout、队列指标、panic recovery。

**第三阶段：清理产品契约。**

1. 不实现 SDK 就从 schema 移除 `sdk`。
2. `route decision` 如果只是 PTY-only，就删掉或降级估算字段。
3. `report accept` 完成生命周期闭环。
4. session 结束后仍能查看 clean_tail/plan/report/artifacts。
5. 统一版本号：README、插件 manifest、crate version、CHANGELOG 不要各说各的。

---

## 一句话评价

你这个项目的方向是对的：用 daemon-owned state、projection、hook-aware board 去约束 Codex/Claude worker，很有价值。但现在最大的问题不是“功能少”，而是**控制面已经宣称自己有安全闸门和生命周期契约，底层实现还没有做到同等强度**。先把 timeout、actor、stop/kill、lease/precondition、hook fail-closed 这五块修掉，再继续做 v5.4/v6，否则越堆越难调。
