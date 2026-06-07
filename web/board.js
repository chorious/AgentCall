const ids = {
  workspace: document.querySelector("#workspace"),
  metrics: document.querySelector("#metrics"),
  sessions: document.querySelector("#sessions"),
  attention: document.querySelector("#attention"),
  reports: document.querySelector("#reports"),
  routes: document.querySelector("#routes"),
  health: document.querySelector("#health"),
  refresh: document.querySelector("#refresh"),
};

async function loadBoard() {
  const res = await fetch("/api/board?view=compact");
  if (!res.ok) throw new Error(await res.text());
  render(await res.json());
}

function render(data) {
  ids.workspace.textContent = data.workspace || "";
  const sessions = values(data.live_daemon_sessions);
  const attention = values(data.attention);
  const reports = data.reports || [];
  const routes = values(data.routes);
  const health = data.runtime_health || {};
  const hookStatus = health.claude_hook_config_status || {};
  ids.metrics.innerHTML = [
    metric("live sessions", sessions.length),
    metric("attention", attention.length),
    metric("reports", reports.length),
    metric("routes", routes.length),
    metric("stale claims", health.stale_claims || 0),
  ].join("");
  ids.sessions.innerHTML = listOrEmpty(sessions, renderSession);
  ids.attention.innerHTML = listOrEmpty(attention, renderAttention);
  ids.reports.innerHTML = listOrEmpty(reports.slice(-12).reverse(), renderReport);
  ids.routes.innerHTML = listOrEmpty(routes.slice(-10).reverse(), renderRoute);
  ids.health.innerHTML = renderHealth(health, hookStatus);
}

function metric(label, value) {
  return `<div class="metric"><span>${escapeHtml(label)}</span><strong>${value}</strong></div>`;
}

function renderSession(session) {
  return card([
    row("name", session.name || session.session_id),
    row("status", badge(session.status)),
    row("cwd", short(session.cwd)),
    row("updated", formatTime(session.updated_at)),
    row("replay", session.replay_bytes || 0),
  ]);
}

function renderAttention(item) {
  const policy = item.policy_block || {};
  return card([
    row("session", item.session || item.session_name || item.name),
    row("attention", badge(item.attention_status || item.status)),
    row("liveness", badge(item.liveness_status)),
    row("source", item.status_source || item.kind),
    policy.active ? row("policy", `${escapeHtml(policy.category || "-")} x${policy.repeat_count || 0}`) : "",
    policy.reason ? `<p>${escapeHtml(policy.reason)}</p>` : "",
    item.headline ? `<p>${escapeHtml(item.headline)}</p>` : "",
  ]);
}

function renderReport(report) {
  return card([
    row("call", report.call_id || report.task_id || report.path),
    row("agent", report.agent || report.session || "-"),
    row("status", badge(report.status)),
    `<p>${escapeHtml(report.summary || "")}</p>`,
  ]);
}

function renderRoute(route) {
  return card([
    row("route", route.route_id || route.id),
    row("runtime", badge(route.runtime || route.recommended_runtime)),
    row("status", badge(route.status)),
    row("session", route.session_name || "-"),
    `<p>${escapeHtml(route.required_next_step || route.objective || "")}</p>`,
  ]);
}

function renderHealth(health, hookStatus) {
  const warnings = values(health.warnings || hookStatus.warnings);
  const missing = values(hookStatus.missing_events);
  return [
    card([
      row("daemon", badge(health.status || "unknown")),
      row("workspace", short(health.workspace)),
      row("claude cwd", short(health.claude_workspace)),
      row("config", short(health.config_path)),
      row("restart", health.restart_required_after_update ? "required after update" : "not required"),
    ]),
    card([
      row("hooks", badge(hookStatus.has_agentcall_hooks ? "ok" : "missing")),
      row("settings", short(hookStatus.settings_path)),
      row("PostToolBatch", badge(hookStatus.post_tool_batch_enabled ? "enabled" : "missing")),
      row("script", badge(hookStatus.hook_script_exists ? "found" : "missing")),
      row("python", badge(hookStatus.python_command_exists ? "found" : "missing")),
    ]),
    card([
      row("bindings", health.runtime_bindings || 0),
      row("unbound", values(health.unbound_live_sessions).length),
      row("event next", health.event_next || "-"),
      row("missing hooks", missing.length ? missing.join(", ") : "none"),
      warnings.length ? `<p>${escapeHtml(warnings.join(" | "))}</p>` : `<p>No warnings</p>`,
    ]),
  ].join("");
}

function row(label, value) {
  return `<div class="row"><span>${escapeHtml(label)}</span><b>${value == null ? "-" : value}</b></div>`;
}

function badge(value) {
  const label = String(value || "-");
  const cls = label.replace(/[^a-z0-9_-]/gi, "-").toLowerCase();
  return `<em class="badge ${cls}">${escapeHtml(label)}</em>`;
}

function card(lines) {
  return `<article>${lines.join("")}</article>`;
}

function listOrEmpty(items, renderer) {
  if (!items.length) return `<div class="empty">No records</div>`;
  return items.map(renderer).join("");
}

function values(value) {
  if (!value) return [];
  if (Array.isArray(value)) return value;
  return Object.values(value);
}

function short(value) {
  if (!value) return "-";
  const text = String(value);
  return escapeHtml(text.length > 42 ? `...${text.slice(-39)}` : text);
}

function formatTime(value) {
  if (!value) return "-";
  if (typeof value === "number") return new Date(value).toLocaleTimeString();
  return escapeHtml(String(value));
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  }[ch]));
}

ids.refresh.addEventListener("click", loadBoard);
loadBoard().catch((err) => {
  ids.health.innerHTML = `<article><p>${escapeHtml(err.message)}</p></article>`;
});
setInterval(loadBoard, 5000);
