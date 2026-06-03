const ids = {
  workspace: document.querySelector("#workspace"),
  metrics: document.querySelector("#metrics"),
  sessions: document.querySelector("#sessions"),
  claims: document.querySelector("#claims"),
  reports: document.querySelector("#reports"),
  transcripts: document.querySelector("#transcripts"),
  events: document.querySelector("#events"),
  refresh: document.querySelector("#refresh"),
};

async function loadBoard() {
  const res = await fetch("/api/board");
  if (!res.ok) throw new Error(await res.text());
  render(await res.json());
}

function render(data) {
  ids.workspace.textContent = data.workspace || "";
  const sessions = values(data.active_sessions);
  const claims = values(data.file_claims);
  const transcripts = values(data.transcripts);
  const reports = data.reports || [];
  const events = data.recent_events || [];
  ids.metrics.innerHTML = [
    metric("sessions", sessions.length),
    metric("claims", claims.filter((item) => item.status === "active").length),
    metric("reports", reports.length),
    metric("events", events.length),
    metric("transcripts", transcripts.length),
  ].join("");
  ids.sessions.innerHTML = listOrEmpty(sessions, renderSession);
  ids.claims.innerHTML = listOrEmpty(claims, renderClaim);
  ids.reports.innerHTML = listOrEmpty(reports.slice(-12).reverse(), renderReport);
  ids.transcripts.innerHTML = listOrEmpty(transcripts, renderTranscript);
  ids.events.innerHTML = listOrEmpty(events.slice(-30).reverse(), renderEvent);
}

function metric(label, value) {
  return `<div class="metric"><span>${escapeHtml(label)}</span><strong>${value}</strong></div>`;
}

function renderSession(session) {
  return card([
    row("id", session.session_id),
    row("status", badge(session.status)),
    row("agent", session.agent),
    row("pid", session.pid),
    row("transcript", short(session.transcript_path)),
  ]);
}

function renderClaim(claim) {
  return card([
    row("file", claim.file),
    row("status", badge(claim.status)),
    row("session", claim.session_id),
    row("tool", claim.tool_name || claim.last_tool_name),
  ]);
}

function renderReport(report) {
  return card([
    row("call", report.call_id),
    row("agent", report.agent),
    row("status", badge(report.status)),
    `<p>${escapeHtml(report.summary || "")}</p>`,
  ]);
}

function renderTranscript(item) {
  return card([
    row("session", item.session_id),
    row("messages", item.messages),
    row("tools", `${item.tool_uses || 0}/${item.tool_results || 0}`),
    `<p>${escapeHtml(item.last_text || "")}</p>`,
  ]);
}

function renderEvent(event) {
  return card([
    row("event", event.type),
    row("time", event.ts),
    `<p>${escapeHtml(event.message || JSON.stringify(event.data || {}))}</p>`,
  ]);
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
  ids.events.innerHTML = `<article><p>${escapeHtml(err.message)}</p></article>`;
});
setInterval(loadBoard, 5000);
