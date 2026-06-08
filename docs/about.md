# About AgentCall

AgentCall 是一个给 Codex 使用的本地多 Agent 控制面。它的核心定位是：**让 Codex 指挥 Claude Code 集群协同工作**。

在复杂项目里，多个 Agent 很容易遇到上下文不完整、任务边界不清、文件互相覆盖、报告难验收等问题。AgentCall 把这些问题收敛到一个本地 daemon 控制面：

- Codex 负责任务拆分、派工、验收和重新组织工程。
- Claude Code PTY worker 负责执行具体代码、文件、调试和报告任务。
- daemon 负责记录状态、绑定 hook session、仲裁文件 claim、生成 readable summary、维护 board。

## 当前版本定位

v4.3 是 plugin-provided MCP + PTY-first + recent-first observability 版本：

- 当前 live worker runtime 只保留 Claude Code PTY。
- `agentcall_route(runtime=auto|pty)` 会启动 daemon-owned PTY utility worker。
- 默认使用 auto mode；不清楚或高风险任务可以显式使用 `pty_workflow=plan_then_auto`。
- Codex plugin 提供 MCP server 和 AgentCall skill guidance，降低不同 Codex session 看不到工具的问题。
- board/session 默认读取 recent hot log，大输出进入 artifact，历史日志按大小归档。
- 5 分钟无更新的历史/unbound session 会从 live 投影清理，Codex-facing patience 默认 60 秒。
- ACP 已从当前实现中删除，不再作为 MCP/daemon route runtime。

## 设计边界

AgentCall v4.3 不自动 kill 用户可见 PTY worker，不尝试恢复旧 Claude 进程，也不让 Python 成为 live state writer。它更像一个本地调度与观测底座：让 Codex 更可靠地看见、控制和验收 Claude Code 的工作，而不是替代 Claude Code 或替代人的最终判断。
