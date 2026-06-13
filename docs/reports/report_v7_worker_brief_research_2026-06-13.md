# AgentCall v7 Worker Brief Compiler 研究报告

日期：2026-06-13
背景：用户指出“共享上下文”这个说法容易带偏。AgentCall 的真实目标不是让多个 agent 共享记忆，而是让 Codex 作为主管，把自己的复杂上下文编译成 Claude Code worker 能执行的短任务简报，避免 Codex 每次手写大段 prompt，也避免把无用上下文倒给 Claude Code。

## 1. 重新定义 v7 问题

AgentCall 的产品核心是：

```text
Codex supervisor
  -> 编译任务简报
  -> Claude Code PTY worker 执行
  -> worker report
  -> Codex 验收 / 合并 / 再次编译
```

所以 v7 的问题不是：

```text
多个 agent 如何共享长期记忆？
```

而是：

```text
Codex 如何把“当前对话 + 项目规则 + 历史报告 + 工具状态 + 用户意图”
编译成“Claude Code 这一次任务真正需要知道的最小工作简报”？
```

这个东西更像：

- Worker Brief Compiler
- Context Firewall
- Supervisor Distillation
- Task Handoff Contract

不应该首先被设计成通用 Memory/RAG 系统。

## 2. 搜索到的关键论文和结论

### 2.1 AGENTS.md 不一定有帮助

论文：[Evaluating AGENTS.md: Are Repository-Level Context Files Helpful for Coding Agents?](https://arxiv.org/abs/2602.11988)

核心发现：

- 研究对象是 coding agents 使用 repo-level context files，例如 `AGENTS.md`。
- 论文发现这类上下文文件在多个设置下倾向于降低任务成功率，同时让推理成本增加超过 20%。
- 行为上，agent 会更尊重这些指令，但也会更广泛探索、跑更多测试、遍历更多文件。
- 结论是：人写的 context file 应该只保留最小必要要求。

对 AgentCall 的含义：

- 不能把 `AGENTS.md` 全文当作 worker brief 的固定前缀。
- v7 要做的是从 `AGENTS.md` 抽取“本任务必须遵守”的几条硬规则，而不是完整注入。
- brief 的成功标准不是“信息多”，而是“无关要求少”。

### 2.2 Context Engineering 目前还没有稳定结构

论文：[Context Engineering for AI Agents in Open-Source Software](https://arxiv.org/abs/2510.21413)

核心发现：

- 研究 466 个开源项目中的 agent context files。
- 上下文文件包含项目结构、构建测试、代码风格、工作流等内容。
- 内容结构并没有成熟标准，呈现方式差异很大：描述式、规定式、禁止式、解释式、条件式都有。
- 论文认为这是一个值得研究的真实 prompt/context engineering 现场。

对 AgentCall 的含义：

- 不能期待开源社区已经给出一个完美的 Worker Brief 模板。
- AgentCall 要做的是把混乱的 repo instructions 结构化为自己的 brief schema。
- brief schema 必须是 daemon 可验证的，而不是纯 Markdown。

### 2.3 Agentic AI Coding Tools 的配置生态

论文：[Configuring Agentic AI Coding Tools: An Exploratory Study](https://arxiv.org/abs/2602.14690)

核心发现：

- 研究 Claude Code、GitHub Copilot、Cursor、Gemini、Codex 等工具的配置机制。
- Context files 是目前最常见机制，AGENTS.md 正在成为跨工具标准。
- Skills 和 Subagents 等高级机制采用度还很浅，很多只是静态说明而不是可执行 workflow。
- Claude Code 用户使用的配置机制最丰富。

对 AgentCall 的含义：

- 只靠让项目维护更多 `AGENTS.md` / skills 文档，不能解决 Codex -> Claude handoff。
- AgentCall 的优势应该在“运行时编译 brief”，而不是要求用户预先写完所有静态配置。
- skill 可以作为 brief 的素材，但不能替代 brief compiler。

### 2.4 Context as a Tool

论文：[Context as a Tool: Context Management for Long-Horizon SWE-Agents](https://arxiv.org/abs/2512.22087)

核心发现：

- 长程 SWE agents 的 append-only context 和被动压缩会导致 context explosion、semantic drift、推理退化。
- 论文提出 CAT，把 context maintenance 变成 agent 可调用工具。
- 结构化 context workspace 包含：
  - stable task semantics
  - condensed long-term memory
  - high-fidelity short-term interactions
- 通过在里程碑处主动压缩历史轨迹，保持有界上下文。

对 AgentCall 的含义：

- 我们不一定要让 Claude 自己管理 context，但 Codex/daemon 应该把 brief 看成一个可构建、可更新、可检查的 workspace。
- Worker Brief 至少要分三层：
  1. 稳定任务语义：目标、边界、输出契约。
  2. 压缩历史事实：上轮报告里和本任务相关的结论。
  3. 高保真短期信息：当前 route/session/report 状态。
- 不能把历史日志直接 append 给下一个 worker。

### 2.5 AgentFold：主动折叠上下文

论文：[AgentFold: Long-Horizon Web Agents with Proactive Context Management](https://arxiv.org/abs/2510.24699)

核心发现：

- ReAct 类 agent 容易积累 noisy raw history。
- 固定地总结完整历史又容易不可逆地丢掉关键细节。
- AgentFold 把 context 当成动态工作区，通过 folding 操作在不同粒度压缩历史轨迹。

对 AgentCall 的含义：

- report accept 之后，不应该只做一份大摘要。
- 应该按事实类型折叠：
  - 可复用规则
  - 已验证工具链事实
  - 已知 blocker
  - 文件/模块级结论
  - 用户决策
- 不同任务 brief 应选择不同粒度，不是读同一份全局 summary。

### 2.6 MetaGPT：SOP 比自由聊天更重要

论文：[MetaGPT: Meta Programming for A Multi-Agent Collaborative Framework](https://arxiv.org/abs/2308.00352)

核心发现：

- MetaGPT 把人类工作流编码成 SOP prompt sequences。
- 用 assembly line paradigm 给不同角色分配任务，并要求中间结果被验证。
- 目标是减少 naive chaining 导致的逻辑不一致和级联幻觉。

对 AgentCall 的含义：

- Worker Brief 不应该是自由 prompt，而应该按 worker kind / task kind 套模板。
- AgentCall 的 worker kind 目前只有 `coding` 和 `report`，这是好事。
- v7 不应该引入十几个角色，而是先定义两类 brief：
  - `coding_brief`
  - `report_brief`

### 2.7 SWE-agent 与 Claude Code：接口设计比模型提示更关键

论文：

- [SWE-agent: Agent-Computer Interfaces Enable Automated Software Engineering](https://arxiv.org/abs/2405.15793)
- [Dive into Claude Code: The Design Space of Today's and Future AI Agent Systems](https://arxiv.org/abs/2604.14228)

核心发现：

- SWE-agent 强调 Agent-Computer Interface 对软件工程 agent 的行为影响很大。
- Claude Code 设计报告指出，核心 loop 很简单，大量复杂度在权限系统、上下文 compaction、hooks、skills、subagents、session storage 等周边系统。

对 AgentCall 的含义：

- v7 的重点不是“写更聪明的 prompt”，而是给 Claude Code 一个更好的任务接口。
- Worker Brief 应该成为 AgentCall 的 ACI 层：告诉 worker 怎么开始、能碰什么、何时报告、如何失败。
- Brief 必须和 daemon 权限、lease、report evidence 对齐，否则只是漂亮文本。

### 2.8 Reflexion：只回流可行动反思

论文：[Reflexion: Language Agents with Verbal Reinforcement Learning](https://arxiv.org/abs/2303.11366)

核心发现：

- Reflexion 让 agent 从反馈信号生成语言反思，存入 episodic memory buffer，用于后续尝试。
- 关键不是存完整轨迹，而是存能改进行为的 verbal feedback。

对 AgentCall 的含义：

- report accept 后回流的不是整篇报告，而是可行动事实：
  - 下次不要做什么。
  - 哪个路径/命令已验证。
  - 哪个假设被推翻。
  - 哪个用户偏好/工程规则应保留。
- 这类事实才应该进入下一次 worker brief。

## 3. GitHub / 工程项目可借鉴点

### 3.1 aider

仓库：[Aider-AI/aider](https://github.com/Aider-AI/aider)
机制：[Repository Map](https://aider.chat/docs/repomap.html)

可借鉴：

- 用 repo map 提供项目结构导航，而不是注入完整文件。
- token budget 是一等参数。
- 当前编辑/目标文件影响 repo map 排序。

AgentCall 落点：

- brief compiler 根据 `write_paths` / `reference_paths` 生成一个小 repo map。
- 只给 Claude Code 路标，让它自己读需要的文件。

### 3.2 Cline / Roo-Code

机制：

- Plan/Act 模式
- Memory Bank
- new_task / Boomerang task
- task todo list

可借鉴：

- handoff 是一个明确子任务，不是复制父层对话。
- Memory Bank 是项目级恢复材料，但它仍然需要任务级筛选。

AgentCall 落点：

- 不复制 Cline 的完整 Memory Bank。
- 只把 Memory Bank 视为 brief 的候选输入源。
- brief compiler 负责决定本次 worker 需要哪几条。

### 3.3 OpenHands

论文：[OpenHands Software Agent SDK](https://arxiv.org/abs/2511.03690)

可借鉴：

- execution lifecycle、sandbox、REST/WebSocket、事件流。
- memory management 是 SDK 可扩展组件。

AgentCall 落点：

- AgentCall 不必复制 OpenHands 平台。
- 只借“事件作为事实来源，projection 给上层消费”的思路。

### 3.4 MetaGPT / ChatDev

可借鉴：

- SOP/流水线能降低自由对话的漂移。
- 中间产物必须被验证。

AgentCall 落点：

- route 不是“把一个大目标丢给 Claude”。
- route 应先转成 `coding_brief` 或 `report_brief`，并包含验收契约。

## 4. Worker Brief 的建议 schema

v7 最小对象应该叫 `WorkerBrief`，而不是 `SharedMemory`。

```json
{
  "brief_id": "brief-route-123",
  "route_id": "route-123",
  "worker_kind": "coding|report",
  "target_workspace": "E:\\Project\\...",
  "claude_cwd": "D:\\guKimi",
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
      "confidence": "high|medium|low"
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
  }
}
```

并生成一个 worker 可读的 Markdown 版本：

```text
.agentcall/briefs/<route_id>.md
.agentcall/briefs/<route_id>.json
```

## 5. 编译管线

建议 v7 的核心不是 RAG，而是一条确定性的编译管线：

```text
Codex route request
  -> BriefInputs 收集
  -> ScopeFilter 过滤无关上下文
  -> RuleExtractor 抽取任务相关硬规则
  -> FactSelector 选择可复用事实
  -> RepoNavigator 生成小型路径/符号导航
  -> ContractCompiler 写输出/失败契约
  -> BriefRenderer 输出 JSON + Markdown
  -> Claude Code PTY worker 读取 brief
```

### 5.1 BriefInputs

输入源：

- route objective
- write_paths / reference_paths
- AGENTS.md / README / CHANGELOG 中的任务相关片段
- accepted reports 的可复用事实
- daemon projection：workspace、leases、policy、report path
- toolchain snapshot

### 5.2 ScopeFilter

负责删除：

- Codex 当前对话中的情绪/探索/反复讨论。
- 与本任务无关的历史报告。
- 全局 AGENTS.md 中不适用的泛化要求。
- 旧版本计划、已归档争论。

### 5.3 RuleExtractor

把自然语言规则转成 brief 条目：

- `must_follow`
- `must_not_do`
- `validation`
- `permission_boundary`

它不需要全自动完美，MVP 可先用规则表和标题匹配。

### 5.4 FactSelector

只选三种事实：

- 已验证事实：测试命令、编译命令、路径规则。
- 已决策事实：用户明确拍板的产品方向。
- 已知坑点：重复出现的 blocker / policy deny / encoding / MCP drift。

不选：

- 未验收的 worker 自述。
- 未经 daemon evidence 的成功声明。
- 已过期版本计划。

### 5.5 ContractCompiler

每个 brief 必须带：

- 成功时交付什么。
- 失败时报告什么。
- 什么时候不要继续重试。
- 哪些文件可写。
- 哪些操作需要停下来问 supervisor。

这一步是 AgentCall 和普通 prompt 最不同的地方。

## 6. 关键工程复杂性

### 6.1 “相关性”不能一开始就上向量库

MVP 应先做 deterministic：

- 路径匹配
- tag 匹配
- report accept 后提取的 structured facts
- AGENTS.md 标题段落匹配

向量检索可以后置。否则 v7 会被 RAG 复杂度吃掉。

### 6.2 Brief 需要可审计

每份 brief 都要保存：

- 它用了哪些 source。
- 哪些 source 被排除。
- token/字符预算。
- 为什么选择这些 relevant facts。

否则 Codex 和用户没法判断 Claude Code 为什么“知道/不知道”某件事。

### 6.3 Brief 不能太聪明

不要让 compiler 自己做产品决策。它只做：

- 选择上下文。
- 格式化任务。
- 编译约束。
- 加输出契约。

真正的任务拆分和验收仍是 Codex 的职责。

### 6.4 Report 回流必须瘦

report accept 后回流的不是全文，而是：

```json
{
  "fact": "...",
  "source_report": "...",
  "applies_to": ["path/tag/task-kind"],
  "confidence": "high",
  "expires": null
}
```

这可以避免“越用越胖”。

## 7. v7 推荐 MVP

### P0：WorkerBrief 文件

- route 时生成 `.agentcall/briefs/<route_id>.json` 和 `.md`。
- handoff prompt 只引用 brief。
- session summary 显示 brief path/source count/estimated tokens。

### P1：BriefInputs + ScopeFilter

- 收集 objective、workspace、paths、AGENTS.md 精简段、toolchain、prior accepted facts。
- 丢弃 Codex 对话噪声和无关历史。

### P2：AcceptedFactStore

- report accept 时，允许 Codex/daemon 提取 3-10 条 reusable facts。
- facts 按 `workspace + tag + path + confidence` 索引。
- 下一次 brief 只按 path/tag 选相关 facts。

### P3：Brief Evaluation

每次 route 记录：

- brief size
- source count
- worker 是否读了 brief
- worker 是否偏离 brief
- report 是否满足 contract

这样 v7 可以真的迭代，而不是凭感觉。

## 8. 明确非目标

- 不做完整 shared memory。
- 不做 worker-to-worker chat。
- 不做大规模向量 RAG。
- 不把 AGENTS.md 全文自动注入每个 worker。
- 不让 Claude Code 看 Codex 全部对话上下文。
- 不让 worker 自己决定哪些历史报告重要。
- 不用 brief compiler 替代 Codex 的任务拆分判断。

## 9. 最终判断

这次搜索反而把方向收窄了：

**AgentCall v7 应该实现一个可审计、可预算、可复用的 Worker Brief Compiler。**

它的输入是 Codex 的复杂监督语境；输出是 Claude Code worker 的最小任务简报。
它不是 memory system 的第一版，也不是 RAG 系统的第一版。

最关键的设计原则：

1. **少即是多**：AGENTS.md 这类 repo context 已有研究显示会增加成本甚至降低成功率，必须裁剪。
2. **任务相关优先**：只给当前 worker 必须知道的规则、事实、路径、契约。
3. **契约比背景重要**：worker 最需要知道的是交付什么、不能做什么、失败怎么报告。
4. **报告回流要瘦**：只回流可复用事实，不回流整篇叙事。
5. **brief 可审计**：每份 brief 必须记录来源、排除项和预算。

如果 v7 只做一个东西，就做：

```text
agentcall_route -> daemon generates WorkerBrief -> Claude Code reads brief -> report accepted -> reusable facts extracted
```

这正对 AgentCall 的根本目的：让 Codex 更轻松、更稳定地指挥 Claude Code 集群工作。
