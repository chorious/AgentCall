# Context Brief A/B/C Metric Smoke：简单组 + 复杂组

日期：2026-06-14  
仓库：`E:\Project\AgentCall`  
实验起点：`2026-06-14T00:23:21.7355588+08:00`

## Summary

本轮按同一 A/B/C 注入设计跑了两组任务：

- **简单组**：总结 AgentCall canonical MCP 工具、worker kind、核心流程与禁令。
- **复杂组**：设计 Context Window / Worker Brief 质量量化评估 harness，要求包含 benchmark、时间指标、100 分质量量规、daemon/event/report 采集、验收阈值与风险。

A/B/C 条件：

- **A raw**：无额外背景，只给 objective 与必要 reference。
- **B short**：短人工背景，明确当前目标、关键路径、禁止项。
- **C model**：局域网模型生成的 Context Exposure Window / Worker Brief 背景。

关键结论：

1. **简单组不能用于判断上下文效果**：simple-A 成功产出报告；simple-B/C 都出现 `pty.stop_requested -> session_ended`，但未写报告。这是控制面异常或外部 stop 干扰样本，应剔除。
2. **复杂组三路都成功**，且均被 daemon-observed report write 验收为 high confidence。
3. **复杂组速度上 B 最快，A 基本相当，C 稍慢**：B 约 307.4s，A 约 309.6s，C 约 326.2s。
4. **复杂组质量上 C 最好，B 次之，A 最弱**：C 的 report 更贴近 WorkerBrief/BriefCompiler 结构；B 对短人工背景价值解释最好；A 覆盖很广但夹带较多 legacy/Python/v2 视角。

## Timing

> 说明：本表用同一实验起点计算 elapsed；route 是顺序发起的，因此这只是 smoke 级时间，不是最终 benchmark 统计。后续正式实验应使用 per-route `session_started -> report_ready`。

| Group | Injection | Status | Event Time | Elapsed |
|---|---|---:|---:|---:|
| simple-A | raw | `report_ready` | 00:25:47.192 | 145.5s |
| simple-B | short | `ended_no_report` | 00:25:52.720 | 151.0s |
| simple-C | model | `ended_no_report` | 00:25:59.802 | 158.1s |
| hard-B | short | `report_ready` | 00:28:29.139 | 307.4s |
| hard-A | raw | `report_ready` | 00:28:31.317 | 309.6s |
| hard-C | model | `report_ready` | 00:28:47.918 | 326.2s |

## Output Artifacts

| Group | Report |
|---|---|
| simple-A | `docs/reports/report_ctx_metric_simple_a_raw_2026-06-14.md` |
| simple-B | missing |
| simple-C | missing |
| hard-A | `docs/reports/report_ctx_metric_hard_a_raw_2026-06-14.md` |
| hard-B | `docs/reports/report_ctx_metric_hard_b_short_2026-06-14.md` |
| hard-C | `docs/reports/report_ctx_metric_hard_c_model_2026-06-14.md` |

## Hard Group Quality Rubric

总分 100：

| Metric | Weight |
|---|---:|
| Benchmark task set coverage | 20 |
| Timing metrics | 15 |
| Quality rubric design | 20 |
| Daemon/event/report data capture | 15 |
| Context/WorkerBrief integration | 15 |
| Risks and failure modes | 10 |
| Self-evaluation of context injection impact | 5 |

## Hard Group Scores

| Group | Coverage | Timing | Rubric | Capture | Context Fit | Risks | Self-Eval | Total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| hard-A raw | 17 | 13 | 18 | 12 | 11 | 9 | 3 | **83** |
| hard-B short | 18 | 15 | 18 | 13 | 13 | 9 | 5 | **91** |
| hard-C model | 20 | 15 | 19 | 13 | 15 | 10 | 5 | **97** |

### Score Notes

**hard-A raw：83**

- 优点：覆盖 benchmark、treatments、时间指标、100 分 rubric、采集路径和阈值，设计完整。
- 缺点：从零读代码导致视角发散，夹带较多 legacy Python/v2/NDJSON 方案；与当前 v6.7.1 SQLite/daemon-first 主线贴合度弱一些。

**hard-B short：91**

- 优点：对“短人工背景减少探索时间”的解释最清楚；指标、阈值、A/B/C 对照和短背景模板都很落地。
- 缺点：仍有若干事件名/文件路径属于建议状态，和当前 daemon 可直接提取的数据之间需要实现层对齐。

**hard-C model：97**

- 优点：最贴近 v7 WorkerBrief 编译器目标，明确 ScopeFilter / RuleExtractor / FactSelector / RepoNavigator / ContractCompiler / BriefRenderer，并给出 8 个 benchmark 与完整采集方案。
- 缺点：报告中假设了若干未来产物，如 `.agentcall/briefs`、`policy_denials.json`、结构化 `brief.json`；作为计划质量很高，但不是当前实现的直接反映。

## Interpretation

### Speed

复杂组单轮 smoke 中，短人工背景 B 最快，raw A 几乎持平，模型背景 C 慢约 18.8 秒。这个结果说明：模型生成上下文在当前形态下不一定降低端到端时间，至少在单轮、单任务、并发运行场景里，额外上下文会带来读取和整合成本。

但 C 的报告质量更高，说明它的收益更像是：

- 降低方向漂移；
- 提高结构完整性；
- 提供更接近最终工程设计的模块化语言；
- 让 worker 更容易围绕 WorkerBrief/ProjectMemory 主线输出。

### Quality

复杂任务中，C 的质量提升比速度提升更明显。B 是非常好的低成本 baseline：人工短背景足以显著改善聚焦性，而不明显增加输入负担。A 能完成任务，但更容易走向“把仓库重新摸一遍”的报告风格。

### Reliability

simple-B/C 无报告终止是本轮最重要的异常。事件显示它们在读了 prompt、AGENTS、README、protocol 等文件后收到 `pty.stop_requested`，随后释放 lease，但没有 report write。后续正式实验要先保证：

- 每路 worker 的 stop 来源可追踪；
- 无报告终止必须进入失败指标；
- 并发实验不能因为控制面清理或人为 stop 污染结果；
- 每次实验必须记录 per-route start/report/stop，而不是只用全局起点。

## Next Experiment Design

建议下一轮这样测：

1. 简单组和复杂组分开跑，避免 6 路并发互相干扰。
2. 每组 A/B/C 各跑至少 3 次。
3. 固定 `route_id -> session_started -> first_tool -> report_ready -> accepted` 五个时间点。
4. simple 组只做速度和正确性，不做深度质量评分。
5. hard 组继续用 100 分 rubric，并要求裁判记录扣分证据。
6. 把 `ended_no_report`、`policy_denied_loop`、`prompt_pending_timeout` 作为独立失败类型，而不是混入耗时均值。

## Bottom Line

这轮 smoke 支持一个更谨慎的结论：

- **短人工背景 B 是当前最划算的上下文注入方式**：速度好、足够聚焦、成本低。
- **模型背景 C 更适合复杂设计/研究任务**：质量最高，但未证明能更快。
- **raw A 仍可用，但主管成本高**：worker 会自己重建大量背景，质量受探索路径影响更大。
- **正式评估前必须先修实验 harness 的 reliability**：特别是 simple-B/C 的无报告终止问题，否则速度数据会被控制面异常污染。
