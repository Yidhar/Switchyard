param(
    [string]$WorkspaceRoot = "E:\Switchyard",
    [string]$Message = "Say exactly: hello switchyard",
    [int]$TimeoutSec = 30,
    [string]$OutDir = "",
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($OutDir)) {
    $OutDir = Join-Path $WorkspaceRoot (".switchyard\smoke\tui-live-smoke-" + (Get-Date -Format 'yyyyMMdd-HHmmss'))
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

if (-not $SkipBuild) {
    Push-Location $WorkspaceRoot
    try {
        $env:PATH = 'C:\Users\yidhar\.cargo\bin;' + $env:PATH
        cargo build -q -p switchyard-tui
    }
    finally {
        Pop-Location
    }
}

$tuiExe = Join-Path $WorkspaceRoot 'target\debug\switchyard-tui.exe'
if (-not (Test-Path $tuiExe)) {
    throw "missing binary: $tuiExe"
}

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class SwitchyardConsoleBridge {
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool FreeConsole();
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AttachConsole(uint pid);
    [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    public static extern IntPtr CreateFile(string name, uint access, uint share, IntPtr sa, uint create, uint flags, IntPtr tmpl);
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool WriteConsoleInputW(IntPtr h, INPUT_RECORD[] recs, uint len, out uint written);
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool GetConsoleScreenBufferInfo(IntPtr h, out CONSOLE_SCREEN_BUFFER_INFO info);
    [DllImport("kernel32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    public static extern bool ReadConsoleOutputW(
        IntPtr h,
        [Out] CHAR_INFO[] buffer,
        COORD bufferSize,
        COORD bufferCoord,
        ref SMALL_RECT readRegion
    );
    [DllImport("user32.dll")] public static extern short VkKeyScan(char ch);

    public const uint GENERIC_READ = 0x80000000;
    public const uint GENERIC_WRITE = 0x40000000;
    public const uint FILE_SHARE_READ = 1;
    public const uint FILE_SHARE_WRITE = 2;
    public const uint OPEN_EXISTING = 3;
    public const short KEY_EVENT = 1;
    public const ushort COMMON_LVB_LEADING_BYTE = 0x0100;
    public const ushort COMMON_LVB_TRAILING_BYTE = 0x0200;

    [StructLayout(LayoutKind.Sequential)]
    public struct COORD { public short X; public short Y; public COORD(short x, short y){ X = x; Y = y; } }

    [StructLayout(LayoutKind.Sequential)]
    public struct SMALL_RECT { public short Left; public short Top; public short Right; public short Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    public struct CONSOLE_SCREEN_BUFFER_INFO {
        public COORD dwSize;
        public COORD dwCursorPosition;
        public short wAttributes;
        public SMALL_RECT srWindow;
        public COORD dwMaximumWindowSize;
    }

    [StructLayout(LayoutKind.Explicit, CharSet=CharSet.Unicode)]
    public struct CHAR_UNION {
        [FieldOffset(0)] public char UnicodeChar;
        [FieldOffset(0)] public byte AsciiChar;
    }

    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    public struct CHAR_INFO {
        public CHAR_UNION Char;
        public ushort Attributes;
    }

    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    public struct INPUT_RECORD { public short EventType; public KEY_EVENT_RECORD KeyEvent; }

    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    public struct KEY_EVENT_RECORD {
        [MarshalAs(UnmanagedType.Bool)] public bool bKeyDown;
        public ushort wRepeatCount;
        public ushort wVirtualKeyCode;
        public ushort wVirtualScanCode;
        public char UnicodeChar;
        public uint dwControlKeyState;
    }

    public static INPUT_RECORD[] Key(ushort vk, char ch) {
        return new INPUT_RECORD[] {
            new INPUT_RECORD { EventType = KEY_EVENT, KeyEvent = new KEY_EVENT_RECORD { bKeyDown = true,  wRepeatCount = 1, wVirtualKeyCode = vk, UnicodeChar = ch } },
            new INPUT_RECORD { EventType = KEY_EVENT, KeyEvent = new KEY_EVENT_RECORD { bKeyDown = false, wRepeatCount = 1, wVirtualKeyCode = vk, UnicodeChar = ch } },
        };
    }

    public static INPUT_RECORD[] CharKey(char ch) {
        ushort vk = 0;
        try { vk = (ushort)(VkKeyScan(ch) & 0xff); } catch {}
        return Key(vk, ch);
    }

    static bool IsPhantomControl(char ch) {
        return char.IsControl(ch) && ch != '\t' && ch != '\n' && ch != '\r';
    }

    static string TrimCaptureLine(string value) {
        if (string.IsNullOrEmpty(value)) {
            return string.Empty;
        }

        int end = value.Length;
        while (end > 0) {
            char ch = value[end - 1];
            if (ch == ' ' || ch == '\0' || ch == '\uFFFD') {
                end--;
                continue;
            }
            break;
        }

        return end == value.Length ? value : value.Substring(0, end);
    }

    public static string[] CaptureVisibleText(IntPtr h) {
        if (!GetConsoleScreenBufferInfo(h, out CONSOLE_SCREEN_BUFFER_INFO info)) {
            throw new InvalidOperationException("GetConsoleScreenBufferInfo failed: " + Marshal.GetLastWin32Error());
        }

        int width = Math.Max(1, info.srWindow.Right - info.srWindow.Left + 1);
        int height = Math.Max(1, info.srWindow.Bottom - info.srWindow.Top + 1);
        var buffer = new CHAR_INFO[width * height];
        var region = info.srWindow;

        if (!ReadConsoleOutputW(
            h,
            buffer,
            new COORD((short)width, (short)height),
            new COORD(0, 0),
            ref region
        )) {
            throw new InvalidOperationException("ReadConsoleOutputW failed: " + Marshal.GetLastWin32Error());
        }

        var lines = new string[height];
        for (int row = 0; row < height; row++) {
            var line = new StringBuilder(width);
            for (int col = 0; col < width; col++) {
                var cell = buffer[(row * width) + col];
                if ((cell.Attributes & COMMON_LVB_TRAILING_BYTE) != 0) {
                    continue;
                }

                char ch = cell.Char.UnicodeChar;
                if (ch == '\0' || ch == '\uFFFD' || IsPhantomControl(ch)) {
                    line.Append(' ');
                } else {
                    line.Append(ch);
                }
            }
            lines[row] = TrimCaptureLine(line.ToString());
        }

        return lines;
    }
}
"@

$script:Process = $null
$script:ConsoleIn = [IntPtr]::Zero
$script:ConsoleOut = [IntPtr]::Zero

function Attach-ToTuiConsole {
    if (-not $script:Process -or $script:Process.HasExited) {
        throw 'switchyard-tui process is not running'
    }
    [SwitchyardConsoleBridge]::FreeConsole() | Out-Null
    if (-not [SwitchyardConsoleBridge]::AttachConsole([uint32]$script:Process.Id)) {
        throw "AttachConsole failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    $script:ConsoleIn = [SwitchyardConsoleBridge]::CreateFile('CONIN$', [SwitchyardConsoleBridge]::GENERIC_READ -bor [SwitchyardConsoleBridge]::GENERIC_WRITE, [SwitchyardConsoleBridge]::FILE_SHARE_READ -bor [SwitchyardConsoleBridge]::FILE_SHARE_WRITE, [IntPtr]::Zero, [SwitchyardConsoleBridge]::OPEN_EXISTING, 0, [IntPtr]::Zero)
    $script:ConsoleOut = [SwitchyardConsoleBridge]::CreateFile('CONOUT$', [SwitchyardConsoleBridge]::GENERIC_READ -bor [SwitchyardConsoleBridge]::GENERIC_WRITE, [SwitchyardConsoleBridge]::FILE_SHARE_READ -bor [SwitchyardConsoleBridge]::FILE_SHARE_WRITE, [IntPtr]::Zero, [SwitchyardConsoleBridge]::OPEN_EXISTING, 0, [IntPtr]::Zero)
}

function Send-Records([SwitchyardConsoleBridge+INPUT_RECORD[]]$Records) {
    [uint32]$written = 0
    [void][SwitchyardConsoleBridge]::WriteConsoleInputW($script:ConsoleIn, $Records, [uint32]$Records.Length, [ref]$written)
}

function Send-Char([char]$Char) {
    Send-Records ([SwitchyardConsoleBridge]::CharKey($Char))
}

function Send-Vk([UInt16]$Vk, [char]$Char = [char]0) {
    Send-Records ([SwitchyardConsoleBridge]::Key($Vk, $Char))
}

function Send-Text([string]$Text) {
    foreach ($ch in $Text.ToCharArray()) {
        Send-Char $ch
    }
}

function Read-ConsoleText {
    return ,([SwitchyardConsoleBridge]::CaptureVisibleText($script:ConsoleOut))
}

function Assert-CaptureClean([string]$Text, [string]$Name) {
    if ($Text.IndexOf([char]0xFFFD) -ge 0) {
        throw "$Name contains replacement characters (console capture artifact)."
    }

    $badControl = $false
    foreach ($ch in $Text.ToCharArray()) {
        if ([char]::IsControl($ch) -and $ch -notin @([char]9, [char]10, [char]13)) {
            $badControl = $true
            break
        }
    }

    if ($badControl) {
        throw "$Name contains unexpected control characters."
    }
}

function Save-Capture([string]$Name) {
    $path = Join-Path $OutDir $Name
    $text = (Read-ConsoleText) -join [Environment]::NewLine
    Assert-CaptureClean -Text $text -Name $Name
    Set-Content -Path $path -Value $text -Encoding UTF8
    return $text
}

function Wait-ForMatch([string]$Pattern, [int]$Seconds, [string]$CaptureName) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        $text = Save-Capture $CaptureName
        if ($text -match $Pattern) {
            return $text
        }
        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)
    throw "timeout waiting for pattern: $Pattern"
}

function Assert-Match([string]$Text, [string]$Pattern, [string]$Message) {
    if ($Text -notmatch $Pattern) {
        throw $Message
    }
}

$script:Process = Start-Process -FilePath $tuiExe -WorkingDirectory $WorkspaceRoot -PassThru
Start-Sleep -Seconds 2
Attach-ToTuiConsole

try {
    $startup = Save-Capture 'startup.txt'
    Assert-Match $startup 'Type a message and press Enter' 'startup screen missing input prompt'

    Send-Text $Message
    Send-Vk 0x0D ([char]13)

    $overview = Wait-ForMatch 'hello switchyard' $TimeoutSec 'overview-after-submit.txt'
    Assert-Match $overview 'hello switchyard' 'overview missing assistant response'
    $overview = Wait-ForMatch 'phase: 空闲' $TimeoutSec 'overview-after-idle.txt'

    Send-Vk 0x09 ([char]9)  # Tab -> transcript
    Start-Sleep -Milliseconds 200
    Send-Char '2'          # provider tab
    Start-Sleep -Milliseconds 400
    $provider = Save-Capture 'provider-tab.txt'
    Assert-Match $provider 'codex 屏幕镜像 \(PIPE\)' 'provider tab did not show PIPE screen mode'
    Assert-Match $provider 'thread.started' 'provider screen view missing protocol content'

    Send-Char 'r'
    Start-Sleep -Milliseconds 300
    $raw = Save-Capture 'provider-raw.txt'
    Assert-Match $raw 'codex 原始输出 \(PIPE\)' 'raw mode title missing'
    Assert-Match $raw '\\n' 'raw mode did not preserve escaped newline view'

    Send-Char 't'
    Start-Sleep -Milliseconds 300
    $timeline = Save-Capture 'provider-timeline.txt'
    Assert-Match $timeline 'codex CLI 时间线' 'timeline title missing'
    Assert-Match $timeline '原始命令: codex' 'timeline missing execution detail'
    Assert-Match $timeline 'hello switchyard' 'timeline missing final assistant text'

    Send-Char 's'
    Start-Sleep -Milliseconds 300
    $screen = Save-Capture 'provider-screen.txt'
    Assert-Match $screen 'codex 屏幕镜像 \(PIPE\)' 'screen mode title missing after switch back'

    Send-Vk 0x72  # F3
    Start-Sleep -Milliseconds 300
    $cycle = Save-Capture 'provider-f3-cycle.txt'
    Assert-Match $cycle 'codex 原始输出 \(PIPE\)' 'F3 did not cycle mode to raw'

    $report = @{
        status = 'ok'
        out_dir = $OutDir
        checks = @(
            'overview-completed',
            'capture-clean',
            'provider-screen-pipe',
            'provider-raw-pipe',
            'provider-timeline',
            'f3-cycle'
        )
    } | ConvertTo-Json -Depth 4
    $report | Set-Content -Path (Join-Path $OutDir 'report.json') -Encoding UTF8
    Write-Output $report
}
catch {
    ($_ | Out-String) | Set-Content -Path (Join-Path $OutDir 'error.txt') -Encoding UTF8
    throw
}
finally {
    if ($script:Process -and -not $script:Process.HasExited) {
        Stop-Process -Id $script:Process.Id -Force
    }
    [SwitchyardConsoleBridge]::FreeConsole() | Out-Null
}
