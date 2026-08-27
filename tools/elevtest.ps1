# Elevation self-heal test: push one fence to TOP, verify the 1s tick pulls
# it back into the desktop band. ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class EH {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int ht, uint f);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  public static IntPtr FindFence(int l, int t) {
    StringBuilder sb = new StringBuilder(64);
    IntPtr res = IntPtr.Zero;
    EnumWindows(delegate(IntPtr h, IntPtr lp) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "DeskFenceFence" && IsWindowVisible(h)) {
        RECT r; GetWindowRect(h, out r);
        if (r.L == l && r.T == t && r.R - r.L > 100) res = h;
      }
      return true;
    }, IntPtr.Zero);
    return res;
  }
}
"@
[void][EH]::SetProcessDpiAwarenessContext([IntPtr](-4))
$f = [EH]::FindFence(512, 0)
"fence(512,0) hwnd=0x{0:x}" -f $f.ToInt64()
# HWND_TOP (0) + SWP_NOMOVE|SWP_NOSIZE|SWP_NOACTIVATE = 0x0002|0x0001|0x0010
[void][EH]::SetWindowPos($f, [IntPtr]::Zero, 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
"elevated to HWND_TOP, waiting 3s for self-heal..."
Start-Sleep -Milliseconds 4500
& powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\z00897910\Desktop\DeskFence\tools\fenceloc.ps1
