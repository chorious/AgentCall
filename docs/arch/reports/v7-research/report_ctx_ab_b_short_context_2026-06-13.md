# AgentCall Context Window / Worker Brief 设计报告（B 组：短人工背景）

日期：2026-06-13
任务：基于短人工背景，为 AgentCall v7 提出 Context Window / Worker Brief 的 prompt/schema 设计。

---

## 1. 已读文件

- `E:\Project\AgentCall\.agentcall\tasks\route-1679274\prompt.md`（本任务控制信封）
- `E:\Project\AgentCall\README.md`（v6.7.1 产品形态与 worker 种类）
- `E:\Project\AgentCall\docs\v6.3-code-plan.md`（primary action、patience、读写策略分层）
- `E:\Project\AgentCall\docs\agentcall-protocol.md`（Codex 监督协议）
- `E:\Project\AgentCall\docs\arch\plan\v2.0-architecture.md`（route-first、daemon single-writer）
- `E:\Project\AgentCall\docs\reports\report_v7_worker_brief_research_2026-06-13.md`（v7 Worker Brief 调研）
- `E:\Project\AgentCall\docs\reports\report_v7_shared_context_research_2026-06-13.md`（v7 共享上下文调研）
- `E:\Project\AgentCall\crates\agentcall-daemon\src\routes.rs`（RouteRequest、ContextRequest、pty_prompt、containment）
- `E:\Project\AgentCall\crates\agentcall-daemon\src\mcp.rs`（MCP 工具面与 session/route 响应）
- `E:\Project\AgentCall\crates\agentcall-daemon\src\prompt_gate.rs`（prompt gate 状态机）
- `E:\Project\AgentCall\plugins\agentcall\skills\agentcall\SKILL.md`（Codex skill 规则）

---

## 2. 设计目标

AgentCall 已经证明：Codex 主管 + Claude Code PTY worker + daemon 状态权威 + report 契约 这条路能跑通。v7 的核心痛点不是“worker 不够聪明”，而是 **Codex 每次派工都要重复解释项目背景，worker 拿到过多或过少上下文，report 验收后又没有结构化回流**。

因此 Context Window / Worker Brief 要满足：

1. **最小可用**：只给本次任务必须知道的信息。
2. **daemon 可验证**：schema 是结构化的，不只是 Markdown。
3. **可审计**：brief 必须记录来源、预算和排除项。
4. **可回流**：report accept 后把可复用事实写回 project memory，而不是全文追加。

---

## 3. 建议 schema：Worker Brief

每个 route 启动时由 daemon 生成一份 `WorkerBrief`，同时输出 JSON（供 daemon/MCP）和 Markdown（供 worker 阅读）。

### 3.1 文件位置

```text
<target_workspace>/.agentcall/briefs/<route_id>.json
<target_workspace>/.agentcall/briefs/<route_id>.md
```

### 3.2 JSON Schema

```json
{
  "brief_id": "brief-route-1679274",
  "route_id": "route-1679274",
  "task_id": "ctx-ab-b-short-context",
  "call_id": "route-1679274",
  "worker_kind": "report",
  "phase": "execute",
  "role": "reviewer",
  "runtime": "pty",
  "target_workspace": "E:\\Project\\AgentCall",
  "claude_cwd": "D:\\guKimi",
  "objective": "Inspect the repo and propose a Context Window / Worker Brief schema design.",
  "supervisor_intent": "Codex wants a concrete, repo-aware brief schema; do not modify source code.",
  "task_scope": {
    "write_paths": [
      "E:\\Project\\AgentCall\\docs\\reports\\report_ctx_ab_b_short_context_2026-06-13.md",
      ".agentcall/workspaces/ctx-ab-b-short-context"
    ],
    "reference_paths": [
      "E:\\Project\\AgentCall\\README.md",
      "E:\\Project\\AgentCall\\docs",
      "E:\\Project\\AgentCall\\crates\\agentcall-daemon\\src"
    ],
    "report_path": "E:\\Project\\AgentCall\\docs\\reports\\report_ctx_ab_b_short_context_2026-06-13.md"
  },
  "hard_boundaries": {
    "do_not_modify_source_code": true,
    "do_not_write_outside": [
      "E:\\Project\\AgentCall\\docs\\reports\\report_ctx_ab_b_short_context_2026-06-13.md",
      ".agentcall/workspaces/ctx-ab-b-short-context"
    ],
    "bash_write_policy": "readonly_only"
  },
  "must_follow": [
    "Write the final report to the exact report_abs_path.",
    "List files_read in the report.",
    "Propose a concrete schema, not a concept."
  ],
  "must_not_do": [
    "Do not edit crates/agentcall-daemon source files.",
    "Do not treat reference_paths as write permission.",
    "Do not fabricate file paths or daemon behavior."
  ],
  "relevant_facts": [
    {
      "kind": "architecture",
      "fact": "AgentCall v2.0 is route-first: board -> route -> session -> report.",
      "source": "docs/arch/plan/v2.0-architecture.md",
      "confidence": "high"
    },
    {
      "kind": "constraint",
      "fact": "Daemon is the single writer for events, file_claims, active_sessions, route_state.",
      "source": "docs/arch/plan/v2.0-architecture.md",
      "confidence": "high"
    },
    {
      "kind": "known_issue",
      "fact": "Dumping full chat history or AGENTS.md into worker prompt increases cost and can reduce success rate.",
      "source": "docs/reports/report_v7_worker_brief_research_2026-06-13.md §2.1",
      "confidence": "high"
    }
  ],
  "repo_navigation": {
    "important_paths": [
      "crates/agentcall-daemon/src/routes.rs",
      "crates/agentcall-daemon/src/mcp.rs",
      "plugins/agentcall/skills/agentcall/SKILL.md"
    ],
    "do_not_scan_unless_needed": [
      "node_modules",
      ".agentcall/research/upstreams"
    ]
  },
  "output_contract": {
    "required_report": true,
    "report_format": "markdown",
    "required_sections": [
      "files_read",
      "proposed_schema",
      "prompt_guidance",
      "risks",
      "open_questions",
      "short_background_evaluation"
    ],
    "changed_files_required": false,
    "tests_required": false
  },
  "failure_contract": {
    "when_blocked": "write a blocker report instead of retrying silently",
    "when_unclear": "ask concise clarification questions in the PTY",
    "permission_denied": "report exact command/path/reason"
  },
  "context_budget": {
    "max_tokens": 8000,
    "estimated_tokens": 2400,
    "sources": [
      "README.md",
      "docs/arch/plan/v2.0-architecture.md",
      "docs/reports/report_v7_worker_brief_research_2026-06-13.md",
      "docs/reports/report_v7_shared_context_research_2026-06-13.md",
      "crates/agentcall-daemon/src/routes.rs"
    ],
    "excluded_sources": [
      "Full Codex conversation history",
      "All upstream research repos",
      "Full AGENTS.md/CLAUDE.md verbatim"
    ]
  }
}
```

### 3.3 Markdown 渲染要点

`.agentcall/briefs/<route_id>.md` 是 worker 实际读到的文件，结构应紧凑：

```markdown
# Worker Brief: route-1679274

## 0. Control Envelope
- worker_kind: report
- target_workspace: E:\Project\AgentCall
- report_path: E:\Project\AgentCall\docs\reports\report_ctx_ab_b_short_context_2026-06-13.md
- bash_write_policy: readonly_only

## 1. Objective
Inspect the repo and propose a Context Window / Worker Brief schema design.

## 2. Hard Boundaries
- Do not modify source code.
- Write only to the report path and session scratch.

## 3. Must Follow
- List files_read.
- Propose a concrete schema.

## 4. Relevant Facts
- AgentCall v2.0 is route-first: board -> route -> session -> report.
- Daemon is the single writer for live state.
- Dumping full chat history hurts success rate.

## 5. Output Contract
- Required report in markdown.
- Include files_read, proposed_schema, prompt_guidance, risks, open_questions, short_background_evaluation.

## 6. Failure Contract
- Blocked -> blocker report.
- Unclear -> concise question.
```

---

## 4. Prompt 使用指引

### 4.1 Codex 侧（主管）

正常派工仍保持现有流程：

```text
agentcall_board(view=compact, filter=attention)
agentcall_route(objective=..., workspace=..., write_paths=..., reference_paths=...)
agentcall_session(name=...)
agentcall_report(action=accept, session_id=...)
```

区别是：Codex 不需要再手动拼接大段背景。`objective` + `reference_paths` + `write_paths` 就是编译 brief 的主要输入。

### 4.2 Worker 侧（Claude Code）

Handoff prompt 从当前的大段文本改为引用 brief：

```text
AgentCall handoff for `route-1679274`. Read and follow `<target_workspace>/.agentcall/briefs/route-1679274.md`. Write the final report to `<report_abs_path>`. When finished, say COMPLETE.
```

worker 启动后先读 brief，再决定读哪些 reference paths，而不是被塞进所有背景。

### 4.3 Daemon 侧（Rust）

在 `routes.rs` 的 `create_context` 或 `start_pty_route` 阶段，把 `ContextRequest` 升级为 `WorkerBrief`：

1. 接收 route 请求中的 `objective`、`write_paths`、`reference_paths`、`acceptance_criteria`。
2. 从 `target_workspace` 读取 `AGENTS.md` / `README.md` / `CLAUDE.md`，按标题/段落匹配提取相关规则。
3. 从 `.agentcall/context/project_memory.ndjson` 按 `path/tag` 选择相关 facts。
4. 生成 `brief_id`、写入 `.agentcall/briefs/<route_id>.json` 和 `.md`。
5. 在 route/session projection 中暴露 `brief_id`、`context_sources`、`context_budget`。

---

## 5. 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| Brief compiler 过度裁剪，worker 缺失关键上下文 | worker 做错范围或重复踩坑 | 保留 `relevant_facts` 和 `repo_navigation`；MVP 用确定性规则而非黑盒模型 |
| Brief 成了另一个 dump 全文的借口 | 失去 Context Window 意义 | 强制 `context_budget` 和 `excluded_sources`；超过预算必须审计 |
| Worker 不读 brief 直接开工 | prompt 引用失效 | handoff prompt 明确“Read and follow ... first”；daemon 可检测是否读取 |
| Project memory 越积越胖 | 后续 brief 膨胀 | report accept 后只回流结构化 facts，不回流全文 |
| 与现有 `context_packet` 重复或冲突 | 两套上下文并存 | 把 `context_packet` 并入 `WorkerBrief`，统一文件位置 |
| `reference_paths` 被误当写边界 | 越界写或 policy deny | brief 中明确区分 `write_paths` / `reference_paths` / `hard_boundaries` |

---

## 6. 待解决问题

1. **Brief compiler 的输入源优先级**：`AGENTS.md`、`CLAUDE.md`、`README.md`、`accepted reports`、`toolchain.json` 的抽取顺序和冲突处理规则是什么？
2. **相关性匹配策略**：MVP 用路径/标签/标题匹配，何时引入 BM25 或 embedding？
3. **Project memory 的写入权限**：report accept 后由 daemon 自动抽取 facts，还是由 Codex 显式审批？
4. **Brief 版本化**：route 重试或 `revise_plan` 时是否生成新 brief_id？旧 brief 如何归档？
5. **Worker 读取 brief 的证据**：daemon 能否通过 hook 或 file claim 确认 worker 已读取 brief？
6. **与现有 `agentcall_context` 工具的衔接**：是否需要新增 `agentcall_context(action=get|search|append)`，还是复用 `agentcall_route` 生成 brief？

---

## 7. 短人工背景是否足够

**结论：基本够用，但有两处明显缺口。**

足够的部分：

- 任务目标清晰：v7 讨论的是避免重复手动注入上下文，Codex 应编译有用任务上下文。
- 当前系统形态已知：Rust daemon + MCP + PTY worker + hooks + report 契约。
- 期望的 brief 内容已知：objective、hard boundaries、reference files、output contract、acceptance criteria、known risks、gaps。

不够的部分：

1. **没有给出 v7 已经产出的调研报告路径**。如果我没主动搜索 `docs/reports/report_v7_*.md`，会重复大量已有结论。
2. **没有说明“Context Window”具体指 worker 可见的 prompt 文件、route 的 JSON 字段，还是 MCP 工具返回的投影**。需要阅读 `routes.rs` 和 `mcp.rs` 后才能确认当前 `context_packet` 已经存在，且应在此基础上演进。

因此，短背景能让一个熟悉 AgentCall 的人快速上手，但对首次接触的 worker 来说，仍需主动探查 repo 中的 v7 资料和当前 `context_packet` 实现。

---

## 8. 最终建议

AgentCall v7 的 Context Window 应该落地为一个 **daemon 生成的 `WorkerBrief` 文件对（JSON + Markdown）**，而不是 worker 之间共享的长记忆。

最小 MVP：

1. route 时自动生成 `.agentcall/briefs/<route_id>.json` 和 `.md`。
2. handoff prompt 只引用 brief 文件和报告路径。
3. session summary 显示 `brief_id`、`context_sources`、`context_budget`。
4. report accept 后由 daemon 抽取 3-10 条结构化 facts 写入 `.agentcall/context/project_memory.ndjson`。

这样既能降低 Codex 的上下文压力，也能让 worker 拿到有边界、可审计、可回流的最小任务简报。
