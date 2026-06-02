param(
    [string]$Title,

    [long]$Handle = 0,

    [Parameter(Mandatory = $true)]
    [string]$Text,

    [switch]$Enter
)

$source = @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public class WindowPasteInjector {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int count);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("kernel32.dll")]
    public static extern uint GetCurrentThreadId();

    [DllImport("user32.dll")]
    public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);

    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    public static extern uint SendInput(uint nInputs, INPUT[] pInputs, int cbSize);

    [DllImport("user32.dll", SetLastError=true)]
    public static extern bool PostMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

    public const int SW_RESTORE = 9;
    public const int INPUT_KEYBOARD = 1;
    public const ushort VK_CONTROL = 0x11;
    public const ushort VK_V = 0x56;
    public const ushort VK_RETURN = 0x0D;
    public const uint KEYEVENTF_KEYUP = 0x0002;
    public const uint WM_PASTE = 0x0302;
    public const uint WM_KEYDOWN = 0x0100;
    public const uint WM_KEYUP = 0x0101;

    [StructLayout(LayoutKind.Sequential)]
    public struct INPUT {
        public int type;
        public KEYBDINPUT ki;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct KEYBDINPUT {
        public ushort wVk;
        public ushort wScan;
        public uint dwFlags;
        public uint time;
        public IntPtr dwExtraInfo;
    }

    public static IntPtr FindWindowByTitle(string title) {
        IntPtr found = IntPtr.Zero;
        string wanted = (title ?? "").Trim();
        EnumWindows(delegate(IntPtr hWnd, IntPtr lParam) {
            if (!IsWindowVisible(hWnd)) return true;
            var sb = new StringBuilder(512);
            GetWindowText(hWnd, sb, sb.Capacity);
            string current = sb.ToString().Trim();
            if (
                String.Equals(current, wanted, StringComparison.OrdinalIgnoreCase) ||
                current.IndexOf(wanted, StringComparison.OrdinalIgnoreCase) >= 0
            ) {
                found = hWnd;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    static INPUT Key(ushort vk, bool up) {
        return new INPUT {
            type = INPUT_KEYBOARD,
            ki = new KEYBDINPUT {
                wVk = vk,
                wScan = 0,
                dwFlags = up ? KEYEVENTF_KEYUP : 0,
                time = 0,
                dwExtraInfo = IntPtr.Zero
            }
        };
    }

    public static int PasteToWindow(string title, long handle, bool enter) {
        IntPtr hWnd = handle != 0 ? new IntPtr(handle) : FindWindowByTitle(title);
        if (hWnd == IntPtr.Zero) return -1;
        ShowWindow(hWnd, SW_RESTORE);
        bool foregroundOk = SetForegroundWindow(hWnd);
        if (!foregroundOk) {
            uint targetPid;
            uint targetThread = GetWindowThreadProcessId(hWnd, out targetPid);
            IntPtr fg = GetForegroundWindow();
            uint fgPid;
            uint fgThread = GetWindowThreadProcessId(fg, out fgPid);
            uint currentThread = GetCurrentThreadId();
            AttachThreadInput(currentThread, targetThread, true);
            if (fgThread != 0) AttachThreadInput(currentThread, fgThread, true);
            foregroundOk = SetForegroundWindow(hWnd);
            if (fgThread != 0) AttachThreadInput(currentThread, fgThread, false);
            AttachThreadInput(currentThread, targetThread, false);
        }
        if (!foregroundOk) return -2;
        System.Threading.Thread.Sleep(500);

        var events = new List<INPUT>();
        events.Add(Key(VK_CONTROL, false));
        events.Add(Key(VK_V, false));
        events.Add(Key(VK_V, true));
        events.Add(Key(VK_CONTROL, true));
        if (enter) {
            events.Add(Key(VK_RETURN, false));
            events.Add(Key(VK_RETURN, true));
        }

        uint sent = SendInput((uint)events.Count, events.ToArray(), Marshal.SizeOf(typeof(INPUT)));
        return sent == events.Count ? 0 : (int)sent;
    }

    public static int PostPasteToWindow(string title, long handle, bool enter) {
        IntPtr hWnd = handle != 0 ? new IntPtr(handle) : FindWindowByTitle(title);
        if (hWnd == IntPtr.Zero) return -1;
        ShowWindow(hWnd, SW_RESTORE);
        if (!PostMessage(hWnd, WM_PASTE, IntPtr.Zero, IntPtr.Zero)) return -3;
        if (enter) {
            System.Threading.Thread.Sleep(100);
            PostMessage(hWnd, WM_KEYDOWN, new IntPtr(VK_RETURN), IntPtr.Zero);
            PostMessage(hWnd, WM_KEYUP, new IntPtr(VK_RETURN), IntPtr.Zero);
        }
        return 0;
    }
}
"@

Add-Type -TypeDefinition $source
Set-Clipboard -Value $Text
$result = [WindowPasteInjector]::PasteToWindow($Title, [int64]$Handle, [bool]$Enter)
if ($result -eq -2) {
    $result = [WindowPasteInjector]::PostPasteToWindow($Title, [int64]$Handle, [bool]$Enter)
}
if ($result -ne 0) {
    Write-Error "Window paste injection failed for '$Title' handle=$Handle with result $result"
    exit 1
}

Write-Output "Pasted prompt into window '$Title' handle=$Handle"
