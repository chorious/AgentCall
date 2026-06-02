from __future__ import annotations

import argparse
import json
import re
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


ANSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)")


class TmuxHandler(BaseHTTPRequestHandler):
    workspace = Path.cwd()
    web_root = Path(__file__).resolve().parent

    def log_message(self, fmt: str, *args) -> None:
        return

    @property
    def sessions_dir(self) -> Path:
        return self.workspace / ".agentcall" / "sessions"

    def do_GET(self) -> None:
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path == "/" or parsed.path == "/tmux":
            self._send_file(self.web_root / "tmux.html", "text/html; charset=utf-8")
            return
        if parsed.path == "/api/sessions":
            self._send_json({"sessions": self._sessions()})
            return
        if parsed.path.startswith("/api/sessions/") and parsed.path.endswith("/tail"):
            name = self._session_name(parsed.path, suffix="/tail")
            query = urllib.parse.parse_qs(parsed.query)
            lines = int(query.get("lines", ["180"])[0])
            self._send_json(self._tail(name, lines=lines))
            return
        self.send_error(404)

    def do_POST(self) -> None:
        parsed = urllib.parse.urlparse(self.path)
        if parsed.path.startswith("/api/sessions/") and parsed.path.endswith("/send"):
            name = self._session_name(parsed.path, suffix="/send")
            body = self._read_json()
            text = str(body.get("text", ""))
            enter = bool(body.get("enter", True))
            self._queue_input(name, text, enter=enter)
            self._send_json({"ok": True})
            return
        if parsed.path.startswith("/api/sessions/") and parsed.path.endswith("/stop"):
            name = self._session_name(parsed.path, suffix="/stop")
            self._queue_event(name, {"type": "stop"})
            self._send_json({"ok": True})
            return
        self.send_error(404)

    def _send_file(self, path: Path, content_type: str) -> None:
        data = path.read_bytes()
        self.send_response(200)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _send_json(self, data: object, status: int = 200) -> None:
        payload = json.dumps(data, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json; charset=utf-8")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _read_json(self) -> dict[str, object]:
        length = int(self.headers.get("content-length", "0"))
        if not length:
            return {}
        return json.loads(self.rfile.read(length).decode("utf-8"))

    def _session_name(self, path: str, *, suffix: str) -> str:
        encoded = path.removeprefix("/api/sessions/").removesuffix(suffix)
        return urllib.parse.unquote(encoded)

    def _sessions(self) -> list[dict[str, object]]:
        if not self.sessions_dir.exists():
            return []
        records = []
        for state_path in sorted(self.sessions_dir.glob("*/state.json")):
            try:
                data = json.loads(state_path.read_text(encoding="utf-8"))
            except Exception:
                continue
            output_path = state_path.parent / "output.log"
            data["output_bytes"] = output_path.stat().st_size if output_path.exists() else 0
            records.append(data)
        records.sort(key=lambda item: (item.get("status") != "running", str(item.get("name", ""))))
        return records

    def _tail(self, name: str, *, lines: int) -> dict[str, object]:
        self._require_safe_name(name)
        path = self.sessions_dir / name / "output.log"
        text = ""
        if path.exists():
            raw = path.read_text(encoding="utf-8", errors="replace")
            text = "\n".join(strip_terminal_noise(raw).splitlines()[-lines:])
        return {"name": name, "text": text}

    def _queue_input(self, name: str, text: str, *, enter: bool) -> None:
        payload = text + ("\r" if enter else "")
        self._queue_event(name, {"type": "input", "text": payload})

    def _queue_event(self, name: str, event: dict[str, object]) -> None:
        self._require_safe_name(name)
        session_dir = self.sessions_dir / name
        if not (session_dir / "state.json").exists():
            raise FileNotFoundError(name)
        input_path = session_dir / "input.ndjson"
        with input_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(event, ensure_ascii=False) + "\n")

    def _require_safe_name(self, name: str) -> None:
        if not name or "/" in name or "\\" in name or ".." in name:
            raise ValueError(f"Unsafe session name: {name}")


def strip_terminal_noise(text: str) -> str:
    text = ANSI_RE.sub("", text)
    text = text.replace("\x00", "")
    text = re.sub(r"\n{4,}", "\n\n\n", text)
    return text


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=3292)
    parser.add_argument("--workspace", default=".")
    args = parser.parse_args()
    TmuxHandler.workspace = Path(args.workspace).resolve()
    server = ThreadingHTTPServer(("localhost", args.port), TmuxHandler)
    print(f"AgentCall tmux viewer: http://localhost:{args.port}")
    server.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
