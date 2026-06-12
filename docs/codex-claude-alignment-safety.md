# Codex / Claude Worker 对齐安全措施

更新日期：2026-06-11

本文记录 AgentCall 当前用于让 Codex 主管侧与 Claude Code PTY worker 侧保持一致的安全措施。这里的“安全”不是单指权限安全，也包括状态一致性、写入边界、误操作防护、低噪声观测、报告验收和失败恢复。

## 核心原则

- Codex 是主管：负责拆任务、派发、等待、验收、合并和最终判断。
- Claude Code 是 worker：负责在指定边界内实现、检查、报告，不拥有最终验收权。
- Rust daemon 是 live state authority：events、claims、routes、sessions、projections、leases、bindings 的 live 控制面应由 daemon 写入。
- MCP 是控制入口，不是状态真相本身：Codex 通过 MCP 调 daemon，daemon 通过 projection/board/session 暴露压缩状态。
- PTY worker 是人类可见 handoff，不是裸命令后台任务；不能自动 kill 可见 PTY，除非 supervisor 明确请求。
- projection-first：Codex 默认读 compact board/session projection，不默认读 raw terminal 或长日志。

## Codex 主管侧安全措施

### 工具入口收敛

Codex 默认只使用小工具面：

```text
agentcall_daemon
agentcall_board
agentcall_route
agentcall_session
agentcall_session_send
agentcall_report
```

已弃用或降级的路径：

- 不默认使用 `delegate` / `workflow`。
- 不默认调用 Python workflow。
- 不把 ACP/SDK 作为主线 runtime。
- 不把 raw PTY 输出作为默认状态源。

### 派发前检查

Codex 应先看：

```text
agentcall_daemon(action=status)
agentcall_board(view=compact, filter=attention)
```

检查重点：

- daemon 是否运行。
- scheduler 是否满载。
- 是否存在 attention session。
- 是否有 active owner/workspace lease。
- 是否有 stale projection、policy block、permission prompt、prompt ack missing。

### 任务切片与所有权

Codex 派工时必须明确：

- objective：任务目标。
- workspace：任务目标目录，不是 Claude 进程 cwd。
- session_name：稳定、可追踪。
- write_paths / report_path / worker_kind：写入边界。
- acceptance_criteria：验收标准。
- role / phase：worker 身份和阶段。

禁止：

- 多个 Codex 同时控制同一个 Claude worker。
- 同一个 worker 被多个 owner 发送控制命令。
- 让 worker 模糊地“随便改项目”。
- 让 Codex 反复发送同一条 prompt 以碰运气。

### 幂等与重放保护

`agentcall_session_send` 的安全动作使用 command envelope：

- `idempotency_key`
- `owner_id`
- `owner_lease_id`
- `lease_generation`
- `precondition`
- `projection_last_session_seq`

当前设计：

- Codex 可以显式传 `idempotency_key`。
- MCP/daemon 适配层应在缺省时自动生成稳定 key。
- 同一 projection 下同一 payload 重复发送应被 dedupe。
- projection 推进后，同样动作可生成新 key。
- `stop` / `kill` / `interrupt` / `approve_plan` / `start_auto` 必须带 destructive precondition。

### 耐心与打断纪律

Codex 不能把 Claude 的安静读文件、思考、整理报告误判为失败。

默认策略：

- `attention_status=none` 且在 patience window 内：等待。
- `waiting_input`：补最小缺失信息。
- `needs_permission`：读结构化菜单，再 `select_option`。
- `blocked_by_policy`：不要重复等待或重复发送同一命令。
- `prompt_ack_missing`：处理 prompt 未提交，不继续排队。
- `report_ready=true`：进入报告验收，而不是继续催促。

`interrupt` 只用于：

- worker 明显跑偏。
- 正在重复被拒绝动作。
- 需要立即回收。
- 可见 PTY 被错误状态卡住。

## Claude Worker 侧安全措施

### 进程 cwd 与 hook 配置

Claude Code worker 的进程 cwd 由 daemon local config 控制：

```json
{
  "claude_workspace": "D:\\guKimi"
}
```

route `workspace` 只表示任务目标目录，不覆盖 Claude 进程 cwd。

Claude hooks 必须安装到：

```text
<claude_workspace>\.claude\settings.local.json
```

原因：

- Claude Code 只读取自己 cwd 下的 `.claude/settings.local.json`。
- hooks 负责把 Claude 内部事件回写 daemon。
- 不在正确 cwd 安装 hooks，会导致 binding、PostToolBatch、permission 状态全部失真。

### Wrapper binding

daemon 启动 PTY 时注入：

```text
AGENTCALL_WRAPPER_SESSION=<session_name>
```

hook payload 带回 `wrapper_session`，daemon 由此建立绑定。

禁止猜测绑定：

- 不用 cwd 猜。
- 不用 PID 猜。
- 不用窗口标题猜。
- 不用“第一个新 session”猜。

未绑定 hook：

- 标记为 unbound。
- 进入 attention。
- 不允许执行写入或非只读 bash。

### Hook 事件语义

关键 hook：

- `SessionStart`
- `UserPromptSubmit`
- `PreToolUse`
- `PostToolUse`
- `PostToolBatch`
- `Notification`
- `Stop`
- `SubagentStop`
- `PreCompact`
- `SessionEnd`

语义约定：

- `Stop` 是普通 turn end，不等于 checkpoint。
- `SubagentStop` 或显式 checkpoint request 才是 checkpoint due。
- permission notification 必须映射成 `needs_permission`。
- `PreToolUse` / `PostToolUse` / `UserPromptSubmit` 表示 worker 有实际进展。
- `PostToolBatch` 是 queued supervisor instruction 注入的重要机会。

## 写入边界与权限安全

### allowed_paths / scratch / report_path

worker 只能写：

- route 明确允许的 `allowed_paths`。
- daemon 为 session 创建的 scratch root。
- 明确的 `report_path`。

report route：

- 不允许写 implementation files。
- 只允许写 route report path 和 session scratch。
- 不允许 TaskCreate 漂移成子实现 worker。
- Bash 默认只允许只读探针。

coding route：

- Write/Edit/MultiEdit 必须命中允许路径。
- Bash 写入仍受策略限制。
- shell 重定向、删除、移动等写入行为不能绕过 policy。

### 文件 claim

原则：

- `Write` / `Edit` / `MultiEdit` 才创建 write claim。
- `Read` / `Glob` / `Grep` 只 observe，不污染 write claim。
- 同文件并发写入必须稳定冲突。
- 不同文件并发应允许。

### policy denial

如果同一 session 多次触发同类 denial：

- 标记 `blocked_by_policy`。
- summary 不应继续显示健康 working。
- Codex 不应继续等待或重复发送同一 prompt。
- wrapper 应给出建议：修改 allowed paths、改任务、请求 blocker report、或 interrupt/stop。

## Lease 与并发安全

### Owner lease

owner lease 防止多 Codex 控制同一 worker。

字段包括：

- owner_id
- owner_lease_id
- lease_generation
- status
- recoverable
- expires_at

控制命令必须匹配当前 lease。

### Workspace lease

workspace lease 防止多个 worker 同时写同一工作区。

模式：

- `Exclusive`
- `SharedReport`

规则：

- coding 任务需要 exclusive。
- report 任务可以 shared report lease。
- workspace key 必须 canonicalize，避免路径拼写绕过。

### 容量控制

daemon scheduler 负责并发上限：

- global max sessions。
- per-owner max sessions。
- 不创建隐藏队列。
- 超额直接返回 capacity exceeded。

### 孤儿 lease

已暴露风险：

- route 留下 owner/workspace lease。
- PTY 没真正存活或 daemon 丢失 live session。
- scheduler 仍把 lease 算作 active，造成幽灵占位。

修正方向：

- 对 active lease 与 live PTY session map 做对账。
- 超过启动宽限且没有 live session 的 recoverable lease 应释放。
- 同步释放 workspace lease。
- report/accept 不应承担 lease release 语义。

## PTY 输入与交互安全

### prompt 提交确认

route 启动 PTY 后不能只看“文本进了输入框”。

需要观察：

- `UserPromptSubmit` hook。
- route prompt gate。
- screen/projection 是否显示 prompt ack。

如果 route 状态长期为：

```text
started_pending_prompt_ack
```

应暴露为：

```text
attention_status=prompt_not_submitted
```

而不是 `working/none`。

### 普通 send 与 queued instruction

Claude Code 当前 turn 运行期间，不一定听得到新的 prompt。

因此：

- working 中的普通指导可进入 queued supervisor instruction。
- 下一次 `PostToolBatch` 或 context hook 再注入。
- 如果 session 未见过 `PostToolBatch`，返回 warning。
- 不把“已排队”伪装成“已送达”。

### 权限菜单

权限菜单不是自然语言输入。

wrapper 应结构化暴露：

```json
{
  "kind": "permission_menu",
  "options": [
    {"index": 1, "semantic": "approve"},
    {"index": 2, "semantic": "inspect"},
    {"index": 3, "semantic": "deny"}
  ]
}
```

Codex 使用：

```text
agentcall_session_send(action=select_option, text="1")
```

禁止：

- 对权限菜单发送自然语言。
- 在 permission prompt 中反复 `continue`。
- 未读菜单就盲选。

## 状态观测安全

### Projection-first

默认读：

```text
agentcall_board(view=compact, filter=attention)
agentcall_session(view=summary)
```

只有以下情况才展开：

- projection_stale。
- low_confidence。
- attention 需要上下文。
- 需要检查 TUI 菜单。
- debug/review。

### TUI 只是辅助

状态优先级：

```text
daemon lifecycle / hook structured events
> report/file validator
> route/prompt gate
> terminal screen snapshot
> raw/clean PTY tail
```

TUI regex 不应覆盖 hook/report/daemon 结构化状态。

### 输出预算

MCP 返回必须控制体积：

- summary 小。
- tui 有硬上限。
- events compact。
- raw/debug 显式请求才返回。
- PostToolUse / PostToolBatch 大输出必须截断和按 hook 类型索引。

目标：

- 不让 Codex 被大日志拖慢。
- 不把几十万字符 tool output 塞进一次 MCP 返回。
- 默认返回“下一步该做什么”，不是“所有曾经发生过什么”。

## 报告与验收安全

### Worker 交付

worker 生命周期结束时必须提供：

- concise report。
- exact change summary。
- files changed。
- tests run。
- failures。
- remaining risks。

### Codex 验收

Codex 不机械 review。

只有以下情况才 review 或二次派工：

- drift。
- blocker。
- failed validation。
- low confidence。
- contradiction。
- 用户要求 review。

### Confidence

报告可信度来源：

- high：报告 + daemon-observed file write / test pass。
- medium：报告存在，但证据不完整。
- low：自然语言报告、缺证据、policy block、permission denial、测试失败、矛盾。

Codex 不能把 Claude 自述当成最终真相。

## Daemon / MCP 边界安全

### Token 与 loopback

`/api/*` 默认要求 daemon token。

允许：

- 本地 loopback。
- 正确 `daemon_token`。

禁止：

- 默认打开 unauthenticated loopback。
- 把 `config/agentcall.local.json` 提交到 git。
- 把 daemon token 写进 README 示例。

### MCP transport

MCP bridge 应：

- 读取本地 config/env token。
- 带 token 调 daemon。
- 失败时返回明确错误。
- 不因为 daemon 重启长期卡死。

当前风险：

- Codex host 中的 MCP transport 可能不会自动重连。
- 需要 plugin/Codex 层 reload 或新 thread 才能重新绑定。

## Python 边界

允许 Python：

- hook installer。
- thin diagnostics。
- smoke tests。
- legacy/manual debug。
- release script glue。

禁止 Python：

- live event writer。
- live claim arbiter。
- live route/session/projection writer。
- Python/Rust 双写一致性方案。
- Python 文件锁作为并发正确性主线。

## 安全操作清单

### Codex 派工前

- [ ] daemon status ok。
- [ ] board compact 可读。
- [ ] 没有 unresolved attention。
- [ ] session_name 唯一。
- [ ] workspace 与 allowed_paths 明确。
- [ ] report_path 或交付方式明确。
- [ ] acceptance criteria 明确。

### Codex 控制 worker 时

- [ ] 默认读 summary，不读 raw。
- [ ] 不在 patience window 内重复催。
- [ ] permission prompt 用 select_option。
- [ ] policy block 不继续等待。
- [ ] destructive action 带 precondition。
- [ ] prompt 未提交时处理 prompt gate，而不是 queue 自然语言。

### Worker 侧

- [ ] hooks 安装在 `D:\guKimi\.claude\settings.local.json`。
- [ ] `AGENTCALL_WRAPPER_SESSION` 存在。
- [ ] binding_source 为 env。
- [ ] 写入只命中 allowed paths / scratch / report_path。
- [ ] 完成后写报告或 exact summary。

### 验收时

- [ ] 读 report。
- [ ] 看 daemon evidence。
- [ ] 看 test result。
- [ ] 看 changed_files 是否和实际 diff 对齐。
- [ ] clean report 直接接受，不机械 review。
- [ ] 有 drift/blocker/low confidence 才 review。

## 当前仍需继续收口的点

- prompt_not_submitted 需要从 route prompt gate 投影成 attention，而不是健康 working。
- non-live session 请求 debug include 应更友好降级，不应让 Codex 误解 400。
- session_send 缺 idempotency_key 已改为 MCP/daemon 自动生成，但运行态需要重启后生效。
- orphaned owner/workspace lease 对账已补方向，但运行态需要重启后验证。
- permission menu 仍需要进一步结构化成明确 options/recommended_action。
- MCP transport 重启/重连体验仍需继续硬化。
