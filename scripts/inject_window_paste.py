from __future__ import annotations

import argparse
import ctypes
import time
import sys
from ctypes import wintypes


SW_RESTORE = 9
INPUT_KEYBOARD = 1
VK_CONTROL = 0x11
VK_V = 0x56
VK_RETURN = 0x0D
KEYEVENTF_KEYUP = 0x0002
WM_PASTE = 0x0302
WM_KEYDOWN = 0x0100
WM_KEYUP = 0x0101


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", wintypes.WORD),
        ("wScan", wintypes.WORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", wintypes.LPARAM),
    ]


class INPUT(ctypes.Structure):
    _fields_ = [
        ("type", wintypes.DWORD),
        ("ki", KEYBDINPUT),
    ]


def main() -> int:
    if sys.platform != "win32":
        raise SystemExit("window paste injection is only supported on Windows")

    parser = argparse.ArgumentParser()
    parser.add_argument("--title")
    parser.add_argument("--handle", type=lambda value: int(value, 0), default=0)
    parser.add_argument("--text", required=True)
    parser.add_argument("--enter", action="store_true")
    args = parser.parse_args()

    set_clipboard(args.text)
    hwnd = wintypes.HWND(args.handle) if args.handle else find_window(args.title or "")
    if not hwnd:
        raise SystemExit(f"Window not found: title={args.title!r} handle={args.handle}")

    result = paste_to_window(hwnd, args.enter)
    if result != 0:
        result = post_paste_to_window(hwnd, args.enter)
    if result != 0:
        raise SystemExit(f"Window paste injection failed with result {result}")

    print(f"Pasted prompt into window title={args.title!r} handle={args.handle}")
    return 0


def find_window(title: str) -> wintypes.HWND:
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    wanted = title.strip().lower()
    found = wintypes.HWND(0)

    enum_proc_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    def callback(hwnd: wintypes.HWND, _lparam: wintypes.LPARAM) -> bool:
        nonlocal found
        if not user32.IsWindowVisible(hwnd):
            return True
        buffer = ctypes.create_unicode_buffer(512)
        user32.GetWindowTextW(hwnd, buffer, 512)
        current = buffer.value.strip().lower()
        if current == wanted or wanted in current:
            found = hwnd
            return False
        return True

    user32.EnumWindows(enum_proc_type(callback), 0)
    return found


def paste_to_window(hwnd: wintypes.HWND, enter: bool) -> int:
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.ShowWindow(hwnd, SW_RESTORE)
    if not user32.SetForegroundWindow(hwnd):
        return -2
    time.sleep(0.5)
    inputs = [
        key(VK_CONTROL, False),
        key(VK_V, False),
        key(VK_V, True),
        key(VK_CONTROL, True),
    ]
    if enter:
        inputs.extend([key(VK_RETURN, False), key(VK_RETURN, True)])
    array = (INPUT * len(inputs))(*inputs)
    sent = user32.SendInput(len(inputs), array, ctypes.sizeof(INPUT))
    return 0 if sent == len(inputs) else int(sent)


def post_paste_to_window(hwnd: wintypes.HWND, enter: bool) -> int:
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.ShowWindow(hwnd, SW_RESTORE)
    if not user32.PostMessageW(hwnd, WM_PASTE, 0, 0):
        return -3
    if enter:
        time.sleep(0.1)
        user32.PostMessageW(hwnd, WM_KEYDOWN, VK_RETURN, 0)
        user32.PostMessageW(hwnd, WM_KEYUP, VK_RETURN, 0)
    return 0


def key(vk: int, up: bool) -> INPUT:
    return INPUT(INPUT_KEYBOARD, KEYBDINPUT(vk, 0, KEYEVENTF_KEYUP if up else 0, 0, 0))


def set_clipboard(text: str) -> None:
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    if not user32.OpenClipboard(None):
        raise_win_error("OpenClipboard failed")
    try:
        user32.EmptyClipboard()
        data = text.encode("utf-16-le") + b"\x00\x00"
        handle = kernel32.GlobalAlloc(0x0002, len(data))
        if not handle:
            raise_win_error("GlobalAlloc failed")
        locked = kernel32.GlobalLock(handle)
        ctypes.memmove(locked, data, len(data))
        kernel32.GlobalUnlock(handle)
        if not user32.SetClipboardData(13, handle):
            raise_win_error("SetClipboardData failed")
    finally:
        user32.CloseClipboard()


def raise_win_error(message: str) -> None:
    code = ctypes.get_last_error()
    raise OSError(code, f"{message}: Win32 error {code}")


if __name__ == "__main__":
    raise SystemExit(main())
