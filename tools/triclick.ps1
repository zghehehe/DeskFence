# Triangle menu probe: real click at (228,14), verify menu stays open. ASCII.
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class TC {
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
[void][TC]::SetProcessDPIAware()
$n0 = (Get-Content "$env:APPDATA/DeskFence/run.log" | Measure-Object -Line).Lines
[TC]::Click(228, 14)
Start-Sleep -Milliseconds 500
Write-Output ("t+500: " + [TC]::MenuProbe())
Start-Sleep -Milliseconds 700
Write-Output ("t+1200: " + [TC]::MenuProbe())
[System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point(960, 350)
Start-Sleep -Milliseconds 200
Add-Type -MemberDefinition '[DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);' -Name ME -Namespace W3
[W3.ME]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 70
[W3.ME]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 500
Write-Output ("after blank: " + [TC]::MenuProbe())
Get-Content "$env:APPDATA/DeskFence/run.log" | Select-Object -Skip $n0 | Select-Object -First 6
