# AgentCall v7 共享上下文研究报告

日期：2026-06-13
任务：研究 AgentCall 第二层“共享上下文语境”问题，并参考 GitHub / 公开项目中做得较好的方案。
执行方式：通过 AgentCall 并发拉起 5 个 report worker，分别研究 coding agents、IDE agents、memory frameworks、context packing，并由本报告综合归纳。

## 1. 本轮执行结果

本轮共启动 5 个 AgentCall report worker：

| Worker | 主题 | 报告状态 |
| --- | --- | --- |
| `ctx-v7-coding-agents-a` | OpenHands / SWE-agent / aider / Continue | accepted，高置信度 |
| `ctx-v7-coding-agents-b` | Cline / Roo-Code / Cody / Tabby / Bloop | accepted，高置信度 |
| `ctx-v7-memory-frameworks` | AutoGen / LangGraph / LangMem / CrewAI / Letta | accepted，高置信度 |
| `ctx-v7-context-packing` | repo-map / Repomix / Context7 / AGENTS.md | accepted，高置信度 |
| `ctx-v7-agentcall-synthesis` | AgentCall v7 方案归纳 | accepted，高置信度 |

收尾状态：所有 worker 已 stop，board 为 `0 attention / 0 runtime_workers`。

额外核验的关键公开资料：

- aider repository map：`https://aider.chat/docs/repomap.html`
- Cline Memory Bank：`https://docs.cline.bot/best-practices/memory-bank`
- LangGraph persistence：`https://docs.langchain.com/oss/python/langgraph/persistence`
- Repomix output formats：`https://repomix.com/guide/output`

## 2. 核心结论

共享上下文不是“让所有 worker 共享一大段聊天记录”。更成熟的系统通常做三件事：

1. **把上下文资产化**：repo map、memory bank、checkpoint、report、decision log 都是可引用、可索引、可版本化的对象。
2. **把上下文分层**：当前任务需要的热上下文、项目长期记忆、原始日志/报告附件必须分开，不应一股脑注入 prompt。
3. **让 supervisor 合成，而不是让 worker 互聊**：worker 的输出通过报告、事实、决策日志回流到统一 store，由父层或 daemon 合成视图。

对 AgentCall 来说，这正好吻合现有路线：Codex 是 supervisor，Rust daemon 是状态权威，Claude Code PTY worker 只做有边界的执行或报告。v7 不应该做 worker-to-worker chat，也不应该复活 ACP；应该做 **daemon-owned shared context layer**。

## 3. 外部项目给出的模式

### 3.1 aider：Repository Map

aider 的 repo map 会把整个仓库压成关键类、函数、签名和调用关系摘要，并按 token budget 截断。公开文档明确说它会随每次请求发送 repo map，且只选最相关的部分进入上下文。

对 AgentCall 的启发：

- route 启动时不应只给 worker 一堆路径，应给一份小型 repo map。
- 非目标文件优先给符号签名，目标 `write_paths` 才给完整实现。
- `reference_paths` 应从“建议路径”升级为 context packet 的输入源。

### 3.2 Cline / Roo-Code：Memory Bank 与任务状态

Cline 的 Memory Bank 是项目内的结构化 markdown 文件：`projectbrief.md`、`activeContext.md`、`systemPatterns.md`、`techContext.md`、`progress.md` 等。它解决的不是“模型更聪明”，而是“新会话从哪里恢复项目背景”。

对 AgentCall 的启发：

- AgentCall 需要自己的 `.agentcall/context/`，把 accepted reports、当前 active context、决策记录沉淀下来。
- worker 启动时引用 context packet，而不是每次让 Codex 重新解释项目历史。
- 多 worker 并发后，应该有 report synthesis，把多个报告合成一份父层可读结论。

### 3.3 LangGraph / LangMem：Thread 与 Store 分离

LangGraph 的 persistence 文档把 memory 分为两层：

- checkpointer：保存单个 thread 的图状态快照。
- store：保存跨 thread 的长期数据，例如事实、用户偏好、共享知识。

对 AgentCall 的启发：

- route/session 级上下文与 project 级记忆必须分开。
- `route_id` 对应短期 context packet；`target_workspace` 对应长期 project memory。
- daemon 重启后可以从事件和 context store 恢复“发生了什么”，但不需要恢复 Claude PTY 进程。

### 3.4 Repomix / Gitingest / Context7：打包、索引、版本化文档

Repomix 把仓库输出为 XML/Markdown/JSON/Plain 等 LLM-friendly 格式，且支持压缩和结构化输出。Context7 的重点是版本正确的第三方库文档。

对 AgentCall 的启发：

- context packet 可以采用 JSON + Markdown 双视图：JSON 供 daemon/MCP，Markdown 供 worker 阅读。
- 外部依赖文档应记录版本，不要让 worker 自己随便搜索到过时文档。
- token 预算、来源列表、压缩策略应该进入 session summary，而不是藏在 prompt 里。

### 3.5 OpenHands / SWE-agent：事件流与 trajectory

OpenHands 更接近 event-sourcing；SWE-agent 更重视 trajectory 文件和可复现执行记录。二者都提示一个方向：长任务不能只留下自然语言总结，必须有可审计轨迹。

对 AgentCall 的启发：

- worker report 需要附带结构化证据索引：读了什么、改了什么、测试了什么、遇到什么 blocker。
- `events`、`report_accept`、`decision_log` 应当能串成一次 route 的 trajectory。
- 原始 PTY/TUI 输出应作为 cold artifact，不进入默认 board/session。

## 4. AgentCall v7 建议对象模型

### 4.1 ContextPacket

每个 route 启动时由 daemon 生成：

```text
<target_workspace>/.agentcall/context/routes/<route_id>.json
<target_workspace>/.agentcall/context/routes/<route_id>.md
```

建议字段：

- `packet_id`
- `route_id`
- `target_workspace`
- `claude_cwd`
- `worker_kind`
- `objective`
- `write_paths`
- `reference_paths`
- `report_path`
- `ground_rules`
- `repo_map`
- `relevant_reports`
- `decisions`
- `toolchain`
- `token_budget`
- `source_list`

原则：handoff prompt 只引用这个 packet 路径和短摘要，不再粘贴大量背景。

### 4.2 ProjectMemory

项目级长期记忆：

```text
<target_workspace>/.agentcall/context/project_memory.ndjson
```

条目类型：

- `report_summary`
- `decision`
- `risk`
- `error_pattern`
- `toolchain_fact`
- `architecture_fact`
- `open_question`

写入规则：

- report accept 后由 daemon 抽取摘要写入。
- Codex 可显式写 decision。
- worker 不能随意污染 project memory；默认只写 route context，项目级写入需要 daemon 规则。

### 4.3 DecisionLog

```text
<target_workspace>/.agentcall/context/decisions.ndjson
```

用于记录“为什么这么做”，而不是只记录“做了什么”。字段应包括：

- `decision_id`
- `made_by`
- `source_route_id`
- `context`
- `alternatives`
- `chosen`
- `rationale`
- `confidence`

### 4.4 ReportSynthesis

多个 worker 报告合成产物：

```text
<target_workspace>/.agentcall/context/synthesis/<synthesis_id>.md
<target_workspace>/.agentcall/context/synthesis/<synthesis_id>.json
```

schema：

- `source_routes`
- `summary`
- `findings`
- `conflicts`
- `risks`
- `decisions`
- `next_actions`

重要原则：synthesis 只能呈现冲突和建议，不自动替代父层判断。

## 5. MCP / daemon 形态建议

保持 MCP 工具面精简。不要为共享上下文新增一堆工具；建议只新增或扩展一个入口：

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

但 Codex 默认路径仍应是：

```text
board -> route -> session -> report -> context synthesis
```

不是让 Codex 自己拼上下文、自己维护记忆文件。

## 6. MVP 切分

### P0：ContextPacket

- route 创建时自动生成 JSON/Markdown context packet。
- prompt 只引用 packet 文件路径。
- session summary 展示 `context_packet_id`、`context_sources`、`context_budget`。

验收：

- 6 个 report worker 并发时，每个 worker 都能从自己的 packet 获取背景。
- Codex prompt 长度明显下降。

### P1：ProjectMemory

- report accept 后抽取 report summary 写入 project memory。
- 支持按 `tag/file_path/keyword` 搜索。

验收：

- 新 worker 能引用前一轮 accepted report 的摘要，而不是重新读全文。

### P2：ReportSynthesis

- 给定 2-6 个 report route，生成统一 synthesis。
- 标记一致发现、冲突、下一步建议。

验收：

- 多个 report worker 审查同一主题时，Codex 可读一份 synthesis 完成父层判断。

### P3：Repo Map / Context Packing

- 先做轻量版：目录树 + Rust/Python/TS 函数签名 + README/AGENTS/CHANGELOG 摘要。
- 后续再考虑 tree-sitter / BM25 / embedding。

验收：

- worker 能少读文件，且 report 能指出它依赖了哪些 context sources。

## 7. 明确非目标

- 不做 worker-to-worker 聊天。
- 不做通用消息总线。
- 不复活 ACP。
- 不把所有历史事件塞进 prompt。
- 不让 worker 直接写 ProjectMemory。
- 不在 v7 MVP 引入完整向量 RAG。
- 不把共享上下文和 MCP transport 修复混成一个巨大版本。
- 不把 `D:\guKimi` 的 Claude cwd 当成任务上下文来源；任务上下文来自 `target_workspace`。

## 8. 对当前 AgentCall 的最小落点

我建议 v7.0 的第一刀不是“大型 RAG”，而是非常工程化的四个文件：

```text
.agentcall/context/routes/<route_id>.json
.agentcall/context/routes/<route_id>.md
.agentcall/context/project_memory.ndjson
.agentcall/context/decisions.ndjson
```

再加两个投影字段：

```json
{
  "context": {
    "packet_id": "ctx-route-...",
    "sources": ["AGENTS.md", "README.md", "report:..."],
    "budget_tokens": 8000,
    "estimated_tokens": 2400
  }
}
```

这样可以保持 AgentCall 的核心简单：daemon 生成、worker 引用、report 回流、Codex 合成。

## 9. 最终判断

AgentCall 的共享上下文层应该解决的是 **父层重复解释项目背景、worker 重复探索、报告无法合成** 这三件事。

最值得参考的不是某一个完整框架，而是组合：

- aider 的 repo map：少量结构摘要帮助 agent 找路。
- Cline 的 Memory Bank：把项目活记忆落成可读文件。
- LangGraph 的 thread/store 分层：短期 route 状态与长期项目记忆分离。
- Repomix 的 structured packet：上下文成为可审计产物。
- OpenHands/SWE-agent 的 event/trajectory：报告背后要有证据链。

如果 v7.0 只做一件事，我会选：**daemon-owned ContextPacket + report accept 后自动回流 ProjectMemory**。这条线足够小，也最能降低 Codex 组织多 Agent 时的上下文压力。
