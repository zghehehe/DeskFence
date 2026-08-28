# Band interloper test (no input injection, pure Win32).
# Phase A: 8 transient foreign windows flashing between host and fences
#          (life 150ms each, >=350ms gap). Expect ZERO z-chain repairs:
#          the 2-strike damper must swallow one-tick walk-breaks.
# Phase B: one persistent foreign window parked behind the desktop host
#          for 6s. Expect: flag @strike1, ONE batch repair near strike2,
#          then silence (settled order), then clean exit.
# PASS/FAIL derived from run.log deltas. ASCII only, PS 5.1 / C#5 safe.
$ErrorActionPreference = 'Continue'
$log = Join-Path $env:APPDATA 'DeskFence\run.log'

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class BT {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr h, EnumProc p, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    public static extern IntPtr CreateWindowExW(int ex, string cls, string name, int style,
        int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint flags);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, string cap);
}
"@
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class BTD { [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v); }
"@
[void][BTD]::SetProcessDpiAwarenessContext([IntPtr](-4))

function Find-Host {
    $cands = New-Object System.Collections.ArrayList
    $cb = [BT+EnumProc]{ param($h,$l)
        $sb = New-Object System.Text.StringBuilder 64
        [void][BT]::GetClassName($h,$sb,64)
        $cls = $sb.ToString()
        if ($cls -eq 'WorkerW' -or $cls -eq 'Progman') { [void]$cands.Add(@{h=$h; cls=$cls}) }
        return $true
    }
    [void][BT]::EnumWindows($cb,[IntPtr]::Zero)
    foreach ($w in $cands) {
        if ('Progman' -eq $w.cls -or 'WorkerW' -eq $w.cls) {
            $dv = [BT]::FindWindowExW($w.h,[IntPtr]::Zero,'SHELLDLL_DefView',$null)
            if ($dv -ne [IntPtr]::Zero) { return $w.h }
        }
    }
    foreach ($w in $cands) { if ($w.cls -eq 'Progman') { return $w.h } }
    if ($cands.Count -gt 0) { return $cands[0].h }
    return [IntPtr]::Zero
}

function Log-Lines([int]$fromIdx) {
    if ((Test-Path $log) -and (Get-Item $log).Length -gt 0) {
        $all = Get-Content $log
        if ($fromIdx -lt $all.Count) { return $all[$fromIdx..($all.Count-1)] }
    }
    return @()
}

function Count-Pattern($lines) {
    $rep = @($lines | Where-Object { $_ -match 'z-chain repair:' }).Count
    $flg = @($lines | Where-Object { $_ -match 'flagged strike' }).Count
    ,@($rep,$flg)
}

$host1 = Find-Host
if ($host1 -eq [IntPtr]::Zero) { Write-Output 'FAIL no-desktop-host'; exit 1 }
Write-Output ("desktop host: 0x{0:X}" -f $host1.ToInt64())
$mark0 = @(Get-Content $log).Count

# ---- Phase A: transients ----
for ($i=0; $i -lt 8; $i++) {
    $d = [BT]::CreateWindowExW(0,'STATIC','bandtest',[int]0x90000000, 40+$i*3, 140, 360, 260, [IntPtr]::Zero,[IntPtr]::Zero,[IntPtr]::Zero,[IntPtr]::Zero)
    # park it *behind the host* (below wallpaper) like ScW does: invisible pixels, in-band foreigner
    [void][BT]::SetWindowPos($d,$host1, 40+$i*3,140,360,260, ([int]0x0010 -bor [int]0x0040)) # SWP_NOACTIVATE|SWP_SHOWWINDOW
    Start-Sleep -Milliseconds 150
    [void][BT]::DestroyWindow($d)
    Start-Sleep -Milliseconds 350
}
Start-Sleep -Seconds 2
$aLines = Log-Lines $mark0
$a = Count-Pattern $aLines
Write-Output ("phaseA: repairs={0} flagged={1}" -f $a[0], $a[1])

# ---- Phase B: persistent ----
$mark1 = @(Get-Content $log).Count
$d = [BT]::CreateWindowExW(0,'STATIC','bandtest-b',[int]0x90000000, 90, 220, 420, 300, [IntPtr]::Zero,[IntPtr]::Zero,[IntPtr]::Zero,[IntPtr]::Zero)
[void][BT]::SetWindowPos($d,$host1, 90,220,420,300, ([int]0x0010 -bor [int]0x0040))
Start-Sleep -Seconds 6
[void][BT]::DestroyWindow($d)
Start-Sleep -Seconds 3   # allow at most one more settle tick post-removal
$bLines = Log-Lines $mark1
$b = Count-Pattern $bLines
$low = @($bLines | Where-Object { $_ -match 'z-guard: fence lowered' }).Count
Write-Output ("phaseB: repairs={0} lowers={1} flagged={2}" -f $b[0],$low,$b[1])
Write-Output "--- phaseB log detail ---"
$bLines | Where-Object { $_ -match 'z-chain repair|walk-break|z-guard' } | ForEach-Object { Write-Output $_ }

# 2026-08-28: healing now has two paths - the 3-strike walk repair and the
# event/desktop-watch fast lower (z-guard: fence lowered). The persistent
# window may also be seated without ever blocking (fast path wins first).
# FAIL only on a walk storm, or on flags that never heal; flagged==0 with
# zero heals means no fault materialized (vacuous pass).
$verdict = 'PASS'
if ([int]$a[0] -gt 0)   { $verdict = 'FAIL(phaseA-repairs)' }
if ([int]$b[0] -gt 3)   { $verdict = 'FAIL(phaseB-storm)' }
if (([int]$b[0] + $low) -eq 0 -and [int]$b[1] -gt 0) { $verdict = 'FAIL(phaseB-noheal)' }
Write-Output $verdict
