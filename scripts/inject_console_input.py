from __future__ import annotations

import argparse
import ctypes
import sys
from ctypes import wintypes


STD_INPUT_HANDLE = -10
GENERIC_WRITE = 0x40000000
FILE_SHARE_READ = 0x00000001
FILE_SHARE_WRITE = 0x00000002
OPEN_EXISTING = 3
KEY_EVENT = 0x0001
VK_RETURN = 0x0D


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [
        ("bKeyDown", wintypes.BOOL),
        ("wRepeatCount", wintypes.WORD),
        ("wVirtualKeyCode", wintypes.WORD),
        ("wVirtualScanCode", wintypes.WORD),
        ("UnicodeChar", wintypes.WCHAR),
        ("dwControlKeyState", wintypes.DWORD),
    ]


class INPUT_RECORD(ctypes.Structure):
    _fields_ = [
        ("EventType", wintypes.WORD),
        ("KeyEvent", KEY_EVENT_RECORD),
    ]


def main() -> int:
    if sys.platform != "win32":
        raise SystemExit("console injection is only supported on Windows")

    parser = argparse.ArgumentParser()
    parser.add_argument("--target-pid", type=int, required=True)
    parser.add_argument("--text", required=True)
    parser.add_argument("--enter", action="store_true")
    args = parser.parse_args()

    written = inject(args.target_pid, args.text, args.enter)
    print(f"Injected {written} console input events into PID {args.target_pid}")
    return 0


def inject(pid: int, text: str, enter: bool) -> int:
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.FreeConsole()
    if not kernel32.AttachConsole(wintypes.DWORD(pid)):
        raise_win_error(f"AttachConsole failed for PID {pid}")

    handle = kernel32.GetStdHandle(STD_INPUT_HANDLE)
    if handle in (0, -1):
        handle = kernel32.CreateFileW(
            "CONIN$",
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            0,
            None,
        )
        if handle in (0, -1):
            kernel32.FreeConsole()
            raise_win_error("CreateFile(CONIN$) failed")

    payload = text + ("\r" if enter else "")
    records = (INPUT_RECORD * (len(payload) * 2))()
    index = 0
    for char in payload:
        vk = VK_RETURN if char == "\r" else 0
        records[index] = make_record(char, vk, True)
        records[index + 1] = make_record(char, vk, False)
        index += 2

    written = wintypes.DWORD()
    ok = kernel32.WriteConsoleInputW(handle, records, len(records), ctypes.byref(written))
    if handle not in (0, -1):
        kernel32.CloseHandle(handle)
    kernel32.FreeConsole()
    if not ok:
        raise_win_error("WriteConsoleInput failed")
    return int(written.value)


def make_record(char: str, vk: int, down: bool) -> INPUT_RECORD:
    return INPUT_RECORD(
        KEY_EVENT,
        KEY_EVENT_RECORD(bool(down), 1, vk, 0, char, 0),
    )


def raise_win_error(message: str) -> None:
    code = ctypes.get_last_error()
    raise OSError(code, f"{message}: Win32 error {code}")


if __name__ == "__main__":
    raise SystemExit(main())
