# Matrix v2: restart deskfence, wait N seconds, 2 rounds of real triangle+
# blank clicks; flashdet runs as a DETACHED process writing to a log file.
# ASCII only.
param([int]$WaitS = 10)
$ErrorActionPreference = 'Continue'
function Stamp { return [DateTime]::Now.ToString("mm:ss.fff") }

taskkill /F /IM deskfence.exe 2>&1 | Out-Null
Start-Sleep -Milliseconds 1200
Start-Process -FilePath "C:\Users\z00897910\Desktop\DeskFence\target\release\deskfence.exe"
"$(Stamp) restarted, waiting ${WaitS}s"
Start-Sleep -Seconds $WaitS

$n0 = (Get-Content "$env:APPDATA\DeskFence\run.log" | Measure-Object -Line).Lines
$fdLog = "$env:TEMP\fd_mat.txt"
$fdErr = "$env:TEMP\fd_mat_err.txt"
Remove-Item $fdLog, $fdErr, "$env:TEMP\fm_ev*.png" -ErrorAction SilentlyContinue
$p = Start-Process -FilePath "C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" `
  -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\Users\z00897910\Desktop\DeskFence\tools\flashdet.ps1','-Seconds','16','-Prefix',"$env:TEMP\fm" `
  -RedirectStandardOutput $fdLog -RedirectStandardError $fdErr -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 2500

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class MX2 {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  public static void Click(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(120);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(50);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
}
"@
[void][MX2]::SetProcessDPIAware()

foreach ($r in 1..2) {
  "$(Stamp) R$r triangle click"
  [MX2]::Click(228, 14)
  Start-Sleep -Milliseconds 1000
  "$(Stamp) R$r blank click"
  [MX2]::Click(1500, 640)
  Start-Sleep -Milliseconds 1800
}
$p.WaitForExit(25000) | Out-Null
"--- flashdet ---"
Get-Content $fdLog
Get-Content $fdErr -ErrorAction SilentlyContinue | Select-Object -First 3
"--- repairs/captures in window ---"
$delta = Get-Content "$env:APPDATA\DeskFence\run.log" | Select-Object -Skip $n0
($delta | Select-String "z-chain repair").Count.ToString() + " repairs"
$delta | Select-String "z-chain repair|wallpaper capture ok" | Select-Object -First 6
