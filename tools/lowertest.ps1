# Direct unit test for fence_lower_if_blocked: insert a VISIBLE window
# between host and fences (mimicking the restore-transition state where
# app windows sit below fences), then verify the z-guard fast path lowers
# the fences below it. ASCII only.
$ErrorActionPreference='Continue'
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class LT {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint c);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, string cap);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    public static extern IntPtr CreateWindowExW(int ex, string cls, string name, int style,
        int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int ht, uint f);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
}
"@
[void][LT]::SetProcessDpiAwarenessContext([IntPtr](-4))
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class LT2 { [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid); }
"@
$dfpid=(Get-Process deskfence | Select-Object -First 1).Id
$fences=New-Object System.Collections.ArrayList
$cb=[LT+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][LT]::GetClassName($h,$sb,64)
    if ($sb.ToString() -eq 'DeskFenceFence') {
        $p=0; [void][LT2]::GetWindowThreadProcessId($h,[ref]$p)
        if ($p -eq $dfpid) { [void]$fences.Add($h) }
    }
    return $true }
[void][LT]::EnumWindows($cb,[IntPtr]::Zero)
$cands=New-Object System.Collections.ArrayList
$cb2=[LT+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][LT]::GetClassName($h,$sb,64)
    if ($sb.ToString() -eq 'Progman' -or $sb.ToString() -eq 'WorkerW') { [void]$cands.Add($h) }
    return $true }
[void][LT]::EnumWindows($cb2,[IntPtr]::Zero)
$hostH=[IntPtr]::Zero
foreach ($w in $cands) { if ([LT]::FindWindowExW($w,[IntPtr]::Zero,'SHELLDLL_DefView',$null) -ne [IntPtr]::Zero) { $hostH=$w; break } }
if ($hostH -eq [IntPtr]::Zero) { Write-Output "FAIL: no host"; exit 1 }
if ($fences.Count -lt 1) { Write-Output "FAIL: no fences"; exit 1 }

function StepsAbove($target) {
    $w=[LT]::GetWindow($hostH,3); $i=0
    while ($w -ne [IntPtr]::Zero -and $i -lt 500) {
        $i++
        if ($w -eq $target) { return $i }
        $w=[LT]::GetWindow($w,3)
    }
    return -1
}
# pick topmost fence = largest steps value
$top = $fences | ForEach-Object { @{h=$_; s=(StepsAbove $_)} } | Sort-Object s -Descending | Select-Object -First 1
Write-Output ("before: fences steps = {0}" -f (($fences | ForEach-Object { StepsAbove $_ }) -join ','))
# create visible window and park it DIRECTLY BELOW the topmost fence
# (insertAfter=X places a window directly below X)
$t=[LT]::CreateWindowExW(0,'STATIC','lowertest',[int]0x90000000, 300,300,420,300, [IntPtr]::Zero,[IntPtr]::Zero,[IntPtr]::Zero,[IntPtr]::Zero)
[void][LT]::SetWindowPos($t,$top.h, 300,300,420,300, ([int]0x0010 -bor [int]0x0040))
Write-Output ("inserted visible STATIC 0x{0:x} below top fence (step {1}), waiting 4s..." -f $t.ToInt64(),$top.s)
Start-Sleep -Seconds 4
$after = $fences | ForEach-Object { StepsAbove $_ }
$ts = StepsAbove $t
Write-Output ("after:  fences steps = {0} ; test window step = {1}" -f ($after -join ','), $ts)
[void][LT]::DestroyWindow($t)
$ok = $true
foreach ($s in $after) { if ($s -gt $ts) { $ok = $false } }  # every fence below the test window
if ($ts -lt 1) { $ok = $false }
if ($ok) { Write-Output "PASS: all fences now below the inserted visible window" }
else { Write-Output "FAIL: fences still above the inserted visible window" }
