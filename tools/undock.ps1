# Unlock fence(0,0) via triangle menu at (228,14), then real-drag it and
# sample z during/after. BOM required.
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class UD {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  public static void Click(int x, int y) {
    SetCursorPos(x, y); System.Threading.Thread.Sleep(150);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero); System.Threading.Thread.Sleep(70);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
  public static void Down() { mouse_event(2, 0, 0, 0, UIntPtr.Zero); }
  public static void Up() { mouse_event(4, 0, 0, 0, UIntPtr.Zero); }
  public static void MoveTo(int x, int y) { SetCursorPos(x, y); System.Threading.Thread.Sleep(60); }
}
"@
[void][UD]::SetProcessDPIAware()
function Z($label) {
  $out = & powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\z00897910\Desktop\DeskFence\tools\fenceloc.ps1 2>&1 | Select-String "aboveHost" | ForEach-Object { $_.Line -replace '.*rect=\((\d+),(\d+)\).*aboveHost\+(\d+)', '[$1,$2]+$3' }
  "$label : $($out -join ' ')"
}

Z "before"
[UD]::Click(228, 14)
Start-Sleep -Milliseconds 1500
$root = [System.Windows.Automation.AutomationElement]::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::MenuItem)
$items = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
if ($items.Count -eq 0) {
  Write-Output "retry: click again"
  [UD]::Click(228, 14)
  Start-Sleep -Milliseconds 1500
  $items = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
}
$clicked = $false
foreach ($i in $items) {
  $n = $i.Current.Name
  if ($n -eq "解锁锁定位置与大小") {
    $r = $i.Current.BoundingRectangle
    [UD]::Click([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
    Write-Output "clicked: $n"
    $clicked = $true
    break
  }
}
if (-not $clicked) {
  Write-Output ("lock item not found; items: " + (($items | ForEach-Object { $_.Current.Name }) -join " | "))
  exit 1
}
Start-Sleep -Milliseconds 800
Z "after unlock"

[UD]::SetCursorPos(122, 40)
Start-Sleep -Milliseconds 300
[UD]::Down()
Start-Sleep -Milliseconds 200
[UD]::MoveTo(300, 300)
Z "drag-1"
[UD]::MoveTo(420, 700)
Z "drag-2"
[UD]::MoveTo(300, 900)
Z "drag-3"
Start-Sleep -Milliseconds 200
[UD]::Up()
Start-Sleep -Milliseconds 500
Z "release+0.5s"
Start-Sleep -Milliseconds 1500
Z "release+2s"
