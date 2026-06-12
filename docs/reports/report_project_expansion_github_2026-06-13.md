# AgentCall 项目拓展与 GitHub 类似项目调研报告

> 调研日期：2026-06-13  
> 目标：为 AgentCall 寻找可借鉴项目、技术路线与拓展方向，同时识别风险和不建议投入的方向。  
> 调研范围：Codex/Claude Code orchestration、PTY worker supervisor、MCP server control plane、terminal session manager、agent orchestration、hook-aware automation、multi-agent coding workflow。

---

## 1. AgentCall 当前定位速览

AgentCall v6.3.0 是一个**本地多 Agent 协作控制面**：让 Codex 通过 MCP 工具监督由 Rust daemon 拥有的 Claude Code PTY utility worker 集群。核心设计要点：

- **双亲模型**：Codex 做任务拆分、监督、验收；Claude Code worker 做边界明确的实现/审查/报告。
- **Rust daemon 为状态权威**：events、claims、sessions、routes、projections、health 均由 daemon 持有。
- **PTY-first**：保留人类可见的终端过程，而不是把 Claude Code 藏到 SDK/ACP 调用后面。
- **Hook-aware**：通过 Claude/Codex hooks（SessionStart、UserPromptSubmit、PostToolUse 等）把运行时观察写回 daemon。
- **Bounded writes**：写工具受 route containment 约束，Bash 默认 readonly-only。
- **Projection-first**：Codex 默认读 compact board/session projection，而不是原始 terminal 输出。
- **Report closure**：`agentcall_route` 自动生成报告路径，`request_report` 是状态转换，报告写入后 projection 进入 `report_ready`。

与市面上的开源/商业方案相比，AgentCall 的差异化在于**本地、紧凑、可观测、强约束**——它不是通用的 agent framework，而是 Codex ↔ Claude Code 之间的**受控执行层**。

---

## 2. 类似/相关 GitHub 项目与技术方向

下面按照“可直接对标”“可借鉴基础设施”“可借鉴编排范式”三个层次展开，覆盖 9 个项目/方向，均超过 6 个的最低要求。

### 2.1 OpenAI Codex CLI（官方 runtime）

- **仓库/URL**：https://github.com/openai/codex（官方 CLI）
- **可借鉴点**：
  - 2026 年已原生支持多线程 subagent：`[agents] max_threads = 6`、`max_depth = 1`。
  - 内建 `default` / `worker` / `explorer` 三种 agent 角色，Codex 自动按任务类型分派。
  - 使用 git worktree 做隔离，每个 agent 独立分支。
- **与 AgentCall 的差异**：
  - Codex CLI 是**单一宿主进程内**的线程/worktree 模型；AgentCall 是**跨进程 PTY 模型**，Codex 与 Claude Code 是不同厂商的 runtime。
  - Codex CLI 没有 hook-aware daemon 投影，也没有文件 claim / bounded write policy。
- **对 AgentCall 的启示**：
  - 子 agent 数量、深度、角色的配置化可以参考；但 AgentCall 不应复制其 in-process 模型，而应坚持跨进程监督。
  - 可借鉴 git worktree 作为可选隔离手段，但 Windows 兼容性需谨慎评估。

### 2.2 oh-my-codex（OMX）— 第三方 Codex/Claude 混合编排

- **仓库/URL**：https://github.com/junghwaYang/oh-my-codex
- **可借鉴点**：
  - 在 tmux pane 中同时运行 Codex、Claude、Gemini 等异构 worker。
  - 32 个专用 agent 角色，支持 `autopilot`、`ulw`（并行）、`ralph`（永不放弃）等模式。
  - 每个 worker 使用独立 git worktree。
- **与 AgentCall 的差异**：
  - OMX 是 tmux 为中心的多 pane 协调，偏“人机可见的并行面板”；AgentCall 是 daemon 投影为中心的“控制面”。
  - OMX 不强调 route containment、file claim、report acceptance。
- **对 AgentCall 的启示**：
  - 未来如果需要支持**异构 worker**（Claude Code + Codex CLI + 其他），tmux/PTY  multiplexing 是可行底座。
  - 不建议直接集成 OMX，但可参考其角色模板和模式切换思路。

### 2.3 Claude Code Agent Teams / Gas Town / Multiclaude / Overstory

这是围绕 Claude Code 的本地/云端多 agent 编排生态，AgentCall 本身已属于该生态的“基础设施层”。

#### 2.3.1 Claude Code Agent Teams（实验性官方能力）

- **启用方式**：`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`
- **可借鉴点**：
  - Team Lead + Teammates 两层结构；teammate 是独立 Claude Code 实例，各自拥有独立 context window。
  - 共享 JSON task list（含依赖图、自动解阻塞）、mailbox 点对点通信、worktree 隔离。
  - `TaskCreate`、`SendMessage`、worktree 隔离是核心 primitive。
- **与 AgentCall 的差异**：
  - AgentCall 的 supervisor 是 Codex，Agent Teams 的 supervisor 是另一个 Claude Code 实例。
  - AgentCall 通过 daemon 做状态权威；Agent Teams 通过 JSON/mailbox 做协调。
- **对 AgentCall 的启示**：
  - AgentCall 可把自己定位为“Agent Teams 的可替代控制面”，用 Rust daemon 提供更稳定的任务队列、文件 claim、报告验收。

#### 2.3.2 Gas Town

- **仓库/URL**：https://github.com/steveyegge/gastown（或对应发布渠道）
- **可借鉴点**：
  - 被称为 "Kubernetes for AI agents"，Mayor agent 分派指定 agent。
  - 通过 git-backed hooks、内置 mailbox/identity/handoff 解决上下文持久化。
  - 可舒适扩展到 20-30 个 agent。
- **与 AgentCall 的差异**：
  - Gas Town 更偏“个人开发者玩具/效率工具”，AgentCall 更偏“工程化控制面”。
- **对 AgentCall 的启示**：
  -  mayor/dispatcher + named agent identity 的模式值得借鉴，可用于 AgentCall 的 route scheduler 扩展。

#### 2.3.3 Multiclaude（Dan Lorenc）

- **仓库/URL**：https://github.com/dlorenc/multiclaude
- **可借鉴点**：
  - “Brownian ratchet”哲学：永远向前推进，CI 通过则自动合并。
  - Supervisor agent 分配任务给 subagent；singleplayer（自动合并）vs multiplayer（团队评审）两种模式。
- **与 AgentCall 的差异**：
  - Multiclaude 强调自动合并，AgentCall 强调显式 report acceptance。
- **对 AgentCall 的启示**：
  - 自动合并/条件通过的门控机制可作为远期探索，但短期内与 AgentCall 的显式验收文化冲突。

#### 2.3.4 Overstory

- **仓库/URL**：https://github.com/jayminwest/overstory
- **可借鉴点**：
  - 生产级系统：两层 agent 定义（base `.md` + per-task overlay）。
  - SQLite 消息队列，8 种消息类型（`worker_done`、`merge_ready`、`dispatch`、`escalation` 等）。
  - FIFO merge queue + 4 层冲突解决、分层健康监控、token instrumentation。
- **与 AgentCall 的差异**：
  - Overstory 是完整应用，AgentCall 是底层控制面。
- **对 AgentCall 的启示**：
  - SQLite 消息队列、分层健康监控、token 统计等是 AgentCall daemon 可中长期内化的能力。

### 2.4 Aider — Git-first 双模型架构

- **仓库/URL**：https://github.com/Aider-AI/aider
- **可借鉴点**：
  - **Architect/Editor 双模型模式**：强模型做规划（Claude Opus / GPT-4o），快/便宜模型做编辑（Sonnet / Haiku / GPT-4o-mini）。
  - Git-native：每次编辑自动提交，代码历史清晰可审计。
  - 支持 40k+ GitHub stars，周处理 15B tokens。
- **与 AgentCall 的差异**：
  - Aider 是单进程、单仓库、单用户工具；AgentCall 是多 worker、supervisor、daemon 状态权威。
  - Aider 的 architect/editor 是模型角色切换，不是跨 agent 协调。
- **对 AgentCall 的启示**：
  - 可在 route 级别引入 `--architect`/`--editor` 模型配对，让 Codex 指定规划用强模型、实现用经济模型。
  - Git-native 提交策略可作为 worker 输出规范的一部分，但不应强制自动合并。

### 2.5 Cline / Roo Code — 从单 agent 到编排层

#### 2.5.1 Cline

- **仓库/URL**：https://github.com/cline/cline
- **可借鉴点**：
  - 58k+ stars，最大开源 AI coding 扩展，支持 30+ 模型提供商。
  - Plan-then-execute 模式：先规划、后执行。
  - **Cline Kanban**：明确把自己定位为“multi-tool agent orchestration layer”，可同时编排 Cline、Claude Code、Codex。
  - MCP Marketplace 生态。
- **与 AgentCall 的差异**：
  - Cline 是 VS Code 插件/桌面应用；AgentCall 是无头 daemon + MCP。
  - Cline 面向人类+AI 协作界面；AgentCall 面向 Codex 监督多个 Claude Code worker。
- **对 AgentCall 的启示**：
  - MCP Marketplace / tool registry 思路可作为 AgentCall plugin 生态的参考。
  - Kanban 式任务板与 AgentCall 的 board projection 可互补：AgentCall 提供状态，外部 UI 可视化。

#### 2.5.2 Roo Code

- **仓库/URL**：https://github.com/RooVetGit/Roo-Code
- **可借鉴点**：
  - 多模式工作流：Code、Architect、Ask、Debug、Custom。
  - Cloud agents：可脱离本地 IDE 在云端运行。
- **与 AgentCall 的差异**：
  - Roo Code 是 IDE 扩展；AgentCall 不依赖 IDE。
- **对 AgentCall 的启示**：
  - 模式/角色模板化可以参考，但 AgentCall 已经通过 route `objective` + skill + prompt 注入实现了类似能力。

### 2.6 OpenHands — 沙盒自主工程师 + 企业控制面

- **仓库/URL**：https://github.com/All-Hands-AI/OpenHands
- **可借鉴点**：
  - 75k+ stars，开源自主软件工程 agent，Docker sandbox 执行。
  - 2026 年 5 月企业版推出 **Agent Control Plane**：多并发 agent、MCP 支持、自动化工作流。
  - 支持 Claude、GPT-4o、Gemini、Llama、Qwen 等模型。
  - SWE-bench 公开基准。
- **与 AgentCall 的差异**：
  - OpenHands 是端到端自主 agent；AgentCall 是“Codex 监督 Claude Code”的本地控制面。
  - OpenHands 用 Docker 沙盒；AgentCall 用本地 PTY + 文件 claim。
- **对 AgentCall 的启示**：
  - Docker/容器化 worker 是 AgentCall 可选的拓展方向，可提升隔离性和可复现性。
  - Agent Control Plane 的并发调度、健康检查、自动化流水线是中长期可借鉴方向。

### 2.7 GitHub Copilot Coding Agent / Cloud Agent

- **仓库/URL**：https://github.com/features/copilot（商业服务，无单一开源仓库）
- **可借鉴点**：
  - 基于 GitHub Actions runner 的云端异步 agent，最大会话 59 分钟。
  - 支持并发会话、自定义 agent、第三方 agent（Claude、Codex）、MCP server。
  - Microsoft Agent Framework 集成：Sequential / Concurrent / Handoff / Group chat 四种工作流。
  - 项目级上下文文件：`AGENTS.md`、`MEMORY.md`、`.agent.md`。
- **与 AgentCall 的差异**：
  - Copilot Coding Agent 是云端、GitHub 生态、异步 PR 驱动；AgentCall 是本地、实时、Codex 监督。
  - Copilot 企业治理（admin 控制、授权模型）是 AgentCall 所没有的。
- **对 AgentCall 的启示**：
  - `AGENTS.md` / `MEMORY.md` 规范已被 AgentCall 部分采用（AGENTS.md），可继续强化。
  - 自定义 agent / skill 的市场化/注册机制是长期生态方向。
  - 不建议直接对标云端异步 PR 代理，这与 AgentCall 的本地 PTY-first 定位冲突。

### 2.8 LangGraph / Mastra / CrewAI — 通用 agent 编排框架

| 项目 | 仓库/URL | 可借鉴点 | 与 AgentCall 的差异 |
|------|----------|----------|---------------------|
| **LangGraph** | https://github.com/langchain-ai/langgraph | `interrupt()` 原生 HITL、per-node checkpointing、supervisor + `Send` 并行 fan-out、Uber/LinkedIn/Klarna 生产案例 | Python-first 通用框架，不绑定任何 coding agent runtime |
| **Mastra** | https://github.com/mastra-ai/mastra | TypeScript-native、`.suspend()`/`.resume()`、supervisor scoring、observability memory、PII redaction | 现代 JS/TS 全栈框架，不聚焦本地 PTY |
| **CrewAI** | https://github.com/crewAIInc/crewAI | 49k+ stars，role-based agents、Flows 事件驱动、memory、全面 MCP 支持（stdio/SSE/streamable HTTP） | 偏角色扮演/业务流程，非 coding-specific |

- **对 AgentCall 的启示**：
  - LangGraph 的 checkpointing / interrupt 模型可用于 AgentCall daemon 的**状态恢复与人工审批门**。
  - Mastra 的 suspend/resume、supervisor scoring 可作为 route lifecycle 的参考。
  - CrewAI 的 role-based flow 与 AgentCall 的 `worker_kind=utility` + route objective 有对应关系。
  - **不建议**把 AgentCall 改造成通用 LangGraph/Mastra 包装器；AgentCall 的价值在于“Codex + Claude Code + 本地约束”的垂直整合。

### 2.9 Terminal Session Manager / PTY 基础设施

| 项目 | 仓库/URL | 可借鉴点 | 与 AgentCall 的差异 |
|------|----------|----------|---------------------|
| **tmux** | https://github.com/tmux/tmux | 最成熟的多会话、detach/reattach、脚本化、AI agent 编排事实标准 | 仅提供会话复用，无 agent 语义 |
| **Zellij** | https://github.com/zellij-org/zellij | Rust 实现、WASM 插件、KDL layout、floating panes、内置 session manager | 内存占用较高（~80MB），目前 Claude Code 支持不如 tmux |
| **TSM** | https://github.com/adibhanna/tsm | Rust 实现的现代 terminal session manager，一 daemon 一会话，支持原生 terminal split、screen restore、Codex/Claude 状态监控 | 偏终端用户工具，无 daemon 编排 |
| **Agent Hand** | https://github.com/weykon/agent-hand | tmux-backed，专为 AI coding agent 设计，状态指示灯（WAITING/RUNNING/READY/IDLE）、priority jump、session group | 轻量 wrapper，无状态权威 |
| **Agent Session Manager** | https://github.com/izll/agent-session-manager | Go + Bubble Tea TUI，管理 Claude/Gemini/Aider/Codex 多 CLI session | UI 层，无 projection/claim |

- **对 AgentCall 的启示**：
  - AgentCall 已经自己实现了 Rust PTY daemon，不需要再依赖 tmux/Zellij 作为核心。
  - 但可参考 TSM 的 screen restore、Agent Hand 的状态灯、Zellij 的 WASM 插件机制来增强 board UI 或本地开发者体验。
  - 若未来需要**多 pane 人机共驾**，Zellij 插件或 tmux 集成可作为可选前端。

### 2.10 MCP Server / Hook-aware 自动化生态

| 项目/方向 | 仓库/URL | 可借鉴点 |
|-----------|----------|----------|
| **madebyaris/agent-orchestration** | https://github.com/madebyaris/agent-orchestration | MCP server 形态的多 agent 协作：shared memory、task queue、resource locks、Cursor rules、AGENTS.md workflows |
| **MCP 生态目录** | https://github.com/AgentMCP/ai-agent-directory | MCP server / agent 编排工具的聚合索引 |
| **devops-ai-skill** | https://github.com/qwedsazxc78/devops-ai-skill | `PostToolUse` Edit/Write → YAML lint / terraform fmt 的自动化示例 |
| **Claude Code Hooks 规范** | https://docs.anthropic.com/en/docs/claude-code/hooks | 30 个 hook event，PreToolUse 可阻塞、PostToolUse 仅顾问 |

- **对 AgentCall 的启示**：
  - AgentCall 的 hook ingest 已是差异化能力，可进一步做成**通用 hook 编排平台**：允许用户注册 `PostToolUse` handler（如自动 format、lint、security scan）。
  - MCP server 形态可让 AgentCall 被非 Codex 的客户端调用，但会稀释产品聚焦，建议仅作为长期选项。

---

## 3. AgentCall 可拓展方向

基于上述调研，结合 AgentCall 现有架构，下面按**短期（3 个月内）**、**中期（3-12 个月）**、**长期（12 个月以上）**给出建议。

### 3.1 短期（3 个月内）—— 夯实现有主线

1. **强化 board/session projection 的“状态灯”语义**
   - 借鉴 Agent Hand 的 WAITING/RUNNING/READY/IDLE 指示灯，把 worker 状态映射为更直观的视觉/文本状态。
   - 在 `agentcall_session` summary 中显式返回 `status_indicator` 字段。

2. **引入轻量级 hook handler 注册机制**
   - 允许在 daemon 配置或 route 中注册 `PostToolUse` handler（如保存文件后自动 `cargo fmt`、`ruff`、`eslint`）。
   - 保持 advisor-only（不阻塞），与 Claude Code hooks 语义一致。

3. **标准化 AGENTS.md / MEMORY.md 工作流**
   - AgentCall 已使用 AGENTS.md，可进一步与社区规范（madebyaris/agent-orchestration 等）对齐，支持从目标仓库自动加载 `AGENTS.md` 作为 route context。

4. **报告模板与置信度细化**
   - v6.3 已有 `overall/artifact/daemon_write/route_match` 四维置信度，可借鉴 Aider 的 git commit 审计、OpenHands 的 SWE-bench 证据，增加 `test_evidence`、`lint_evidence` 等维度。

5. **worktree 隔离试点**
   - 在 route 中可选启用 git worktree，防止多个 worker 写同一分支冲突。
   - 先在 read/report 类 route 试点，写实现路径的 route 保持独占 workspace lease。

### 3.2 中期（3-12 个月）—— 扩展能力与生态

1. **异构 worker 支持（Claude Code + Codex CLI + 其他）**
   - 当前 route 默认启动 Claude Code PTY。可抽象 `runtime` trait，允许启动 Codex CLI、OpenHands、Aider 等作为 worker。
   - 保持 daemon 状态权威，worker 之间通过 hook ingest 统一上报。

2. **任务队列与依赖图**
   - 借鉴 Overstory 的 SQLite message queue、Claude Code Agent Teams 的 shared task list，在 daemon 中实现简单的 task DAG。
   - Codex 可一次提交多个 route，daemon 按依赖自动调度和解阻塞。

3. **人工审批门（HITL）**
   - 借鉴 LangGraph `interrupt()` / Mastra `.suspend()`，在关键状态转换（如 plan approval、高风险写操作）暂停，等待人类或 Codex 显式 `approve`。
   - 与 `submit_pending_prompt` 等现有门控自然衔接。

4. **MCP server 形态发布**
   - 当前 AgentCall MCP 是 repo-local plugin。可发布独立 MCP server，让 Claude Desktop、Cline、Roo Code 等也能调用 AgentCall daemon。
   - 注意：这会扩大攻击面，需要更严格的 token/scope 控制。

5. **Web UI / TUI board**
   - 当前 `/board` 是静态页面。可借鉴 TSM、Agent Session Manager 做更丰富的实时 board：live sessions、attention、policy blocks、report status。

### 3.3 长期（12 个月以上）—— 平台化与商业化探索

1. **Agent Control Plane 2.0**
   - 从“Codex 监督 Claude Code”扩展到“多 supervisor 监督多类型 worker”。
   - 支持自定义 agent 角色、skill marketplace、资源配额、审计日志。

2. **容器化 / 云端 worker**
   - 可选 Docker/容器化执行环境，提升隔离性和可复现性。
   - 云端托管版本：让 AgentCall daemon 运行在远程服务器，本地 Codex/Claude 通过安全通道连接。
   - **风险**：与本地 PTY-first 定位冲突，需作为可选模式而非默认。

3. **企业治理与合规**
   - 多用户、RBAC、模型授权、审计轨迹、敏感数据 redaction（参考 Mastra PII redaction）。
   - 与 GitHub/GitLab 企业账号集成。

4. **证据驱动的自动验收**
   - 引入测试、lint、类型检查、安全扫描作为 report acceptance 的硬门槛。
   - 借鉴 Multiclaude 的“CI 通过则自动合并”，但保留最终 human/Codex 否决权。

---

## 4. 风险与不建议投入的方向

### 4.1 高风险方向

1. **改造成通用 agent framework**
   - LangGraph/CrewAI/Mastra 已经在通用编排上非常成熟，AgentCall 不应正面竞争。
   - 风险：失去“Codex + Claude Code 本地控制面”的清晰定位，陷入功能同质化。

2. **云端异步 PR agent（对标 Copilot Coding Agent / Jules）**
   - 这需要云基础设施、GitHub App 集成、企业合规，投入巨大。
   - 与 AgentCall 的本地 PTY-first、人类可见的核心价值冲突。

3. **深度绑定单一厂商 runtime**
   - 若过度依赖 Claude Code 私有协议或 Codex 内部实现，容易被上游变更打破。
   - 应坚持基于公开协议（MCP、Claude Code hooks、PTY）的抽象层。

4. **自动合并/自动发布**
   - 无人工或强证据门控的自动合并容易导致代码事故。
   - AgentCall 当前的显式 report acceptance 是安全资产，不应轻易放弃。

### 4.2 不建议做的方向

1. **重新实现 tmux/Zellij 作为核心**
   - AgentCall 已经有 Rust PTY daemon，再引入外部 terminal multiplexer 会增加复杂度。
   - 仅在“人机共驾多 pane UI”场景下作为可选前端，不作为核心路径。

2. **把 Python 重新作为 live state writer**
   - AGENTS.md 已明确禁止。Python 适合做脚本/诊断，不应写 events/claims/routes/sessions/projections。

3. **恢复 ACP/SDK 作为默认 runtime**
   - v3.0 已明确移除，v6.x 冻结计划也不建议恢复。除非用户明确要求，否则 PTY-first 不变。

4. **追求大而全的 plugin marketplace 过早**
   - 在核心控制面未稳定前， marketplace 会分散维护精力。
   - 建议先做好 3-5 个官方 skill 模板，再考虑社区扩展。

### 4.3 需要持续关注的外部变化

- **Claude Code hooks 规范升级**：Anthropic 可能增加/修改 hook event，AgentCall 需要兼容。
- **Codex CLI 多 agent 能力**：如果 Codex CLI 的 subagent/worktree 模型更成熟，可能减少外部编排需求。
- **MCP 协议演进**：streamable HTTP、A2A（Agent-to-Agent）等新兴协议可能影响 AgentCall 的 transport 设计。
- **GitHub / 云平台 agent 战略**：大平台如果免费提供强大多 agent 能力，会压缩本地工具的付费空间。

---

## 5. 结论

AgentCall 的核心价值是**本地、受控、可观测的 Codex-to-Claude Code 多 worker 协调**。短期内应继续夯实 projection、hook、report closure、bounded write 等差异化能力；中期可向异构 worker、任务队列、HITL 审批门、独立 MCP server 扩展；长期可探索容器化、云端托管、企业治理，但需警惕与本地 PTY-first 定位的冲突。

不建议把 AgentCall 改造成通用 agent framework 或云端异步 PR agent，也不建议过早追求 marketplace。应保持产品聚焦，把“可靠的本地 multi-agent 工程协调”做到极致。

---

## 6. 参考来源

- OpenAI Codex CLI / multi-agent: [Firecrawl blog](https://www.firecrawl.dev/blog/codex-multi-agent-orchestration)、[oh-my-codex](https://github.com/junghwaYang/oh-my-codex)
- Claude Code multi-agent: [Shipyard blog](https://shipyard.build/blog/claude-code-multi-agent/)、[Morph LLM](https://www.morphllm.com/ai-agent-orchestration)、[Overstory](https://github.com/jayminwest/overstory)
- Aider: [AI FOSS](https://aifoss.dev/blog/aider-review-2026/)、[Aider-AI/aider](https://github.com/Aider-AI/aider)
- Cline / Roo Code: [Augment Code](https://www.augmentcode.com/tools/open-source-agent-orchestrators)、[Cline GitHub](https://github.com/cline/cline)、[Roo Code GitHub](https://github.com/RooVetGit/Roo-Code)
- OpenHands: [The AI Agent Index](https://theaiagentindex.com/agents/openhands)、[All-Hands-AI/OpenHands](https://github.com/All-Hands-AI/OpenHands)
- GitHub Copilot Coding Agent: [GitHub Docs](https://docs.github.com/en/copilot/concepts/agents/coding-agent/about-coding-agent)、[GitHub Blog](https://github.blog/news-insights/product-news/github-copilot-meet-the-new-coding-agent/)
- LangGraph / Mastra / CrewAI: [LangGraph GitHub](https://github.com/langchain-ai/langgraph)、[Mastra GitHub](https://github.com/mastra-ai/mastra)、[CrewAI GitHub](https://github.com/crewAIInc/crewAI)
- Terminal session managers: [TSM](https://github.com/adibhanna/tsm)、[Agent Hand](https://github.com/weykon/agent-hand)、[Zellij](https://github.com/zellij-org/zellij)、[tmux](https://github.com/tmux/tmux)
- MCP / Hook automation: [madebyaris/agent-orchestration](https://github.com/madebyaris/agent-orchestration)、[devops-ai-skill](https://github.com/qwedsazxc78/devops-ai-skill)、[Morph LLM hooks](https://www.morphllm.com/claude-code-hooks)
