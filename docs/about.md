# About AgentCall

AgentCall 是一个给 Codex 使用的本地多 Agent 控制面。它的核心定位是：**让 Codex 指挥 Claude Code PTY worker 集群协同工作**。

在复杂项目里，多个 Agent 很容易遇到上下文不完整、任务边界不清、文件互相覆盖、报告难验收、父层不断读 terminal 读到烦躁等问题。AgentCall 把这些问题收敛到本地 daemon：

- Codex 负责任务拆分、派工、等待策略、验收和工程整合。
- Claude Code PTY worker 负责执行具体代码、文件、调试、审查和报告任务。
- Rust daemon 负责状态权威、hook binding、file claim、route/session/report projection、runtime health 和 compact board。

## 当前版本定位

v5.3 是 hard-gate closure checkpoint：

- 当前默认 runtime 是 Claude Code PTY utility worker。
- ACP/SDK 不再作为默认产品路线。
- `agentcall_route(runtime=auto|pty)` 负责启动 daemon-owned PTY worker。
- Claude Code worker 强制在 daemon config 的 `claude_workspace` 下启动，读取该目录的 `.claude/settings.local.json`。
- Codex plugin 提供 MCP server 和 AgentCall supervisor skill，降低不同 Codex session 看不到工具的问题。
- board/session 默认读取 projection 和 recent hot logs，大输出进入 artifact，历史日志按大小归档。
- 写入 route `report_path` 会把 worker 标记为 `report_ready`，让 Codex 可以验收而不是继续盲等。
- read-only worker 默认拒绝 `TaskCreate`，防止审查任务漂移成实现任务。

## 设计边界

AgentCall 不自动替人决定最终合并，不试图恢复旧 Claude 进程，不把 Python 作为 live state writer，也不让 Codex 默认依赖 raw PTY 文本判断状态。

它更像一个本地调度与观测底座：让 Codex 更可靠地看见、控制和验收 Claude Code 的工作，同时保持人类可以打开 PTY 看见真实过程。

## 当前未闭合事项

v5.3 仍有底层 open gates：actor panic guard、control/output channel isolation、stop/kill 语义拆分、daemon restart 后 orphan projection、report accept 后 lease release。当前状态详见 [v5.3 closure status](reports/v5.3-closure-status.md)。
