# Real drag simulation: grab fence (512,0), drag across neighbors, release.
# Sample z positions DURING drag and after; verify never above normal windows.
# ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class DG {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  public static void Down() { mouse_event(2, 0, 0, 0, UIntPtr.Zero); }
  public static void Up() { mouse_event(4, 0, 0, 0, UIntPtr.Zero); }
  public static void MoveTo(int x, int y) { SetCursorPos(x, y); System.Threading.Thread.Sleep(60); }
}
"@
[void][DG]::SetProcessDPIAware()
function Z($label) {
  $out = & powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\z00897910\Desktop\DeskFence\tools\fenceloc.ps1 2>&1 | Select-String "aboveHost" | ForEach-Object { $_.Line -replace '.*rect=\((\d+),(\d+)\).*aboveHost\+(\d+)', '[$1,$2]+$3' }
  "$label : $($out -join ' ')"
}

Z "before drag"
[DG]::SetCursorPos(578, 40)
Start-Sleep -Milliseconds 300
[DG]::Down()
Start-Sleep -Milliseconds 200
# drag right-down across fence (256,587)
[DG]::MoveTo(500, 300)
Z "dragging mid-1"
[DG]::MoveTo(400, 700)
Z "dragging mid-2"
[DG]::MoveTo(320, 800)
Z "dragging mid-3"
Start-Sleep -Milliseconds 200
[DG]::Up()
Start-Sleep -Milliseconds 500
Z "after release +0.5s"
Start-Sleep -Milliseconds 1500
Z "after release +2s"
"--- repairs ---"
Get-Content "$env:APPDATA\DeskFence\run.log" | Select-String "z-chain repair" | Select-Object -Last 2
