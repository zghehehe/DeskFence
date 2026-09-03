# Replicate deskfence file-rename EDIT (same styles/font) and measure wrap metrics.
# Pure ASCII on purpose (PS5.1 reads BOM-less files as GBK). CJK text built from char codes.
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class EditProbe {
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateWindowExW(int ex, string cls, string name, int style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern bool SetWindowTextW(IntPtr h, string s);
    [DllImport("user32.dll")] public static extern int SendMessageW(IntPtr h, int msg, IntPtr wp, IntPtr lp);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, int msg, IntPtr wp, ref RECT lp);
    [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
    [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
    [DllImport("gdi32.dll")] public static extern bool GetTextMetricsW(IntPtr dc, ref TEXTMETRICW tm);
    [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr dc, IntPtr obj);
    [DllImport("user32.dll")] public static extern bool SystemParametersInfoW(uint act, uint sz, ref LOGFONTW lf, uint flags);
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateFontIndirectW(ref LOGFONTW lf);
    [DllImport("user32.dll")] public static extern int GetDpiForSystem();
    [DllImport("user32.dll")] public static extern IntPtr SetTimer(IntPtr h, IntPtr id, uint ms, IntPtr proc);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct LOGFONTW {
        public int Height; public int Width; public int Escapement; public int Orientation; public int Weight;
        public byte Italic; public byte Underline; public byte StrikeOut; public byte CharSetB; public byte OutPrecision;
        public byte ClipPrecision; public byte Quality; public byte PitchAndFamily;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string FaceName;
    }
    [StructLayout(LayoutKind.Sequential)] public struct TEXTMETRICW {
        public int Height; public int Ascent; public int Descent; public int InternalLeading; public int ExternalLeading;
        public int AveCharWidth; public int MaxCharWidth; public int Weight; public int Overhang; public int DigitizedAspectX;
        public int DigitizedAspectY; public char FirstChar; public char LastChar; public char DefaultChar; public char BreakChar;
        public byte Italic; public byte Underlined; public byte StruckOut; public byte PitchAndFamily; public byte CharSetB;
    }
    public const int WS_POPUP = unchecked((int)0x80000000), WS_VISIBLE = 0x10000000;
    public const int ES_LEFT = 0x0, ES_MULTILINE = 0x4, ES_AUTOVSCROLL = 0x40, ES_NOHIDESEL = 0x100;
    public const int EM_GETRECT = 0xB2, EM_GETLINECOUNT = 0xBA, EM_LINEINDEX = 0xBB, EM_LINELENGTH = 0xC1;
    public const int EM_POSFROMCHAR = 0xD6;
    public static int PosCharX(IntPtr e, int idx) {
        int p = SendMessageW(e, EM_POSFROMCHAR, (IntPtr)idx, IntPtr.Zero);
        return p & 0xFFFF;
    }
    public const int EM_GETMARGINS = 0xD4, EM_GETFIRSTVISIBLELINE = 0xCE, WM_GETFONT = 0x31, EM_SETSEL = 0xB1, EM_SCROLLCARET = 0xB7;

    public static IntPtr Make(int w, int h) {
        return CreateWindowExW(0x80, "EDIT", "", WS_POPUP | WS_VISIBLE | ES_LEFT | ES_MULTILINE | ES_AUTOVSCROLL | ES_NOHIDESEL, 60, 60, w, h, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
    }
    public static IntPtr MakeStyled(int w, int h, int style) {
        return CreateWindowExW(0x80, "EDIT", "", WS_POPUP | WS_VISIBLE | style, 60, 60, w, h, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
    }
    public static IntPtr IconFont() {
        LOGFONTW lf = new LOGFONTW();
        SystemParametersInfoW(0x1F, (uint)Marshal.SizeOf(typeof(LOGFONTW)), ref lf, 0);
        return CreateFontIndirectW(ref lf);
    }
    public static IntPtr IconFontAppStyle() {
        LOGFONTW lf = new LOGFONTW();
        lf.Height = -18; lf.Weight = 400; lf.FaceName = "Microsoft YaHei UI";
        return CreateFontIndirectW(ref lf);
    }
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int h2, uint flags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, ref RECT r);
    public static string FontInfo() {
        LOGFONTW lf = new LOGFONTW();
        SystemParametersInfoW(0x1F, (uint)Marshal.SizeOf(typeof(LOGFONTW)), ref lf, 0);
        return lf.FaceName + " h=" + lf.Height + " weight=" + lf.Weight + " charset=" + lf.CharSetB;
    }
    public static int LineCount(IntPtr e) { return SendMessageW(e, EM_GETLINECOUNT, IntPtr.Zero, IntPtr.Zero); }
    public static int FirstVisible(IntPtr e) { return SendMessageW(e, EM_GETFIRSTVISIBLELINE, IntPtr.Zero, IntPtr.Zero); }
    public static RECT FmtRect(IntPtr e) { RECT r = new RECT(); SendMessageW(e, EM_GETRECT, IntPtr.Zero, ref r); return r; }
    public static int Margins(IntPtr e) { return SendMessageW(e, EM_GETMARGINS, IntPtr.Zero, IntPtr.Zero); }
    public static int LineStart(IntPtr e, int i) { return SendMessageW(e, EM_LINEINDEX, (IntPtr)i, IntPtr.Zero); }
    public static int LineLen(IntPtr e, int i) { return SendMessageW(e, EM_LINELENGTH, (IntPtr)LineStart(e, i), IntPtr.Zero); }
    public static IntPtr Font(IntPtr e) { return (IntPtr)SendMessageW(e, WM_GETFONT, IntPtr.Zero, IntPtr.Zero); }
    public static TEXTMETRICW Tm(IntPtr e) {
        IntPtr dc = GetDC(e); TEXTMETRICW tm = new TEXTMETRICW();
        IntPtr f = Font(e); IntPtr old = IntPtr.Zero;
        if (f != IntPtr.Zero) old = SelectObject(dc, f);
        GetTextMetricsW(dc, ref tm);
        if (old != IntPtr.Zero) SelectObject(dc, old);
        ReleaseDC(e, dc); return tm;
    }
}

public class LineBuf {
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string s;
}
'@

[void][EditProbe]::SetProcessDpiAwarenessContext([IntPtr](-4))
Write-Host ("system DPI=" + [EditProbe]::GetDpiForSystem() + " font: " + [EditProbe]::FontInfo())

# test text: 新建 文本文档学习学习 + 寻*25 + .txt
$find = [string][char]0x5BFB
$txt = ([string][char]0x65B0) + ([string][char]0x5EFA) + ' ' + ([string][char]0x6587) + ([string][char]0x672C) + ([string][char]0x6587) + ([string][char]0x6863) + ([string][char]0x5B66) + ([string][char]0x4E60) + ([string][char]0x5B66) + ([string][char]0x4E60) + ($find * 25) + '.txt'
Write-Host ("text len(utf16)=" + $txt.Length)

$font = [EditProbe]::IconFont()

foreach ($w in 111, 116, 120, 122, 127, 145) {
    $e = [EditProbe]::Make($w, 60)
    if ($e -eq [IntPtr]::Zero) { Write-Host "create failed"; continue }
    [void][EditProbe]::SendMessageW($e, 0x30, $font, [IntPtr]1)  # WM_SETFONT
    [void][EditProbe]::SetWindowTextW($e, $txt)
    [void][EditProbe]::SendMessageW($e, [EditProbe]::EM_SETSEL, [IntPtr]0, [IntPtr]::Zero - [IntPtr]1)
    Start-Sleep -Milliseconds 250
    $lc = [EditProbe]::LineCount($e)
    $rc = [EditProbe]::FmtRect($e)
    $tm = [EditProbe]::Tm($e)
    $mg = [EditProbe]::Margins($e)
    $lens = @()
    for ($i = 0; $i -lt $lc; $i++) { $lens += [EditProbe]::LineLen($e, $i) }
    Write-Host ("w={0} fmtRect=({1},{2})-({3},{4}) innerW={5} margins(L={6},R={7}) lineCount={8} pitch(h+ext)={9}+{10} lens={11}" -f `
        $w, $rc.L, $rc.T, $rc.R, $rc.B, ($rc.R - $rc.L), ($mg -band 0xFFFF), ($mg -shr 16), $lc, $tm.Height, $tm.ExternalLeading, ($lens -join ','))
    # again after another delay to see if line count settles
    Start-Sleep -Milliseconds 400
    $lc2 = [EditProbe]::LineCount($e)
    if ($lc2 -ne $lc) { Write-Host ("  later lineCount=" + $lc2) }
    [void][EditProbe]::DestroyWindow($e)
}


# --- replicate the app's exact sequence: create -> set font -> set text -> EM_SETSEL
# -> adjust(est-height resize) -> 120ms re-query -> resize -> 500ms re-query
$fontApp = [EditProbe]::IconFontAppStyle()
Write-Host ("app-style font em test:")
foreach ($w in 111, 120) {
    $e = [EditProbe]::Make($w, 81)
    [void][EditProbe]::SendMessageW($e, 0x30, $fontApp, [IntPtr]1)
    [void][EditProbe]::SetWindowTextW($e, $txt)
    [void][EditProbe]::SendMessageW($e, [EditProbe]::EM_SETSEL, [IntPtr]0, [IntPtr]35)
    $lc0 = [EditProbe]::LineCount($e)
    $tm0 = [EditProbe]::Tm($e)
    # immediate est-style resize: h = 7*lineH+8 (what the app computes at t=0 when EM lags)
    [void][EditProbe]::SetWindowPos($e, [IntPtr]::Zero, 0, 0, $w, (7 * ($tm0.Height + $tm0.ExternalLeading) + 8), 0x14)
    $lc1 = [EditProbe]::LineCount($e)
    Start-Sleep -Milliseconds 120
    $lc2 = [EditProbe]::LineCount($e)
    Start-Sleep -Milliseconds 500
    $lc3 = [EditProbe]::LineCount($e)
    $tm1 = [EditProbe]::Tm($e)
    $rc3 = [EditProbe]::FmtRect($e)
    Write-Host ("w={0} t=0ms lc={1} | after resize lc={2} | +120ms lc={3} | +500ms lc={4} tmH={5} ext={6} fmt=({7},{8})-({9},{10})" -f `
        $w, $lc0, $lc1, $lc2, $lc3, $tm1.Height, $tm1.ExternalLeading, $rc3.L, $rc3.T, $rc3.R, $rc3.B)
    $lens = @()
    for ($i = 0; $i -lt $lc3; $i++) { $lens += [EditProbe]::LineLen($e, $i) }
    Write-Host ("   final lens=" + ($lens -join ','))
    [void][EditProbe]::DestroyWindow($e)
}


# --- ES_CENTER check: wrap + per-line first-char x (centering) at w=120
$style = 0x1 -bor 0x4 -bor 0x40 -bor 0x100  # CENTER|MULTILINE|AUTOVSCROLL|NOHIDESEL
$ec = [EditProbe]::MakeStyled(120, 200, $style)
[void][EditProbe]::SendMessageW($ec, 0x30, $fontApp, [IntPtr]1)
[void][EditProbe]::SetWindowTextW($ec, $txt)
Start-Sleep -Milliseconds 200
$lc = [EditProbe]::LineCount($ec)
$lens = @(); $xs = @()
for ($i = 0; $i -lt $lc; $i++) {
    $lens += [EditProbe]::LineLen($ec, $i)
    $idx = [EditProbe]::LineStart($ec, $i)
    $xs += [EditProbe]::PosCharX($ec, $idx)
}
Write-Host ("ES_CENTER w=120 lineCount={0} lens={1} firstCharX={2} (interior 3..115, centered = 3+(112-wid)/2)" -f $lc, ($lens -join ','), ($xs -join ','))
[void][EditProbe]::DestroyWindow($ec)
