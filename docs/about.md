# About AgentCall

AgentCall 是一个给 Codex 使用的本地多 Agent 控制面。它的核心定位是：**让 Codex 指挥 Claude Code 集群协同工作**。

在复杂项目里，单个 Agent 往往会遇到上下文不完整、任务切分困难、并发修改不可控、结果难验收等问题。AgentCall 把这些问题收敛到一个本地 daemon 控制面：

- Codex 负责拆任务、派工、验收和重新组织工程。
- Claude Code worker 负责执行具体代码、配置、文档或验证任务。
- daemon 负责记录状态、绑定 hook session、仲裁文件 claim、汇总 readable summary、维护 board。

AgentCall v2.0 的重点不是替代 Codex 或 Claude Code，而是把它们放进一个更稳定的协作协议里。Codex 不需要长期读 raw terminal；它默认读 board 和 session summary。Claude Code 也不需要知道整个组织流程；它只需要在 bounded lifecycle 内完成任务并写 report。

## 为什么需要它

- 多个 Claude Code 同时工作时，需要知道谁在改什么。
- Codex 需要低成本看到 worker 是否等待输入、需要权限、已完成 report 或出现冲突。
- PTY 对人类可视化友好，ACP 对 Codex 工具化调用友好，两条路线需要统一 route。
- hooks 比 TUI grep 更可信，应成为 summary 状态的主来源。
- 本机配置、Claude Code 启动 cwd、MCP transport 和 daemon 更新必须分层，否则每次改工具都会引发断线。

## v2.0 已经解决什么

- daemon single-writer：共享 live 状态由 Rust daemon 写入。
- route-first MCP：Codex 默认走 `board -> route -> session/report`。
- PTY + ACP 双 runtime：可视化 handoff 和 bounded child invocation 都进入 daemon 控制面。
- hook-aware binding：wrapper session 与 Claude hook session 用 env 绑定，不靠 cwd、PID 或窗口标题猜测。
- readable wrapper：raw output、clean output、llm_summary 分层。
- daemon-local config：Claude Code 启动 cwd 从 `config/agentcall.local.json` 读取，模板可提交，本机值不提交。

## 边界

AgentCall v2.0 不自动 kill 用户可见 PTY worker，不做 Claude Code 生命周期自动销毁，不把 Python 作为 live state writer，也不依赖 Claude 内部私有状态变量。它更像一个本地调度与观测底座：让 Codex 更可靠地组织 Claude Code，而不是替代人的最终判断。
