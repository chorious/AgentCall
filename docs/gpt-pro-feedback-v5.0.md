# 给 GPT Pro 的反馈：AgentCall v5.0 架构收敛问题

## Summary

Pro 的判断我基本同意：AgentCall 已经不是一个简单 MCP tool wrapper，而是在接近 **Agent Runtime / Agent Orchestrator**。当前问题不是“再加一点日志、队列、缓存”就能解决，而是边界没有彻底收敛：

```text
MCP 是协议入口
daemon/backend 是编排和状态权威
PTY 是执行 adapter
logs/transcripts 是事实材料，不是业务状态
semantic events / projections 才是 Codex 默认状态源
```

这份反馈记录三件事：

1. 我认可 Pro 评估中的关键点。
2. 我在本地实现中遇到的具体困难。
3. 我已经搜索/克隆到的参考项目与可能解法，以及希望 Pro 继续判断的问题。

## 我认可 Pro 判断的地方

### 1. MCP 必须降级为薄入口

这点和我们本地性能排查完全吻合。

当前电路是：

```text
Codex App
  -> stdio MCP transport
    -> agentcall-mcp.exe
      -> HTTP /api/mcp/call
        -> agentcall-daemon
          -> board/session/route/report
```

`agentcall-mcp.exe` 本身不应该承担业务复杂度。它应该只做：

- stdio JSON-RPC 适配。
- daemon bootstrap/status。
- 工具列表/工具调用转发。
- timeout。
- response cap。
- timing log。

当前慢的问题不是 MCP bridge 业务太多，而是 MCP call 后面的 daemon 查询路径太重，且 stdio 单通道会被慢请求拖住。

### 2. Backend / daemon 应该是控制平面

我们目前 Rust daemon 已经是 live state 的唯一写者，但它还没有完成“控制平面”应有的架构化：

- session lifecycle 是隐式散落的，不是明确 state machine。
- PTY session 有进程和 buffer，但不是完整 actor。
- event log 已经存在，但查询时仍会现场扫描和推理。
- projection 不完整，Codex 默认查询仍可能触碰 cold path。

所以 Pro 提出的 `Session / Run / Turn / Event / Artifact / Worker / Lease / CancellationToken` 很有价值。

### 3. 日志必须分层

这点我们已经踩过很多次：

- raw PTY output 很大、很脏、包含 TUI 噪声。
- hook events 是结构化信号。
- MCP/daemon latency 需要 observability。
- Codex 需要的是 compact summary，不是 raw output。

v4.3 做了 recent logs / hook logs / artifact payload，但还不够。缺的是更明确的三层边界：

```text
raw stream       -> 复现现场
semantic events  -> 状态机和 UI
observability    -> tracing / latency / counters
```

### 4. 并发要按 session 隔离，不按“到处加锁”

我们现在已经要求：

- 一个 Claude PTY 对应一个 wrapper session。
- hook 通过 `AGENTCALL_WRAPPER_SESSION` 绑定。
- 不允许多 Codex 对单 Claude 串线。

但实际实现还不够强：`session_send`、queued supervisor instruction、stop/interrupt、permission menu 等仍是函数式调用，不是严格的 session actor mailbox。

我同意 Pro 的判断：应该逐步转向 **每个 session 一个 actor / single writer inbox**。

## 我这边的真实困难

### 1. Codex MCP transport 生命周期不可完全控制

我们多次遇到：

- daemon 已经重启且健康，但 Codex 当前 MCP transport 仍指向旧进程。
- 新工具面在某些 Codex session 里不可见。
- plugin-provided MCP / dynamic MCP 工具加载行为不稳定。

这迫使我们把 MCP bridge 尽量做薄，把工具面变化放到 daemon 后面。但即便如此，Codex App 对 MCP server 的重新绑定仍然是一个外部约束。

希望 Pro 帮忙判断：

```text
AgentCall 是否应该彻底固定 MCP schema，
把所有业务能力都收敛到少数 stable tools，如 call_api / board / session / route？
```

### 2. Claude Code PTY 行为不是普通 CLI

实际观察：

- Claude Code 在当前 turn 执行完之前经常不接受新自然语言 prompt。
- plan mode 输出可能落在 Claude transcript/plan file，而不是 TUI 可见文本。
- permission menu 是 TUI 状态，不是稳定结构化 API。
- `select_option` 可用，但 Codex 需要 wrapper 提供结构化菜单语义。
- PTY clean output 会被装饰词和 TUI animation 污染。
- Claude 完成 report 后，可能继续执行尾部输入或误触发文本，例如 “fix P0 issues”。

所以 PTY 确实适合作为通用 adapter，但不能作为业务状态源。

希望 Pro 判断：

```text
PTY adapter 中哪些状态应由 hook-derived events 权威化，
哪些状态可以从 TUI parser 弱提取？
是否应该明确禁止 TUI parser 改变 liveness/attention，只能提供 hints？
```

### 3. Rust daemon single-writer 已经做了一半

我们已经多次收敛：

- Python 不再作为 live state writer。
- hooks POST daemon。
- daemon 写 events / claims / sessions / bindings。
- MCP 走 daemon。
- ACP 已退出默认 runtime。

但历史包袱仍在：

- Python 薄脚本、installer、legacy/debug 仍存在。
- `.agentcall` 下有旧 events、hook logs、legacy sessions。
- daemon query 路径仍在读 JSON 文件和日志 tail。

希望 Pro 判断：

```text
下一步是先做 projection JSON 文件，
还是直接引入 SQLite event store / projection table？
```

我的直觉：P0 先用内存 projection + 小 JSON snapshot 更稳，SQLite 留给 v5.x 后续。

### 4. Windows 本机环境是强约束

本项目长期在 Windows / PowerShell / Codex App / Claude Code 下跑：

- Claude cwd 必须强制为 `D:\guKimi`，因为 hooks/settings 只在那里生效。
- route request 的 `workspace` 是任务目标，不是 Claude process cwd。
- UTF-8、Windows path、长路径、PTY/ConPTY、进程树 kill 都是现实问题。
- `zellij` 克隆时也遇到 Windows 长路径 checkout 问题。

这意味着很多 Unix daemon 设计不能直接照搬。

希望 Pro 判断：

```text
Session actor 的进程管理层，Windows 下应该如何定义 stop / interrupt / kill / orphaned？
是否需要明确 process group / job object 抽象？
```

### 5. Codex 监督 Claude 的体验问题

当前产品目标不是只跑后台任务，而是让 Codex 监督 Claude Code worker：

- Codex 需要知道谁在工作、谁需要介入、谁完成 report。
- Codex 很容易对 Claude “等太久”失去耐心。
- 所以 summary 需要 patience hints。
- 但 summary 过重又导致 MCP 慢。

这形成一个张力：

```text
状态太少 -> Codex 焦虑、乱重试
状态太多 -> MCP 慢、上下文重
```

希望 Pro 帮忙判断：

```text
给 Codex 的最小状态摘要应该包含哪些字段？
哪些字段必须默认返回，哪些必须按 include 展开？
```

## 已搜索 / 克隆到的参考项目

### 1. Zellij

仓库：https://github.com/zellij-org/zellij

学习点：

- Rust terminal multiplexer。
- `PTY Bus` 明确独立。
- `Screen` 管理显示状态。
- `background_jobs` 处理 session metadata 和 render coalescing。
- `thread_bus` 做模块间消息传递。

对应 AgentCall：

```text
Zellij PTY bus     -> AgentCall session/PTY runtime
Zellij screen      -> AgentCall clean output / projection
background_jobs    -> AgentCall maintenance / snapshot writer
thread_bus         -> AgentCall actor/message bus
```

重点文件：

- `.agentcall/research/upstreams/zellij/zellij-server/src/pty.rs`
- `.agentcall/research/upstreams/zellij/zellij-server/src/pty_writer.rs`
- `.agentcall/research/upstreams/zellij/zellij-server/src/thread_bus.rs`
- `.agentcall/research/upstreams/zellij/zellij-server/src/background_jobs.rs`

### 2. Vector

仓库：https://github.com/vectordotdev/vector

学习点：

- event/log pipeline。
- source / transform / sink 分层。
- bounded buffer / backpressure。
- internal events / metrics。
- file source checkpoint / tailing。

对应 AgentCall：

```text
Vector topology        -> AgentCall event pipeline
Vector buffers         -> AgentCall bounded queues
Vector internal_events -> AgentCall observability schema
Vector file-source     -> AgentCall log tail/checkpoint
```

重点目录：

- `.agentcall/research/upstreams/vector/src/topology`
- `.agentcall/research/upstreams/vector/src/internal_events`
- `.agentcall/research/upstreams/vector/lib/vector-buffers`
- `.agentcall/research/upstreams/vector/lib/file-source`

### 3. Crush

仓库：https://github.com/charmbracelet/crush

学习点：

- 现代 agent app。
- session / hooks / permission / skills / MCP / LSP / SQLite。
- hooks engine 独立于 agent。
- permission 在 tool wrapper 层组合。
- pubsub 解耦 agent、UI、services。

对应 AgentCall：

```text
Crush hooks        -> AgentCall hook ingest / decision aggregation
Crush permission   -> AgentCall allowed_paths / policy deny
Crush session/db   -> AgentCall session persistence
Crush pubsub       -> AgentCall projection updates
Crush filetracker  -> AgentCall read/write tracking
```

重点目录：

- `.agentcall/research/upstreams/crush/internal/hooks`
- `.agentcall/research/upstreams/crush/internal/permission`
- `.agentcall/research/upstreams/crush/internal/session`
- `.agentcall/research/upstreams/crush/internal/pubsub`
- `.agentcall/research/upstreams/crush/internal/filetracker`

## 我目前倾向的解决路线

### P0：先修 hot path，不先换数据库

1. `agentcall_board(view=compact, filter=attention)` 不再调用完整 `session_summary()`。
2. 新增 `session_projection` / `board_projection`。
3. `agentcall_session` 默认只读 projection。
4. `include=clean_tail` 才清洗 output。
5. `include=plan` 才读 transcript/plan artifact。
6. MCP bridge 加 timeout + timing log + response cap。

理由：

- 这是最小迁移。
- 能立刻验证性能判断。
- 不需要一次性引入 SQLite/actor 全重构。

### P1：引入 session actor

每个 PTY session 一个 actor：

```text
SessionActor
  inbox:
    SendInput
    QueueSupervisorInstruction
    SelectOption
    Interrupt
    Stop
    RequestReport
  owns:
    pty process
    stdin writer
    replay buffers
    state projection
    seq counter
```

外部不能直接写 PTY，只能发 actor command。

### P2：event store / projection store

先继续 NDJSON + JSON projection，确认字段稳定后再判断是否需要 SQLite。

SQLite 可能适合：

- events query。
- session state。
- reports index。
- file tracker。
- idempotency keys。

但不建议 P0 立刻上 SQLite，否则会掩盖真正的边界问题。

## 想请 Pro 重点评估的问题

### 问题 1：P0 是否应该先做 projection，还是直接做 actor/event store？

我当前倾向：

```text
先 projection fast path，再 actor/event store。
```

理由是性能故障最直接，且改动范围更小。

但风险是：projection 如果没有 actor/state machine 支撑，可能只是新的缓存补丁。

希望 Pro 判断这一步是否会走偏。

### 问题 2：最小 MCP tool schema 应该是什么？

Pro 建议：

```text
cc_session_create
cc_session_send
cc_session_status
cc_session_read_events
cc_session_cancel
cc_session_artifacts
```

我们当前是：

```text
agentcall_daemon
agentcall_board
agentcall_route
agentcall_session
agentcall_session_send
agentcall_report
```

是否应该继续保留当前工具名，但重定义语义？还是 v5.0 直接收敛成 session-oriented API？

### 问题 3：semantic event schema 应该多早稳定？

当前 events 已经存在，但历史字段混乱。是否需要现在就定义强 schema：

```text
event_id
session_id
run_id
seq
source
type
severity
payload
trace_id
span_id
```

还是先只保证 projection 正确，event schema 后置？

### 问题 4：Codex-facing summary 最小字段

我认为默认 summary 应该只包含：

```text
session
liveness_status
attention_status
needs_attention
pending_interaction
policy_block
last_progress_age_seconds
patience_status
suggested_wait_seconds
report_ready
binding_source
warnings
```

Pro 是否认为还必须有：

- current_task？
- tokens？
- last_error？
- route context？
- allowed_paths？
- files_written？

### 问题 5：PTY adapter 与 SDK adapter 的边界

我们已经实际放弃 ACP 作为默认路线，因为 Codex App 内可视性差，且 ACP child lifecycle/projection 很难让 Codex 直观看到。

但 Pro 认为 PTY 不应永远是核心接口，这我同意。

问题是：

```text
在 Claude Code 官方 SDK/CLI 结构化能力不稳定、可视性不足时，
是否应该仍然先把 PTY adapter 做成统一 AgentRuntime 的第一个实现？
```

我的倾向：是。先把 PTY actor 做好，再挂 SDK adapter。

## 给 Pro 的请求

请不要泛泛建议“加 Redis / Kafka / tracing / worker pool”。

请帮忙判断：

1. v5.0 应该先做 projection fast path，还是直接 actor/event-store 重构？
2. MCP 工具面是否应该保持现有名字，只改变内部语义？
3. Session actor 的最小状态机应该有哪些状态和事件？
4. Codex-facing summary 的最小字段集合是什么？
5. Windows + PTY + Claude Code 场景下，cancel/interrupt/stop/orphaned 应该如何定义？
6. 如何设计 6 个最小故障测试，逼出正确架构？

## 我的当前结论

Pro 的方向是对的，但我会把迁移切得更保守：

```text
v5.0 P0:
  MCP timeout/timing/response cap
  board attention fast path
  session summary projection-only default

v5.1:
  session actor inbox
  idempotent commands
  projection updates from actor/hook events

v5.2:
  stronger event schema
  optional SQLite/index store
  adapter trait for future SDK runtime
```

核心原则保持不变：

```text
MCP 是入口，不是 runtime。
daemon 是控制面，不是日志考古器。
PTY 是 adapter，不是业务状态源。
semantic event / projection 是 Codex 默认状态源。
```
