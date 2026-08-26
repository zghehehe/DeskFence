# Test: can a foreign process read Progman's CURRENT surface via plain
# window-DC BitBlt (no forced repaint, unlike PrintWindow)?
# Pure ASCII. PS 5.1 + C#5 compatible.
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class WP {
  [DllImport("user32.dll")] public static extern IntPtr SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string name);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr dc);
  [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int w, int h);
  [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr dc, IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool BitBlt(IntPtr d, int x, int y, int w, int h, IntPtr s, int sx, int sy, uint rop);
  [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr o);
  [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr dc);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[void][WP]::SetProcessDpiAwarenessContext([IntPtr](-4))

$h = [IntPtr]::Zero
$cb = {
  param([IntPtr]$w, [IntPtr]$l)
  $sb = New-Object System.Text.StringBuilder 256
  [void][WP]::GetClassName($w, $sb, 256)
  if ($sb.ToString() -eq "Progman" -and $script:h -eq [IntPtr]::Zero) { $script:h = $w }
  return $true
}
[WP]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
if ($h -eq [IntPtr]::Zero) { throw "Progman not found" }
$r = New-Object WP+RECT
[void][WP]::GetWindowRect($h, [ref]$r)
$w = $r.R - $r.L
$ht = $r.B - $r.T
"host=0x{0:x} rect={1}x{2}" -f $h.ToInt64(), $w, $ht

$src = [WP]::GetDC($h)
$mem = [WP]::CreateCompatibleDC($src)
$bmp = [WP]::CreateCompatibleBitmap($src, $w, $ht)
$old = [WP]::SelectObject($mem, $bmp)

# warmup + timing: plain BitBlt from the host window DC, 30 reps
[void][WP]::BitBlt($mem, 0, 0, $w, $ht, $src, 0, 0, 0x00CC0020)
$sw = [Diagnostics.Stopwatch]::StartNew()
for ($i = 0; $i -lt 30; $i++) {
  [void][WP]::BitBlt($mem, 0, 0, $w, $ht, $src, 0, 0, 0x00CC0020)
}
$sw.Stop()
"BitBlt x30: {0:N1} ms total, {1:N2} ms/op" -f $sw.Elapsed.TotalMilliseconds, ($sw.Elapsed.TotalMilliseconds / 30)

# snapshot to PNG for visual verification
Add-Type -AssemblyName System.Drawing
$png = "C:\Users\z00897910\Desktop\DeskFence\tools\wp_bitblt.png"
$img1 = [System.Drawing.Bitmap]::FromHbitmap($bmp)
$img1.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)

# stats on a sparse grid
$n = 0; $nonBlack = 0; $sr = 0.0; $sg = 0.0; $sb = 0.0
for ($x = 0; $x -lt $w; $x += 17) {
  for ($y = 0; $y -lt $ht; $y += 17) {
    $c = $img1.GetPixel($x, $y)
    $n++
    $sr += $c.R; $sg += $c.G; $sb += $c.B
    if ($c.R -gt 10 -or $c.G -gt 10 -or $c.B -gt 10) { $nonBlack++ }
  }
}
"sampled={0} nonBlack={1} meanRGB=({2:N0},{3:N0},{4:N0})" -f $n, $nonBlack, ($sr/$n), ($sg/$n), ($sb/$n)

# stability: capture again 1.5s later, count differing grid pixels
Start-Sleep -Milliseconds 1500
[void][WP]::BitBlt($mem, 0, 0, $w, $ht, $src, 0, 0, 0x00CC0020)
$img2 = [System.Drawing.Bitmap]::FromHbitmap($bmp)
$diff = 0
for ($x = 0; $x -lt $w; $x += 17) {
  for ($y = 0; $y -lt $ht; $y += 17) {
    $c1 = $img1.GetPixel($x, $y)
    $c2 = $img2.GetPixel($x, $y)
    if ([Math]::Abs($c1.R - $c2.R) -gt 2 -or [Math]::Abs($c1.G - $c2.G) -gt 2 -or [Math]::Abs($c1.B - $c2.B) -gt 2) { $diff++ }
  }
}
"stability: {0}/{1} grid pixels differ between two captures 1.5s apart" -f $diff, $n

[void][WP]::SelectObject($mem, $old)
[void][WP]::DeleteObject($bmp)
[void][WP]::DeleteDC($mem)
[void][WP]::ReleaseDC($h, $src)
$img1.Dispose()
$img2.Dispose()

# Control: same GDI pipeline but source = whole screen (GetDC NULL),
# to prove the black above is a property of the host-window path, not our code.
$scr = [WP]::GetDC([IntPtr]::Zero)
$mem2 = [WP]::CreateCompatibleDC($scr)
$bmp2 = [WP]::CreateCompatibleBitmap($scr, $w, $ht)
$old2 = [WP]::SelectObject($mem2, $bmp2)
[void][WP]::BitBlt($mem2, 0, 0, $w, $ht, $scr, 0, 0, 0x00CC0020)
$img3 = [System.Drawing.Bitmap]::FromHbitmap($bmp2)
$img3.Save("C:\Users\z00897910\Desktop\DeskFence\tools\wp_screen.png", [System.Drawing.Imaging.ImageFormat]::Png)
$nb = 0; $nn = 0
for ($x = 0; $x -lt $w; $x += 17) {
  for ($y = 0; $y -lt $ht; $y += 17) {
    $c = $img3.GetPixel($x, $y)
    $nn++
    if ($c.R -gt 10 -or $c.G -gt 10 -or $c.B -gt 10) { $nb++ }
  }
}
"control (screen DC): {0}/{1} nonBlack" -f $nb, $nn
[void][WP]::SelectObject($mem2, $old2)
[void][WP]::DeleteObject($bmp2)
[void][WP]::DeleteDC($mem2)
[void][WP]::ReleaseDC([IntPtr]::Zero, $scr)
$img3.Dispose()
