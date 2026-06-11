from __future__ import annotations

import argparse
from pathlib import Path
import sys


def main() -> int:
    configure_stdio()
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", default=None)
    args = parser.parse_args()
    report_path = args.report
    print("AGENTCALL_FAKE_READY", flush=True)
    for raw in sys.stdin:
        text = raw.rstrip("\r\n")
        report_path = report_path or report_path_from_prompt(text)
        print(f"AGENTCALL_FAKE_INPUT chars={len(text)}", flush=True)
        if "AGENTCALL_SMOKE_PING" in text:
            if report_path:
                write_report(Path(report_path), text)
            print("AGENTCALL_SMOKE_PONG", flush=True)
        if "AGENTCALL_SMOKE_EXIT" in text:
            print("AGENTCALL_FAKE_BYE", flush=True)
            return 0
    print("AGENTCALL_FAKE_EOF", flush=True)
    return 0


def configure_stdio() -> None:
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except AttributeError:
            pass


def write_report(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "\n".join(
            [
                "# AgentCall Fake Worker Smoke Report",
                "",
                "status: completed",
                "summary: deterministic fake PTY worker accepted actor input and wrote a report.",
                "verdict: pass",
                f"evidence: received input containing {len(text)} characters.",
                "files_read: []",
                f"changed_files: [{path.as_posix()}]",
                "risks: []",
                "next_recommended_action: accept_report",
                "context_sufficiency: sufficient",
                "",
            ]
        ),
        encoding="utf-8",
    )


def report_path_from_prompt(text: str) -> str | None:
    marker = "Write the final report to `"
    start = text.find(marker)
    if start < 0:
        return None
    start += len(marker)
    end = text.find("`", start)
    if end <= start:
        return None
    return text[start:end]


if __name__ == "__main__":
    raise SystemExit(main())
