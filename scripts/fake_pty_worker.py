from __future__ import annotations

import sys


def main() -> int:
    configure_stdio()
    print("AGENTCALL_FAKE_READY", flush=True)
    for raw in sys.stdin:
        text = raw.rstrip("\r\n")
        print(f"AGENTCALL_FAKE_INPUT chars={len(text)}", flush=True)
        if "AGENTCALL_SMOKE_PING" in text:
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


if __name__ == "__main__":
    raise SystemExit(main())
