# Probe triangle click on fence(512,0). ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class MC3 {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  public static void Click(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(140);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(60);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
  public static IntPtr tray = IntPtr.Zero;
  public static void tray_probe() {
    StringBuilder tb = new StringBuilder(64);
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, tb, 64);
      if (sb.ToString() == "DeskFenceTray" && IsWindowVisible(h)) tray = h;
      return true;
    }, IntPtr.Zero);
    if (tray != IntPtr.Zero) PostMessageW(tray, 0x8001, IntPtr.Zero, (IntPtr)0x0205);
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
[void][MC3]::SetProcessDPIAware()
$n0 = (Get-Content "$env:APPDATA/DeskFence/run.log" | Measure-Object -Line).Lines

[MC3]::tray_probe()
Start-Sleep -Milliseconds 500

Start-Sleep -Milliseconds 500
Write-Output ("t+500: " + [MC3]::MenuProbe())
Start-Sleep -Milliseconds 700
Write-Output ("t+1200: " + [MC3]::MenuProbe())
[MC3]::Click(960, 350)
Start-Sleep -Milliseconds 500
Write-Output ("after blank: " + [MC3]::MenuProbe())
Get-Content "$env:APPDATA/DeskFence/run.log" | Select-Object -Skip $n0 | Select-Object -First 5
