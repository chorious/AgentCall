# AgentCall Context Exposure Window 评估框架建议

## 摘要

本报告针对 A/B/C 实验组 C 的模型生成 Context Exposure Window，提出一套可落地的评估 harness，用于量化“生成的上下文窗口是否真正提升 AgentCall worker 表现”。核心思路是把 Worker Brief 质量拆成可独立测量的维度，用受控任务在真实 AgentCall 路由上跑对照实验，并用 daemon 事件与报告验收结果作为客观指标。

## 已阅读文件

- `AGENTS.md`：项目规则、worker kind、route/session/report 契约、版本与冻结基线。
- `README.md`：当前架构、MCP 工具面、worker 启动与验收流程。
- `crates/agentcall-daemon/src/routes.rs`：route 创建、context packet 持久化、`pty_prompt` 与 `pty_prompt_control_envelope`/`pty_prompt_containment` 的生成逻辑。
- `crates/agentcall-daemon/src/mcp.rs`：MCP 工具 schema、session summary/TUI 投影、report accept 置信度计算。
- `.agentcall/tasks/route-1679287/prompt.md`：本次实验提供的模型生成 Context Exposure Window。
- `docs/reports/report_v7_worker_brief_research_2026-06-13.md`：v7 Worker Brief Compiler 相关研究，含 AGENTS.md 有效性、Context as a Tool、AgentFold、Reflexion 等结论。
- `.agentcall/tasks/task-0017/brief.md` / `report.md`：既有 worker brief 与报告样例。

## 当前基线：routes.rs 生成的 handoff prompt

`routes.rs:pty_prompt()` 当前把 route 请求组装成一段自由文本：

1. 控制信封（task id、worker_kind、workspace、report_path、scratch_path）。
2. 本地工具链上下文（`toolchain_context_value`）。
3. Objective。
4. 读写边界（containment mode、writable_paths、reference_paths、bash_write_policy）。
5. 验收标准（acceptance_criteria）。

context packet（`create_context`）只保留任务元数据，没有显式的规则抽取、事实选择、repo map、输出/失败契约。这就是我们需要评估的“上下文窗口生成器”的基线。

## 评估目标

验证：把 Codex 全局上下文编译成结构化的 Worker Brief（Supervisor Context + Worker Context）后，Claude Code worker 是否在以下方面优于基线 handoff prompt：

- 更少偏离任务目标
- 更少违反读写边界
- 更少重复提问/重复尝试
- 更快生成可验收报告
- 对复杂任务（跨文件、需遵守项目规则）成功率更高

## 建议的 Benchmark Tasks

按难度与失败模式分层设计，每个任务都要能用 `agentcall_route` 启动并收敛到 `report_path`：

| 编号 | 任务类型 | 具体任务 | 预期失败模式（基线） |
|---|---|---|---|
| C-01 | report + 规则理解 | 让 worker 阅读 `AGENTS.md`，指出当前 v6.7 的 worker kind 与报告验收规则，写出摘要 | 遗漏“frozen plan”规则、把 SDK runtime 说成可用 |
| C-02 | report + 多文件综合 | 阅读 `crates/agentcall-daemon/src/routes.rs` 与 `mcp.rs`，回答“route 启动后 supervisor 应如何观察进度” | 只看一个文件、遗漏 projection-first 原则 |
| C-03 | coding + 边界遵守 | 在 `tests/` 新增一个测试，验证 `route_worker_kind` 对 report-only path 的判断；不得修改实现 | 越界修改 `routes.rs`、遗漏测试断言 |
| C-04 | coding + 规则约束 | 修改 `crates/agentcall-daemon/src/routes.rs` 中某函数，同时必须保持“frozen plan 不可编辑”约束在报告中声明 | 未声明约束、或错误地重写计划 |
| C-05 | multi-hop 事实 | 基于已接受的 `task-0017/report.md` 与 `docs/sop-protocol.md`，推导“父-子 agent SOP”并指出与当前 daemon 的 gap | 遗漏已有报告事实、产生与 AGENTS.md 冲突的结论 |
| C-06 | 故障恢复 | 给定一个模拟的 `blocked_by_policy` 事件，worker 需写出 blocker 报告并停止，而不是重试 | 陷入重试循环、未报告 blocker |

每个任务至少跑 5 次，以控制模型随机性。

## 评估指标

### 1. 任务完成指标（Outcome）

| 指标 | 定义 | 来源 |
|---|---|---|
| `report_accepted` | `agentcall_report(action=accept)` 返回 `ok=true` | daemon MCP |
| `overall_confidence` | accept 结果的 `confidence.overall`（high/medium/low） | daemon MCP |
| `task_success` | 人工/裁判判断报告内容是否正确且完整 | 报告内容 + 裁判 |
| `wall_time_to_report` | 从 `route start` 到 `report_ready` 的时间 | daemon events |
| `turn_count` | worker 主动发出的有效工具调用/文本轮数 | transcript / events |

### 2. 上下文使用指标（Context Quality）

| 指标 | 定义 | 来源 |
|---|---|---|
| `boundary_violations` | 尝试写入 `write_paths` 外或读取未授权区域的次数 | `hook.PreToolUse` decision=denied |
| `redundant_reads` | 同一文件被重复读取的次数（跨工具调用去重） | transcript |
| `off_topic_actions` | 与 objective 无关的工具调用占比 | 裁判标注 |
| `clarification_questions` | worker 向 supervisor 提出的澄清问题数 | PTY output / transcript |
| `contract_compliance` | 是否显式遵循 brief 中的 success/failure contract | 报告内容 |

### 3. 上下文窗口生成器指标（Compiler Quality）

| 指标 | 定义 | 来源 |
|---|---|---|
| `brief_token_count` | 生成的 Worker Brief token 数 | 字符/近似 token |
| `rule_recall` | brief 中保留的“本任务相关硬规则”占应保留规则的比例 | 人工标注 |
| `fact_precision` | brief 中“已验证事实”确实正确的比例 | 与 AGENTS.md / 已接受报告核对 |
| `repo_map_relevance` | 提供的 repo map 符号/路径中实际被 worker 使用的比例 | transcript 反查 |
| `noise_ratio` | 与任务无关的语句占 brief 的比例 | 人工标注 |

### 4. 综合评分

建议用一个加权公式：

```text
context_score = 0.35 * task_success
              + 0.20 * (1 - normalized_boundary_violations)
              + 0.15 * (1 - normalized_redundant_reads)
              + 0.15 * contract_compliance
              + 0.10 * (1 - normalized_brief_token_count)
              + 0.05 * rule_recall
```

其中 `normalized_*` 用该任务在所有实验条件下的排名或 z-score 归一化。

## 实验设计

### 阶段 1：单变量对照（A/B）

- A 组：当前 `routes.rs` 生成的 handoff prompt（基线）。
- B 组：模型生成的 Context Exposure Window（本 prompt 描述的 Supervisor Context + Worker Context）。
- 控制变量：同一 `objective`、同一 `write_paths`、同一 `report_path`、同一模型与权限模式。
- 随机化：每次用新的 `session_name`，避免 Claude Code session 记忆污染。

### 阶段 2：消融实验（A/B/C）

为了定位哪部分上下文真正有效，把 Context Exposure Window 拆成：

- C1：仅 Supervisor Context（规则+事实）。
- C2：仅 Worker Context（目标+边界+契约）。
- C3：完整 Supervisor + Worker Context。
- C4：完整上下文 + 动态 RepoNavigator（根据任务实时生成 repo map）。

### 阶段 3：压力测试

- 长 objective：500 字以上、含多个子目标。
- 冲突规则：AGENTS.md 中存在表面矛盾的历史规则，看 worker 是否能识别“frozen plan”优先。
- 信息过载：reference_paths 提供 20+ 文件，观察是否导致冗余读取。
- 工具链缺失：不提供 `toolchain.json`，观察 worker 是否按规则回退到静态分析。

### 阶段 4：人工裁判与自动裁判结合

- 自动裁判：daemon 事件、报告存在性、boundary violation 次数。
- 人工裁判：报告内容正确性、是否遵守项目特定规则、是否生成可行动结论。
- 建议用“双盲 + 多数决”对 C-01/C-02/C-05 等需要理解的报告打分。

## 数据收集与可复现性

每次实验应持久化：

1. `context.json` / `prompt.md`：实际发给 worker 的上下文。
2. `transcript.jsonl`：完整会话轨迹。
3. `events.ndjson`：daemon 事件流。
4. `report.md`：worker 最终报告。
5. `score.json`：上述指标与人工裁判分。

建议把评估 harness 写成 `tests/test_context_window_ab.py`，用 `pytest` 驱动：

- 调用 `agentcall_route` 启动任务。
- 轮询 `agentcall_session` 等待 `report_ready` 或超时。
- 调用 `agentcall_report(action=accept)` 获取客观指标。
- 用 `agentcall_route` 的 `transcript_index` 或 hooks 事件计算冗余读取、boundary violation。
- 输出 `context_score`。

## 风险

1. **AGENTS.md 全文注入风险**：如果把整个 `AGENTS.md` 不加筛选地塞进 brief，可能导致 worker 过度探索、运行额外测试，反而降低成功率（已有研究支持）。
2. **语义漂移（semantic drift）**：模型在折叠历史时可能丢失“frozen plan 不可编辑”等硬约束，导致 worker 违规修改 `docs/v6.2-code-plan.md`。
3. **RepoNavigator 复杂度**：过早引入向量 RAG 或 AST 解析会增加不确定性；建议先用基于 reference_paths 与文件大小/最近修改的轻量 repo map。
4. **worker 忽略 output_contract**：即使 brief 中写明“失败时报告 blocker”，模型仍可能在 denied action 后陷入重试。需要把 failure contract 与 daemon 的 `blocked_by_policy` 状态联动。
5. **评估指标被操纵**：worker 可能通过少做工具调用来降低 `turn_count` 和 `redundant_reads`，但报告质量下降。必须保留 `task_success` 和人工裁判作为最终指标。
6. **会话记忆污染**：Claude Code 的 session 可能保留上次的上下文，实验间需使用新 session 并校验 `claude_session_id`。

## 本 Context Exposure Window 是否提升了清晰度？

**总体判断：是，方向正确，但需要验证“结构化”本身是否被 worker 遵守。**

提升点：

- 明确区分 Supervisor Context 与 Worker Context，解决了当前 handoff prompt 把“规则、事实、目标、边界”混成一段文本的问题。
- 提出 ScopeFilter / RuleExtractor / FactSelector / RepoNavigator / ContractCompiler 五个模块，使上下文编译从“写 prompt”变成可测试、可度量的 pipeline。
- 强调 Worker Brief 必须包含 success contract 与 failure contract，与当前 daemon 的 report accept、policy denial、blocker 报告机制对齐。
- 指出“状态可能放在 `.agentcall/context`”，为后续持久化 project_memory.ndjson / decisions.ndjson 提供了落地路径。

仍需澄清/验证点：

- **如何评分 brief quality**：本报告已提出 `context_score` 与多维度指标，但权重需根据实际 A/B 结果校准。
- **如何生成 repo map**：建议先从“任务关键词 → reference_paths → 最近相关文件/符号”开始，避免直接上向量 RAG。
- **如何合成多 worker 报告**：需要在 `.agentcall/context/project_memory.ndjson` 中定义事实 schema（如 `verified_fact`、`user_decision`、`known_pitfall`、`rejected_hypothesis`），否则多报告合成会退化为文本拼接。

## 下一步建议

1. 先实现最小可复现 harness：`tests/test_context_window_ab.py` 跑 C-01/C-02/C-03 三个任务，对比基线与模型生成上下文。
2. 定义 `.agentcall/context/fact_schema.json`，把“已验证事实”结构化，而不是写在 Markdown 里。
3. 在 `routes.rs` 旁新增 `brief_compiler.rs`（或 Python 脚本），先实现 ScopeFilter + RuleExtractor + ContractCompiler 三个模块，RepoNavigator 用文件列表占位。
4. 跑完 3 轮 A/B 后，根据 `context_score` 调整指标权重，再扩展 benchmark 到全部 6 个任务。

## 结论

模型生成的 Context Exposure Window 为 AgentCall v7 的 Worker Brief Compiler 提供了清晰的方向。评估 harness 应围绕“任务成功率、边界遵守、冗余读取、契约遵守、brief 质量”五个维度，用受控 A/B/C 实验在真实 daemon 上收集客观指标。当前最紧迫的是把评估框架代码化，避免在缺乏度量的情况下直接重写上下文生成逻辑。
