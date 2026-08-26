# Hover probe: minimize all, real-move cursor onto a point, wait, report pos.
# ASCII only. Args: -X -Y (target), -WaitMs
param(
  [Parameter(Mandatory=$true)][int]$X,
  [Parameter(Mandatory=$true)][int]$Y,
  [int]$WaitMs = 1200
)
(New-Object -ComObject Shell.Application).MinimizeAll()
Start-Sleep -Milliseconds 700
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class HV {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out PT p);
  [StructLayout(LayoutKind.Sequential)] public struct PT { public int X, Y; }
}
"@
[void][HV]::SetProcessDPIAware()
# land 40px left of target, then one real relative move onto it
[HV]::SetCursorPos($X - 40, $Y) | Out-Null
Start-Sleep -Milliseconds 200
[HV]::mouse_event(1, 40, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds $WaitMs
$p = New-Object HV+PT
[void][HV]::GetCursorPos([ref]$p)
"cursor now at $($p.X),$($p.Y)"
