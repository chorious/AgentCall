# Plan Review — v2.4：ACP Background Supervisor

Reviewer: Opus 4.8 · 日期: 2026-06-04
关联: `plan_review_v2.3_OPUS.md`、`acp_sop_gate_review_OPUS.md`、`acp_python_vs_rust_OPUS.md`、`python_debt_audit_OPUS.md`、CHANGELOG `v0.6.1`(single-writer)
状态: 方向认可、应做;有一个内在矛盾 + 一个被高估的承诺需先解决

---

## 0. 一句话结论

目前几版里**架构最成熟的一版**:把 ACP 生命周期从 route 剥出、daemon-owned `acp_invocations.json` + 重启孤儿检测 + route 瘦身,正面解决了 in-flight 不可见(早期 #1)与 `routes.json` 膨胀。

但有**一个内在矛盾**("无人值守" vs "不 kill 只标 checkpoint_due")和**一个被高估的承诺**("可恢复"),不解决会让 unattended worker 的定位自相拆台。

---

## 1. 命门:"无人值守" 与 "不 kill,只标 checkpoint_due" 互斥

plan 同时说了两件相互拆台的事:

1. 定位 = **bounded unattended worker**(无人值守);
2. 停滞处理 = 超 10 分钟无 progress → `checkpoint_due=true`,**但不 kill**;默认 `timeout_seconds` **disabled**。

**矛盾**:`checkpoint_due` 是给"值守者"看的信号,可你定义它无人值守。没人盯 board,`checkpoint_due` 就是林中无人听见倒下的树——卡死 worker 会**永远**挂着,因为唯一的自动后备(timeout)被默认关闭。

**还混淆了两类边界**:

- plan 的论证是"权限明确 → 不造成破坏 → 可以慢慢跑",只覆盖**安全边界**。
- 但只读/report-only 的 worker 慢慢跑,**仍在烧 token / 烧钱 / 占进程/线程**——这是**成本边界**,权限管不了。卡在读文件循环里的 ACP child,一小时是真金白银 + 一个常驻线程 + 一个常驻子进程。

**结论**:无人值守的 worker **必须有自动硬上限,默认 ON,设宽松值(如 30 分钟)而非无穷**。`checkpoint_due`(10 分钟)作为更早的软警告保留。**别把唯一的自动 backstop 默认关掉**——否则 "unattended" 名不副实,实际依赖有人值守。

---

## 2. 被高估的承诺:ACP 工作**不可恢复**,只有记录可恢复

Summary 称 ACP 要"可恢复"。但 ACP child 是 daemon 经 **stdio 派生的子进程**(`crates/agentcall-daemon/src/acp.rs`,stdin/stdout/stderr piped)。daemon 一死,stdio 管道断裂,**child 即废,无法跨新 daemon 进程 re-attach**。

重启后能"恢复"的只有:**invocation 记录** + worker **已写入 `report_path` 的部分内容**。**运行中的工作本身丢失,只能重跑。**

两个后果:

1. `orphaned_after_daemon_restart` 是对的,但**"可恢复"是 overclaim**。诚实表述:孤儿 = 工作已丢、需重跑(report-only 任务幂等,重跑便宜,故可接受)。
2. **与项目运维模型冲突**:README 写"更新 daemon 后需要重启 daemon"。即**每次 daemon 更新都会把所有在飞长 ACP 作业变孤儿**。想要长后台 worker,却又频繁重启 daemon——长作业撞上的恰是最常做的操作。文档须讲清:启动长 ACP 前别更新 daemon;或长任务优先 PTY。

---

## 3. 孤儿检测:别 probe pid,用 daemon boot-id

plan 说"扫描未完成 invocation,无活进程可关联就标孤儿"。若机制是"存 `child_pid`、重启查 pid 存活"——**Windows 上不可靠**:pid 复用会把无关进程误判为"worker 还活着"(false running)。

由 §2:**stdio child 活不过 daemon,重启后不存在"还在跑"的情况——全死了。** 故无需 probe:给每个 daemon 实例一个 **boot-id**,重启后**凡 `running` 且 owning boot-id ≠ 当前的一律标孤儿**。比扫 pid 又简单又正确。

---

## 4. 必须钉死的依赖/归属(plan 未写)

1. **report contract validator 在哪?** v2.4 用 `report_contract_status` / `failed_report_contract` 把验收变成状态机一等转移(completed vs failed 由它决定)。同 `acp_python_vs_rust_OPUS.md` 的老问题:**校验器移进 Rust,还是调只读 Python?** 现在 load-bearing,必须拍板。且按 `acp_sop_gate_review_OPUS.md`,校验应是 **per-template schema**,不是通用 "report contract"——plan 这里说通用,要对齐。
2. **依赖 SOP gate 的权限牙齿先落地**。"写只能命中 report_path、读限 allowed_paths" 正是 SOP gate review 的 **per-template permission deny**(改 `select_permission_option`,`acp.rs:271`)。若强制未做,v2.4 这几句退回 prompt-only 许愿。**应显式声明 v2.4 依赖 v2.2.x 的 permission 强制。**

---

## 5. 次要但真实

- **heartbeat 写放大 / events.ndjson 噪声**:60s heartbeat 既更新 state 又写 `acp.heartbeat` event。`events.ndjson` append-only,跑 2 小时 = 120 条纯噪声事件。建议:**liveness 只写入会被覆盖的 `acp_invocations.json`(`last_heartbeat_at`);events.ndjson 仅在状态变化时发**(progress/checkpoint_due/completed/failed)。
- **缺并发上限**:"慢慢跑" + 默认无 timeout + 不 kill = 卡死 worker **累积**。`active_acp_invocations` 暴露在 health 却无约束。**补并发上限 + 触顶策略**(拒新 ACP route 还是排队)。
- **`last_progress_summary` 是 adapter 耦合 best-effort**:压成 "read LoginScene.cs" 依赖解析 toolCall 结构,不同 adapter 结构不一(v2.3 待拍板已提)。**确保无状态转移依赖这段 prose**——状态只由结构化信号(permission 请求、report 文件存在、进程退出)驱动,summary 仅供展示。这块最 speculative,可降级/后置,不 gate 发布。
- **测试用真实墙钟会慢/flaky**:10 分钟 checkpoint_due、60s heartbeat 需**可注入时钟/可配阈值**。`acp_heartbeat_interval_seconds` 已可配(好),checkpoint 阈值也应可配。
- **缺 clean shutdown 测试**:干净关闭时 daemon 应**主动 kill 自有 child**,孤儿只该出现在 crash。test plan 未覆盖。

---

## 6. "可观测 / 可恢复 / 可验收" 兑现

| 承诺 | 兑现 |
|---|---|
| 可观测 | ✅ heartbeat + board 投影,扎实 |
| 可恢复 | ⚠️ 仅记录+部分 report,**运行中工作不可恢复**(§2) |
| 可验收 | ✅ report contract——前提 validator 真存在且 per-template(§4) |

---

## 7. 待拍板

- [ ] **默认 hard budget 改 ON**(宽松上限,如 30 分钟),而非 disabled——无人值守不能依赖有人看 checkpoint_due。
- [ ] **"可恢复" 降级为诚实表述**(孤儿=工作丢失需重跑),并写明与"频繁重启 daemon"的运维张力。
- [ ] 孤儿检测改 **boot-id**,弃 pid probe。
- [ ] validator 归属(Rust vs 只读 Python)+ per-template schema。
- [ ] 显式声明依赖 SOP gate 的 permission 强制。
- [ ] 并发上限 + 触顶策略。
- [ ] heartbeat 降噪(state 覆盖 vs events.ndjson append)。

---

## 8. 总评

架构方向对,route 瘦身与孤儿检测是真进步,应该做。两件事必须先解决否则定位站不住:**(1) 默认硬上限 ON;(2) "可恢复" 诚实降级 + 承认运维张力**。再加 validator 归属与 SOP gate 依赖两个拍板。
