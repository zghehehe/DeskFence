param([int]$Seconds = 420)
$ErrorActionPreference = 'Stop'

$src = @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public class RenProbe2 {
    public delegate bool EnumFunc(IntPtr h, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumFunc cb, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr h, EnumFunc cb, IntPtr lp);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, int m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("gdi32.dll")] public static extern int GetObject(IntPtr h, int n, ref LOGFONT lf);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L; public int T; public int R; public int B; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct LOGFONT {
        public int H; public int W; public int Esc; public int Orient; public int Weight;
        public byte Italic; public byte Under; public byte Strike; public byte Charset;
        public byte OutPrec; public byte ClipPrec; public byte Quality; public byte Pitch;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string Face;
    }
    public static string Class(IntPtr h) { var sb = new StringBuilder(256); GetClassName(h, sb, 256); return sb.ToString(); }
    public static string Txt(IntPtr h) { var sb = new StringBuilder(128); GetWindowText(h, sb, 128); return sb.ToString(); }
    public static string Chain(IntPtr h) {
        var parts = new List<string>();
        IntPtr cur = h;
        for (int i = 0; i < 5 && cur != IntPtr.Zero; i++) {
            parts.Add(Class(cur) + "@" + cur.ToInt64().ToString("X"));
            cur = GetParent(cur);
        }
        return string.Join(" < ", parts.ToArray());
    }
    public static string Dump(IntPtr h) {
        RECT r; GetWindowRect(h, out r);
        long st = GetWindowLong(h, -16); long ex = GetWindowLong(h, -20);
        IntPtr f = SendMessage(h, 0x0031, IntPtr.Zero, IntPtr.Zero);
        string font = "none";
        if (f != IntPtr.Zero) {
            LOGFONT lf = new LOGFONT();
            if (GetObject(f, Marshal.SizeOf(typeof(LOGFONT)), ref lf) != 0) {
                font = lf.Face + " h=" + lf.H + " w=" + lf.Weight;
            }
        }
        return string.Format(
            "EDIT text='{0}' rect=({1},{2})-({3},{4}) size={5}x{6} style=0x{7:X} exstyle=0x{8:X} font=[{9}] chain='{10}'",
            Txt(h), r.L, r.T, r.R, r.B, r.R - r.L, r.B - r.T, st, ex, font, Chain(h));
    }
    public static IntPtr FindSysList() {
        IntPtr target = IntPtr.Zero;
        EnumWindows(delegate(IntPtr h, IntPtr lp) {
            string c = Class(h);
            if (c == "SysListView32" || c == "SHELLDLL_DefView") { target = h; return false; }
            // WorkerW/Progman 也可能有子链
            if (c == "WorkerW" || c == "Progman") {
                IntPtr v = FindClassChild(h, "SHELLDLL_DefView");
                if (v != IntPtr.Zero) {
                    IntPtr lv = FindClassChild(v, "SysListView32");
                    if (lv != IntPtr.Zero) { target = lv; return false; }
                }
            }
            return true;
        }, IntPtr.Zero);
        return target;
    }
    public static IntPtr FindClassChild(IntPtr h, string cls) {
        IntPtr found = IntPtr.Zero;
        EnumChildWindows(h, delegate(IntPtr c, IntPtr lp) {
            if (Class(c) == cls) { found = c; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static List<string> FindDesktopEdits() {
        var found = new List<string>();
        IntPtr lv = FindSysList();
        if (lv == IntPtr.Zero) { found.Add("NO-SYSLISTVIEW"); return found; }
        found.Add("LV @" + lv.ToInt64().ToString("X") + " visible=" + IsWindowVisible(lv));
        EnumChildWindows(lv, delegate(IntPtr c, IntPtr lp) {
            if (Class(c) == "Edit") {
                found.Add(Dump(c) + " | chain='" + Chain(c) + "'");
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
Add-Type -TypeDefinition $src

$deadline = (Get-Date).AddSeconds($Seconds)
$seen = @{}
$i = 0
Write-Output ("PROBE-START " + (Get-Date -Format 'HH:mm:ss'))
while ((Get-Date) -lt $deadline) {
    $i++
    foreach ($e in [RenProbe2]::FindDesktopEdits()) {
        if (-not $seen.ContainsKey($e)) {
            $seen[$e] = $true
            Write-Output ("HIT[{0:HH:mm:ss.fff}] {1}" -f (Get-Date), $e)
        }
    }
    Start-Sleep -Milliseconds 300
}
Write-Output ("PROBE-END " + (Get-Date -Format 'HH:mm:ss') + " samples=" + $i)
