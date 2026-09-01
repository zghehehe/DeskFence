# Read-only: current desktop icon listview + fence window visibility.
$ErrorActionPreference='Continue'
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public class DS1 {
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string cap);
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, string cap);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
}
"@
$progman = [DS1]::FindWindowW("Progman", $null)
"Progman=0x{0:x} vis={1}" -f $progman, [DS1]::IsWindowVisible($progman)
$defview = [DS1]::FindWindowExW($progman, [IntPtr]::Zero, "SHELLDLL_DefView", $null)
"DefView=0x{0:x} vis={1}" -f $defview, [DS1]::IsWindowVisible($defview)
$lv = [DS1]::FindWindowExW($defview, [IntPtr]::Zero, "SysListView32", "FolderView")
"SysListView32(native icons)=0x{0:x} vis={1}" -f $lv, [DS1]::IsWindowVisible($lv)
# WorkerW-hosted DefView (alternative desktop host)
$w = [IntPtr]::Zero
$found = [IntPtr]::Zero
while ($true) {
  $w = [DS1]::FindWindowExW([IntPtr]::Zero, $w, "WorkerW", $null)
  if ($w -eq [IntPtr]::Zero) { break }
  $dv = [DS1]::FindWindowExW($w, [IntPtr]::Zero, "SHELLDLL_DefView", $null)
  if ($dv -ne [IntPtr]::Zero) {
    $lv2 = [DS1]::FindWindowExW($dv, [IntPtr]::Zero, "SysListView32", "FolderView")
    "WorkerW=0x{0:x} DefView=0x{1:x} SysListView32=0x{2:x} vis={3}" -f $w, $dv, $lv2, [DS1]::IsWindowVisible($lv2)
  }
}
