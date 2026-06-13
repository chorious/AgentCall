# AgentCall v7 共享上下文 / Context Exposure 研究：Agentic RAG、记忆系统与认知架构

日期：2026-06-14  
仓库：`E:\Project\AgentCall`  
报告目标：为 v7「WorkerBrief / Context Exposure」设计提供研究输入，不改动源码。

---

## 摘要

AgentCall 的 v7 核心问题不是「如何让 worker 检索到正确文档」（普通 RAG），而是：

> 当一个已经能自主调用工具、执行代码、读写文件的子 agent 被派去执行一项任务时，主控应该给它暴露什么样的协作语境？
> 这个语境要比 prompt 更丰富、比把整段会话/仓库全塞进去更精简，而且必须能被 daemon 显式构造、版本化与验收。

本报告围绕 **Agentic RAG、记忆系统、认知架构、上下文工程、自改进上下文、GraphRAG / 知识图谱记忆、RAG 评估** 七个方向做文献与工业实践扫描，提炼对 AgentCall 的启发，并指出哪些常见 RAG 做法不适合直接移植到 v7。

---

## 1. 关键概念

### 1.1 Agentic RAG：从"检索-生成管道"到"能行动的检索主体"

传统 RAG 是静态流水线：把文档切块、嵌入、检索、拼进 prompt。Agentic RAG 则把检索能力交给一个能推理、能规划、能调用工具、能维护记忆的 agent [Agentic RAG: A Survey](https://arxiv.org/html/2501.09136v4)。

它的四个能力层：

| 能力 | 含义 | AgentCall 对应映射 |
|---|---|---|
| 推理与规划 | 决定要不要检索、检索什么、检索几次 | worker 读到 brief 后决定读哪些文件 |
| 工具使用 | 搜索、API、数据库、代码执行 | Claude Code 的 Read/Grep/Bash/Agent 等工具 |
| 记忆机制 | 跨 turn、跨 session 保留状态 | daemon 的 SQLite store、session 投影、report |
| 自反思与适应 | 评估检索结果、失败后 replan | worker 的 report / supervisor review 闭环 |

**关键转变**：AgentCall worker 本身就是 Agentic RAG 的主体。所以 v7 不需要再做一个"RAG 层"让 worker 被动查询；需要设计的是**主控给 worker 的"任务语境对象"**，让 worker 的自主检索在正确边界内发生。

### 1.2 记忆四分法：工作记忆 / 情景记忆 / 语义记忆 / 程序记忆

CoALA（Cognitive Architectures for Language Agents）把 agent 记忆分为四类 [Cognitive Architectures for Language Agents](https://arxiv.org/html/2309.02427v3)：

| 类型 | 内容 | 生命周期 | v7 映射 |
|---|---|---|---|
| 工作记忆（Working） | 当前 context window 里的全部内容 | 单次推理 | worker 收到的 WorkerBrief + 运行时工具输出 |
| 情景记忆（Episodic） | 具体事件：上次跑了什么测试、哪份报告写过什么 | 按时间/任务索引 | per-route report、session 日志、accepted reports |
| 语义记忆（Semantic） | 事实与关系：项目结构、API 约定、模块依赖 | 长期、需 consolidation | `AGENTS.md`、`LLMTips.md`、代码知识图谱 |
| 程序记忆（Procedural） | 怎么做：测试命令、提交规范、review 流程 | 成功一次后固化 | `CLAUDE.md`、skill 文件、codex/claude hooks |

新研究 MNEMA 进一步提出"记忆原生架构"：agent 是持久记忆系统，LLM 只是被调用的服务 [MNEMA](https://zenodo.org/records/20010220)。这对 AgentCall 的启示是：**daemon 应该成为记忆的权威（state authority），而 worker 是临时被注入一段工作记忆的执行体**。

### 1.3 上下文工程：写、选、压、隔

LangChain 把 Context Engineering 总结为四个动作 [Context Engineering - LangChain](https://www.langchain.com/blog/context-engineering-for-agents)：

1. **Write**：把信息写到 context window 外（文件、向量库、图谱）。
2. **Select**：按任务把相关内容选入 context。
3. **Compress**： summarization / distillation，让窗口装得下。
4. **Isolate**：把上下文拆给不同子 agent，避免互相污染。

AgentCall v6.7 已经在做隔离（`coding` / `report` worker kinds、workspace lease）。v7 需要在 **Select** 和 **Compress** 上显式化：daemon 不应该只传一个 `objective` 字符串，而应该传一个**已经选好的、结构化的语境包**。

### 1.4 自改进上下文：Agentic Context Engineering (ACE)

斯坦福/UC Berkeley 的 ACE 框架把上下文演化拆成三个角色 [Agentic Context Engineering](https://arxiv.org/html/2510.04618v3)：

- **Generator**：产生推理轨迹。
- **Reflector**：从成功/失败中提炼洞察。
- **Curator**：把洞察整合成结构化上下文更新。

关键机制：

- **Delta updates**：局部修改上下文，而不是每次重写整个 prompt。
- **Grow-and-refine**：在扩展上下文与去冗余之间平衡。
- **Multi-epoch adaptation**：重复访问同类任务，逐步强化上下文。

对 AgentCall 的意义：v7 的 WorkerBrief 不应是每次重新生成的长 prompt，而应是**可增量更新的结构化对象**。上一次同类型 route 的 accepted report 应该能 delta 合并进新的 brief。

### 1.5 GraphRAG / 知识图谱记忆

Microsoft GraphRAG 的核心不是"更好的向量检索"，而是把文本库抽成显式知识图谱，支持两类查询 [Microsoft GraphRAG](https://arxiv.org/pdf/2404.16130)：

- **Local Search**：围绕实体做邻居遍历（"这份合同付款条款是什么？"）。
- **Global Search**：基于社区摘要做全局综合（"过去三个月项目报告的主主题是什么？"）。

2025-2026 的演进方向是 **Agentic GraphRAG**：通过 MCP 暴露 `search_memory` / `write_memory`，让 agent 自己读写图谱 [Building Agentic GraphRAG Systems](https://www.decodingai.com/p/agentic-graphrag)。

对 AgentCall：repo 级别的语义记忆用 GraphRAG/KG 很合适，但**不要把它当上下文 dump 进 prompt**。它应该是 worker 可调用的长期记忆层，按需检索。

### 1.6 RAG 评估：从答案质量到过程可追溯

RAG 评估已从"答案对不对"扩展到：

- **证据溯源**：claim 是否有可靠来源支撑。
- **执行溯源**：trace 是否完整、依赖是否清晰。
- **安全鲁棒**：记忆注入、后门、隐私泄露检测。
- **调试恢复**：失败能否定位、能否审计 [From Agent Traces to Trust](https://arxiv.org/html/2606.04990v1)。

专用 benchmark：

- **RAGCap-Bench**：测 agentic RAG 的规划能力（EMc/F1c） [RAGCap-Bench](https://arxiv.org/html/2510.13910v1)。
- **RAGAS / DeepEval / Arize**：生产级 faithfulness、context precision、recall [RAGAS](https://docs.ragas.io/)。

对 AgentCall：v7 评估 Context Exposure 的效果，不能只看最终报告好不好，而要看 **worker 是否用对了暴露给他的信息、是否少走弯路、是否能被审计**。

---

## 2. 对 AgentCall 的启发

### 2.1 把 worker 当作"有自主检索能力的执行体"，而不是"需要被喂全文的模型"

v7 的设计前提已经成立：worker 有 Read/Grep/Bash/Agent 等工具，能自己探索仓库。所以主控不需要替 worker 读完所有相关文件；主控需要做的是：

- 划定**任务边界**（做什么、不做什么）。
- 给出**高置信度背景**（项目约定、关键路径、已知风险）。
- 暴露**可查询的记忆入口**（report 索引、知识图谱、过往 route 摘要）。

### 2.2 daemon 应该是"记忆权威"，worker 是"临时工作记忆载体"

v6.7 已经把 SQLite store 作为推荐 backend。v7 可以在此基础上，让 daemon 维护：

| 记忆层级 | daemon 维护 | worker 获得 |
|---|---|---|
| 程序记忆 | `AGENTS.md`、hooks、skill 定义 | 直接注入 brief 的规则区 |
| 语义记忆 | repo 知识图谱、模块依赖、API 约定 | 按需 `search_memory` 调用 |
| 情景记忆 | 历史 routes、reports、失败模式 | 相关摘要或检索入口 |
| 工作记忆 | 当前 board/route/session 投影 | WorkerBrief 本体 |

### 2.3 WorkerBrief 是 Context Exposure 的核心对象

参考 hard-C smoke 报告中提到的 `ScopeFilter / RuleExtractor / FactSelector / RepoNavigator / ContractCompiler / BriefRenderer` 结构，WorkerBrief 至少应包含：

1. **Goal**：一句话任务目标。
2. **Scope**：做什么、不做什么、边界。
3. **Background**：项目/任务相关的事实（语义记忆精选）。
4. **Rules**：程序记忆（必须遵守的约束、禁止项）。
5. **References**：可检索的记忆入口（文件、reports、图谱查询）。
6. **Tools**：本次允许的工具子集。
7. **Artifacts**：期望产出与验收标准。
8. **Handoff**：结果如何回传、谁来验收。

### 2.4 用 GraphRAG 做 repo 语义记忆，但按需查询

对于 AgentCall 这种代码仓库，模块依赖、接口约定、控制流关系是结构化的。GraphRAG/KG 适合存储：

- crate/module 依赖
- MCP tool 与 daemon API 的调用关系
- hook 与配置项的绑定
- 历史重构决策（ADR）之间的依赖

但 worker 不应该收到整张图；它应该收到**图的查询入口**和若干预计算的社区摘要/关键实体。

### 2.5 评估 Context Exposure 要看"过程指标"，不能只看结果

v7 实验应该测：

- **方向漂移率**：worker 是否偏离主线探索无关文件。
- **工具命中率**：worker 读到的文件与任务的相关性。
- **context 利用率**：worker 实际使用了 brief 中多少信息。
- **report 置信度**：daemon 是否观察到 report write。
- **主管成本**：Codex 需要多少轮 review/correction 才能 accept。

---

## 3. 哪些常见 RAG 做法不适合

### 3.1 一次性向量检索 + prompt 拼接

传统 RAG 把检索结果直接塞进用户 prompt。AgentCall worker 已经能自己检索，所以**不需要主控替它检索后喂答案**。否则：

- 检索结果可能遗漏 worker 实际需要的细节。
- 浪费 token 在 worker 本会忽略的内容上。
- 剥夺 worker 的多跳推理能力。

### 3.2 把整个仓库或完整会话历史塞进 brief

"上下文越多越好"是误区。研究证实存在 **context rot**：无关信息会降低模型准确性 [Context Engineering for AI Coding](https://yrzhe.top/project/context-engineering-for-ai-coding-why-your-200k-token-window-is-lying-to-you)。AgentCall 的 v7 目标应该是**精准暴露**，而不是**全量暴露**。

### 3.3 用 RAG 替代任务特定的结构化 brief

RAG 擅长回答开放域事实问题，不擅长：

- 告诉 worker 当前任务边界。
- 传达"不要动 v6.2 plan"这类禁令。
- 指定输出格式和验收标准。

这些属于**程序记忆 + 任务契约**，必须显式写入 WorkerBrief，不能指望 retrieval 动态拼出来。

### 3.4 扁平化的记忆存储

把所有历史记录当向量检索，会丢失：

- 时间顺序与因果关系（情景记忆）。
- 结构化关系（语义记忆）。
- 行为规则（程序记忆）。

AgentCall 应该区分三类记忆，而不是一个统一的"memory DB"。

### 3.5 只读知识图谱 / 静态索引

如果 GraphRAG/KG 是只读的，就无法反映 worker 刚写完的 report、刚发现的失败模式。v7 应该允许**受控写回**：worker 完成任务后，把新事实/关系写进 daemon 维护的图谱或 report 索引。

### 3.6 忽略 context isolation 与 handoff

多 agent 场景下，把父 agent 的完整状态传给子 agent 会造成：

- 噪音污染。
- 敏感信息泄露。
- 子 agent 被父 agent 的中间失败误导。

AgentCall 已有 workspace lease 隔离；v7 需要在**上下文层面**也做显式 handoff，只传必要的任务包。

---

## 4. 应该如何定义信息暴露对象

### 4.1 信息暴露对象 = WorkerBrief（Worker Context Exposure Object）

建议 v7 把"给子 agent 暴露的协作语境"定义为一个**显式、结构化、可版本化**的对象，而不是一段自由文本 prompt。

命名为 **WorkerBrief**（或 Context Exposure Object / CEO），由 daemon 的 BriefCompiler 构造。

### 4.2 WorkerBrief 的推荐结构

```yaml
brief:
  meta:
    route_id: route-pty-1764853
    version: 1.0.0
    parent_session: ...
    target_workspace: E:\Project\AgentCall
    worker_kind: report | coding
    created_at: 2026-06-14T...
  goal: "研究 v7 Context Exposure 设计，输出中文报告"
  scope:
    in:
      - Agentic RAG 概念
      - 记忆系统分类
      - 对 AgentCall 的启发
      - 不适合的做法
      - 信息暴露对象定义
    out:
      - 不改源码
      - 不修改 v6.2 frozen plan
  background:
    project:
      - "AgentCall 是 Codex 监督 Claude Code PTY worker 的 Rust daemon"
      - "v6.7.1 为当前版本，v6.2 plan 冻结"
    task:
      - "v7 聚焦 Context Exposure / WorkerBrief"
  rules:
    must:
      - "只写报告，不改源码"
      - "最终报告写到指定 report_abs_path"
    must_not:
      - "修改 crates/、plugins/、scripts/ 下任何文件"
      - "重写 docs/v6.2-code-plan.md"
  references:
    files:
      - path: E:\Project\AgentCall\AGENTS.md
        reason: 项目使命与当前主线
    reports:
      - path: docs/reports/report_ctx_metric_simple_hard_abc_2026-06-14.md
        reason: A/B/C smoke 实验结论
    memory_queries:
      - "GraphRAG agentic memory"
      - "episodic semantic procedural memory agents"
  tools:
    allowed:
      - Read
      - Grep
      - WebSearch
      - WebFetch
      - Write
    denied:
      - Bash
      - Edit
  artifacts:
    primary:
      path: E:\Project\AgentCall\docs\reports\report_v7_context_exposure_agentic_rag_research_2026-06-14.md
      format: markdown
      acceptance:
        - "包含关键概念、启发、不适合做法、暴露对象定义"
        - "中文撰写"
        - "引用来源"
  handoff:
    to: supervisor
    on_complete: request_report
    acceptance_criteria:
      overall: high
      artifact: high
      daemon_write: high
      route_match: high
```

### 4.3 暴露对象应该支持的操作

| 操作 | 含义 | 由谁执行 |
|---|---|---|
| Compile | 从 board/route/session/repo 状态构造 brief | daemon / BriefCompiler |
| Render | 把 brief 渲染成 worker 可读文本 | BriefRenderer |
| Inject | 在 worker 启动时写入 context | daemon PTY handoff |
| Query | worker 运行时通过 tool 查询更多记忆 | worker -> daemon memory API |
| Update | 任务中因新发现而局部更新 brief | worker 请求 / daemon 批准 |
| Consolidate | 任务结束后把新事实写回长期记忆 | daemon（可能异步） |

### 4.4 暴露粒度原则

1. **程序记忆默认注入**：规则、禁令、流程必须显式在 brief 中，不依赖检索。
2. **语义记忆按需查询**：repo 结构、API 约定通过 GraphRAG/KG 查询入口暴露。
3. **情景记忆摘要化**：历史 route/report 只传摘要和检索入口，不传原始长文本。
4. **工作记忆最小化**：brief 本身控制在几千 token，让 worker 的 context window 主要留给推理和工具输出。
5. **可审计**：brief 版本、来源、更新记录都要能追溯到 daemon store。

### 4.5 对 v7 实现的建议

- 在 daemon 中新增 `BriefCompiler`：输入 route + board projection + selected memory，输出 `WorkerBrief`。
- `WorkerBrief` 先以 JSON/YAML 结构化存储，再渲染为 Claude/Codex 可读文本。
- repo 语义记忆用 GraphRAG/KG 维护，但**只在 brief 中暴露查询入口**，不把图谱 dump 进 context。
- 情景记忆用 report 索引 + 自动摘要，worker 可通过 `search_reports` 调用。
- 程序记忆来源固定为 `AGENTS.md`、`CLAUDE.md`、skill 文件、hook 规则。
- 评估指标从"report 好不好"扩展到"worker 使用了多少 brief 信息、走了多少冤枉路"。

---

## 5. 结论

AgentCall v7 的 Context Exposure 不是"给 worker 做 RAG"，而是"**把一个能自主行动的子 agent 放到正确的协作语境里**"。

关键判断：

1. Worker 已经有检索能力，主控应提供**边界、规则、记忆入口**，而不是替它检索。
2. 记忆应分层：程序记忆显式注入、语义记忆按需查询、情景记忆摘要化、工作记忆最小化。
3. WorkerBrief 是核心信息暴露对象，应结构化、可版本化、可审计。
4. GraphRAG/KG 适合做 repo 语义记忆，但必须作为可查询层，而非上下文 dump。
5. 评估要从结果质量扩展到过程可追溯性、context 利用率、主管成本。

下一步建议：基于本报告结构，设计 `WorkerBrief` schema 与 `BriefCompiler` 的最小可行实现，并在 A/B/C 实验框架下测量不同暴露策略对 worker 方向漂移率和 report 质量的影响。

---

## 参考来源

- [Agentic Retrieval-Augmented Generation: A Survey on Agentic RAG](https://arxiv.org/html/2501.09136v4)
- [Retrieval Augmented Generation Evaluation in the Era of Large Language Models: A Comprehensive Survey](https://arxiv.org/html/2504.14891v1)
- [RAGCap-Bench: Benchmarking Capabilities of LLMs in Agentic Retrieval Augmented Generation Systems](https://arxiv.org/html/2510.13910v1)
- [From Agent Traces to Trust: Evidence Tracing and Execution Provenance in LLM Agents](https://arxiv.org/html/2606.04990v1)
- [Cognitive Architectures for Language Agents](https://arxiv.org/html/2309.02427v3)
- [MNEMA: A Memory-Native Episodic-Semantic Architecture for Persistent LLM Agents](https://zenodo.org/records/20010220)
- [Agentic Context Engineering: Evolving Contexts for Self-Improving Language Models](https://arxiv.org/html/2510.04618v3)
- [Context Engineering for Agents - LangChain](https://www.langchain.com/blog/context-engineering-for-agents)
- [Effective context engineering for AI agents - Anthropic](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
- [Context Engineering for AI Coding: Why Your 200K Token Window Is Lying to You](https://yrzhe.top/project/context-engineering-for-ai-coding-why-your-200k-token-window-is-lying-to-you)
- [Microsoft GraphRAG](https://arxiv.org/pdf/2404.16130)
- [Building Agentic GraphRAG Systems](https://www.decodingai.com/p/agentic-graphrag)
- [Comparing Memory Systems for LLM Agents: Vector, Graph, and Event Logs](https://www.marktechpost.com/2025/11/10/comparing-memory-systems-for-llm-agents-vector-graph-and-event-logs/)
- [MemGPT: Towards LLMs as Operating Systems](https://arxiv.org/pdf/2310.08560)
- [Best practices for Claude Code subagents - PubNub](https://www.pubnub.com/blog/best-practices-for-claude-code-sub-agents/)
- [Subagents & Context Isolation - ClaudeWorld](https://claude-world.com/tutorials/s04-subagents-and-context-isolation/)
- [Evaluating AGENTS.md: Are Repository-Level Context Files Helpful](https://arxiv.org/html/2602.11988v1)
- [AI Memory System vs RAG: Differences, Tradeoffs, and Use Cases](https://atlan.com/know/ai-memory-system-vs-rag/)
- [Agent Memory Architectures: Patterns and Trade-offs](https://atlan.com/know/agent-memory-architectures/)
- [Self-Improving AI Agents: The 2026 Guide](https://o-mega.ai/articles/self-improving-ai-agents-the-2026-guide)
- [AutoContext: Instance-Level Context Learning for LLM Agents](https://arxiv.org/html/2510.02369v3)
