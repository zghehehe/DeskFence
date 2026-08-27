# Persistence test: set each desktop state via real menu click, restart the
# app, verify the state survived. BOM required (Chinese literals).
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class PT {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr a, string cls, string t);
  public static IntPtr tray = IntPtr.Zero;
  public static void Find() {
    var sb = new System.Text.StringBuilder(64);
    EnumWindows(delegate(IntPtr h, IntPtr l) { GetClassName(h, sb, 64); if (sb.ToString() == "DeskFenceTray" && IsWindowVisible(h)) tray = h; return true; }, IntPtr.Zero);
  }
  public static int VisibleFences() {
    var sb = new System.Text.StringBuilder(64);
    int n = 0;
    EnumWindows(delegate(IntPtr h, IntPtr l) { GetClassName(h, sb, 64); if (sb.ToString() == "DeskFenceFence" && IsWindowVisible(h)) n++; return true; }, IntPtr.Zero);
    return n;
  }
  static IntPtr Prog() {
    var sb = new System.Text.StringBuilder(64);
    IntPtr r = IntPtr.Zero;
    EnumWindows(delegate(IntPtr h, IntPtr l) { GetClassName(h, sb, 64); if (sb.ToString() == "Progman" && IsWindowVisible(h)) r = h; return true; }, IntPtr.Zero);
    return r;
  }
  public static bool IconsVisible() {
    IntPtr lv = FindWindowExW(Prog(), IntPtr.Zero, "SHELLDLL_DefView", null);
    if (lv == IntPtr.Zero) return false;
    lv = FindWindowExW(lv, IntPtr.Zero, "SysListView32", null);
    return lv != IntPtr.Zero && IsWindowVisible(lv);
  }
  public static void Click(int x, int y) {
    SetCursorPos(x, y); System.Threading.Thread.Sleep(150);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero); System.Threading.Thread.Sleep(70);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
}
"@
[void][PT]::SetProcessDpiAwarenessContext([IntPtr](-4))

function Click-MenuItem($name) {
  [PT]::Find()
  [void][PT]::PostMessageW([PT]::tray, 0x8001, [IntPtr]::Zero, [IntPtr]0x0205)
  Start-Sleep -Milliseconds 900
  $root = [System.Windows.Automation.AutomationElement]::RootElement
  $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::MenuItem)
  $items = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
  foreach ($i in $items) {
    if ($i.Current.Name -eq $name) {
      $r = $i.Current.BoundingRectangle
      [PT]::Click([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
      return $true
    }
  }
  return $false
}
function Restart-App {
  taskkill /F /IM deskfence.exe 2>&1 | Out-Null
  Start-Sleep -Milliseconds 1200
  Start-Process -FilePath "C:\Users\z00897910\Desktop\DeskFence\target\release\deskfence.exe"
  Start-Sleep -Seconds 6
}
function Show-State($label) {
  "{0,-34} fences={1} icons={2} state={3}" -f $label, [PT]::VisibleFences(), [PT]::IconsVisible(), (Get-Content "$env:APPDATA\DeskFence\settings.json" | ConvertFrom-Json).desktop_state
}

Restart-App
Show-State "baseline normal (expect 6/False/normal)"

# zen -> restart -> still zen
$ok = Click-MenuItem "隐藏全部栅栏"
Start-Sleep -Milliseconds 1200
Show-State "set zen (expect 1/False/zen): click=$ok"
Restart-App
Show-State "restart in zen (expect 1/False/zen)"

# back to normal via menu, then native -> restart -> still native
$ok = Click-MenuItem "恢复栅栏桌面"
Start-Sleep -Milliseconds 1500
Show-State "back to normal (expect 6/False/normal): click=$ok"
$ok = Click-MenuItem "恢复原始桌面"
Start-Sleep -Milliseconds 1200
Show-State "set native (expect 1/True/native): click=$ok"
Restart-App
Show-State "restart in native (expect 1/True/native)"

# restore normal and leave it there
$ok = Click-MenuItem "恢复栅栏桌面"
Start-Sleep -Milliseconds 1500
Show-State "final normal (expect 6/False/normal): click=$ok"
"--- boot logs ---"
Get-Content "$env:APPDATA\DeskFence\run.log" | Select-String "boot restores desktop_state|zen boot" | Select-Object -Last 4
