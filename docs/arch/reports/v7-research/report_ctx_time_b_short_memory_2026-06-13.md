# AgentCall v7 Project Memory 事实 Schema 与 Report-Accept 抽取流设计

日期：2026-06-13
任务：A/B/C 实验组 B —— 基于短人工背景，为 AgentCall v7 设计 Project Memory 事实 schema 与报告验收后的结构化抽取流。

---

## 1. 已读文件

- 控制信封与本任务：
  - `E:\Project\AgentCall\.agentcall\tasks\route-1687977\prompt.md`
- 项目规则与产品形态：
  - `E:\Project\AgentCall\AGENTS.md`
  - `E:\Project\AgentCall\README.md`
- v7 相关前期研究（同日期）：
  - `E:\Project\AgentCall\docs\reports\report_v7_worker_brief_research_2026-06-13.md`
  - `E:\Project\AgentCall\docs\reports\report_v7_shared_context_research_2026-06-13.md`
  - `E:\Project\AgentCall\docs\reports\report_ctx_ab_a_raw_objective_2026-06-13.md`
  - `E:\Project\AgentCall\docs\reports\report_ctx_ab_b_short_context_2026-06-13.md`
  - `E:\Project\AgentCall\docs\reports\report_ctx_ab_c_model_context_2026-06-13.md`
  - `E:\Project\AgentCall\.agentcall\reports\ctx-v7-memory-frameworks.md`
- 当前实现（报告验收与上下文生成）：
  - `E:\Project\AgentCall\crates\agentcall-daemon\src\routes.rs`
  - `E:\Project\AgentCall\crates\agentcall-daemon\src\mcp.rs`
- 既有报告契约：
  - `E:\Project\AgentCall\src\agentcall\v2\reports.py`
  - `E:\Project\AgentCall\src\agentcall\v2\context.py`
  - `E:\Project\AgentCall\.agentcall\state\reports.index.json`
  - `E:\Project\AgentCall\.agentcall\state\context_index.json`

---

## 2. 设计目标

v7 已确定的方向是：Codex 主管把复杂上下文编译成 Claude Code worker 可读的 `WorkerBrief`，而不是每次手写大段 prompt。本报告聚焦 **report accept 之后的回流层**：如何把验收通过的可复用结论，沉淀为 daemon 管理的 Project Memory，供后续 route 的 brief compiler 选取。

核心约束来自现有代码与规则：

1. **daemon 是状态权威**（`AGENTS.md`）：事件、claims、routes、reports 都由 Rust daemon 写入，worker 不能直接写项目级记忆。
2. **当前 `accept_report_for_session` 只验证报告存在性**：`mcp.rs:1777` 检查 `exists && non_empty && matched_route_report_path`，更新 route 状态为 `report_accepted`，但没有抽取结构化事实。
3. **`context_packet` 只是原始路由字段**：`routes.rs:267` 生成的 JSON 仅包含 `objective`、`write_paths`、`reference_paths` 等请求字段，没有规则裁剪、事实选择或 repo navigation。
4. **worker 输出契约已有结构化基础**：`src/agentcall/v2/reports.py` 定义了 `ChildReport` schema，含 `changed_files`、`commands_run`、`tests`、`risks`、`open_questions`、`context_sufficiency` 等字段，可作为抽取输入。

因此，Project Memory 不是让 worker 写摘要，而是 **daemon 在报告验收后从报告 + 事件证据中提炼结构化事实**。

---

## 3. 建议的事实 Schema

每个事实都是单条可独立引用的记录，存放在 workspace 级 ndjson 中，按作用域索引。

### 3.1 存储位置

```text
<target_workspace>/.agentcall/context/project_memory.ndjson
<target_workspace>/.agentcall/context/decisions.ndjson
```

### 3.2 事实条目 Schema

```json
{
  "fact_id": "fact-7f8a9b2c",
  "schema_version": 1,
  "entry_type": "verified_fact",
  "created_at": "2026-06-13T12:34:56Z",
  "updated_at": "2026-06-13T12:34:56Z",
  "expires_at": null,
  "status": "active",

  "fact": "cargo test --workspace 是此仓库 Rust 侧标准验证命令",
  "detail": "运行前需确认 daemon 已停止，避免 SQLite WAL 锁竞争。",

  "source": {
    "kind": "accepted_report",
    "route_id": "route-1680001",
    "session_id": "worker-a",
    "report_path": "E:/Project/AgentCall/docs/reports/report_xxx.md",
    "report_section": "validation",
    "evidence": [
      {"kind": "command_run", "command": "cargo test --workspace", "result": "pass"},
      {"kind": "file_mentioned", "path": "crates/agentcall-daemon/src/lib.rs"}
    ]
  },

  "confidence": {
    "overall": "high",
    "artifact": "high",
    "daemon_write": "high",
    "route_match": "high"
  },

  "applies_to": {
    "workspaces": ["E:/Project/AgentCall"],
    "paths": ["crates/agentcall-daemon", "crates/agentcall-mcp"],
    "tags": ["rust", "testing", "validation"],
    "worker_kinds": ["coding", "report"]
  },

  "scope": {
    "frozen_plan_sensitive": false,
    "version_baseline": "v6.7.1",
    "replaces_fact_ids": []
  },

  "provenance": {
    "extracted_by": "daemon",
    "extractor_version": "project-memory-v1",
    "reviewed_by": null,
    "accept_hash": "sha256:..."
  }
}
```

### 3.3 事实类型定义

| entry_type | 含义 | 示例 |
|---|---|---|
| `verified_fact` | 经 report + 证据验证的可复用事实 | 测试命令、工具路径、镜像配置 |
| `user_decision` | 用户明确拍板的产品/工程决策 | 保留 frozen v6.2 plan、不做 ACP |
| `known_pitfall` | 已踩过且可能复现的坑 | MCP transport 在热重启后需完全关闭 Codex |
| `rejected_hypothesis` | 被推翻的假设 | 以为 `submit_pending_prompt` 是正常路径，实为 debug |
| `toolchain_fact` | 本地工具链特定事实 | 127.0.0.1:10708 用于 GitHub 下载 |
| `architecture_fact` | 架构约束 | daemon 是 events/claims/sessions 的唯一写入者 |
| `boundary_rule` | 从 AGENTS.md / 项目规则裁剪出的任务相关硬规则 | report worker 不得修改实现文件 |
| `open_question` | 尚未解决但值得后续 route 注意的问题 | brief compiler 的 path/tag 匹配策略 |

### 3.4 Decision Log 条目

`decisions.ndjson` 记录“为什么这么做”，与 `project_memory.ndjson` 中的 `user_decision` 类型互补：

```json
{
  "decision_id": "dec-3a4b5c6d",
  "made_at": "2026-06-13T12:34:56Z",
  "made_by": "codex",
  "source_route_id": "route-1680002",
  "context": "v7 context window 设计评审",
  "alternatives": ["shared memory block", "worker-to-worker chat", "full RAG"],
  "chosen": "daemon-owned WorkerBrief + ProjectMemory",
  "rationale": "保持 Codex 主管、daemon 权威、worker 边界执行的现有分工，避免 AutoGen 0.2 式广播膨胀。",
  "confidence": "high",
  "status": "active"
}
```

---

## 4. Report-Accept 抽取管线

抽取应在 `accept_report_for_session` 成功后触发，由 daemon 独占写入。

### 4.1 触发条件

```text
agentcall_report(action=accept, session_id=...) -> ok=true
  -> confidence.overall ∈ {high, medium}
  -> route.status := report_accepted
  -> daemon 调用 extract_facts(route_id, report_path, evidence)
```

当前 `mcp.rs:1823` 已更新 route 状态为 `report_accepted`，只需在其后追加抽取调用。

### 4.2 抽取步骤

```text
Input: route record + report file + daemon events
  |
  v
1. ValidateAcceptEvidence
   - 报告文件存在且非空
   - confidence.overall >= medium
   - 如 overall=high，必须有 daemon_observed_write 证据
   |
   v
2. ParseReport
   - 如报告含 JSON frontmatter（ChildReport schema），直接解析
   - 否则做轻量 Markdown section 提取
   |
   v
3. GatherEvidence
   - 从 events.ndjson 中收集本 session 的：
     * 文件写入事件（tool_name=Write/Edit/MultiEdit, decision=allowed）
     * 命令运行事件（Bash tool result）
     * 测试运行事件
     * policy denial 事件
   |
   v
4. ExtractFacts
   - 按 entry_type 分类提取
   - 每个事实必须绑定 source.route_id 和 evidence
   |
   v
5. DeduplicateAndMerge
   - 与现有 project_memory.ndjson 按语义 key 去重
   - 冲突时按 confidence/timestamp/source 仲裁
   |
   v
6. WriteProjectMemory
   - daemon 以追加方式写入 ndjson
   - 同时更新 `.agentcall/context/index.json`
```

### 4.3 与现有 WorkerBrief 的衔接

`routes.rs` 的 `create_context`（或未来 `brief_compiler.rs`）在生成 brief 时：

1. 读取 `project_memory.ndjson` 和 `decisions.ndjson`。
2. 按当前 route 的 `workspace`、`write_paths`、`reference_paths`、`worker_kind` 做 path/tag 匹配。
3. 只选 `status=active` 且未过期的事实。
4. 按 `confidence.overall` 排序，优先 `high`。
5. 把前 N 条（如 3-10 条）写入 brief 的 `relevant_facts`。

---

## 5. 验证与置信度

### 5.1 可验证性

| 验证点 | 方法 |
|---|---|
| 事实是否来自 accepted report | 检查 `source.route_id` 对应 route.status == report_accepted |
| 事实是否有证据支撑 | `source.evidence` 至少一条，且与 events.ndjson 可核对 |
| 事实是否被 brief 引用 | brief 中 `relevant_facts` 包含 fact_id |
| 事实是否过期 | `expires_at` 或 `version_baseline` 与当前版本比对 |
| 抽取是否正确 | 写单元测试：给定样例报告，断言输出 facts 的 entry_type 和 applies_to |

### 5.2 置信度传递

沿用 `mcp.rs` 已有的四分置信：

```json
{
  "overall": "high|medium|low",
  "artifact": "high|low",
  "daemon_write": "high|low",
  "route_match": "high|low"
}
```

- `overall=high` 的事实才能进入默认 brief。
- `overall=medium` 的事实仅当没有 high 且任务高度相关时才可选入，并在 brief 中标注为 medium。
- `overall=low` 的事实只入 store，不进入 brief。

---

## 6. 冲突与过期处理

### 6.1 冲突检测

两条事实冲突当且仅当：

- `entry_type` 相同或语义相近（如两个 `verified_fact` 对同一命令给出不同结论）。
- `applies_to.paths` 或 `applies_to.tags` 有交集。
- `fact` 文本在语义上矛盾（MVP 可用关键词/命令路径精确匹配；后续可引入 embedding 相似度）。

### 6.2 冲突仲裁规则

1. **daemon-observed evidence 优先于纯报告文本**。
2. **confidence.overall 高者优先**。
3. **timestamp 新者优先**（但 `frozen_plan_sensitive=true` 的事实除非用户显式解冻，否则不覆盖）。
4. **`user_decision` 优先于 `verified_fact` 推导**。
5. 仲裁结果写入 `decisions.ndjson`，并把被覆盖事实的 `status` 改为 `superseded`，填充 `superseded_by`。

### 6.3 过期处理

| 过期条件 | 动作 |
|---|---|
| `expires_at` 到达 | `status := expired`，brief compiler 不再选取 |
| `version_baseline` 与当前产品版本不一致 | 保留但降 confidence，brief 中标注为“基于旧版本” |
| 源报告被删除或 route 状态被回滚 | `status := invalid`，保留审计但不使用 |
| 用户显式拒绝某条事实 | `status := rejected`，并记录拒绝原因 |

---

## 7. 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| 抽取器过度生成事实 | project_memory 迅速膨胀，brief 变胖 | 每份报告限制 3-10 条；低置信度不入 brief；设总条目上限并定期归档 |
| 事实脱离上下文被误用 | worker 把仅适用于旧版本/特定路径的结论泛化 | `applies_to` 必须精确到 path/tag；`frozen_plan_sensitive` 和 `version_baseline` 默认启用 |
| 幻觉级联 | 未验证的 worker 自述被当成事实 | 只抽取 `daemon_observed_write` 或明确 `user_decision` 支撑的内容 |
| 冲突仲裁错误 | 新报告推翻已被接受的正确事实 | 仲裁规则透明、可审计；重大冲突保留多版本并提示 Codex |
| 与现有 `context_packet` 重复 | 两套上下文并存 | v7 把 `context_packet` 升级为 `WorkerBrief`，project memory 作为 brief 的输入源之一 |
| 抽取阻塞 report accept | 报告验收变慢或失败 | 抽取为异步后台任务，不影响 accept 响应；失败写入 `extraction_errors.ndjson` |

---

## 8. 待解决问题

1. **抽取器实现位置**：放在 Rust daemon 中作为内置模块，还是 Python 脚本供 daemon 调用？考虑到 daemon 是状态权威且 `AGENTS.md` 要求“Python 不做 live writer”，抽取逻辑宜在 Rust 中实现。
2. **事实语义冲突判定**：MVP 用路径/命令精确匹配足够吗？何时引入 embedding/BM25？
3. **用户决策入口**：Codex 是否可以通过新 MCP action（如 `agentcall_context(action=decision)`）显式写入 `user_decision`？
4. **报告格式假设**：当前报告多为 Markdown 自由文本，是否应要求 worker report 必须包含结构化 frontmatter（ChildReport schema）才能被抽取？
5. **跨 workspace 共享**：Project Memory 是按 `target_workspace` 隔离，还是允许父子 workspace 继承？
6. **垃圾回收**：长期不用的 expired/superseded 事实是归档到 `.agentcall/context/archive/` 还是直接删除？

---

## 9. 短人工背景是否减少了探索时间

**结论：基本够用，但本任务仍需要主动探查 repo 才能给出具体设计。**

足够的部分：

- 目标清晰： report accept 后把可复用事实沉淀为 Project Memory。
- 约束已知： daemon 是写入权威、worker 不直接写项目记忆。
- 可参考材料丰富：仓库内已有同日期 v7 Worker Brief / shared context / A/B/C 三份报告，术语和方向已经对齐。

不够的部分：

1. **没有给出当前报告验收代码的具体位置**。如果未主动搜索 `accept_report_for_session` 和 `create_context`，很难把设计锚定到真实 hook 点。
2. **没有说明 Project Memory 应如何与现有 `context_packet` 共存**。需要阅读 `routes.rs` 和 `mcp.rs` 后才能确认：`context_packet` 只是原始字段，未做事实选择；accept 后也没有抽取。
3. **没有给出事实类型的优先级**。虽然提示提到 `verified_fact`、`user_decision`、`known_pitfall`、`rejected_hypothesis`，但没说明如何映射到现有报告字段和事件证据。

因此，短背景能让我快速定位到 v7 相关资料和大方向，但要把 schema 和抽取流落到具体文件/函数，仍需主动阅读 daemon 实现。与 A 组“无预编译背景”相比，本任务节省了理解 Worker Brief 概念的时间；与 C 组“模型生成完整 Context Exposure Window”相比，本任务需要自己补充代码锚点，没有现成模块清单可follow。

---

## 10. 最终建议

v7 Project Memory 的最小 MVP：

1. **新增四个文件**：
   - `<workspace>/.agentcall/context/project_memory.ndjson`
   - `<workspace>/.agentcall/context/decisions.ndjson`
   - `<workspace>/.agentcall/context/index.json`
   - `<workspace>/.agentcall/context/extraction_errors.ndjson`
2. **扩展 `accept_report_for_session`**：在 `mcp.rs:1823` 更新 route 状态后，调用事实抽取器。
3. **抽取器输入**：accepted report 文本、`ChildReport` frontmatter（如有）、本 session 的 daemon events。
4. **抽取器输出**：3-10 条结构化事实，按 entry_type 分类，附 evidence 和 confidence。
5. **brief compiler 读取 project memory**：按 path/tag/worker_kind 匹配，把相关事实填入 `relevant_facts`。
6. **冲突/过期处理**：定义仲裁规则，superseded 事实保留但不再进入 brief。

这样即可让 report accept 真正产生“可被后续 worker 复用的项目记忆”，而不是只更新一次 route 状态。
