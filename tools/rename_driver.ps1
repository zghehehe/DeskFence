param([string]$Mode = "test")
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$src = @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public class Drv {
    public delegate bool EnumFunc(IntPtr h, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumFunc cb, IntPtr lp);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);
    [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
    [DllImport("user32.dll")] public static extern IntPtr PostMessageW(IntPtr h, int m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, int m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    public static string Class(IntPtr h) { var sb = new StringBuilder(256); GetClassName(h, sb, 256); return sb.ToString(); }
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L; public int T; public int R; public int B; }
    public static int[] Rect(IntPtr h) {
        RECT r; GetWindowRect(h, out r);
        return new int[] { r.L, r.T, r.R - r.L, r.B - r.T };
    }
    public static List<IntPtr> Fences() {
        var found = new List<IntPtr>();
        EnumWindows(delegate(IntPtr h, IntPtr lp) {
            if (Class(h) == "DeskFenceFence" && IsWindowVisible(h)) { found.Add(h); }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static IntPtr FindFenceEdit() {
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
    public static string DumpEdit(IntPtr h) {
        RECT r; GetWindowRect(h, out r);
        long st = GetWindowLong(h, -16); long ex = GetWindowLong(h, -20);
        long sel = SendMessageW(h, 0x00B0, IntPtr.Zero, IntPtr.Zero).ToInt64();
        long first = SendMessageW(h, 0x00CE, IntPtr.Zero, IntPtr.Zero).ToInt64();
        long lines = SendMessageW(h, 0x00BA, IntPtr.Zero, IntPtr.Zero).ToInt64();
        var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
        string txt = sb.ToString();
        string shown = txt;
        if (shown.Length > 18) { shown = shown.Substring(0, 18) + "..(len=" + txt.Length + ")"; }
        return string.Format(
            "EDIT text='{0}' rect=({1},{2})-({3},{4}) size={5}x{6} sel=({7},{8}) firstvis={9} lines={10} style=0x{11:X} ex=0x{12:X}",
            shown, r.L, r.T, r.R, r.B, r.R - r.L, r.B - r.T,
            sel & 0xFFFF, (sel >> 16) & 0xFFFF, first, lines, st, ex);
    }
    public static void SetTextLong(IntPtr h, string s) {
        IntPtr p = Marshal.StringToHGlobalUni(s);
        SendMessageW(h, 0x000C, IntPtr.Zero, p);
        Marshal.FreeHGlobal(p);
    }
}
'@
Add-Type -TypeDefinition $src
[Drv]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$fences = [Drv]::Fences()
if ($fences.Count -eq 0) { Write-Output "NO-FENCE-WINDOW"; exit 1 }
Write-Output ("fence-windows=" + $fences.Count)

# 选宽度最小的栅栏(单列,图标少,点击最稳)
$best = [IntPtr]::Zero; $bw = 999999
foreach ($f in $fences) {
    $wr = [Drv]::Rect($f)
    if ($wr[2] -lt $bw) { $bw = $wr[2]; $best = $f }
}
$wr = [Drv]::Rect($best)
Write-Output ("target fence rect=" + ($wr -join ','))

$dpi = 1.5
$pad = 6.0 * $dpi; $title = 26.0 * $dpi
$cw = [float]($wr[2] - 2.0 * $pad - 2.0)
$ch = ([float]($wr[3] - $title - $pad * 2.0 - 2.0)) / 4.0
$relX = [int]($pad + $cw / 2.0)
$relY = [int]($title + $pad + $ch / 2.0)
$cx = $wr[0] + $relX
$cy = $wr[1] + $relY
Write-Output ("click screen=(" + $cx + "," + $cy + ") rel=(" + $relX + "," + $relY + ") cell=" + $cw + "x" + $ch)

[Drv]::SetCursorPos($cx, $cy) | Out-Null
Start-Sleep -Milliseconds 250
$lp = [IntPtr](($relY * 65536) + $relX)
[Drv]::PostMessageW($best, 0x0201, [IntPtr]1, $lp) | Out-Null
Start-Sleep -Milliseconds 120
[Drv]::PostMessageW($best, 0x0202, [IntPtr]0, $lp) | Out-Null
Write-Output "click1 done (select)"
Start-Sleep -Milliseconds 700
[Drv]::SetCursorPos($cx, $cy) | Out-Null
[Drv]::PostMessageW($best, 0x0201, [IntPtr]1, $lp) | Out-Null
Start-Sleep -Milliseconds 120
[Drv]::PostMessageW($best, 0x0202, [IntPtr]0, $lp) | Out-Null
Write-Output "click2 done (slow-click rename)"
Start-Sleep -Milliseconds 600

$edit = [Drv]::FindFenceEdit()
if ($edit -eq [IntPtr]::Zero) { Write-Output "AFTER-OPEN NO-EDIT (rename did not trigger)"; exit 1 }
Write-Output ("AFTER-OPEN " + [Drv]::DumpEdit($edit))

# 长名换字 → 120ms 定时器应自动增高
$longname = "新建 文本文档学习学习学习寻寻寻寻寻寻寻寻寻寻学习学习学习寻寻寻寻寻寻寻寻.txt"
[Drv]::SetTextLong($edit, $longname)
Start-Sleep -Milliseconds 400
Write-Output ("AFTER-SETLONG " + [Drv]::DumpEdit($edit))
Start-Sleep -Milliseconds 400
Write-Output ("AFTER-SETLONG(2) " + [Drv]::DumpEdit($edit))

# 截图编辑框区域
$er = [Drv]::Rect($edit)
$x = [Math]::Max(0, $er[0] - 30); $y = [Math]::Max(0, $er[1] - 30)
$wd = $er[2] + 60; $ht = $er[3] + 60
$bmp = New-Object System.Drawing.Bitmap($wd, $ht)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
$bmp.Save("C:\Users\z00897910\Desktop\DeskFence\tools\rename_live_shot.png", [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "SCREENSHOT -> tools/rename_live_shot.png"

# Esc 取消,不落地
[Drv]::PostMessageW($edit, 0x0100, [IntPtr]0x1B, [IntPtr]0) | Out-Null
Start-Sleep -Milliseconds 400
$e2 = [Drv]::FindFenceEdit()
if ($e2 -eq [IntPtr]::Zero) { Write-Output "ESC-CLOSED ok" } else { Write-Output "ESC-CLOSED FAILED (still open)"; [Drv]::PostMessageW($e2, 0x0100, [IntPtr]0x1B, [IntPtr]0) | Out-Null }
