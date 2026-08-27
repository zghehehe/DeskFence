# Three-state menu test via UIA menu-item discovery + real clicks.
# Verifies labels per state AND the transitions.
# ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class SU {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr a, string cls, string t);
  public static IntPtr tray = IntPtr.Zero;
  public static void Find() {
    StringBuilder sb = new StringBuilder(64);
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "DeskFenceTray" && IsWindowVisible(h)) tray = h;
      return true;
    }, IntPtr.Zero);
  }
  public static int VisibleBigFences() {
    StringBuilder sb = new StringBuilder(64);
    int n = 0;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "DeskFenceFence" && IsWindowVisible(h)) n++;
      return true;
    }, IntPtr.Zero);
    return n;
  }
  static IntPtr Progman() {
    StringBuilder sb = new StringBuilder(64);
    IntPtr res = IntPtr.Zero;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "Progman" && IsWindowVisible(h)) res = h;
      return true;
    }, IntPtr.Zero);
    return res;
  }
  public static bool IconsVisible() {
    IntPtr lv = FindWindowExW(Progman(), IntPtr.Zero, "SHELLDLL_DefView", null);
    if (lv == IntPtr.Zero) return false;
    lv = FindWindowExW(lv, IntPtr.Zero, "SysListView32", null);
    return lv != IntPtr.Zero && IsWindowVisible(lv);
  }
  public static void Click(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(150);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(70);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
}
"@
[void][SU]::SetProcessDpiAwarenessContext([IntPtr](-4))
[SU]::Find()
"tray=0x{0:x}" -f [SU]::tray.ToInt64()

function Open-Menu {
  [void][SU]::PostMessageW([SU]::tray, 0x8001, [IntPtr]::Zero, [IntPtr]0x0205)
  Start-Sleep -Milliseconds 900
  $root = [System.Windows.Automation.AutomationElement]::RootElement
  $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::MenuItem)
  $items = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
  $list = @()
  foreach ($i in $items) { $list += $i }
  return ,$list
}
function Click-Item($items, $name) {
  foreach ($i in $items) {
    if ($i.Current.Name -eq $name) {
      $r = $i.Current.BoundingRectangle
      [SU]::Click([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
      return $true
    }
  }
  return $false
}
function State($label) {
  Start-Sleep -Milliseconds 1400
  "{0,-30} fences={1} icons={2}" -f $label, [SU]::VisibleBigFences(), [SU]::IconsVisible()
}

State "A: normal (expect 6/False)"
$items = Open-Menu
"A menu labels: " + (($items | ForEach-Object { $_.Current.Name }) -join " | ")
$ok = Click-Item $items "隐藏全部栅栏"
"clicked hide: $ok"
State "B: zen (expect 0/False)"
$items = Open-Menu
"B menu labels: " + (($items | ForEach-Object { $_.Current.Name }) -join " | ")
$ok = Click-Item $items "恢复原始桌面"
"clicked restore-native: $ok"
State "C: native (expect 0/True)"
$items = Open-Menu
"C menu labels: " + (($items | ForEach-Object { $_.Current.Name }) -join " | ")
$ok = Click-Item $items "恢复栅栏桌面"
"clicked restore-fences: $ok"
State "A: back to normal (expect 6/False)"
"--- log ---"
Get-Content "$env:APPDATA\DeskFence\run.log" | Select-String "zen mode|original desktop" | Select-Object -Last 3
