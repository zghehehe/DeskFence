$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$src = @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public class Closer {
    public delegate bool EnumFunc(IntPtr h, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumFunc cb, IntPtr lp);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
    [DllImport("user32.dll")] public static extern IntPtr PostMessageW(IntPtr h, int m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L; public int T; public int R; public int B; }
    public static string Class(IntPtr h) { var sb = new StringBuilder(256); GetClassName(h, sb, 256); return sb.ToString(); }
    public static IntPtr Find() {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr h, IntPtr lp) {
            if (Class(h) == "Edit" && IsWindowVisible(h)) {
                long ex = GetWindowLong(h, -20);
                if ((ex & 0x80) != 0) { found = h; return false; }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static int[] Rect(IntPtr h) {
        RECT r; GetWindowRect(h, out r);
        return new int[] { r.L, r.T, r.R - r.L, r.B - r.T };
    }
    public static void Esc(IntPtr h) {
        PostMessageW(h, 0x0100, (IntPtr)0x1B, IntPtr.Zero);
    }
    public static void Dpi() {
        SetProcessDpiAwarenessContext((IntPtr)(-4));
    }
}
'@
Add-Type -TypeDefinition $src
[Closer]::Dpi()

$e = [Closer]::Find()
if ($e -eq [IntPtr]::Zero) { Write-Output "no-edit-open"; exit 0 }
$r = [Closer]::Rect($e)
Write-Output ("edit rect=" + ($r -join ','))
$x = [Math]::Max(0, $r[0] - 30); $y = [Math]::Max(0, $r[1] - 30)
$wd = $r[2] + 60; $ht = $r[3] + 60
$bmp = New-Object System.Drawing.Bitmap($wd, $ht)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
$bmp.Save("C:\Users\z00897910\Desktop\DeskFence\tools\rename_live_shot.png", [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "SCREENSHOT -> tools/rename_live_shot.png"
[Closer]::Esc($e)
Start-Sleep -Milliseconds 500
if ([Closer]::Find() -eq [IntPtr]::Zero) { Write-Output "ESC-CLOSED ok" } else {
    Write-Output "still open, closing again"
    $e2 = [Closer]::Find()
    if ($e2 -ne [IntPtr]::Zero) { [Closer]::Esc($e2) }
    Start-Sleep -Milliseconds 500
    if ([Closer]::Find() -eq [IntPtr]::Zero) { Write-Output "ESC-CLOSED ok(2nd)" } else { Write-Output "STILL OPEN" }
}
