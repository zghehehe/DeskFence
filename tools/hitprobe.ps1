# Right-click at a point, report visible #32768 menu windows with owner pid.
# ASCII only. Args: -X -Y
param([Parameter(Mandatory=$true)][int]$X, [Parameter(Mandatory=$true)][int]$Y)
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class MT {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  public delegate bool CB(IntPtr h, IntPtr lp);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr lp);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
"@
[void][MT]::SetProcessDPIAware()
[MT]::SetCursorPos($X, $Y) | Out-Null
Start-Sleep -Milliseconds 120
[MT]::mouse_event(0x0008, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 60
[MT]::mouse_event(0x0010, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 500
$sb = New-Object System.Text.StringBuilder 64
$found = @()
$cb = {
  param([IntPtr]$h, [IntPtr]$l)
  [void][MT]::GetClassName($h, $sb, 64)
  if ($sb.ToString() -eq "#32768" -and [MT]::IsWindowVisible($h)) {
    $p = 0
    [void][MT]::GetWindowThreadProcessId($h, [ref]$p)
    $script:found += "menu hwnd=0x$($h.ToString('X')) pid=$p"
  }
  return $true
}
[MT]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
$df = 0
Get-Process deskfence -ErrorAction SilentlyContinue | ForEach-Object { $df = $_.Id }
$ex = 0
Get-Process explorer -ErrorAction SilentlyContinue | ForEach-Object { $ex = $_.Id }
"deskfence pid=$df explorer pid=$ex"
if ($found.Count -eq 0) { "NO MENU FOUND" } else { $found | ForEach-Object { $_ } }
# dismiss
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.SendKeys]::SendWait("{ESC}")
Start-Sleep -Milliseconds 200
[MT]::SetCursorPos(1919, 1279) | Out-Null
"done"
