# Right-click probe: click at (X,Y), list visible #32768 menus with owner pid.
# ASCII only.
param([Parameter(Mandatory=$true)][int]$X, [Parameter(Mandatory=$true)][int]$Y)
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class MP2 {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  public delegate bool CB(IntPtr h, IntPtr lp);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr lp);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  public static string ClickAndProbe(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(120);
    mouse_event(0x0008, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(60);
    mouse_event(0x0010, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(500);
    StringBuilder sb = new StringBuilder(64);
    StringBuilder result = new StringBuilder();
    EnumWindows(delegate(IntPtr h, IntPtr lp) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "#32768" && IsWindowVisible(h)) {
        uint p;
        GetWindowThreadProcessId(h, out p);
        RECT r;
        GetWindowRect(h, out r);
        result.Append("MENU hwnd=0x" + h.ToString("X") + " pid=" + p + " rect=" + r.L + "," + r.T + "-" + r.R + "," + r.B + ";");
      }
      return true;
    }, IntPtr.Zero);
    return result.ToString();
  }
}
"@
[void][MP2]::SetProcessDPIAware()
$res = [MP2]::ClickAndProbe($X, $Y)
$df = (Get-Process deskfence -ErrorAction SilentlyContinue | Select-Object -First 1).Id
$ex = (Get-Process explorer -ErrorAction SilentlyContinue | Select-Object -First 1).Id
"deskfence pid=$df explorer pid=$ex"
if ($res -eq "") { "NO MENU FOUND" } else { $res }
# dismiss via ESC keybd_event (SendKeys may fail without foreground rights)
Add-Type -MemberDefinition '[DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);' -Name K2 -Namespace W
[W.K2]::keybd_event(0x1B, 0, 0, [UIntPtr]::Zero)
[W.K2]::keybd_event(0x1B, 0, 2, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 250
[void][MP2]::SetCursorPos(1919, 1279)
"done"
