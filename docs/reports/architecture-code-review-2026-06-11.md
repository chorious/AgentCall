# AgentCall 架构与代码审查报告

- 日期：2026-06-11
- 审查范围：`crates/`（Rust daemon / mcp / hook 三个 crate）、`src/agentcall/`（Python 旧实现）、`scripts/`、`tests/`、根目录清单文件
- 构建状态：`cargo check --workspace` 通过（仅信息级，无编译错误）
- 方法：静态阅读 + 关键不变量交叉验证（session 生命周期、`active_sessions.json` 形状、containment 策略）

> 结论摘要：核心 Rust daemon 设计清晰（单写者 store-writer、事件投影、租约调度都成体系），但存在 **2 个会在正常运行中触发的数据/状态 bug**，**1 处本地安全面**，以及若干一致性与架构清晰度问题。下面按严重程度分级，每条给出证据位置与修改建议。

---

## 一、架构总览

实际主线（与 README/AGENTS.md 一致）是 Rust daemon：

```
Codex → agentcall-mcp (stdio) → daemon HTTP API → SessionActor/PTY → Claude worker
                                      ↑ hooks POST /api/hooks/ingest
```

三个 crate 职责划分合理：

- `agentcall-daemon`：HTTP、PTY 运行时、hooks、routes、投影、租约、日志卫生。单写者 `StoreWriterRuntimeStore`（`store.rs`）把所有写操作串行到一个专用线程，是好设计。
- `agentcall-mcp`：稳定的 stdio MCP 桥，超时分类清晰（`daemon_client.rs`）。
- `agentcall-hook`：独立 hook 二进制（codex 侧）。

**主要架构问题是“双实现并存”**：除 Rust 主线外，仓库还保留了一整套 Python 实现（`src/agentcall/` v3.0.0，含 `store.py`、`sessions.py`、`supervisor.py`、`v2/orchestrator.py`），以及 `agentcall-hook` 这个**直接写状态文件**的旁路。它们与 daemon 的状态 schema 已经发生漂移（详见问题 6、7）。三处版本号也不一致：README 写 `v5.3.0`，Cargo/pyproject/MCP 都是 `3.0.0`。

---

## 二、严重问题（会在正常运行中触发）

### 🔴 P1-1　退出的 session 永不从 `state.sessions` 移除 → 调度容量被永久占满

**证据**：`crates/agentcall-daemon/src/session.rs:248` `spawn_waiter` 在子进程退出时只做了：

```rust
state.actors.lock().unwrap().remove(&session.name);   // 只移除 actor
// 没有 state.sessions...remove(&session.name)
```

全代码库 grep 不到任何对 `state.sessions` 的 `remove`。后果：

1. `list_sessions`（`session.rs:302`）会一直返回已退出的 session（status 变成 `exited:...` 但仍在 map 里）。
2. 调度器 `scheduler.rs:49` `let live_sessions = state.sessions.lock().unwrap().len();`，再 `active_sessions = live_sessions.max(active_leases)`（`:56`）。退出的 session 仍计入 `live_sessions`，于是 **`max_sessions`（默认 6）会被已死进程逐步耗尽**，最终所有 `agentcall_route mode=start` 报 `capacity_exceeded`，必须重启 daemon。
3. `start_session:83` 对同名 session 返回 `"session already exists"`，导致**同名 worker 无法重启**。

注意：owner/workspace 租约在退出时**会**释放（`:275`、`:283`），所以 `active_leases` 会回落，但 `live_sessions` 不会——`.max()` 让僵尸条目继续主导计数。`summary.rs:cleanup_stale_runtime_state` 只清理磁盘上的 `active_sessions.json` 投影，**不碰内存里的 `state.sessions`**。

**修改建议**：在 `spawn_waiter` 的退出收尾处移除内存条目，与 actor 移除并列：

```rust
state.actors.lock().unwrap().remove(&session.name);
state.sessions.lock().unwrap().remove(&session.name);   // 新增
```

若希望前端在退出后短时间内仍能读到 summary，可改为“保留但标记 terminated，并在调度计数/`list_sessions` 中过滤掉非 running”。最简单正确的做法是直接移除——退出事件已落盘，投影/事件日志仍可回溯。建议补一条测试：start → kill → 确认 `scheduler_decision.active_sessions` 回到 0 且同名可再次 start。

---

### 🔴 P1-2　`checkpoint_session` 把 `active_sessions.json` 当数组读，会清空对象数据

**证据**：`crates/agentcall-daemon/src/routes.rs:203`

```rust
let path = agent_dir.join("state").join("active_sessions.json");
let mut sessions = read_json_file(&path, json!([]));          // 默认数组
let mut items = sessions.as_array().cloned().unwrap_or_default();  // 当对象时 → None → 空 vec
...
sessions = json!(items);
write_json_file(&path, &sessions)?;                           // 写回 []，原对象数据丢失
```

但**所有其他读写方都把它当对象 `{}`**：

- `hooks.rs:508 upsert_active_session_locked`：`sessions[session_id] = session;`（对象）
- `hooks.rs:303 context_injection`：`sessions.as_object()...len()`
- `summary.rs:63 / :402`：`as_object()` 读取与清理
- `agentcall-hook/src/main.rs:193`：`sessions[&session_id] = session;`（对象）

**后果**：只要在任意 hook 写入 `active_sessions.json`（已成对象）之后调用 `POST /api/sessions/{name}/checkpoint`，`as_array()` 返回 `None`，`unwrap_or_default()` 得到空 vec，最终把整个文件覆盖成 `[]`，**抹掉所有活动 session 投影**，board / context_injection 的 `active_sessions` 计数随之归零。

**修改建议**：让 checkpoint 与其余代码统一用对象形状：

```rust
let mut sessions = read_json_file(&path, json!({}));
if !sessions.is_object() { sessions = json!({}); }
let obj = sessions.as_object_mut().unwrap();
let now = chrono::Utc::now().to_rfc3339();
match obj.get_mut(session_id) {
    Some(existing) if existing.is_object() => {
        let e = existing.as_object_mut().unwrap();
        e.insert("status".into(), json!("checkpoint_requested"));
        e.insert("updated_at".into(), json!(now));
    }
    _ => { obj.insert(session_id.into(), json!({
        "session_id": session_id, "status": "checkpoint_requested",
        "runtime": "daemon", "created_at": now, "updated_at": now,
    })); }
}
write_json_file(&path, &sessions)?;
```

补测试：先 `upsert_active_session_locked` 写两个 session，再 checkpoint 第一个，断言第二个仍在、第一个 status 变更。

---

## 三、中等问题

### 🟠 P2-1　本地 HTTP API 无认证 + `Access-Control-Allow-Origin: *`（CSRF / DNS-rebinding 面）

**证据**：`http.rs:686` 所有响应带 `Access-Control-Allow-Origin: *`；`main.rs:64` 绑定 `127.0.0.1`，但没有任何 token/Origin 校验。WebSocket 输入路径（`http.rs:436 websocket_input_command`）直接构造 `CommandEnvelopeV1` 并 `submit_session_command`，**绕过** `prepare_session_send_command` 的幂等/租约/precondition 校验。

**后果**：当用户浏览器打开任意恶意网页时，该页面可以向 `http://127.0.0.1:3293` 发起跨站请求：`POST /api/routes`（启动 `claude` PTY worker）、`POST /api/sessions/{name}/input`（向 worker 注入任意输入）、`POST /api/sessions/{name}/stop`。CORS `*` 对简单请求/部分预检放行，配合 DNS rebinding 可绕过“仅 localhost”假设。鉴于该 API 能拉起进程并向其注入指令，风险等级为中。

**修改建议**（择一或组合）：
- 去掉 `Access-Control-Allow-Origin: *`，仅对真正需要的本地 UI 资源放行；
- 对所有 `POST /api/*` 校验 `Origin`/`Host`（拒绝非 `localhost`/非预期端口的 Host 头，防 rebinding）；
- 引入启动时随机生成的 loopback token（写入 `.agentcall/state/`，MCP 桥与本地 UI 读取后带 `Authorization` 头）。

### 🟠 P2-2　containment 路径策略不消解 `..`，可被路径穿越绕过

**证据**：`hooks.rs:1425 path_within_or_equal` → `normalize_compare_path` → `normalize_workspace_path`（`:1486`）只做 `\`→`/` 和去除前导 `./`，**不折叠 `..`**。判定是纯字符串 `starts_with(parent + "/")`。

**后果**：写入 `docs/reports/../../anything` 会规范化为 `docs/reports/../../anything`，`starts_with("docs/reports/")` 为真，于是被判“在 writable_paths 内”而放行，实际目标在工作区之外。`route_report_path` 用的 `normalized_route_path`（`:1242`）走了 `canonicalize()`，行为不一致；containment 这条路径校验是其中较弱的一环。

**修改建议**：比较前对路径做词法归一（折叠 `.`/`..`，如 `path-clean` 思路或自实现 segment 栈），或对存在的路径统一 `canonicalize()` 后再比较，使 containment 与 `normalized_route_path` 行为一致。补穿越用例测试。

### 🟠 P2-3　`bash_readonly_allowed` 用子串/前缀判定，既会误杀也不够稳

**证据**：`hooks.rs:1362`，`forbidden` 是子串列表（`">"`, `"rm "`, `"echo "`…），`allowed` 是前缀白名单（`"cat "`, `"git diff"`…）。

**问题**：
- 误杀：合法只读命令 `git log --oneline | cat` 含 `| ` 之外不触发，但 `rg "remove-item"`（搜索字面量）会因含 `remove-item` 子串被拒；`cat report_echo.txt` 含 `echo ` 被拒。
- 它是前缀白名单（默认拒绝），所以不构成“放行危险命令”的直接绕过，但子串黑名单与前缀白名单混用使策略难以推理、易随命令写法误判。

**修改建议**：明确这是“建议性 hook 拦截”而非强隔离（真正写仍由 Claude 执行）。若要保留，至少对命令做一次分词/按 `&& || | ;` 切分逐段判定，并把黑名单从“子串”改为“首 token 匹配”，减少误杀。长期更稳的是依赖 Claude 自身 permission-mode + 明确 allowed_paths，而非在 daemon 侧重做 shell 解析。

### 🟠 P2-4　读路径产生写副作用：GET /api/board 会改盘并抢写锁

**证据**：`summary.rs:47 board_state` 与 `:133 runtime_health` 都调用 `cleanup_stale_runtime_state`，后者 `state.state_writer.lock()`（`:399`）并 `write_json_file` 改 `active_sessions.json` / `pending_supervisor_instructions.json`。

**后果**：一个纯查询接口会写磁盘并与真正的写操作争用全局写锁；高频轮询 board 时既放大 IO，也可能与 ingest_hook 互相阻塞。属设计气味，非崩溃级。

**修改建议**：把 stale 清理从读路径剥离，改为（a）由 store-writer 线程定期 tick，或（b）在 session 退出/ingest 等已有写事务里顺带清理；读接口保持只读。

---

## 四、一致性 / 架构清晰度问题

### 🟡 P3-1　`agentcall-hook` 直接写 daemon 拥有的状态文件，绕过单写者

**证据**：`agentcall-hook/src/main.rs:128 write_json` 直接覆盖 `.agentcall/state/active_sessions.json`、`file_claims.json`，**不经过 daemon 的 `state_writer` 互斥，也不经过 store-writer 线程**。同一批文件 daemon 侧由 `ingest_hook` 持锁写。两进程并发写同一文件无跨进程锁 → race/截断风险。

而且两边 schema 已漂移：hook crate 写的 session 条目缺 `wrapper_session` / `binding_source`；事件 id 由 `next_event_number()`（`:154`，每次 O(n) 数行）独立生成，写入 `events.ndjson`，而 daemon 写 `events/recent.ndjson` 并用原子计数器——两套 id 空间会重叠。

`scripts/agentcall_arch_audit.py:45` 有 `check_actor_writer_boundary`（PTY 输入必须走 SessionActor），却**没有**审查“谁能写 state 文件”。

**修改建议**：明确单写者边界——所有状态写入都应经 daemon（如 Python claude-hook 那样 POST `/api/hooks/ingest`）。让 `agentcall-hook` 也改为 HTTP 上报；若必须保留离线直写模式，应共享 `write_json_file` 的原子 tmp+rename 并文档化“daemon 不运行时才允许”。可在 arch_audit 增加一条断言禁止非 daemon 代码写 `state/*.json`。

### 🟡 P3-2　Python `src/agentcall/`（v2/v3 实现）与 Rust 主线重复且已被取代

**证据**：`src/agentcall/cli.py`、`store.py`、`sessions.py`、`supervisor.py`、`v2/orchestrator.py` 等约 21 个 .py 文件实现了一套独立的 route/context/report/session 概念；`tests/test_sop_flow.py` 测的是这套 Python 实现，而非 Rust daemon 行为。README/AGENTS 已声明主线是 PTY+Rust，“historical ACP/SDK plans are archived”。

**后果**：维护者难以判断哪套是事实来源；CI 测试覆盖的是非主线代码，给人“有测试保护”的错觉但保护错了对象。

**修改建议**：明确二者关系——要么将 `src/agentcall/` 整体移入 `docs/`/`.local_archive/` 或独立 legacy 分支并在 README 标注“deprecated, not the runtime”，要么删除。保留 Python 仅作为 CLI 包装时，应让其调用 daemon HTTP，而非自带第二套 store。把测试重心移到 daemon（已有 `tests/test_v061_hook_daemon_ingest.py` 是正确方向）。

### 🟡 P3-3　版本号三处不一致

README `v5.3.0` / `Cargo.toml` 各 crate `3.0.0` / `pyproject.toml` `3.0.0` / MCP `SERVER_VERSION 3.0.0`。建议单一事实来源（如以 daemon crate 版本为准，README 顶部 checkpoint 标签与之对齐，或在发布脚本里校验三者一致——`agentcall_dev.py` 已有 release 检查，可加这条断言）。

### 🟡 P3-4　`url_decode` 是不完整的百分号解码

**证据**：`http.rs:728` 只替换 `%20 %2F %5C %3A` 四种，不处理通用 `%XX`、不处理 `+`。当前路径里的 session 名受 `safe_name` 限制（仅字母数字/`-`/`_`），所以**目前不致命**，但作为通用 query/path 解码器是错的，未来若放宽命名会埋雷。建议替换为标准百分号解码，或重命名函数表明其仅处理有限集合。

---

## 五、轻微问题 / 观察

- **过期 claim 只在 PreToolUse 标记 stale**（`hooks.rs:698 mark_expired_claims_stale` 仅此处调用），release/post 路径不清理，`runtime_health.stale_claims` 会缓慢累积。可在 store-writer tick 中统一清。
- **thread-per-connection 无上限**（`main.rs:81`），SSE/WS 长连接各占一线程。localhost 场景可接受，但恶意/异常客户端可堆积线程。可加连接数上限或迁移到带上限的线程池。
- **`spawn_reader` 的 `\x1b[6n` 自动应答**（`session.rs:199`）用上一块的 3 字节尾部拼接扫描，理论上跨块切分的转义序列可能重复应答；影响极小。
- **actor 命令通道串行**（`actor.rs:128`）：单个 PTY 写卡住会阻塞后续命令，调用方 5s `recv_timeout`（`actor.rs:112`）返回超时但 actor 线程仍卡。可考虑写操作加超时或单独线程。
- **`.gitignore` 健康**：`git ls-files` 未发现被跟踪的 `target/`、`__pycache__`、`.agentcall/` 等产物，arch_audit 的 `check_no_tracked_build_outputs` 在起作用，良好。
- **`find_known_wrapper_binding`**（`hooks.rs:586`）按 `claude_session_id` **或** `transcript_path` 任一命中即绑定；极端情况下两个 session 共享 transcript 路径会误绑。低概率。

---

## 六、修复优先级建议

| 优先级 | 问题 | 动作 |
|---|---|---|
| P1 | session 退出不出 map（P1-1） | `spawn_waiter` 增加 `state.sessions...remove`，补 start→kill→restart 测试 |
| P1 | checkpoint 清空 active_sessions（P1-2） | 统一对象形状，补测试 |
| P2 | 本地 API 无认证 + CORS `*`（P2-1） | 去 `*`、校验 Origin/Host 或加 loopback token |
| P2 | containment 路径穿越（P2-2） | 比较前折叠 `..` 或统一 canonicalize |
| P2 | bash 只读判定脆弱（P2-3） | 降级为建议性并分段判定，或交回 Claude permission |
| P2 | 读路径写副作用（P2-4） | stale 清理移出 GET 处理 |
| P3 | hook crate 旁路写状态（P3-1） | 收敛到 daemon 单写者，arch_audit 加断言 |
| P3 | Python 旧实现并存（P3-2） | 归档/删除或改为调用 daemon |
| P3 | 版本号不一致（P3-3） | 单一来源 + release 校验 |

---

## 附：已验证的不变量

1. `state.sessions` 全库无 `remove` 调用——确认 P1-1。
2. `active_sessions.json`：`checkpoint_session` 用数组，`hooks.rs`/`summary.rs`/`agentcall-hook` 全用对象——确认 P1-2。
3. `normalize_workspace_path` 不折叠 `..`，`path_within_or_equal` 纯字符串前缀——确认 P2-2。
4. `cargo check --workspace` 通过，无编译错误；现有单元测试覆盖 store-writer 串行、租约、投影、PostToolUse 截断等，但不覆盖上述两个 P1 路径。
