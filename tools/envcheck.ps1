# Environment health check for DeskFence debugging.
# Answers ONE question: is the current anomaly caused by the CODE or by a
# polluted runtime environment (hours of test kills, injected windows,
# dual instances, repeated desktop toggles dirty the Explorer z-stack)?
#
# Usage:  powershell -File tools/envcheck.ps1
# Verdict CLEAN  -> anomaly is (probably) code; investigate code.
# Verdict DIRTY  -> run tools/envreset.ps1 first, retest the SAME build,
#                   only judge code if the anomaly survives a clean env.
#
# Read-only: enumerates windows, never moves/kills anything. ASCII only.
$ErrorActionPreference='Continue'
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class EC {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint c);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, string cap);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int i);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int cb);
}
"@
$problems = New-Object System.Collections.ArrayList

# 1) exactly one deskfence.exe process
$procs = @(Get-Process deskfence -ErrorAction SilentlyContinue)
if ($procs.Count -eq 0) {
    Write-Output "CHECK process: NONE RUNNING (start DeskFence first)"
    exit 1
}
if ($procs.Count -gt 1) {
    [void]$problems.Add("multi-instance: $($procs.Count) deskfence.exe processes (pids: $(($procs|ForEach-Object Id) -join ','))")
    Write-Output "CHECK process: DIRTY - $($procs.Count) instances"
} else {
    Write-Output "CHECK process: ok (pid $($procs[0].Id))"
}
$livepid = $procs[0].Id

# 2) DeskFence window census + zombie windows from dead instances
$wins = New-Object System.Collections.ArrayList
$cb=[EC+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][EC]::GetClassName($h,$sb,64)
    $c=$sb.ToString()
    if ($c -eq 'DeskFenceFence' -or $c -eq 'DeskFenceMenuHost' -or $c -eq 'DeskFenceTray') {
        $p=0; [void][EC]::GetWindowThreadProcessId($h,[ref]$p)
        [void]$wins.Add(@{h=$h;cls=$c;pid=$p})
    }
    return $true }
[void][EC]::EnumWindows($cb,[IntPtr]::Zero)
$zombies = @($wins | Where-Object { $_.pid -ne $livepid })
$fences  = @($wins | Where-Object { $_.cls -eq 'DeskFenceFence' -and $_.pid -eq $livepid })
if ($zombies.Count -gt 0) {
    [void]$problems.Add("zombie windows from dead pids: $(($zombies|ForEach-Object {"$($_.cls)/$($_.pid)"}) -join ' '))")
    Write-Output "CHECK windows : DIRTY - $($zombies.Count) orphan DeskFence windows"
} elseif ($fences.Count -eq 5) {
    Write-Output "CHECK windows : ok (5 fences + host classes, no orphans)"
} else {
    [void]$problems.Add("fence count = $($fences.Count) (expected 5)")
    Write-Output "CHECK windows : WARN - fence count $($fences.Count)"
}

# 3) desktop host present
$cands = New-Object System.Collections.ArrayList
$cb2=[EC+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][EC]::GetClassName($h,$sb,64)
    if ($sb.ToString() -eq 'Progman' -or $sb.ToString() -eq 'WorkerW') { [void]$cands.Add($h) }
    return $true }
[void][EC]::EnumWindows($cb2,[IntPtr]::Zero)
$hostH=[IntPtr]::Zero
foreach ($w in $cands) { if ([EC]::FindWindowExW($w,[IntPtr]::Zero,'SHELLDLL_DefView',$null) -ne [IntPtr]::Zero) { $hostH=$w; break } }
if ($hostH -eq [IntPtr]::Zero) {
    [void]$problems.Add("desktop host (Progman/WorkerW + DefView) not found - Explorer desktop not ready or broken")
    Write-Output "CHECK host    : DIRTY - no desktop host"
} else {
    Write-Output ("CHECK host    : ok (0x{0:X})" -f $hostH.ToInt64())
}

# 4) every fence above host within a sane walk (structural sanity)
if ($hostH -ne [IntPtr]::Zero -and $fences.Count -gt 0) {
    $bad = 0
    foreach ($f in $fences) {
        $w=[EC]::GetWindow($hostH,3); $i=0; $found=$false
        while ($w -ne [IntPtr]::Zero -and $i -lt 600) {
            $i++
            if ($w -eq $f.h) { $found=$true; break }
            $w=[EC]::GetWindow($w,3)
        }
        if (-not $found) { $bad++ }
    }
    if ($bad -gt 0) {
        [void]$problems.Add("$bad fence(s) not above host within 600 steps (sunk or band depth exploded)")
        Write-Output "CHECK fences  : DIRTY - $bad of $($fences.Count) not in band"
    } else {
        Write-Output "CHECK fences  : ok (all above host)"
    }
}

if ($problems.Count -eq 0) {
    Write-Output "VERDICT: CLEAN - anomaly is likely CODE; debug the code."
} else {
    Write-Output "VERDICT: DIRTY - run tools/envreset.ps1, retest the SAME build, then judge."
    $problems | ForEach-Object { Write-Output "  - $_" }
}
