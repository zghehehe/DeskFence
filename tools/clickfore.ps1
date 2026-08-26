# Click, then dump foreground + visible top-level windows (API-based, no pixels).
# ASCII only.
param([int]$X = 122, [int]$Y = 34, [int]$Right = 0)
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class FW {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  public delegate bool CB(IntPtr h, IntPtr lp);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr lp);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public static string Dump() {
    StringBuilder sb = new StringBuilder(256);
    StringBuilder res = new StringBuilder();
    IntPtr fg = GetForegroundWindow();
    GetClassName(fg, sb, 256); string fgc = sb.ToString();
    sb.Clear(); GetWindowText(fg, sb, 256);
    res.Append("foreground=0x" + fg.ToString("X") + " cls=" + fgc + " title=" + sb.ToString() + "\n");
    EnumWindows(delegate(IntPtr h, IntPtr lp) {
      if (IsWindowVisible(h)) {
        uint p; GetWindowThreadProcessId(h, out p);
        GetClassName(h, sb, 256); string c = sb.ToString();
        sb.Clear(); GetWindowText(h, sb, 256);
        if (c != "ApplicationFrameWindow" && c != "Windows.UI.Core.CoreWindow") {
          res.Append("vis hwnd=0x" + h.ToString("X") + " cls=" + c + " pid=" + p + " title=" + sb.ToString() + "\n");
        }
      }
      return true;
    }, IntPtr.Zero);
    return res.ToString();
  }
}
"@
[void][FW]::SetProcessDPIAware()
"[before click]"
[FW]::Dump()
[FW]::SetCursorPos($X, $Y) | Out-Null
Start-Sleep -Milliseconds 150
if ($Right -eq 1) {
  [FW]::mouse_event(0x0008, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 60
  [FW]::mouse_event(0x0010, 0, 0, 0, [UIntPtr]::Zero)
} else {
  [FW]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
  Start-Sleep -Milliseconds 60
  [FW]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
}
Start-Sleep -Milliseconds 600
"[after click]"
[FW]::Dump()
