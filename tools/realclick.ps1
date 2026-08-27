# REAL-click flash test (user-authorized). Lock-aware: probes lock state
# after every click and aborts cleanly if the session locks.
# ASCII only.
param([int]$Rounds = 3)
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class RC {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  public static string FgClass() {
    IntPtr h = GetForegroundWindow();
    StringBuilder sb = new StringBuilder(64);
    GetClassName(h, sb, 64);
    return sb.ToString();
  }
}
"@
[void][RC]::SetProcessDPIAware()
function Stamp { return [DateTime]::Now.ToString("mm:ss.fff") }
function Shot($path) {
  Add-Type -AssemblyName System.Windows.Forms
  Add-Type -AssemblyName System.Drawing
  try {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $g.Dispose()
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    return $true
  } catch { return $false }
}
function RealClick($x, $y) {
  [RC]::SetCursorPos($x, $y) | Out-Null
  Start-Sleep -Milliseconds 120
  [RC]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)  # LEFTDOWN
  Start-Sleep -Milliseconds 60
  [RC]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)  # LEFTUP
}
function Locked() { return [RC]::FgClass() -eq 'Windows.UI.Core.CoreWindow' }

(New-Object -ComObject Shell.Application).MinimizeAll()
Start-Sleep -Milliseconds 900
"$(Stamp) baseline fg=$([RC]::FgClass())"
if (Locked) { "ALREADY LOCKED - abort"; exit 1 }

$n0 = (Get-Content "$env:APPDATA\DeskFence\run.log" | Measure-Object -Line).Lines
$fdLog = "$env:TEMP\fd_real.txt"
Remove-Item "$env:TEMP\fr_ev*.png" -ErrorAction SilentlyContinue
$fdJob = Start-Job -ScriptBlock { & C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\Users\z00897910\Desktop\DeskFence\tools\flashdet.ps1 -Seconds 22 -Prefix "$env:TEMP\fr" *> $using:fdLog }
Start-Sleep -Milliseconds 2000

foreach ($r in 1..$Rounds) {
  "$(Stamp) R$r REAL click fence triangle (228,14)"
  RealClick 228 14
  Start-Sleep -Milliseconds 900
  "$(Stamp) R$r locked=$(Locked) menu_fg_hint: click done"
  if (Locked) { "$(Stamp) LOCKED after triangle click - stopping"; break }
  "$(Stamp) R$r REAL click desktop blank (1500,640)"
  RealClick 1500 640
  Start-Sleep -Milliseconds 1200
  "$(Stamp) R$r locked=$(Locked)"
  if (Locked) { "$(Stamp) LOCKED after blank click - stopping"; break }
  Start-Sleep -Milliseconds 1500
}

$ok = Shot "$env:TEMP\after_real.png"
"$(Stamp) post-test screenshot ok=$ok (False=session likely locked)"
Wait-Job $fdJob -Timeout 20 | Out-Null
"--- flashdet ---"
Get-Content $fdLog
"--- run.log delta ---"
Get-Content "$env:APPDATA\DeskFence\run.log" | Select-Object -Skip $n0
Remove-Job $fdJob -Force -ErrorAction SilentlyContinue
"done"
