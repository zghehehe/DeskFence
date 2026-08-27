# Tray menu probe: PostMessage open, verify menu stays open. ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class TP {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  public static IntPtr tray = IntPtr.Zero;
  public static void Find() {
    StringBuilder sb = new StringBuilder(64);
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "DeskFenceTray" && IsWindowVisible(h)) tray = h;
      return true;
    }, IntPtr.Zero);
  }
  public static void OpenTrayMenu() {
    PostMessageW(tray, 0x8001, IntPtr.Zero, (IntPtr)0x0205);
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
[void][TP]::SetProcessDPIAware()
$n0 = (Get-Content "$env:APPDATA/DeskFence/run.log" | Measure-Object -Line).Lines
[TP]::Find()
[TP]::OpenTrayMenu()
Start-Sleep -Milliseconds 500
Write-Output ("t+500: " + [TP]::MenuProbe())
Start-Sleep -Milliseconds 700
Write-Output ("t+1200: " + [TP]::MenuProbe())
# close it via a real click elsewhere
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point(960, 350)
Start-Sleep -Milliseconds 200
Add-Type -MemberDefinition '[DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);' -Name ME -Namespace W2
[W2.ME]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 70
[W2.ME]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 500
Write-Output ("after blank: " + [TP]::MenuProbe())
Get-Content "$env:APPDATA/DeskFence/run.log" | Select-Object -Skip $n0 | Select-Object -First 6
