# Clean single-shot triangle menu test. ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class TCC {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  public static void Click(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(150);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(70);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
  public static void Esc() {
    keybd_event(0x1B, 0, 0, UIntPtr.Zero);
    keybd_event(0x1B, 0, 2, UIntPtr.Zero);
  }
  public static string MenuProbe() {
    StringBuilder sb = new StringBuilder(64);
    string r = "";
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "#32768" && IsWindowVisible(h)) r += "OPEN;";
      return true;
    }, IntPtr.Zero);
    return r == "" ? "CLOSED" : r;
  }
}
"@
[void][TCC]::SetProcessDPIAware()
$n0 = (Get-Content "$env:APPDATA/DeskFence/run.log" | Measure-Object -Line).Lines
# clean slate: click far bottom-right (no fences, no menus) + Esc
[TCC]::Click(1900, 640)
Start-Sleep -Milliseconds 400
[TCC]::Esc()
Start-Sleep -Milliseconds 500
Write-Output ("pre-check: " + [TCC]::MenuProbe())
Write-Output "=== triangle click ==="
[TCC]::Click(228, 14)
Start-Sleep -Milliseconds 500
Write-Output ("t+500: " + [TCC]::MenuProbe())
Start-Sleep -Milliseconds 800
Write-Output ("t+1300: " + [TCC]::MenuProbe())
[TCC]::Esc()
Start-Sleep -Milliseconds 400
Write-Output ("after esc: " + [TCC]::MenuProbe())
Get-Content "$env:APPDATA/DeskFence/run.log" | Select-Object -Skip $n0 | Select-Object -First 8
