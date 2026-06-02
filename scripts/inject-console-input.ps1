param(
    [Parameter(Mandatory = $true)]
    [int]$TargetPid,

    [Parameter(Mandatory = $true)]
    [string]$Text,

    [switch]$Enter
)

$source = @"
using System;
using System.Runtime.InteropServices;

public class ConsoleInputInjector {
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool AttachConsole(uint dwProcessId);

    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool FreeConsole();

    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern IntPtr GetStdHandle(int nStdHandle);

    [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    public static extern IntPtr CreateFile(
        string lpFileName,
        uint dwDesiredAccess,
        uint dwShareMode,
        IntPtr lpSecurityAttributes,
        uint dwCreationDisposition,
        uint dwFlagsAndAttributes,
        IntPtr hTemplateFile
    );

    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool CloseHandle(IntPtr hObject);

    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool WriteConsoleInput(
        IntPtr hConsoleInput,
        INPUT_RECORD[] lpBuffer,
        uint nLength,
        out uint lpNumberOfEventsWritten
    );

    public const int STD_INPUT_HANDLE = -10;
    public const uint GENERIC_WRITE = 0x40000000;
    public const uint FILE_SHARE_READ = 0x00000001;
    public const uint FILE_SHARE_WRITE = 0x00000002;
    public const uint OPEN_EXISTING = 3;
    public const ushort KEY_EVENT = 0x0001;
    public const ushort VK_RETURN = 0x0D;

    [StructLayout(LayoutKind.Sequential)]
    public struct INPUT_RECORD {
        public ushort EventType;
        public KEY_EVENT_RECORD KeyEvent;
    }

    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    public struct KEY_EVENT_RECORD {
        [MarshalAs(UnmanagedType.Bool)]
        public bool bKeyDown;
        public ushort wRepeatCount;
        public ushort wVirtualKeyCode;
        public ushort wVirtualScanCode;
        public char UnicodeChar;
        public uint dwControlKeyState;
    }

    public static int Inject(uint pid, string text, bool sendEnter, out int written) {
        written = 0;
        FreeConsole();
        if (!AttachConsole(pid)) {
            return Marshal.GetLastWin32Error();
        }

        IntPtr input = GetStdHandle(STD_INPUT_HANDLE);
        if (input == IntPtr.Zero || input.ToInt64() == -1) {
            input = CreateFile(
                "CONIN$",
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                IntPtr.Zero,
                OPEN_EXISTING,
                0,
                IntPtr.Zero
            );
            if (input == IntPtr.Zero || input.ToInt64() == -1) {
                int err = Marshal.GetLastWin32Error();
                FreeConsole();
                return err == 0 ? -1 : err;
            }
        }

        string payload = sendEnter ? text + "\r" : text;
        INPUT_RECORD[] records = new INPUT_RECORD[payload.Length * 2];
        int idx = 0;
        foreach (char ch in payload) {
            ushort vk = ch == '\r' ? VK_RETURN : (ushort)0;

            records[idx] = new INPUT_RECORD {
                EventType = KEY_EVENT,
                KeyEvent = new KEY_EVENT_RECORD {
                    bKeyDown = true,
                    wRepeatCount = 1,
                    wVirtualKeyCode = vk,
                    wVirtualScanCode = 0,
                    UnicodeChar = ch,
                    dwControlKeyState = 0
                }
            };
            idx++;

            records[idx] = new INPUT_RECORD {
                EventType = KEY_EVENT,
                KeyEvent = new KEY_EVENT_RECORD {
                    bKeyDown = false,
                    wRepeatCount = 1,
                    wVirtualKeyCode = vk,
                    wVirtualScanCode = 0,
                    UnicodeChar = ch,
                    dwControlKeyState = 0
                }
            };
            idx++;
        }

        uint eventsWritten;
        bool ok = WriteConsoleInput(input, records, (uint)records.Length, out eventsWritten);
        written = (int)eventsWritten;
        int lastError = ok ? 0 : Marshal.GetLastWin32Error();
        CloseHandle(input);
        FreeConsole();
        return lastError;
    }
}
"@

Add-Type -TypeDefinition $source
$written = 0
$err = [ConsoleInputInjector]::Inject([uint32]$TargetPid, $Text, [bool]$Enter, [ref]$written)
if ($err -ne 0) {
    Write-Error "Console injection failed for PID $TargetPid with Win32 error $err"
    exit 1
}

Write-Output "Injected $written console input events into PID $TargetPid"
