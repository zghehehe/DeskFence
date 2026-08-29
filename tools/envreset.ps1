# Environment reset for DeskFence debugging: rebuild a clean desktop.
# What it does (the exact procedure that recovered the golden version on
# 2026-08-29 after hours of test pollution):
#   1) stop deskfence
#   2) restart Explorer (rebuilds the desktop window chain, junk layers,
#      SPES hook-layer positions, zombie tray icons)
#   3) wait for the desktop host to be ready
#   4) start deskfence, wait for all fences to attach
#   5) verify: single process, 5 fences above host
#
# Usage:  powershell -File tools\envreset.ps1            (do it)
#         powershell -File tools\envreset.ps1 -CheckOnly (print plan only)
param([switch]$CheckOnly)
$ErrorActionPreference='Continue'
$root = Split-Path -Parent $PSScriptRoot
$exe  = Join-Path $root 'target\release\deskfence.exe'

Write-Output "plan: stop deskfence -> restart explorer -> wait host -> start deskfence -> verify"
if ($CheckOnly) { Write-Output "(CheckOnly: nothing executed)"; exit 0 }

taskkill /F /IM deskfence.exe 2>&1 | Out-Null
Start-Sleep -Milliseconds 800
taskkill /F /IM explorer.exe 2>&1 | Out-Null
Start-Sleep -Seconds 2
Start-Process explorer.exe
Write-Output "explorer restarted, waiting for desktop host..."

Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class ER {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, string cap);
}
"@
$hostReady = $false
for ($i=0; $i -lt 30; $i++) {
    Start-Sleep -Seconds 1
    $cands = New-Object System.Collections.ArrayList
    $cb=[ER+EnumProc]{ param($h,$l)
        $sb=New-Object System.Text.StringBuilder 64
        [void][ER]::GetClassName($h,$sb,64)
        if ($sb.ToString() -eq 'Progman' -or $sb.ToString() -eq 'WorkerW') { [void]$cands.Add($h) }
        return $true }
    [void][ER]::EnumWindows($cb,[IntPtr]::Zero)
    foreach ($w in $cands) {
        if ([ER]::FindWindowExW($w,[IntPtr]::Zero,'SHELLDLL_DefView',$null) -ne [IntPtr]::Zero) { $hostReady=$true; break }
    }
    if ($hostReady) { break }
}
if (-not $hostReady) { Write-Output "FAIL: desktop host not ready after 30s"; exit 1 }
Write-Output ("host ready after ~{0}s" -f ($i+1))

Start-Process $exe
$fencesUp = $false
for ($i=0; $i -lt 15; $i++) {
    Start-Sleep -Seconds 1
    $n = & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'envcheck.ps1') | Select-String 'VERDICT|CHECK fences'
    $line = ($n | Where-Object { $_ -match 'CHECK fences' }) -join ''
    if ($line -match 'ok') { $fencesUp=$true; break }
}
$verdict = & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'envcheck.ps1') | Select-String 'VERDICT'
Write-Output $verdict
if ($fencesUp) { Write-Output "ENVRESET OK - clean desktop, fences attached. Retest the SAME build now." }
else { Write-Output "ENVRESET INCOMPLETE - fences not settled; check run.log boot lines." }
