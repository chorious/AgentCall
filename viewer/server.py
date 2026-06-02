from __future__ import annotations

import argparse
import json
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


class ViewerHandler(BaseHTTPRequestHandler):
    agentapi_url = "http://localhost:3287"
    root = Path(__file__).resolve().parent

    def log_message(self, fmt: str, *args) -> None:
        return

    def do_GET(self) -> None:
        if self.path == "/" or self.path.startswith("/?"):
            self._send_file(self.root / "proxy-index.html", "text/html; charset=utf-8")
            return
        if self.path == "/api/status":
            self._proxy("GET", "/status")
            return
        if self.path == "/api/messages":
            self._proxy("GET", "/messages")
            return
        self.send_error(404)

    def do_POST(self) -> None:
        if self.path == "/api/message":
            self._proxy("POST", "/message")
            return
        self.send_error(404)

    def _send_file(self, path: Path, content_type: str) -> None:
        data = path.read_bytes()
        self.send_response(200)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _proxy(self, method: str, path: str) -> None:
        body = None
        headers = {}
        if method == "POST":
            length = int(self.headers.get("content-length", "0"))
            body = self.rfile.read(length)
            headers["content-type"] = self.headers.get("content-type", "application/json")
        req = urllib.request.Request(
            self.agentapi_url + path,
            data=body,
            headers=headers,
            method=method,
        )
        try:
            with urllib.request.urlopen(req, timeout=30) as res:
                data = res.read()
                content_type = res.headers.get("content-type", "application/json")
                self.send_response(res.status)
                self.send_header("content-type", content_type)
                self.send_header("content-length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
        except urllib.error.HTTPError as exc:
            data = exc.read()
            self.send_response(exc.code)
            self.send_header("content-type", exc.headers.get("content-type", "text/plain"))
            self.end_headers()
            self.wfile.write(data)
        except Exception as exc:
            data = json.dumps({"error": str(exc)}).encode("utf-8")
            self.send_response(502)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=3291)
    parser.add_argument("--agentapi-url", default="http://localhost:3287")
    args = parser.parse_args()
    ViewerHandler.agentapi_url = args.agentapi_url.rstrip("/")
    server = ThreadingHTTPServer(("localhost", args.port), ViewerHandler)
    print(f"AgentCall viewer proxy: http://localhost:{args.port} -> {ViewerHandler.agentapi_url}")
    server.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
