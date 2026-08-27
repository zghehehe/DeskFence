# Menu-then-blank-click repro WITHOUT real input: pure PostMessage.
# 1) WM_RBUTTONUP on fence title -> triangle context menu opens (track()).
# 2) WM_CANCELMODE to menu host -> closes menu like a dismiss.
# 3) WM_LBUTTONDOWN/UP to the desktop listview at a blank point -> the
#    click Explorer would receive after menu dismissal.
# Also samples fence-window visibility around each step. ASCII only.
$ErrorActionPreference='Continue'
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class MR {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, string cap);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
}
"@
[void][MR]::SetProcessDpiAwarenessContext([IntPtr](-4))
$fences = New-Object System.Collections.ArrayList
$cb=[MR+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][MR]::GetClassName($h,$sb,64)
    if ($sb.ToString() -eq 'DeskFenceFence' -and [MR]::IsWindowVisible($h)) {
        [void]$fences.Add(@{h=$h})
    }
    return $true }
[void][MR]::EnumWindows($cb,[IntPtr]::Zero)
if ($fences.Count -eq 0) { Write-Output 'no visible fence'; exit 1 }
# pick the biggest-order first fence (band order walk not needed; use first)
$f1 = $fences[0].h
Write-Output ("fence hwnd=0x{0:X} count={1}" -f $f1.ToInt64(), $fences.Count)
# find desktop listview via Progman->DefView
$prog=[IntPtr]::Zero
$cands=New-Object System.Collections.ArrayList
$cb2=[MR+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][MR]::GetClassName($h,$sb,64)
    $c=$sb.ToString()
    if ($c -eq 'Progman' -or $c -eq 'WorkerW') { [void]$cands.Add($h) }
    return $true }
[void][MR]::EnumWindows($cb2,[IntPtr]::Zero)
foreach ($h in $cands) {
    $dv=[MR]::FindWindowExW($h,[IntPtr]::Zero,'SHELLDLL_DefView',$null)
    if ($dv -ne [IntPtr]::Zero) { $lv=[MR]::FindWindowExW($dv,[IntPtr]::Zero,'SysListView32',$null); if ($lv -ne [IntPtr]::Zero){ $prog=$lv; break } }
}
Write-Output ("listview hwnd=0x{0:X}" -f $prog.ToInt64())
$menuhost=[IntPtr]::Zero
$cb3=[MR+EnumProc]{ param($h,$l) return $true }
function VisSample($tag){
    $vis=@()
    foreach($f in $fences){ $vis += ("0x{0:X}:{1}" -f $f.h.ToInt64(), [MR]::IsWindowVisible($f.h)) }
    Write-Output ("VIS[{0}] {1}" -f $tag, ($vis -join ' '))
}
VisSample 'before'
for ($round=1; $round -le 2; $round++) {
    # open triangle context menu: right-button up in upper-left of the fence (title zone)
    [void][MR]::PostMessageW($f1, 0x0205, [IntPtr]6, [IntPtr](60 -shl 16 -bor 120)) # WM_RBUTTONUP
    Start-Sleep -Milliseconds 500
    # cancel menu through its owner chain: broadcast cancel to fence + desktop
    [void][MR]::SendMessageW($f1, 0x001F, [IntPtr]::Zero, [IntPtr]::Zero)          # WM_CANCELMODE
    Start-Sleep -Milliseconds 200
    # blank desktop click that lands after dismissal
    [void][MR]::PostMessageW($prog, 0x0201, [IntPtr]8,  [IntPtr](300 -shl 16 -bor 700)) # WM_LBUTTONDOWN
    Start-Sleep -Milliseconds 60
    [void][MR]::PostMessageW($prog, 0x0202, [IntPtr]8,  [IntPtr](300 -shl 16 -bor 700)) # WM_LBUTTONUP
    Start-Sleep -Milliseconds 900
    VisSample "after-r$round"
}
Start-Sleep -Milliseconds 1500
VisSample 'final'
