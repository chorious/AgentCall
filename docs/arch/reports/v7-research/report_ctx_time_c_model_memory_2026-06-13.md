# AgentCall Project Memory 事实抽取：最小基准与数据模型建议

> 任务：A/B/C 实验组 C（模型生成 Context Exposure Window），为 AgentCall v7 设计从已验收 worker 报告中抽取 Project Memory 事实的最小基准与数据模型。
> 日期：2026-06-13
> 性质：report-only，不修改源代码。

---

## 1. 已阅读文件

- `E:\Project\AgentCall\.agentcall\tasks\route-1687990\prompt.md`（本任务控制信封与 Context Exposure Window）。
- `E:\Project\AgentCall\AGENTS.md`：项目纪律、worker kind、冻结计划、版本规则、MCP 流程。
- `E:\Project\AgentCall\docs\reports\report_ctx_ab_a_raw_objective_2026-06-13.md`：A 组报告，无预编译背景下的上下文暴露 API/数据模型建议。
- `E:\Project\AgentCall\docs\reports\report_ctx_ab_b_short_context_2026-06-13.md`：B 组报告，短人工背景下的 WorkerBrief schema 与 prompt 指引。
- `E:\Project\AgentCall\docs\reports\report_ctx_ab_c_model_context_2026-06-13.md`：C 组前置报告，Context Exposure Window 评估框架建议。
- `E:\Project\AgentCall\docs\reports\report_v7_shared_context_research_2026-06-13.md`：v7 共享上下文研究，含 ContextPacket / ProjectMemory / DecisionLog / ReportSynthesis 对象模型。
- `E:\Project\AgentCall\docs\reports\report_v7_worker_brief_research_2026-06-13.md`：v7 Worker Brief 研究（引用关系）。
- `E:\Project\AgentCall\docs\reports\report_v6.7_demo_known_issues_and_v7_questions_2026-06-13.md`：v6.7 demo 观察，可作为事实抽取样例来源。
- `E:\Project\AgentCall\docs\reports\v5.4-implementation-closure.md`：已验收实现闭合报告，含证据表格与验证命令，适合作为抽取样例。
- `E:\Project\AgentCall\docs\arch\plan\v0.4-orchestration-roadmap.md`：早期编排路线图，定义 context packet、decision log、shared state 等概念起源。
- `E:\Project\AgentCall\.agentcall\state\` 目录：确认当前存在 `project.json`、`decisions.ndjson` 占位（仅 21 字节，尚未启用），尚无真正的 `.agentcall/context/` 目录或 `project_memory.ndjson`。

---

## 2. 问题定义

AgentCall v7 的关键闭环不是“让 worker 读更多上下文”，而是：

> **worker 产出 report → daemon 验收 report → 把可复用事实沉淀为 Project Memory → 后续 worker 的 brief 自动引用这些事实。**

因此需要一个可度量、可复现的**事实抽取系统**，包括：

1. 事实 schema（Fact 长什么样）。
2. 抽取 pipeline（从 accept 的 report 到结构化 facts）。
3. 验证指标（抽得对不对、全不全、是否过时）。
4. 基准任务（用真实报告测试抽取质量）。
5. 冲突/过期处理机制。

---

## 3. 建议的事实 Schema

每个 fact 是 `project_memory.ndjson` 中的一行 JSON。按当前 AgentCall 形态，建议以下最小字段：

```json
{
  "fact_id": "fact-20260613-7a3f",
  "entry_type": "toolchain_fact",
  "fact": "使用 cargo test --workspace 运行 Rust 测试，pytest 运行 Python 测试",
  "source_report": "docs/reports/v5.4-implementation-closure.md",
  "source_route": "route-1678xxx",
  "applies_to": {
    "paths": ["crates/agentcall-daemon", "tests"],
    "tags": ["rust", "testing", "validation"]
  },
  "confidence": "high",
  "evidence": [
    "v5.4 closure 中 'cargo workspace tests: passed' 与 'pytest: 17 passed'"
  ],
  "created_at": "2026-06-11T00:00:00Z",
  "updated_at": "2026-06-11T00:00:00Z",
  "stale_after": null,
  "superseded_by": null,
  "status": "active"
}
```

### 3.1 `entry_type` 枚举

| 类型 | 含义 | 示例 |
|---|---|---|
| `toolchain_fact` | 验证/构建/部署命令或环境事实 | `cargo test --workspace` |
| `architecture_fact` | 架构边界与职责 | daemon 是状态权威 |
| `constraint` | 项目硬规则 | 不得编辑冻结计划 |
| `known_issue` | 已确认的缺陷或限制 | MCP transport `Transport closed` |
| `risk` | 未解决但已记录的风险 | lease TOCTOU 在并发写时危险 |
| `decision` | 已做出的设计/产品决策 | v7 不做 worker-to-worker 聊天 |
| `rejected_hypothesis` | 已被证伪的假设 | “ACP 应复活”已被否 |
| `open_question` | 尚未闭合的问题 | ProjectMemory 写入由 daemon 还是 Codex 审批 |

### 3.2 字段约束

- `fact_id`：daemon 生成，UUID 或 route 前缀 + 哈希。
- `source_report`：必须是已验收报告的路径（`agentcall_report(action=accept)` 返回 ok）。
- `source_route`：可选，用于溯源。
- `confidence`：`high` / `medium` / `low`。`high` 仅当报告中有明确证据或测试通过时。
- `evidence`：字符串数组，每条指向报告中的具体段落/表格/命令输出。
- `stale_after`：时间戳或版本号；工具链事实可设版本敏感日期。
- `status`：`active` / `stale` / `conflicted` / `superseded`。

---

## 4. 抽取 Pipeline

建议 Pipeline 分为四个阶段，全部在 daemon 内完成（worker 不直接写 Project Memory）：

```text
┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
│  1. Ingest      │ -> │  2. Chunk       │ -> │  3. Extract     │ -> │  4. Validate    │
│  accepted report│    │  by section     │    │  structured     │    │  + Write        │
└─────────────────┘    └─────────────────┘    └─────────────────┘    └─────────────────┘
```

### 4.1 Ingest

触发条件：`agentcall_report(action=accept)` 返回 `ok=true`。

输入：

- `report_path`
- `route_id`
- `worker_kind`
- `confidence`（daemon accept 时给出）
- `target_workspace`

### 4.2 Chunk by section

把 Markdown 报告按标题分块。常见块：

- `Summary` / `摘要`
- `Closed Items` / `已闭合项`
- `Known Issues` / `已知问题`
- `Risks` / `风险`
- `Open Questions` / `待解决问题`
- `Validation` / `验证`
- `Files Changed` / `变更文件`

每个块作为独立抽取单元，保留到原始段落的映射。

### 4.3 Extract

对每个块，用规则 + 轻量模型生成候选 facts：

| 块类型 | 抽取规则 | 产出 entry_type |
|---|---|---|
| 表格“问题/严重度/条件” | 每行生成 `risk` 或 `known_issue` | `risk`, `known_issue` |
| 表格“Area/Result/Evidence” | 每行生成 `architecture_fact` 或 `toolchain_fact` | `architecture_fact`, `toolchain_fact` |
| “Validation” 中的命令与结果 | 提取命令作为 `toolchain_fact` | `toolchain_fact` |
| “Risks” 列表 | 每项生成 `risk` | `risk` |
| “Open Questions” | 每项生成 `open_question` | `open_question` |
| “最终建议/结论”中涉及架构边界 | 生成 `decision` 或 `architecture_fact` | `decision`, `architecture_fact` |
| 明确否定某方案 | 生成 `rejected_hypothesis` | `rejected_hypothesis` |

抽取 prompt 示例：

```text
从以下已验收的 AgentCall worker 报告段落中抽取结构化事实。
每个事实必须是 daemon 可验证的：有来源、有证据、 confidence 不超过 high。
只输出 JSON Lines，每行一个 fact，字段：fact_id, entry_type, fact, evidence, confidence, applies_to.tags。
段落：
---
{chunk_text}
---
```

### 4.4 Validate + Write

验证规则：

1. `fact` 不得与现有 active fact 直接矛盾（见第 6 节冲突处理）。
2. `confidence=high` 必须有非空 `evidence`。
3. `source_report` 必须存在于已验收报告索引。
4. 同一 report 内不得生成重复 fact（去重）。
5. `entry_type` 必须在允许枚举内。

通过后写入 `.agentcall/context/project_memory.ndjson`。

---

## 5. 验证指标

建议把抽取质量拆为三类指标：

### 5.1 覆盖指标（Coverage）

| 指标 | 定义 | 目标 |
|---|---|---|
| `fact_recall` | 人工标注的“应抽取事实”中，被 pipeline 抽中的比例 | ≥ 0.75 |
| `section_recall` | 含事实的段落中，至少抽出一个 fact 的段落比例 | ≥ 0.85 |
| `type_coverage` | 8 种 `entry_type` 至少各有 1 条样例 | 100%（基准集） |

### 5.2 精确指标（Precision）

| 指标 | 定义 | 目标 |
|---|---|---|
| `fact_precision` | 抽出的 facts 中，人工判定为“正确且有价值”的比例 | ≥ 0.80 |
| `evidence_precision` | `evidence` 字段确实能在 source_report 中找到对应的比例 | ≥ 0.90 |
| `hallucination_rate` | fact 中包含 source_report 未提及内容的比例 | ≤ 0.05 |

### 5.3 效用指标（Utility）

| 指标 | 定义 | 目标 |
|---|---|---|
| `brief_use_rate` | 后续 route 的 brief 中实际引用了该 fact 的比例 | ≥ 0.40 |
| `redundant_read_reduction` | 引用 fact 的 worker 相比未引用时，平均少读文件数 | ≥ 1 |
| `downstream_task_success` | 引用 fact 的 worker 任务成功率 | ≥ 基线 + 10% |

---

## 6. 基准任务（Benchmark Tasks）

用 6 份已验收的真实报告构建基准集：

| 编号 | 报告 | 考察能力 | 期望抽取事实数 |
|---|---|---|---|
| M-01 | `v5.4-implementation-closure.md` | 从证据表格抽取 toolchain/architecture facts | 8-12 |
| M-02 | `report_v6.7_demo_known_issues_and_v7_questions_2026-06-13.md` | 从问题表格抽取 risks / known_issues | 6-10 |
| M-03 | `report_v7_shared_context_research_2026-06-13.md` | 从外部框架比较中抽取 decisions / architecture facts | 8-12 |
| M-04 | `report_v7_worker_brief_research_2026-06-13.md` | 从研究结论中抽取 constraints / rejected_hypotheses | 5-8 |
| M-05 | `report_ctx_ab_a_raw_objective_2026-06-13.md` | 从 API 数据模型建议中抽取 architecture facts / decisions | 6-10 |
| M-06 | `report_ctx_ab_b_short_context_2026-06-13.md` | 从 schema 建议中抽取 constraints / open_questions | 4-6 |

每个任务评估：

1.  golden facts：由人工从报告中标注应抽取的事实。
2.  pipeline output：运行抽取 pipeline。
3.  计算 `fact_recall`、`fact_precision`、`evidence_precision`。

**基线对比：**

- Baseline A：简单规则（正则/关键词）抽取，无模型。
- Baseline B：把整份报告塞给模型，让模型自由输出 facts。
- Proposed：分块 + 规则分类 + 模型抽取 + 验证。

---

## 7. 冲突与过期处理

### 7.1 冲突检测

当新 fact 与现有 active fact 语义冲突时，标记为 `conflicted`：

```json
{
  "status": "conflicted",
  "conflict_with": ["fact-20260611-abc1"],
  "resolution": "pending_supervisor"
}
```

冲突判定方法（由轻到重）：

1. **精确匹配**：相同 `entry_type` + 相同 `applies_to.paths` + 相似 `fact` 文本。
2. **向量相似度**：用 embedding 比较 fact 语义，阈值 0.92 以上视为潜在冲突。
3. **关键词反义**：同一路径/标签下出现矛盾谓词（如“应该做 X” vs “不应该做 X”）。

### 7.2 冲突解决

- 自动解决：若旧 fact 的 `source_report` 版本更旧，且新 fact 有更高 `confidence`，则标记旧 fact 为 `superseded_by: new_fact_id`。
- 人工解决：若两者 `confidence` 相同或来源独立，则保留 `conflicted` 状态，由 Codex 在 board 中决定。
- 禁止自动删除：任何 fact 只标记 `stale` / `superseded`，不物理删除，保留审计链。

### 7.3 过期处理

- 时间过期：`stale_after` 到期后，状态变为 `stale`。
- 版本过期：若 `fact` 涉及工具链版本，且 `Cargo.lock` / `pyproject.toml` 升级后命令不再适用，自动标记 `stale`。
- 源码变更：若 `applies_to.paths` 中的文件被大幅重写，daemon 可触发 fact 复核。

### 7.4 写入权限

- **只有 daemon** 能在 report accept 后写入 Project Memory。
- worker 只能读，不能写。
- Codex 可通过 MCP `agentcall_context(action=append, type=decision)` 显式写入 `decisions.ndjson`，但不可直接改 `project_memory.ndjson`。

---

## 8. 风险

| 风险 | 影响 | 缓解 |
|---|---|---|
| **抽取幻觉** | 把报告中的建议当成已验证事实 | 强制 `evidence` 字段；`high` confidence 必须可追溯到报告原文 |
| **事实过度泛化** | 把具体观察写成通用结论 | `applies_to.paths` 必须具体；默认加 `scope: report-local` |
| **Project Memory 膨胀** | 越积越多，brief 引用时反而噪音大 | 按 `entry_type` 重要性排序；低 confidence facts 60 天后归档 |
| **冲突无法收敛** | 两个报告互相矛盾，一直 `conflicted` | 引入 `decision_id` 裁决机制；用户可一键接受/拒绝 |
| **worker 利用 Project Memory 跳过验证** | 引用过时 fact 导致错误 | 每次引用时展示 `updated_at` 与 `status`；stale fact 不得默认注入 |
| **MCP transport 不稳定** | 查询/写入 Project Memory 失败 | daemon 侧本地文件是权威；MCP 失败时退化为文件读取 |

---

## 9. 模型生成的 Context Exposure Window 是否减少了探索时间

**结论：是的，明显减少了。**

具体表现：

1. **目标聚焦**：Context Exposure Window 明确把任务定位为“Project Memory 事实抽取的最小基准与数据模型”，而不是泛泛地讨论 Context Window。worker 不需要从“上下文是什么”开始重新推导。
2. **关键文件已知**：Supervisor Context 中已说明 `decisions.ndjson`、`project_memory.ndjson`、ReportSynthesis、Worker Brief 等概念，worker 可以直接引用 A/B 组报告和 v7 研究，而不必重新 grep 整个仓库找相关文件。
3. **边界清晰**：Worker Context 中给出 `worker_kind: report`、只写 report/scratch、Bash readonly，worker 不会误入实现修改或越界写入。
4. **剩余成本**：仍然需要确认 `.agentcall/state/` 中没有真正的 `project_memory.ndjson`，以及阅读 A/B 组报告来对齐术语（如 `ContextPacket` vs `WorkerBrief`）。但这些确认性阅读远短于从仓库摸索出整个 v7 上下文模型。

与 A 组（无预编译背景）相比，本任务不需要从头阅读 `routes.rs`、`hooks.rs`、`projection.rs` 来理解上下文生成机制；与 B 组（短人工背景）相比，本任务额外获得了“Project Memory”这一具体焦点的定位，因此探索范围更窄、更可控。

---

## 10. 最小落地建议

1. **先建 benchmark 数据集**：从 M-01 到 M-06 六份报告人工标注 golden facts，作为后续 pipeline 迭代基准。
2. **实现最小抽取脚本**：`scripts/extract_project_memory.py`，输入 `report_path + route_id`，输出候选 facts JSONL，先不写入，只供人工复核。
3. **定义验证 harness**：`tests/test_project_memory_extraction.py` 计算 recall / precision / evidence precision。
4. **再接入 daemon**：在 `agentcall_report(action=accept)` 后调用抽取脚本，经规则验证后写入 `.agentcall/context/project_memory.ndjson`。
5. **最后让 brief 引用**：在 `WorkerBrief.relevant_facts` 中按 `applies_to` 匹配 project memory，完成闭环。

---

## 11. 开放问题

1. `decisions.ndjson` 与 `project_memory.ndjson` 中 `decision` 类型的事实是否需要合并，还是保持两层？
2. `confidence` 应完全由抽取模型决定，还是由 daemon 根据 `report_accept_confidence` 和证据强度联合决定？
3. 是否允许 Codex 显式“升级”一个 `medium` confidence fact 为 `high`？
4. 多报告合成（ReportSynthesis）产生的新事实，是否也应回流到 Project Memory？
5. 是否需要对 `fact` 文本做版本化快照，以便追踪语义漂移？
