# About AgentCall

AgentCall 是一个给 Codex 使用的本地多 Agent 控制面。它的核心定位是：**让 Codex 指挥 Claude Code PTY worker 集群协同工作**。

在复杂项目里，多个 Agent 很容易遇到上下文不完整、任务边界不清、文件互相覆盖、报告难验收、父层不断读 terminal 读到烦躁等问题。AgentCall 把这些问题收敛到本地 daemon：

- Codex 负责任务拆分、派工、等待策略、验收和工程整合。
- Claude Code PTY worker 负责执行具体代码、文件、调试、审查和报告任务。
- Rust daemon 负责状态权威、hook binding、file claim、route/session/report projection、runtime health 和 compact board。

## 当前版本定位

v6.6 是 slim Codex control plane 的控制面硬化版：

- 当前默认 runtime 是 Claude Code PTY utility worker。
- ACP/SDK 不再作为默认产品路线。
- `agentcall_route(...)` 默认启动 daemon-owned PTY worker；Codex 不需要选择 runtime 或估算任务大小。
- `agentcall_session(name)` 默认返回归一化 worker state、why、primary_action、available/debug actions、report 和短期 control token。
- Claude Code worker 强制在 daemon config 的 `claude_workspace` 下启动，读取该目录的 `.claude/settings.local.json`。
- Codex plugin 提供 MCP server 和 AgentCall supervisor skill，降低不同 Codex session 看不到工具的问题。
- board/session 默认读取 compact projection，不把历史 session、raw terminal、tool output 混入正常控制面。
- prompt gate 把 `UserPromptSubmit` 作为任务真正开始的结构化确认；未确认的卡住 prompt 默认继续等待，`submit_pending_prompt` 只作为 debug/recovery 动作。
- stale `prompt_pending_ack` 会由 daemon 自动发起一次 prompt commit，不再要求 Codex 正常路径手动点 `submit_pending_prompt`。
- 安全锁错误码由 Rust `ErrorCode` 枚举产出，再序列化为稳定 JSON code，避免继续扩散字符串错误码。
- SQLite/WAL store 可以按默认 6 并发 fan-out 写入；JSON 后端仍保持单 writer 作为安全回退。
- 写入 route `report_path` 会把 worker 标记为 `report_ready`，让 Codex 可以验收而不是继续盲等。
- 正常 worker 只分两类：`coding` worker 写实现路径并独占 workspace；`report` worker 只写报告/scratch 并共享 workspace lease。
- `read_only` route 参数和纯只读 worker 工作线已移除；审查、调研、复核这类任务应使用 `report` worker。

## 设计边界

AgentCall 不自动替人决定最终合并，不试图恢复旧 Claude 进程，不把 Python 作为 live state writer，也不让 Codex 默认依赖 raw PTY 文本判断状态。

它更像一个本地调度与观测底座：让 Codex 更可靠地看见、控制和验收 Claude Code 的工作，同时保持人类可以打开 PTY 看见真实过程。

## 当前未闭合事项

v6.x 的冻结基线见 [v6.2 code plan](v6.2-code-plan.md)。如果实现中发现新的真实压力问题，应写入 `docs/reports/`，不要改写冻结计划本身。
