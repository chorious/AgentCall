# OPUS Review — AgentCall

> 评审者: Claude Opus 4.8 · 日期: 2026-06-03
> 范围: README + 三份架构文档、Python `v2/` 编排层、`store/claims/supervisor`、三个 Rust crate（mcp/hook/daemon）、`tests/test_sop_flow.py`

---

## 一、架构层面的根本问题

### 1. 同一套协议逻辑用两种语言各写了一遍，且已经漂移
file-claim 策略与 store 在 Python（`v2/claims.py`、`store.py`）和 Rust（`agentcall-hook/main.rs`）里各实现一次——claim 获取/释放、event-id 计数、state 文件初始化全部重复。这不是抽象，是复制。而且已经漂移出真实 bug：

- Rust `now_stamp()` 产出 `"unix:1700000000"`（`hook/main.rs:469`），Python `utc_now()` 产出 ISO-8601。**两者写进同一个 `events.ndjson`**。任何解析 `ts` 的消费者都会在其中一半事件上崩。

### 2. Rust「后端层」实质是 Python CLI 的子进程代理
`agentcall-mcp` 几乎每个工具都 `python -m agentcall ...`（`mcp/main.rs:961`），并且每次调用后再 `emit_mcp_event` 又 spawn 一次 python（`:908`）。所以一个只读的 `agentcall_board` = **两次 Python 解释器冷启动**。`route_task` 是纯内存函数，也被绕去子进程跑（`:654`）。README 把 `crates/` 称作「Rust 后端层」，但 MCP server 本身没有后端逻辑，只是 IPC 外壳。名实不副。

### 3. 核心卖点是无锁的——而产品定位是并发
state 全是共享 JSON / NDJSON 文件，read-modify-write，**没有任何锁**。但 SessionStart 上下文显示 `active_sessions: 8`，README 主线就是「多 agent 在同一 workspace 协作」。于是：

- `evaluate_pre_tool_use`（`claims.py:58` / `hook:221`）是教科书式 TOCTOU：读 claims → 查冲突 → 写 claims，中间无原子性。两个 agent 同时声明同一文件会双双通过。**「file claim 冲突保护」这个核心功能在并发下不成立。**
- 三个写者（Python store、Rust hook、Rust daemon）并发追加同一 `events.ndjson` 并覆写同一批 state json，last-writer-wins，丢更新。
- `next_event_id`（`store.py:209`）/ `next_event_number`（`hook:147`）每次 append 都读全文件数行 → 单写 O(n²)，并发下两次 append 拿到同一个 `evt-NNN`。

### 4. 硬编码个人机器路径进了「发布版」
`DEFAULT_CLAUDE_WORKSPACE = r"D:\guKimi"` 出现在 mcp、daemon 以及多个工具 schema 的 `default` 里。v0.5.1 的代码默认指向另一个项目目录，任何换机器/换用户都坏。

---

## 二、确定性的代码缺陷

| 位置 | 问题 |
|---|---|
| `store.py:401` `task_status_from_report` | `if exit_code==0: return FAILED / else return FAILED` —— 两个分支完全相同。要么是没写完的意图（本该区分「干净退出但无报告」vs「崩溃」），要么是死逻辑。被 `supervisor.py:62` 真实调用。 |
| `acp.py:35,90` | `timeout_seconds` 存了从不使用；`_read()` 的 `readline()` 无限阻塞，agent 挂起即永久卡死。stderr 只在 stdout 关闭后才读（`:161`）→ stderr 缓冲区写满会死锁（经典 subprocess 陷阱）。 |
| `orchestrator.py:324` | 靠 `call_id.endswith("executor-02")` 决定要不要写 `report.md` —— 魔法后缀硬耦合 `call_number=2`。改个编号就静默失效。 |
| `orchestrator.py:307` `_validate_changed_files_exist` | 用 `self.store.root` 解析 changed_files，但真实 child workspace 是 `claude_workspace`（≠ `.agentcall` root）。**校验在查错误的目录**——文档宣称「防止报告沦为纯叙事」，真实 ACP 运行下要么恒失败、要么查了无关目录，等于没校验。 |
| `claims.py:137` / `hook:391` | `session_id` 回退到 `"unknown-session"`，把所有缺 id 的会话坍缩成同一身份 → 冲突检测对它们全部失效；又用 `transcript_path` 当回退 id，导致同一会话在不同 hook 下可能是不同身份。 |
| `reports.py:169` `validate_report_dict` | 手写的部分 schema 检查，与已有的完整 `REPORT_JSON_SCHEMA` 不等价：`additionalProperties:false`、`context_sufficiency` 内层约束都没真正强制（没用 `jsonschema`）。两层校验（contract + dict）职责重叠又不一致。 |
| `router.py:83` | 子串 `in text` 的 bag-of-words + 任意加权；`confidence` 是分差的确定性线性式，包装成概率。子串匹配是 footgun（`"long"`∈"be**long**ing"）。中英双语硬编码 hint 集，维护性差。 |

**claim 无 TTL / 无 liveness（设计级缺陷）**：claim 只在 Stop/SessionEnd/SubagentStop hook 触发时释放。session 崩溃或 hook 没装，claim 永久 `active`，对所有人死锁。8 会话的工作区里这是迟早的事，没有 PID 探活或过期回收。

---

## 三、功能实现层面的批评

### 1. 旗舰路径（live ACP）从未真正端到端跑通
v2 文档自承「Live Claude ACP execution is not used in the simulation yet」。测试里只有一个 fake python ACP agent（`test:558`），没有对真实 `@agentclientprotocol/claude-agent-acp` 的验证。被宣传为「flagship child-call path」的东西，目前是未经真实验证的桩。

### 2. 多 agent 并发是卖点，却 0 并发测试
测试覆盖了 happy/reject/review 各条单线路径，但没有任何测试同时起两个会话争一个文件。整个产品要解决的就是并发协调，而这正是唯一没测的地方。

### 3. report 契约是「自证」的，parent 验收基本是信任
PLAN 模式靠 prompt 文本叫 child「不要改文件」，parent 端只检查 `changed_files==[]`。但如果 child **真改了文件却漏报**，系统完全发现不了——没有真实 git diff / 测试执行校验（文档 Next Work 也列着「Add real diff/test validators」「budget and timeout enforcement」均未做）。所以「report 是契约」目前更像「report 是 child 的一面之词」，验收强度被高估。

### 4. 半重构状态
工作树有 8 个文件未提交、`runtime.py` 未跟踪；文档分 v0.4/v0.5/v0.5.1/v2/v3 多层叙事，与当前代码并不严格对齐。对一个强调「SOP 落进代码」的项目，状态本身的卫生需要更严。

---

## 四、值得肯定的部分

- 「parent owns context / child owns one burst / report is contract」的概念分层是对的，bounded lifecycle 方向有价值。
- ACP stdio JSON-RPC 客户端写得规整：permission 自动 allow、update 缓冲、按 request_id 匹配、未知 client method 正确回 -32601（`acp.py:105-148`）。
- 事件日志用 NDJSON 并对坏行做跳过+计数告警（`store.py:236`）。
- report 拒收路径（超 turn、越界路径、上下文不足）测试覆盖到位。

---

## 五、按优先级的建议

1. **[P0] 定一个状态权威（single source of truth）。** 要么所有写都过 Rust daemon（带内存锁/单写线程），Python 只通过它读写；要么引入文件锁 + 原子写（写临时文件再 rename）。先消除 TOCTOU 和双实现漂移——否则核心功能在并发下是假的。
2. **[P0] 统一时间戳格式**，并加一个跨 Python/Rust 的契约测试，防再漂移。
3. **[P0] claim 加 TTL + PID 探活回收**，否则死锁不可避免。
4. **[P1] 修 `task_status_from_report`**；删 `validate_report_dict` 改用真正的 `jsonschema`；`_validate_changed_files_exist` 改用 child workspace + git diff。
5. **[P1] 给并发写一个真正的压力测试**（N 个进程抢同一文件），当作核心功能的验收门。
6. **[P2] 清掉硬编码 `D:\guKimi`**，改环境变量/配置必填。
7. **[P2] ACP client 落实 `timeout_seconds`**（线程读 + 超时 kill），并发读 stderr。
