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
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, int m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, int m, IntPtr w, ref RECT l);
    [DllImport("user32.dll")] public static extern bool SendMessageTimeout(IntPtr h, int m, IntPtr w, IntPtr l, uint flags, uint timeout, out IntPtr result);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
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
    public static string Txt(IntPtr h) { var sb = new StringBuilder(512); GetWindowText(h, sb, 512); return sb.ToString(); }
    public static string RectText(RECT r) { return string.Format("({0},{1})-({2},{3}) {4}x{5}", r.L, r.T, r.R, r.B, r.R-r.L, r.B-r.T); }
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
        RECT outer; GetWindowRect(h, out outer);
        RECT client; GetClientRect(h, out client);
        RECT format = new RECT();
        SendMessage(h, 0x00B2, IntPtr.Zero, ref format);
        long st = GetWindowLong(h, -16); long ex = GetWindowLong(h, -20);
        IntPtr margins = SendMessage(h, 0x00D4, IntPtr.Zero, IntPtr.Zero);
        IntPtr selection = SendMessage(h, 0x00B0, IntPtr.Zero, IntPtr.Zero);
        int selStart = unchecked((ushort)(selection.ToInt64() & 0xFFFF));
        int selEnd = unchecked((ushort)((selection.ToInt64() >> 16) & 0xFFFF));
        long lines = SendMessage(h, 0x00BA, IntPtr.Zero, IntPtr.Zero).ToInt64();
        IntPtr f = SendMessage(h, 0x0031, IntPtr.Zero, IntPtr.Zero);
        string font = "none";
        if (f != IntPtr.Zero) {
            LOGFONT lf = new LOGFONT();
            if (GetObject(f, Marshal.SizeOf(typeof(LOGFONT)), ref lf) != 0) {
                font = string.Format("face='{0}' h={1} w={2} esc={3} orient={4} italic={5} under={6} strike={7} charset={8} out={9} clip={10} quality={11} pitch={12}",
                    lf.Face, lf.H, lf.Weight, lf.Esc, lf.Orient, lf.Italic, lf.Under, lf.Strike,
                    lf.Charset, lf.OutPrec, lf.ClipPrec, lf.Quality, lf.Pitch);
            }
        }
        return string.Format(
            "EDIT text='{0}' outer={1} client={2} format={3} margins=L{4}/R{5} sel={6}-{7} lines={8} dpi={9} style=0x{10:X} exstyle=0x{11:X} font=[{12}] chain='{13}'",
            Txt(h), RectText(outer), RectText(client), RectText(format),
            unchecked((ushort)(margins.ToInt64() & 0xFFFF)), unchecked((ushort)((margins.ToInt64() >> 16) & 0xFFFF)),
            selStart, selEnd, lines, GetDpiForWindow(h), st, ex, font, Chain(h));
    }
    public static IntPtr FindSysList() {
        IntPtr target = IntPtr.Zero;
        EnumWindows(delegate(IntPtr h, IntPtr lp) {
            string c = Class(h);
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
        RECT outer; GetWindowRect(lv, out outer);
        RECT client; GetClientRect(lv, out client);
        IntPtr spacing;
        bool spacingOk = SendMessageTimeout(lv, 0x1033, IntPtr.Zero, IntPtr.Zero, 0x0002, 200, out spacing);
        string spacingText = "unavailable";
        if (spacingOk) {
            long packed = spacing.ToInt64();
            spacingText = unchecked((ushort)(packed & 0xFFFF)) + "x" + unchecked((ushort)((packed >> 16) & 0xFFFF));
        }
        found.Add(string.Format("LV @{0:X} visible={1} outer={2} client={3} dpi={4} spacing={5}",
            lv.ToInt64(), IsWindowVisible(lv), RectText(outer), RectText(client), GetDpiForWindow(lv), spacingText));
        EnumChildWindows(lv, delegate(IntPtr c, IntPtr lp) {
            if (Class(c) == "Edit") found.Add(Dump(c));
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
