# Plan Review — AgentCall v0.8(Route-First Rust Core + Python 债务清理)

Reviewer: Opus 4.8 · 日期: 2026-06-04
关联: `plan_review_v0.7_OPUS.md`、`plan_hook_aware_binding_v0.7.1.md`、`report_review_OPUS.md`、`python_debt_audit_OPUS.md`

---

## 0. 本轮已拍板的决策(并入本 plan)

1. **binding 已修复** → v0.8 可依赖 hook-aware binding;保留一个**上线前验证 gate**(route-pty 起一个 session 必须真绑定)。
2. **ACP 切分**:v0.8a 让 **Rust 成为 route/ACP 的入口与控制面**(invocation、超时、state、board 都归 daemon),**不碰 ACP 核心实现**——核心调用暂时仍由现有 Python ACP driver 承担,但被 daemon 有界包裹;**Rust 原生 ACP client 重写推迟到 v0.8b**。
3. context/transcript/report 的 daemon endpoint **明确输出**(见 §5)。
4. **timeout-kill vs auto-kill 边界写清**(见 §4)。
5. **优先填 MCP 的坑**(双写 + 无超时),排在 ACP 重写之前。
6. **强化 auto 逻辑**:`route` 要求调用方提交**预估时间**与**预估代码量**作为路由输入;并为废弃项**设定时间点**(见 §3、§6)。

---

## 1. 总评

终局正确:`board → route → session/report`、delegate 废除、Python 退胶水、核心逻辑入 Rust。非目标清单克制到位。本轮按拍板**切成 v0.8a / v0.8b 两版**,把最重的 ACP 原生重写隔离出去,先用一版低风险改动让 route-first 体验落地、并把 MCP 的双写与无超时两个坑填掉。

---

## 2. 版本切分

### v0.8a — Route 入口统一 + MCP 坑填平(低风险,先发)
不碰 ACP 核心实现,只让 Rust 接管入口与控制面:

- **route 三合一**:新 canonical `agentcall_route` 取代 `route_task` + `delegate` + `delegate_acp`。
- **delegate 废除**:`agentcall_delegate*` 移出 `tools/list` 与 capabilities;旧 handler 仅返回"已废弃,请用 `agentcall_route`",**不再跑 Python workflow**。
- **binding gate**:`route(mode=start, runtime=pty)` 起的 session 必须在 board/summary 中显示 `binding_source=env`(复用已修复的 binding);CI/smoke 加一条断言。
- **ACP 走 Rust 入口、核心仍 Python(有界)**:`route(mode=start, runtime=acp)` → daemon `POST /api/routes` → **daemon 拥有 invocation 记录、施加 bounded timeout、超时 kill、写 state/board**;ACP 实际执行暂时由 daemon 派生现有 Python ACP driver 完成。
  - **关键约束(防双写回潮)**:被 daemon 包裹的 Python ACP driver **只许把结果经 stdout 返回 daemon,禁止自己写 `.agentcall` state**;落盘一律由 daemon 完成。否则 v0.8a 又把 store.py 拉回 live 写路径。
- **关三个 Python 写口**(见 §5):checkpoint/report、context、transcript-index 迁 daemon endpoint。
- **超时止血**:在 ACP 原生化之前,所有残留 `run_agentcall_owned` 调用即刻包 bounded timeout(daemon 侧或 MCP 侧),消除裸 `.output()` 挂死。

### v0.8b — Rust 原生 ACP client(高风险,隔离)
- ACP client 迁入 Rust:stdio JSON-RPC、request id 匹配、stdout/stderr 并发读、timeout、超时 kill。
- 用 fake ACP server 测试 derisk。
- 完成后,daemon 不再派生 Python ACP driver;`v2/drivers.py` 降为 legacy/debug。

> 切分理由:Rust 原生 ACP ≈ 其余改动之和,且为 greenfield。绑在一版会让整个 v0.8 被它 gate 住。v0.8a 先让 board→route→pty/acp 的体验与单写不变量落地。

---

## 3. 强化 auto:route 要求提交预估,并可解释

`agentcall_route` 参数:

```jsonc
{
  "objective": "…",
  "workspace": "…",
  "mode": "recommend | start",
  "runtime": "auto | pty | acp",
  // mode=recommend 或 runtime=auto 时必填:
  "estimated_minutes": 12,      // 调用方预估时长
  "estimated_loc": 80,          // 调用方预估代码量(或 estimated_files)
  "needs_continuity": false,    // 是否需要多轮/会话续接
  "risk": "low | medium | high"
}
```

- `runtime=auto` 时,daemon router 用 `estimated_minutes` / `estimated_loc` / `needs_continuity` / `risk` **评分**:短小一次性 → ACP;大、长、需续接、高风险 → PTY handoff。
- 缺 `estimated_*` 且 `runtime=auto` → **拒绝并要求补全**(强制调用方先估算,杜绝"盲目 auto")。
- 返回 `recommended_runtime` + `reason` + `score_breakdown`,可解释。
- route scoring 逻辑从 Python `route` 子命令迁入 Rust 核心(评分维度**不得简化**,保留 estimated_files/needs_continuity/risk/phase 等)。

---

## 4. timeout-kill vs auto-kill 边界(消歧)

非目标"不实现 Claude Code 生命周期自动销毁/自动 kill"与 ACP"超时 kill"不矛盾,边界如下:

- ✅ **允许**:kill daemon **自己派生的、有界的 ACP 子进程**(超时回收资源)。这是 daemon 对自有 child 的生命周期管理。
- ❌ **禁止**:auto-kill 一个 live Claude **PTY handoff session**(用户可见的交互会话)。PTY 只能由显式 `session stop` 终止。

一句话:**有界 ACP invocation 可超时回收;交互 PTY 不自动销毁。**

---

## 5. Public Interfaces(含明确的三个写口迁移)

### MCP canonical
`agentcall_board` · `agentcall_route` · `agentcall_session` · `agentcall_session_send` · `agentcall_report`

### daemon API
- `POST /api/routes` / `GET /api/routes/{id}` — route 调度与状态
- `GET /api/board` / `GET /api/sessions/{name}/summary` — 已有
- **新增(关 Python 写口)**:
  - `POST /api/sessions/{name}/checkpoint` ← 取代 `python checkpoint request`(`agentcall_report`/`checkpoint_request` 改 daemon_post)
  - `POST /api/context` ← 取代 `python context create`(`context_packet_create` 改 daemon_post)
  - `POST /api/transcripts/index` ← 取代 `python transcript index`(Python 至多做**只读 JSONL 解析**返回 JSON,daemon 落盘)

完成后 `store.py` 在默认路径上**零写调用**;`.agentcall` state 唯一写入者为 daemon。

### route 返回
- `route(mode=start, runtime=acp)` → ACP invocation handle(`route_id`)
- `route(mode=start, runtime=pty)` → PTY session handle(`session_name`)
- board 同时展示 route / ACP invocation / PTY session / report / attention。

---

## 6. 时间点(废弃与里程碑)

> 以下为建议日期,按实际节奏调整。

| 日期 | 事项 |
|---|---|
| 2026-06-04 | v0.8a 启动;`agentcall_delegate*` 即刻从默认 `tools/list` 隐藏(仅留废弃错误) |
| 2026-06-11 | v0.8a 目标完成:route 统一 + 三写口迁移 + 超时止血 + binding gate 通过 |
| 2026-06-18 | `agentcall_route_task` 只读 alias **标记 deprecated**(文档/capabilities 注明) |
| 2026-06-25 | v0.8b 目标完成:Rust 原生 ACP client,fake-server 测试绿 |
| 2026-07-02 | 移除 `agentcall_route_task` alias 与 `agentcall_delegate*` 兼容 handler;`v2/drivers.py` 降 legacy |

---

## 7. Test Plan(在原 plan 上补充)

- `cargo test --workspace` + `python -m pytest -q` + hook UTF-8 smoke。
- `tools/list` 不含 `agentcall_delegate` / `agentcall_delegate_acp`。
- `route(mode=recommend, runtime=auto)` 返回可解释建议;**缺 `estimated_*` 时被拒**。
- `route(mode=start, runtime=acp)`:**MCP 不直接 shell python**;daemon 持有 invocation,有界、超时可 kill;**包裹的 Python driver 不写 `.agentcall`**。
- `route(mode=start, runtime=pty)`:起 daemon PTY,且 **binding_source=env**(gate),board/summary 可见。
- 三写口:`checkpoint`/`context`/`transcript index` 经 daemon endpoint;`rg` 验证默认路径无 `python -m agentcall (workflow simulate|checkpoint|context|transcript)`。
- 单写不变量:压测期间 `.agentcall/state` 与 `events.ndjson` 仅 daemon PID 持有写句柄。
- fake ACP server(v0.8b):stdout/stderr 并发读、timeout kill、request id 匹配。

---

## 8. 待拍板

- [ ] §6 日期是否按建议落地,还是另定节奏。
- [ ] `estimated_loc` vs `estimated_files` 作为 auto 评分主输入,取哪个(或都收)。
- [ ] v0.8a 中被 daemon 包裹的 Python ACP driver,是否就地改造成"只读返回、不写 state",还是新写一个薄 executor。
- [ ] `context_packet_create` 是并入 session input artifact,还是独立 `/api/context`。
