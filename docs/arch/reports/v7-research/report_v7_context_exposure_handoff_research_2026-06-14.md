# AgentCall v7 Context Exposure / Handoff 研究报告

**日期：** 2026-06-14  
**任务：** 在 2026-06-13 已有「共享上下文」与「Worker Brief Compiler」研究的基础上，进一步扩大搜索范围，聚焦 **multi-agent handoff、agent shared memory、context engineering、agent workspace/context packet、software engineering agents** 中的类似设计，评估它们如何处理「给子 agent 什么上下文」，并给出对 AgentCall 当前 brief/context 设计的反驳与可落地架构建议。  
**范围：** 只写报告，不改源码。

---

## 摘要

当前 AgentCall v6.7.1 的核心 handoff 模型是：Codex 作为 supervisor 直接构造 prompt，通过 `agentcall_route(objective, workspace, write_paths, reference_paths)` 把任务丢给 Claude Code PTY worker；worker 执行后写 report，Codex 验收。这个模型已经能跑通 6 并发 report worker，但它把「给子 agent 什么上下文」这件事完全交给了 Codex 的即兴判断。

本次扩大搜索后发现，主流系统已经不再把 handoff 当作「复制粘贴 prompt」，而是把它当作一个需要显式设计的 **context exposure 工程问题**：

- **Claude Code AgentTool** 用 sidechain transcript + summary-only return 防止父上下文爆炸。
- **OpenAI Agents SDK** 用 `input_filter`、`input_type`、`nest_handoff_history` 精确控制子 agent 能看到什么。
- **LangGraph** 把状态分成 thread-scoped checkpointer 与 cross-thread store。
- **A2A** 用结构化的 Task object（status/artifacts/history/metadata/contextId）做跨 agent 委托。
- **Cline Memory Bank**、**OpenClaw**、**aider** 都强调把上下文资产化、分层、预算化。
- **OWASP/学术安全研究**则提醒我们：共享上下文如果设计不好，本身就是 agent cascading injection（ACI）和 context over-sharing 的攻击面。

核心结论：AgentCall v7 不应满足于「给 worker 一个 objective 和一堆 reference_paths」，而应该实现一个 **daemon-owned Worker Brief Compiler + Context Packet + ProjectMemory** 的三层架构，让 handoff 变成可审计、可预算、可复用、可隔离的契约。

---

## 1. 相关项目/论文/框架列表

| 类别 | 名称 | 关键标签 | 与 AgentCall 的相关性 |
|------|------|----------|----------------------|
| **Claude Code 原生** | [Claude Code AgentTool / Subagent](https://github.com/VILA-Lab/Dive-into-Claude-Code) | sidechain transcript, summary-only return, isolation modes | 直接同类：父 agent 如何委托子 agent |
| **论文** | [Dive into Claude Code: The Design Space of Today's and Future AI Agent Systems](https://arxiv.org/abs/2604.14228) | 98.4% infrastructure, permission, compaction, hooks | 理解 Claude Code 作为基础设施的上下文设计哲学 |
| **OpenAI** | [OpenAI Agents SDK Handoff](https://github.com/openai/openai-agents-python) | `input_filter`, `nest_handoff_history`, `input_type` | 控制 handoff 时上下文暴露的精确机制 |
| **PydanticAI** | [PydanticAI Multi-Agent Patterns](https://pydantic.dev/docs/ai/guides/multi-agent-applications/) | deps propagation, capabilities, tool filtering | 类型驱动的上下文传播与过滤 |
| **LangChain/LangGraph** | [LangGraph Persistence](https://docs.langchain.com/oss/python/langgraph/persistence) | checkpointer vs store, thread-scoped vs cross-thread | 短期状态与长期记忆的工程分层 |
| **Microsoft** | [AutoGen Memory](https://microsoft.github.io/autogen/stable//user-guide/agentchat-user-guide/memory.html) | chat history as memory, HandoffMessage, external memory | 对话即记忆与显式 handoff 的对比 |
| **CrewAI** | [CrewAI Memory](https://docs.crewai.com/concepts/memory) | short-term, long-term, entity, contextual memory | crew 级共享记忆模型 |
| **Google** | [A2A Protocol](https://a2a-protocol.org/latest/specification/) | Task object, artifacts, history, metadata | 跨 agent 委托的标准化结构化状态 |
| **Anthropic** | [MCP](https://modelcontextprotocol.io/) / [CA-MCP](https://arxiv.org/abs/2601.11595) | shared context store, tool collaboration | 工具级上下文共享与多 agent 协作 |
| **记忆基础设施** | [Hindsight](https://hindsight.vectorize.io/guides/2026/04/23/guide-why-tool-using-agents-need-shared-memory) / [Mem0](https://docs.mem0.ai/) / [Zep](https://help.getzep.com/) | shared memory, fact extraction, knowledge graph | 为什么 tool-using agents 需要共享记忆 |
| **跨工具 handoff** | [Relay](https://github.com/topics/agent-handoff) / [ai-sync](https://github.com/Oreolion/ai-sync) / [OpenClaw](https://docs.openclaw.ai/concepts/agent-workspace) | session bridge, handoff files, context discipline | 跨模型/跨工具上下文桥接 |
| **IDE agent** | [Cline Memory Bank](https://docs.cline.bot/best-practices/memory-bank) | projectbrief, activeContext, systemPatterns, progress | 项目活记忆的文件化 |
| **Coding agent** | [aider repo map](https://aider.chat/docs/repomap.html) | token budget, ranked symbols | 仓库结构摘要而非全文注入 |
| **学术研究** | [Context Engineering for AI Agents in OSS](https://arxiv.org/abs/2510.21413) | 466 个项目的 context files | context 工程还没有统一标准 |
| **学术研究** | [Evaluating AGENTS.md](https://arxiv.org/abs/2602.11988) | AGENTS.md 可能降低成功率、增加成本 | 对「把 AGENTS.md 全文当 brief」的警告 |
| **学术研究** | [Context as a Tool](https://arxiv.org/abs/2512.22087) | CAT, context workspace, milestone compression | 把 context maintenance 变成 agent 可调用的工具 |
| **学术研究** | [AgentFold](https://arxiv.org/abs/2510.24699) | proactive context folding | 动态折叠历史轨迹 |
| **学术研究** | [RCR-Router](https://arxiv.org/abs/2508.04903) | role-aware context routing | 静态/全上下文路由的问题 |
| **学术研究** | [ContextBench](https://arxiv.org/abs/2602.05892) | `<PATCH_CONTEXT>` extraction protocol | 上下文提取的标准化评估 |
| **安全研究** | [OWASP MCP10:2025](https://owasp.org/www-project-mcp-top-10/2025/MCP10-2025%E2%80%93ContextInjection%26OverSharing) / [Systematic Analysis of MCP Security](https://arxiv.org/abs/2508.12538) | context injection, over-sharing, ACI | 共享上下文的安全攻击面 |

---

## 2. 它们如何处理「给子 agent 什么上下文」

### 2.1 Claude Code AgentTool：sidechain transcript + summary-only return

Claude Code 的 [`AgentTool`](https://github.com/VILA-Lab/Dive-into-Claude-Code) 是原生子 agent 机制。它的关键设计是：

- 每个 subagent 写自己的 `.jsonl` transcript + `.meta.json`，**完整子对话不进入父上下文**。
- 只有 subagent 的**最终响应摘要**回到父对话。
- 三种隔离模式：worktree（文件系统隔离）、remote（云端）、in-process（仅对话隔离）。
- 子 agent 团队据说消耗约 **7× 标准 session token**，所以 summary-only return 是防止上下文爆炸的关键。

**对 AgentCall 的启示：** AgentCall 的 Claude Code PTY worker 本质上就是外部子 agent。如果 Codex 把每个 worker 的完整探索过程都保留在自己的上下文里，Codex 很快就会爆。应该让 daemon 做 projection/summary，Codex 只读 compact board/session。

### 2.2 OpenAI Agents SDK：用 `input_filter` 精确控制暴露面

OpenAI Agents SDK 的 [`handoff()`](https://github.com/openai/openai-agents-python) 提供了最精细的上下文控制参数：

| 参数 | 作用 |
|------|------|
| `input_type` | 结构化 handoff 元数据（如 `{"reason": "duplicate_charge", "priority": "high"}`） |
| `input_filter` | 在子 agent 看到之前转换 `HandoffInputData` |
| `nest_handoff_history` | 把之前对话折叠成一条 summary message |
| `on_handoff` | handoff 时执行回调（如预取数据、记录日志） |

内置 filter 如 `remove_all_tools` 可以移除工具调用痕迹，防止子 agent 把父 agent 的工具结果当成自己的。`nest_handoff_history` 在 v0.6 默认开启，v0.7 改为 opt-in，因为与 server-managed conversation 有冲突。

**对 AgentCall 的启示：** AgentCall 目前没有任何等价于 `input_filter` 的机制。Codex 构造的 prompt 里可能混入不必要的 Codex 自身对话历史、工具结果、情绪性探索内容。v7 需要一个编译阶段，把这些过滤掉。

### 2.3 PydanticAI：显式传播依赖与能力过滤

[PydanticAI](https://pydantic.dev/docs/ai/guides/multi-agent-applications/) 的多 agent 模式分 5 级，从 agent delegation 到 programmatic handoff。它的上下文控制强调：

- 显式传播 `deps=ctx.deps` 和 `usage=ctx.usage`。
- 用 **Capabilities**（如 `prepare_tools`）过滤子 agent 可见的工具。
- 用 `message_history` 过滤传给下一个 agent 的消息历史。
- guardrails：input/output guard、PII detection、secret redaction。

**对 AgentCall 的启示：** 不要把「上下文」只理解为文本。工具可见性、token 预算、依赖对象、敏感信息过滤都是 context exposure 的一部分。

### 2.4 LangGraph：checkpointer 与 store 的分层

[LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence) 把记忆明确分成两层：

| 机制 | 范围 | 用途 |
|------|------|------|
| **Checkpointers** | 单 thread | 图状态快照、对话连续性、故障恢复、time travel |
| **Store** | 跨 thread | 用户偏好、事实、共享知识 |

2025 年新增 cross-thread memory、semantic search、node-level caching、LangGraph Swarm/Supervisor。

**对 AgentCall 的启示：** AgentCall 的 `route_id` 对应 checkpointer（短期 route/session 状态），`target_workspace` 对应 store（长期项目记忆）。现在两者混在一起，v7 应该显式拆分。

### 2.5 AutoGen：chat history as memory，但生产需要外部记忆

[AutoGen](https://microsoft.github.io/autogen/stable//user-guide/agentchat-user-guide/memory.html) 的核心设计是「对话即记忆」：完整 message history 在 agent 间传递。但生产上这不够，所以提供了：

- `ListMemory`（短期）、`TeachableAgent`（长期向量库）、RAG memory、外部 Mem0/Zep/Hindsight 集成。
- v0.4+ 的 `HandoffMessage` 显式标记 agent 间切换。
- `Memory` protocol：`query / update_context / add / clear / close`。

关键教训：单 session 的 chat history 无法支撑跨 session 协作，**必须把记忆基础设施从 agent 编排中解耦**。

**对 AgentCall 的启示：** AgentCall 已经用 Rust daemon 做了事件存储，这是优势。但事件流不等于可检索记忆，需要把事件流蒸馏成结构化 facts。

### 2.6 CrewAI：crew 级共享记忆

[CrewAI Memory](https://docs.crewai.com/concepts/memory) 提供四级记忆：

- **Short-term**：当前 execution 的 ChromaDB RAG 上下文。
- **Long-term**：SQLite3 持久化跨 session 结果与决策。
- **Entity**：RAG-based 知识图谱，追踪人/地/概念。
- **Contextual**：融合上述层的集成视图。

限制：记忆在单个 crew 内共享，跨 crew 不自动共享；架构偏向单体多 agent，不适合分布式微服务。

**对 AgentCall 的启示：** AgentCall 的 worker 不是固定 crew，而是动态 route。需要一个类似 project memory 的层，但按 workspace + tag + path 索引，而不是按 crew。

### 2.7 Google A2A：用 Task object 做结构化 handoff

[A2A Protocol](https://a2a-protocol.org/latest/specification/) 的核心是 Task object：

| 字段 | 含义 |
|------|------|
| `id` | 唯一任务 ID |
| `sessionId` | 关联任务组 |
| `status` | TaskStatus（submitted/working/input-required/completed/failed/canceled/rejected） |
| `artifacts` | 结构化输出产物 |
| `history` | 交互消息历史 |
| `metadata` | 自定义元数据（trace ID、parent ID、路由信息） |
| `contextId` | 跨多 agent 工作流的上下文集合 ID |

A2A 强调 **structured over free-text**：用类型化的 skill 参数替代原始 prompt，降低 prompt injection 风险。

**对 AgentCall 的启示：** AgentCall 的 route 可以映射为 A2A Task，report 可以映射为 artifacts，session history 可以映射为 history，route metadata 可以映射为 metadata。这种结构化对齐能让 handoff 更安全、更可审计。

### 2.8 MCP / CA-MCP：共享上下文 store

[MCP](https://modelcontextprotocol.io/) 是 agent ↔ 工具/数据的垂直协议，本身 stateless，但 [Context-Aware MCP](https://arxiv.org/abs/2601.11595) 提出 Shared Context Store（SCS），让多 agent 工作流减少冗余、实现知识转移。问题在于 MCP 的 shared context 也引入了 [OWASP MCP10:2025](https://owasp.org/www-project-mcp-top-10/2025/MCP10-2025%E2%80%93ContextInjection%26OverSharing) 定义的 context injection & over-sharing 风险。

**对 AgentCall 的启示：** AgentCall 的 daemon 已经是状态权威，可以扮演 SCS 的角色，但必须做严格的 context isolation 和 TTL，否则会变成 ACI 攻击面。

### 2.9 Cline Memory Bank：项目活记忆的文件化

[Cline Memory Bank](https://docs.cline.bot/best-practices/memory-bank) 用 6 个 markdown 文件保存项目状态：

- `projectbrief.md`：核心需求与目标
- `productContext.md`：产品背景
- `activeContext.md`：当前工作焦点
- `systemPatterns.md`：架构与设计模式
- `techContext.md`：技术栈与约束
- `progress.md`：状态与里程碑

Cline 每次任务都要读全部 Memory Bank 文件，更新则在发现新模式或显式请求时触发。

**对 AgentCall 的启示：** 不要复制 Cline 的「全读」策略（那会增加上下文负担），但可以把 Memory Bank 文件作为 BriefCompiler 的输入源之一，由 compiler 决定本次 worker 需要哪几条。

### 2.10 aider repo map：用结构化摘要代替全文

[aider repo map](https://aider.chat/docs/repomap.html) 把整个仓库压缩成关键类、函数、签名和调用关系摘要，按 token budget 截断，只把最相关的部分送入上下文。

**对 AgentCall 的启示：** route 启动时不应只给 worker 一堆路径，应该给一份小型 repo map；非目标文件给符号签名，目标 `write_paths` 才给完整实现。

### 2.11 OpenClaw：HANDOFF.md 与 70/80% context 纪律

[OpenClaw](https://docs.openclaw.ai/concepts/agent-workspace) 的 handoff 机制非常工程化：

- `memory/handoffs/YYYY-MM-DD-HHMM-context-handoff.md` 保存任务交接状态。
- **70% 规则**：context >= 70% 时写 handoff 并停止；>= 80% 时立即停止。
- 文件大小限制：per-file 20K 字符，total bootstrap 150K 字符。
- `SHARED_CONTEXT.md` 用于跨 agent 协作。

**对 AgentCall 的启示：** AgentCall 应该引入 context budget 和 handoff 触发机制，而不是等 Claude Code 自己触发 compaction。

### 2.12 学术研究：Context as a Tool / AgentFold / RCR-Router / ContextBench

- **[Context as a Tool](https://arxiv.org/abs/2512.22087)**：长程 SWE agent 的 append-only context 会导致 context explosion 和 semantic drift。应该把 context maintenance 变成 agent 可调用的工具，context workspace 分三层：stable task semantics、condensed long-term memory、high-fidelity short-term interactions。
- **[AgentFold](https://arxiv.org/abs/2510.24699)**：通过 folding 操作在不同粒度压缩历史轨迹，按事实类型折叠（规则、工具链事实、blocker、文件级结论、用户决策）。
- **[RCR-Router](https://arxiv.org/abs/2508.04903)**：静态路由和全上下文路由都有问题，需要 role-aware context routing + structured memory。
- **[ContextBench](https://arxiv.org/abs/2602.05892)**：要求 agent 输出 `<PATCH_CONTEXT>`，精确到文件和行范围，才能评估上下文提取效果。
- **[Evaluating AGENTS.md](https://arxiv.org/abs/2602.11988)**：AGENTS.md 这类 repo-level context file 在多个设置下会降低任务成功率并增加 20%+ 成本，agent 会更尊重指令但也更广泛探索。

**对 AgentCall 的启示：** 不能把 AGENTS.md 全文当 brief；需要结构化提取、按任务过滤、按事实类型折叠。

---

## 3. 对 AgentCall 当前 brief/context 设计的反驳

基于 README/AGENTS.md 和已有 v7 研究报告，当前 AgentCall 的上下文模型存在以下可被外部证据反驳的假设：

### 反驳 1：「给 worker 一个 objective + reference_paths 就够了」

当前 `agentcall_route` 的核心参数是 `objective`、`workspace`、`write_paths`、`reference_paths`。但外部研究显示，成功的 handoff 需要更多结构化字段：

- A2A 的 Task object 有 status/artifacts/history/metadata/contextId。
- OpenAI Agents SDK 的 handoff 有 input_filter/input_type/nest_handoff_history。
- Claude Code AgentTool 有 isolation mode、tools allowlist、max turns。

**反驳：** 只给 objective 和路径，worker 必须自己重建「能做什么、不能做什么、交付什么、失败怎么报告」。这会导致探索漂移和重复劳动。

### 反驳 2：「reference_paths 是建议，worker 会自己判断」

当前 `reference_paths` 被定义为「read/context recommendations for the worker, not daemon-enforced read permissions」。

**反驳：** 这正是 simple-A 能成功而 simple-B/C 出现 `ended_no_report` 的原因之一（见 `report_ctx_metric_simple_hard_abc_2026-06-14.md`）。worker 可能过度探索 reference 中的文件，也可能忽略关键 reference。外部系统（aider、Cline、OpenClaw）都强调 context 必须被主动编译和预算化，而不是被动建议。

### 反驳 3：「Codex 直接构造 prompt 最高效」

当前设计让 Codex 直接写长 prompt 给 worker。

**反驳：** [Evaluating AGENTS.md](https://arxiv.org/abs/2602.11988) 和 [Context Engineering for AI Agents in OSS](https://arxiv.org/abs/2510.21413) 都说明，人写/模型写的长 context file 会增加成本甚至降低成功率。Codex 的当前对话里包含情绪、探索、反复讨论，这些噪声如果被复制到 worker prompt，会污染子 agent 的注意力。需要一个 BriefCompiler 做过滤。

### 反驳 4：「context 就是文本，塞进 prompt 就行」

当前模型把上下文当作要塞进 prompt 的文本。

**反驳：** PydanticAI 的 capabilities、OpenAI Agents SDK 的 input_filter、LangGraph 的 checkpointer/store 都说明，context 还包括：工具可见性、token 预算、依赖对象、状态快照、隔离模式、安全过滤。AgentCall 需要把这些非文本维度也纳入 handoff 设计。

### 反驳 5：「report 全文回流是保存知识的最佳方式」

当前 report 是一份 markdown 文档，accept 后存入文件系统。

**反驳：** [Reflexion](https://arxiv.org/abs/2303.11366) 和 AgentFold 都指出，应该回流的是**可行动事实**（规则、验证过的命令、被推翻的假设、用户决策），而不是整篇 narrative。否则 project memory 会越用越胖，新 worker 读不完。

### 反驳 6：「所有 worker 共享同一个 claude_workspace 就够了」

当前 Claude Code worker 的 cwd 固定为 `D:\guKimi`，route 的 `workspace` 只是任务目标目录。

**反驳：** Claude Code AgentTool 提供 worktree/remote/in-process 三种隔离模式；OpenClaw 用 git worktree；AutoGen/LangGraph 强调 workspace isolation。对于 coding worker，共享 cwd 会增加文件冲突和意外修改风险。v7 应该支持按 route 选择隔离模式。

### 反驳 7：「daemon 投影已经替代了原始 PTY 输出，所以 context 问题解决了」

v6.7 强调 projection-first，Codex 读 compact board/session 而不是 raw PTY。

**反驳：** projection 解决的是「状态可见性」，不是「handoff 时该暴露什么给子 agent」。前者是控制面，后者是 context engineering。两者都需要，但后者在当前设计中几乎是空白。

### 反驳 8：「context 没有安全风险，因为都是本地工具」

**反驳：** [OWASP MCP10:2025](https://owasp.org/www-project-mcp-top-10/2025/MCP10-2025%E2%80%93ContextInjection%26OverSharing)、[Systematic Analysis of MCP Security](https://arxiv.org/abs/2508.12538)、[Palo Alto Networks 的 MCP sampling 攻击研究](https://unit42.paloaltonetworks.com/model-context-protocol-attack-vectors/) 都表明，共享上下文是 agent cascading injection 和 data exfiltration 的关键攻击面。AgentCall 如果让多个 worker 共享 context store，必须做隔离、TTL、输出消毒。

---

## 4. 可落地的架构建议

### 4.1 总体架构：三层 + 一编译器

```text
┌─────────────────────────────────────────────┐
│  Layer 4: ReportSynthesis / DecisionLog     │  ← 多 report 合成、决策追溯
├─────────────────────────────────────────────┤
│  Layer 3: ProjectMemory                     │  ← 跨 route 的可复用事实
├─────────────────────────────────────────────┤
│  Layer 2: ContextPacket / WorkerBrief       │  ← 单次 route 的 handoff 契约
├─────────────────────────────────────────────┤
│  Layer 1: Worker Brief Compiler             │  ← 确定性编译管线
├─────────────────────────────────────────────┤
│  Layer 0: Isolation & Security              │  ← workspace lease + context isolation
└─────────────────────────────────────────────┘
```

### 4.2 Worker Brief Compiler 管线

建议把 `agentcall_route` 的内部实现改为确定性编译管线：

```text
Codex route request
  -> BriefInputs 收集（objective、workspace、paths、AGENTS.md 精简段、toolchain、accepted facts）
  -> ScopeFilter 过滤无关上下文（Codex 对话噪声、无关历史报告、过期计划）
  -> RuleExtractor 抽取任务相关硬规则（must_follow / must_not_do）
  -> FactSelector 选择可复用事实（已验证、已决策、已知坑点）
  -> RepoNavigator 生成小型 repo map
  -> ContractCompiler 写输出/失败契约
  -> BriefRenderer 输出 JSON + Markdown
  -> Claude Code PTY worker 读取 brief
```

每个阶段都要记录审计日志：选了什么、排除了什么、为什么。

### 4.3 ContextPacket / WorkerBrief schema

```json
{
  "brief_id": "brief-route-123",
  "route_id": "route-123",
  "worker_kind": "coding|report",
  "target_workspace": "E:\\Project\\AgentCall",
  "claude_cwd": "D:\\guKimi",
  "isolation_mode": "shared|exclusive|worktree",
  "objective": "...",
  "supervisor_intent": "...",
  "task_scope": {
    "write_paths": [],
    "reference_paths": [],
    "report_path": "..."
  },
  "must_follow": [
    "Only facts/constraints required for this task."
  ],
  "must_not_do": [
    "Task-specific prohibitions, not global noise."
  ],
  "relevant_facts": [
    {
      "kind": "toolchain|decision|known_issue|prior_report",
      "fact": "...",
      "source": "report/session/file",
      "confidence": "high|medium|low",
      "applies_to": ["path", "tag"]
    }
  ],
  "repo_navigation": {
    "important_paths": [],
    "symbols_or_modules": [],
    "do_not_scan_unless_needed": []
  },
  "output_contract": {
    "required_report": true,
    "changed_files_required": true,
    "tests_required": "if code changed"
  },
  "failure_contract": {
    "when_blocked": "write blocker report instead of retry loop",
    "permission_denied": "report exact command/path/reason"
  },
  "context_budget": {
    "budget_tokens": 8000,
    "estimated_tokens": 2400,
    "source_count": 5
  },
  "source_list": [
    {"path": "AGENTS.md", "included_sections": ["Worker Discipline"]},
    {"report": "report_ctx_metric_hard_c_model_2026-06-14.md", "facts": ["..."]}
  ]
}
```

输出路径：

```text
<target_workspace>/.agentcall/briefs/<route_id>.json
<target_workspace>/.agentcall/briefs/<route_id>.md
```

### 4.4 ProjectMemory：瘦事实回流

report accept 后，daemon 提取 3-10 条可复用事实：

```json
{
  "fact": "SQLite is the recommended RuntimeStore backend for live multi-worker use.",
  "source_report": "report_v6.7_demo_known_issues_and_v7_questions_2026-06-13.md",
  "applies_to": ["crates/agentcall-daemon", "store_backend"],
  "confidence": "high",
  "kind": "toolchain_fact",
  "expires": null
}
```

存储：`<target_workspace>/.agentcall/context/project_memory.ndjson`

索引：按 `workspace + tag + path + kind + confidence`。

写入规则：worker 不能直接写 project memory；只有 report accept 或 Codex 显式 decision 时由 daemon 写入。

### 4.5 DecisionLog：记录「为什么」

```text
<target_workspace>/.agentcall/context/decisions.ndjson
```

字段：decision_id, made_by, source_route_id, context, alternatives, chosen, rationale, confidence。

### 4.6 ReportSynthesis：多 worker 报告合成

当多个 report worker 审查同一主题时，daemon 生成 synthesis：

```textn<target_workspace>/.agentcall/context/synthesis/<synthesis_id>.md
<target_workspace>/.agentcall/context/synthesis/<synthesis_id>.json
```

Schema：source_routes, summary, findings, conflicts, risks, decisions, next_actions。

原则：synthesis 只呈现冲突和建议，不自动替代父层判断。

### 4.7 与 A2A / MCP 概念对齐

| AgentCall 概念 | A2A 概念 | MCP 概念 |
|----------------|----------|----------|
| route | Task | tool call |
| session | Task.status/history | session |
| report | Artifact | tool result |
| brief metadata | Task.metadata | context metadata |
| project memory | shared context | Shared Context Store |
| context_id | contextId | sessionId |

这种对齐不是为了实现 A2A/MCP，而是让 AgentCall 的 handoff 语义更清晰、更安全。

### 4.8 安全设计

- **Context Isolation**：coding worker 默认 exclusive workspace lease；report worker shared；高危操作可选 worktree。
- **TTL**：project memory facts 可设置 expires，自动清理过期事实。
- **Output Sanitization**：worker report 回流前扫描 secrets、credentials。
- **No Secrets in Handoff**：brief 中不得包含 daemon_token、API keys、passwords。
- **Audit Trail**：每份 brief 记录 source_list 和排除项，便于追踪「worker 为什么知道/不知道某件事」。

### 4.9 MCP / daemon API 建议

保持 MCP 工具面精简，建议只新增/扩展一个入口：

```text
agentcall_context(action=get|search|append|synthesize|decision)
```

daemon HTTP 可更细：

```text
GET  /api/context/routes/{route_id}
POST /api/context/routes/{route_id}/append
GET  /api/context/project
POST /api/context/project/append
GET  /api/context/search
POST /api/context/synthesize
POST /api/context/decision
```

Codex 默认路径仍是：

```text
board -> route -> session -> report -> context synthesis
```

### 4.10 MVP 切分

| 阶段 | 内容 | 验收标准 |
|------|------|----------|
| **P0** | WorkerBrief 文件 + BriefCompiler 管线 | route 时生成 `.agentcall/briefs/<route_id>.json/.md`；handoff prompt 只引用 brief；session summary 显示 brief path/source count/estimated tokens |
| **P1** | ProjectMemory fact extraction | report accept 后提取 3-10 条 reusable facts；新 worker 能引用前一轮 accepted report 的摘要 |
| **P2** | ReportSynthesis | 给定 2-6 个 report route，生成统一 synthesis，标记一致/冲突/建议 |
| **P3** | Repo map / context packing | 轻量版：目录树 + Rust/Python/TS 函数签名 + README/AGENTS/CHANGELOG 摘要 |
| **P4** | Security hardening | context isolation mode、TTL、secret scanning、audit trail |

---

## 5. 明确非目标

- 不做 worker-to-worker 聊天。
- 不做通用消息总线。
- 不复活 ACP。
- 不把所有历史事件塞进 prompt。
- 不让 worker 直接写 ProjectMemory。
- 不在 v7 MVP 引入完整向量 RAG。
- 不把共享上下文和 MCP transport 修复混成一个巨大版本。
- 不把 `D:\guKimi` 的 Claude cwd 当成任务上下文来源；任务上下文来自 `target_workspace`。
- 不让 brief compiler 替代 Codex 的任务拆分判断。
- 不把 AGENTS.md 全文自动注入每个 worker。

---

## 6. 风险与开放问题

1. **简单组 B/C 的 `ended_no_report` 异常**（见 `report_ctx_metric_simple_hard_abc_2026-06-14.md`）需要先修实验 harness 的 reliability，否则 v7 的 context 效果评估会被污染。
2. **MCP transport closed** 仍是 Codex 使用路径的卡点，如果 v7 目标是普通 Codex session 稳定使用 AgentCall，需要并行修 MCP 生命周期。
3. **brief compiler 的「相关性」判断**不能一上来就上向量库，先用确定性规则（路径匹配、tag 匹配、标题段落匹配），否则 v7 会被 RAG 复杂度吃掉。
4. **context isolation 与性能**：worktree 隔离会带来磁盘开销，需要按任务类型选择默认模式。
5. **安全与便利的平衡**：过度隔离会降低 worker 效率，过度共享会增加 ACI 风险，需要通过实验确定默认策略。
6. **与 frozen v6.2 plan 的关系**：v7 是新产品方向，不应修改 v6.2 plan；新证据应写入 reports 并在 v7 plan 中单独规划。

---

## 7. 最终判断

AgentCall v7 的 Context Exposure / Handoff 层应该解决三个根问题：

1. **父层重复解释项目背景** → 用 ProjectMemory 沉淀 accepted facts。
2. **worker 重复探索** → 用 WorkerBrief Compiler 生成带 repo map 和约束的最小简报。
3. **报告无法合成** → 用 ReportSynthesis 把多个 worker 输出变成父层可判断的视图。

最值得参考的不是某一个完整框架，而是它们的组合：

- Claude Code AgentTool 的 sidechain transcript 和隔离模式 → 防止父上下文爆炸。
- OpenAI Agents SDK 的 `input_filter` / `nest_handoff_history` → 精确控制上下文暴露。
- A2A 的 Task object → 结构化 handoff 语义。
- LangGraph 的 checkpointer/store 分层 → 短期 route 状态与长期项目记忆分离。
- Cline Memory Bank / aider repo map → 项目活记忆与结构化导航。
- OpenClaw 的 70/80% 规则 → context budget 纪律。
- OWASP/学术安全研究 → context isolation 与 ACI 防护。

如果 v7 只做一个东西，就做：

```text
agentcall_route -> daemon generates WorkerBrief -> Claude Code reads brief -> report accepted -> reusable facts extracted to ProjectMemory
```

这条线足够小，也最能降低 Codex 组织多 Agent 时的上下文压力，同时与 AgentCall 的 daemon-authority、projection-first、bounded-write 核心路线完全一致。

---

## 参考资料

- [Dive into Claude Code: The Design Space of Today's and Future AI Agent Systems](https://arxiv.org/abs/2604.14228)
- [Claude Code AgentTool / Subagent Research](https://github.com/VILA-Lab/Dive-into-Claude-Code)
- [OpenAI Agents SDK](https://github.com/openai/openai-agents-python)
- [PydanticAI Multi-Agent Patterns](https://pydantic.dev/docs/ai/guides/multi-agent-applications/)
- [LangGraph Persistence](https://docs.langchain.com/oss/python/langgraph/persistence)
- [AutoGen Memory](https://microsoft.github.io/autogen/stable//user-guide/agentchat-user-guide/memory.html)
- [CrewAI Memory](https://docs.crewai.com/concepts/memory)
- [Google A2A Protocol Specification](https://a2a-protocol.org/latest/specification/)
- [Context-Aware MCP (CA-MCP)](https://arxiv.org/abs/2601.11595)
- [Hindsight: Why Tool-Using Agents Need Shared Memory](https://hindsight.vectorize.io/guides/2026/04/23/guide-why-tool-using-agents-need-shared-memory)
- [Hindsight: Building Multi-Agent Systems with Shared Memory](https://hindsight.vectorize.io/guides/2026/04/21/guide-building-multi-agent-systems-with-shared-memory)
- [MemoryLake: Why Multi-Agent Teams Need Shared Memory](https://www.memorylake.ai/en/blogs/multi-agent-memory)
- [Cline Memory Bank](https://docs.cline.bot/best-practices/memory-bank)
- [aider Repository Map](https://aider.chat/docs/repomap.html)
- [OpenClaw Agent Workspace](https://docs.openclaw.ai/concepts/agent-workspace)
- [Loadout: Cross-agent context handoff: the four patterns that actually work](https://useloadout.com/blog/cross-agent-context-handoff)
- [LangChain: How and when to build multi-agent systems](https://www.langchain.com/blog/how-and-when-to-build-multi-agent-systems)
- [Anthropic: Effective Context Engineering for AI Agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
- [Context Engineering for AI Agents in Open-Source Software](https://arxiv.org/abs/2510.21413)
- [Evaluating AGENTS.md: Are Repository-Level Context Files Helpful for Coding Agents?](https://arxiv.org/abs/2602.11988)
- [Context as a Tool: Context Management for Long-Horizon SWE-Agents](https://arxiv.org/abs/2512.22087)
- [AgentFold: Long-Horizon Web Agents with Proactive Context Management](https://arxiv.org/abs/2510.24699)
- [RCR-Router: Efficient Role-Aware Context Routing for Multi-Agent LLM Systems with Structured Memory](https://arxiv.org/abs/2508.04903)
- [ContextBench: A Benchmark for Context Retrieval in Coding Agents](https://arxiv.org/abs/2602.05892)
- [OWASP MCP10:2025 – Context Injection & Over-Sharing](https://owasp.org/www-project-mcp-top-10/2025/MCP10-2025%E2%80%93ContextInjection%26OverSharing)
- [Systematic Analysis of MCP Security](https://arxiv.org/abs/2508.12538)
- [Palo Alto Networks: New Prompt Injection Attack Vectors Through MCP Sampling](https://unit42.paloaltonetworks.com/model-context-protocol-attack-vectors/)
- [Cross-agent context handoff: the four patterns that actually work](https://useloadout.com/blog/cross-agent-context-handoff)
- [Relay / agent-handoff GitHub topic](https://github.com/topics/agent-handoff)
- [ai-sync: Cross-platform AI agent synchronization](https://github.com/Oreolion/ai-sync)
- [Subagents: How to Run Parallelism Inside a Single Agent Session Without Poisoning the Parent](https://ranjankumar.in/subagents-parallelism-inside-session)
- [h5i: Shared Versioned Context for Claude Code & Codex](https://h5i.dev/)
