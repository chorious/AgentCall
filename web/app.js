const grid = document.querySelector("#paneGrid");
const tabs = document.querySelector("#sessionTabs");
const launcher = document.querySelector("#launcher");
const paneMap = new Map();
let visibleNames = "";

function splitCommand(text) {
  const matches = text.match(/"[^"]+"|'[^']+'|\S+/g) || [];
  return matches.map((part) => part.replace(/^['"]|['"]$/g, ""));
}

async function api(path, options = {}) {
  const res = await fetch(path, options);
  if (!res.ok) {
    const text = await res.text();
    throw new Error(text);
  }
  return res.json();
}

function chooseVisible(sessions) {
  const running = sessions.filter((session) => session.status === "running");
  const rest = sessions.filter((session) => session.status !== "running");
  return [...running, ...rest].slice(0, 4);
}

async function refreshSessions() {
  const data = await api("/api/sessions");
  const sessions = data || [];
  renderTabs(sessions);
  ensurePanes(chooseVisible(sessions));
  updateMeta(chooseVisible(sessions));
}

function renderTabs(sessions) {
  tabs.innerHTML = sessions.map((session) => (
    `<button class="tab ${session.status}" title="${escapeHtml(session.command.join(" "))}">${escapeHtml(session.name)}</button>`
  )).join("");
}

function ensurePanes(sessions) {
  const names = sessions.map((session) => session.name).join("\u0000");
  if (names === visibleNames) return;
  visibleNames = names;
  for (const entry of paneMap.values()) {
    entry.socket.close();
    entry.resizeObserver.disconnect();
    entry.term.dispose();
  }
  paneMap.clear();
  grid.innerHTML = "";
  for (let index = 0; index < 4; index += 1) {
    const session = sessions[index];
    if (!session) {
      const empty = document.createElement("article");
      empty.className = "pane empty";
      empty.textContent = "No session";
      grid.appendChild(empty);
      continue;
    }
    createPane(session);
  }
}

function createPane(session) {
  const pane = document.createElement("article");
  pane.className = "pane";
  pane.innerHTML = `
    <div class="pane-head">
      <span class="icon">›</span>
      <span class="title"></span>
      <span class="grow"></span>
      <span class="meta"></span>
      <span class="state"></span>
    </div>
    <div class="terminal-host"></div>
    <form class="composer">
      <input autocomplete="off" placeholder="send to ${escapeHtml(session.name)}">
      <button>send</button>
    </form>
  `;
  grid.appendChild(pane);

  const term = new Terminal({
    cursorBlink: true,
    convertEol: false,
    fontFamily: 'Consolas, "Cascadia Mono", "Microsoft YaHei UI", monospace',
    fontSize: 14,
    lineHeight: 1.12,
    theme: {
      background: "#050505",
      foreground: "#eeeeee",
      cursor: "#ffffff",
      selectionBackground: "#4b4b4b",
      black: "#050505",
      red: "#ff5d73",
      green: "#2ee179",
      yellow: "#ffd166",
      blue: "#56a8ff",
      magenta: "#d787ff",
      cyan: "#70e0e0",
      white: "#eeeeee",
      brightBlack: "#777777",
      brightRed: "#ff7a8a",
      brightGreen: "#5df0a0",
      brightYellow: "#ffe08a",
      brightBlue: "#7bbcff",
      brightMagenta: "#e0a0ff",
      brightCyan: "#9ff5f5",
      brightWhite: "#ffffff"
    }
  });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open(pane.querySelector(".terminal-host"));
  fit.fit();
  term.focus();

  const socket = createTerminalSocket(session.name, term);
  term.onData((data) => {
    socket.send({ type: "input", data });
  });

  const form = pane.querySelector(".composer");
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const input = form.querySelector("input");
    const text = input.value;
    if (!text.trim()) return;
    input.value = "";
    socket.send({ type: "input", data: `${text}\r` });
    term.focus();
  });

  const resizeObserver = new ResizeObserver(() => {
    fit.fit();
    socket.send({ type: "resize", cols: term.cols, rows: term.rows });
  });
  resizeObserver.observe(pane.querySelector(".terminal-host"));

  paneMap.set(session.name, { pane, term, fit, socket, resizeObserver });
}

function createTerminalSocket(sessionName, term) {
  const protocol = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${protocol}://${location.host}/api/sessions/${encodeURIComponent(sessionName)}/ws`);
  const pending = [];

  ws.onopen = () => {
    while (pending.length) {
      ws.send(JSON.stringify(pending.shift()));
    }
  };
  ws.onmessage = (message) => {
    const event = JSON.parse(message.data);
    if (event.kind === "output" || event.kind === "replay") {
      term.write(event.data);
    }
  };

  return {
    send(message) {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(message));
      } else if (ws.readyState === WebSocket.CONNECTING) {
        pending.push(message);
      }
    },
    close() {
      ws.close();
    }
  };
}

function updateMeta(sessions) {
  for (const session of sessions) {
    const entry = paneMap.get(session.name);
    if (!entry) continue;
    entry.pane.querySelector(".title").textContent = session.name;
    entry.pane.querySelector(".meta").textContent = session.command.join(" ");
    const state = entry.pane.querySelector(".state");
    state.textContent = session.status;
    state.className = `state ${session.status}`;
  }
}

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (ch) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    "\"": "&quot;",
    "'": "&#39;"
  }[ch]));
}

launcher.addEventListener("submit", async (event) => {
  event.preventDefault();
  const name = document.querySelector("#sessionName").value.trim();
  const command = splitCommand(document.querySelector("#sessionCommand").value);
  if (!name || command.length === 0) return;
  await api("/api/sessions", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name, command, cols: 100, rows: 36 })
  });
  await refreshSessions();
});

refreshSessions();
setInterval(refreshSessions, 3000);
