param([int]$Seconds = 75)
$ErrorActionPreference = 'Stop'

$src = @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public class RenProbe {
    public delegate bool EnumFunc(IntPtr h, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumFunc cb, IntPtr lp);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder sb, int n);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
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
        for (int i = 0; i < 4 && cur != IntPtr.Zero; i++) {
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
        long sel = SendMessage(h, 0x00B0, IntPtr.Zero, IntPtr.Zero).ToInt64();
        long first = SendMessage(h, 0x00CE, IntPtr.Zero, IntPtr.Zero).ToInt64();
        long lines = SendMessage(h, 0x00BA, IntPtr.Zero, IntPtr.Zero).ToInt64();
        return string.Format(
            "EDIT len={0} text='{1}' rect=({2},{3})-({4},{5}) size={6}x{7} sel=({8},{9}) firstvis={10} lines={11} style=0x{12:X} ex=0x{13:X} chain='{14}'",
            GetWindowTextLength(h), Txt(h), r.L, r.T, r.R, r.B, r.R - r.L, r.B - r.T,
            sel & 0xFFFF, (sel >> 16) & 0xFFFF, first, lines, st, ex, Chain(GetParent(h)));
    }
    public static List<string> FindEdits() {
        var found = new List<string>();
        EnumWindows(delegate(IntPtr h, IntPtr lp) {
            if (Class(h) == "Edit" && IsWindowVisible(h) && GetWindowTextLength(h) >= 0) {
                found.Add(Dump(h));
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
    $edits = [RenProbe]::FindEdits()
    foreach ($e in $edits) {
        if (-not $seen.ContainsKey($e)) {
            $seen[$e] = $true
            Write-Output ("HIT[{0:HH:mm:ss.fff}] {1}" -f (Get-Date), $e)
        }
    }
    Start-Sleep -Milliseconds 250
}
Write-Output ("PROBE-END " + (Get-Date -Format 'HH:mm:ss') + " samples=" + $i)
