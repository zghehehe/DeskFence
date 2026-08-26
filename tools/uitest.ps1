param(
    [Parameter(Mandatory=$true)][string]$Action,
    [int]$X = 0,
    [int]$Y = 0,
    [string]$Path = "",
    [int]$RightClick = 0,
    [int]$DelayMs = 0,
    [int]$ToX = 700,
    [int]$ToY = 600
)
# ASCII-only on purpose: no BOM issues with GBK codepage readers.
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class UiCtl {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern IntPtr FindWindowW([MarshalAs(UnmanagedType.LPWStr)] string cls, IntPtr name);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT r);
}
"@ -PassThru | Out-Null
try { [UiCtl]::SetProcessDPIAware() | Out-Null } catch {}

function Shot([string]$file) {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $g.Dispose()
    $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Output ("shot " + $b.Width + "x" + $b.Height + " -> " + $file)
}

switch ($Action) {
    'shot' { Shot $Path }
    'click' {
        if ($DelayMs -gt 0) { Start-Sleep -Milliseconds $DelayMs }
        [UiCtl]::SetCursorPos($X, $Y) | Out-Null
        Start-Sleep -Milliseconds 80
        if ($RightClick -eq 1) {
            [UiCtl]::mouse_event(0x0008, 0, 0, 0, [UIntPtr]::Zero)  # RIGHTDOWN
            Start-Sleep -Milliseconds 60
            [UiCtl]::mouse_event(0x0010, 0, 0, 0, [UIntPtr]::Zero)  # RIGHTUP
        } else {
            [UiCtl]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)  # LEFTDOWN
            Start-Sleep -Milliseconds 60
            [UiCtl]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)  # LEFTUP
        }
        Write-Output ("clicked " + $X + "," + $Y + " right=" + $RightClick)
    }
    'traymenu' {
        # Simulate the tray icon callback exactly as Explorer delivers it:
        # TRAY_MSG = WM_APP+1, lparam low word = WM_RBUTTONUP (0x0205)
        $hwnd = [UiCtl]::FindWindowW("DeskFenceTray", [IntPtr]::Zero)
        if ($hwnd -eq [IntPtr]::Zero) { Write-Output "tray hwnd not found"; exit 2 }
        $lp = [IntPtr]0x0205
        [UiCtl]::PostMessageW($hwnd, 0x8001, [IntPtr]::Zero, $lp) | Out-Null
        Write-Output ("tray callback posted hwnd=0x" + $hwnd.ToString("X"))
    }
    'activewin' {
        $h = [UiCtl]::GetForegroundWindow()
        $r = New-Object UiCtl+RECT
        [UiCtl]::GetWindowRect($h, [ref]$r) | Out-Null
        Write-Output ("foreground=0x" + $h.ToString("X") + " rect=" + $r.L + "," + $r.T + "," + $r.R + "," + $r.B)
    }
    'menuwin' {
        $src = @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class MenuProbe {
    delegate bool CB(IntPtr h, IntPtr lp);
    [DllImport("user32.dll")] static extern bool EnumWindows(CB cb, IntPtr lp);
    [DllImport("user32.dll")] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out RECT r);
    public static string Probe() {
        StringBuilder sb = new StringBuilder(64);
        string found = "";
        EnumWindows(delegate(IntPtr h, IntPtr lp) {
            GetClassName(h, sb, 64);
            if (sb.ToString() == "#32768" && IsWindowVisible(h)) {
                RECT r; GetWindowRect(h, out r);
                found += "MENU hwnd=0x" + h.ToString("X") + " rect=" + r.L + "," + r.T + "," + r.R + "," + r.B + ";";
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
"@
        Add-Type -TypeDefinition $src -PassThru | Out-Null
        $r = [MenuProbe]::Probe()
        if ($r -eq "") { Write-Output "NO-MENU" } else { Write-Output $r }
    }
    'dialog' {
        $src = @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class DlgProbe {
    delegate bool CB(IntPtr h, IntPtr lp);
    [DllImport("user32.dll")] static extern bool EnumWindows(CB cb, IntPtr lp);
    [DllImport("user32.dll")] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    public static string Probe() {
        StringBuilder sb = new StringBuilder(64);
        StringBuilder wt = new StringBuilder(256);
        string f = "";
        EnumWindows(delegate(IntPtr h, IntPtr lp) {
            GetClassName(h, sb, 64);
            if (IsWindowVisible(h) && sb.ToString() == "#32770") {
                GetWindowTextW(h, wt, 256);
                f += "DIALOG 0x" + h.ToString("X") + " text=" + wt.ToString() + ";";
            }
            return true;
        }, IntPtr.Zero);
        return f;
    }
}
"@
        Add-Type -TypeDefinition $src -PassThru | Out-Null
        $r = [DlgProbe]::Probe()
        if ($r -eq "") { Write-Output "NO-DIALOG" } else { Write-Output $r }
    }
    'keys' {
        # -Path carries the SendKeys string, e.g. "{ESC}"
        $w = New-Object -ComObject WScript.Shell
        $w.SendKeys($Path)
        Write-Output "sent: $Path"
    }
    'burst' {
        # burst <prefix> : 12 shots ~80ms apart, numbered
        Add-Type -AssemblyName System.Windows.Forms
        Add-Type -AssemblyName System.Drawing
        $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
        for ($i = 0; $i -lt 12; $i++) {
            $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
            $g.Dispose()
            $f = "{0}_{1:d2}.png" -f $Path, $i
            $bmp.Save($f, [System.Drawing.Imaging.ImageFormat]::Png)
            $bmp.Dispose()
            Write-Output $f
            Start-Sleep -Milliseconds 80
        }
    }
    'drag' {
        # drag: press at (X,Y), move to (ToX,ToY) in steps, screenshot $Path
        # mid-drag, move back and release at origin (no reorder side effect)
        $fx = $X; $fy = $Y; $tx = $ToX; $ty = $ToY
        [UiCtl]::SetCursorPos($fx, $fy) | Out-Null
        Start-Sleep -Milliseconds 250
        [UiCtl]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 300
        $steps = 20
        for ($i = 1; $i -le $steps; $i++) {
            $nx = $fx + [int](($tx - $fx) * $i / $steps)
            $ny = $fy + [int](($ty - $fy) * $i / $steps)
            [UiCtl]::SetCursorPos($nx, $ny) | Out-Null
            Start-Sleep -Milliseconds 25
        }
        Start-Sleep -Milliseconds 600
        if ($Path -ne "" -and $Path -ne "-") { Shot $Path }
        for ($i = 1; $i -le $steps; $i++) {
            $nx = $tx + [int](($fx - $tx) * $i / $steps)
            $ny = $ty + [int](($fy - $ty) * $i / $steps)
            [UiCtl]::SetCursorPos($nx, $ny) | Out-Null
            Start-Sleep -Milliseconds 15
        }
        [UiCtl]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
        Write-Output "dragged $fx,$fy -> $tx,$ty and back"
    }
    'default' { }
    default { Write-Output "unknown action" }
}
