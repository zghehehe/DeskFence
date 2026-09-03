$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$src = @'
using System;
using System.Text;
using System.Runtime.InteropServices;

public class EditFit {
    [DllImport("user32.dll")] public static extern IntPtr CreateWindowExW(int ex, [MarshalAs(UnmanagedType.LPWStr)] string cls, IntPtr title, uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, int m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetWindowTextW(IntPtr h, [MarshalAs(UnmanagedType.LPWStr)] string s);
    [DllImport("user32.dll")] public static extern int GetWindowTextLengthW(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, [MarshalAs(UnmanagedType.LPWStr)] StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
    [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] public static extern bool GetTextExtentPoint32W(IntPtr dc, [MarshalAs(UnmanagedType.LPWStr)] string s, int len, out SIZE sz);
    [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
    [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr o);
    [DllImport("gdi32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CreateFontW(int h, int w, int esc, int orient, int weight, uint i, uint u, uint strike, uint charset, uint outprec, uint clipprec, uint quality, uint pitch, [MarshalAs(UnmanagedType.LPWStr)] string face);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
    [StructLayout(LayoutKind.Sequential)] public struct SIZE { public int cx; public int cy; }

    // WS_POPUP|WS_VISIBLE|ES_LEFT|ES_MULTILINE|ES_AUTOVSCROLL|ES_NOHIDESEL
    public static uint Style() { return 0x90000000u | 0x1u | 0x4u | 0x40u | 0x80u; }
    public static IntPtr MakeEdit(int x, int y, int w, int h) {
        return CreateWindowExW(0x88, "EDIT", IntPtr.Zero, Style(), x, y, w, h, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
    }
    public static IntPtr MakeFont() {
        return CreateFontW(-18, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 0, 0, "Segoe UI");
    }
    public static int LineCount(IntPtr edit) {
        return SendMessageW(edit, 0x00BA, IntPtr.Zero, IntPtr.Zero).ToInt32();
    }
    public static int SelRaw(IntPtr edit) {
        return SendMessageW(edit, 0x00B0, IntPtr.Zero, IntPtr.Zero).ToInt32();
    }
    public static void SetSel(IntPtr edit, int a, int b) {
        SendMessageW(edit, 0x00B1, (IntPtr)a, (IntPtr)b);
    }
    public static int TextPx(IntPtr edit, out int len) {
        len = GetWindowTextLengthW(edit);
        var sb = new StringBuilder(len + 1);
        GetWindowTextW(edit, sb, len + 1);
        IntPtr dc = GetDC(edit);
        IntPtr f = SendMessageW(edit, 0x0031, IntPtr.Zero, IntPtr.Zero);
        IntPtr of = IntPtr.Zero;
        if (f != IntPtr.Zero) { of = SelectObject(dc, f); }
        SIZE sz; bool ok = GetTextExtentPoint32W(dc, sb.ToString(), len, out sz);
        if (f != IntPtr.Zero) { SelectObject(dc, of); }
        ReleaseDC(edit, dc);
        return ok ? sz.cx : -1;
    }
}
'@
Add-Type -TypeDefinition $src

[EditFit]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$name = "新建 文本文档学习学习学习寻寻寻寻寻寻寻寻寻寻学习学习学习.txt"
$dot = $name.LastIndexOf('.')
$baseU16 = $name.Substring(0, $dot).Length
Write-Output ("name utf16 chars=" + $name.Length + " base=" + $baseU16)

$font = [EditFit]::MakeFont()

# 阶段 1:纯 API 实测(不可见)
$edit = [EditFit]::MakeEdit(0, 0, 233, 81)
[EditFit]::SendMessageW($edit, 0x0030, $font, [IntPtr]1) | Out-Null
[EditFit]::SetWindowTextW($edit, $name) | Out-Null
$lc = [EditFit]::LineCount($edit)
$len = 0
$tw = [EditFit]::TextPx($edit, [ref]$len)
$inner = 233 - 10
$est = [Math]::Ceiling($tw / $inner)
Write-Output ("API-TEST w=233 inner=223 text_px=" + $tw + " EM_GETLINECOUNT=" + $lc + " est_lines=" + $est + " fit_h=" + [Math]::Round($est * 18 + 8))

# 选择范围实测:基名字符数
[EditFit]::SetSel($edit, 0, $baseU16)
$raw = [EditFit]::SelRaw($edit)
Write-Output ("SEL-TEST set=(0," + $baseU16 + ") raw=0x" + $raw.ToString("X") + " lo=" + ($raw -band 0xFFFF) + " hi=" + (($raw -shr 16) -band 0xFFFF) + " base_len=" + $baseU16)
[EditFit]::DestroyWindow($edit) | Out-Null

# 阶段 2:可见复现(与栅栏同几何),按计算高度展示后截图
$x = 300; $y = 300; $w = 233
$h2 = [int][Math]::Round($est * 18 + 8)
$edit2 = [EditFit]::MakeEdit($x, $y, $w, $h2)
[EditFit]::SendMessageW($edit2, 0x0030, $font, [IntPtr]1) | Out-Null
[EditFit]::SetWindowTextW($edit2, $name) | Out-Null
[EditFit]::SetSel($edit2, 0, $baseU16)
Start-Sleep -Milliseconds 600
$bmp = New-Object System.Drawing.Bitmap(($w + 40), ($h2 + 40))
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($x - 20, $y - 20, 0, 0, $bmp.Size)
$bmp.Save("C:\Users\z00897910\Desktop\DeskFence\tools\edit_fit_shot.png", [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
[EditFit]::DestroyWindow($edit2) | Out-Null
Write-Output ("VISUAL-TEST edit visible at (300,300) size=" + $w + "x" + $h2 + " -> tools/edit_fit_shot.png")
