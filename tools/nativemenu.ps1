# Differential: native desktop right-click menu with overlap window.
# If the wash appears here too, it's DWM-level, not DeskFence's menu.
# ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class NR {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  public static void RightClick(int x, int y) {
    SetCursorPos(x, y); System.Threading.Thread.Sleep(150);
    mouse_event(8, 0, 0, 0, UIntPtr.Zero); System.Threading.Thread.Sleep(70);
    mouse_event(0x10, 0, 0, 0, UIntPtr.Zero);
  }
  public static void Click(int x, int y) {
    SetCursorPos(x, y); System.Threading.Thread.Sleep(150);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero); System.Threading.Thread.Sleep(70);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
}
"@
[void][NR]::SetProcessDpiAwarenessContext([IntPtr](-4))
# overlap window over fence area
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object System.Windows.Forms.Form
$f.Text = "NR-TEST"
$f.StartPosition = "Manual"
$f.Left = 8; $f.Top = 8; $f.Width = 900; $f.Height = 1100
$f.BackColor = [System.Drawing.Color]::FromArgb(40,40,46)
$f.Show()
[System.Windows.Forms.Application]::DoEvents()
Start-Sleep -Milliseconds 1200
$fdLog = "$env:TEMP\fd_nr.txt"
Remove-Item "$env:TEMP\fnr_ev*.png", $fdLog -ErrorAction SilentlyContinue
$p = Start-Process -FilePath "C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" `
  -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\Users\z00897910\Desktop\DeskFence\tools\flashdet.ps1','-Seconds','12','-Prefix',"$env:TEMP\fnr" `
  -RedirectStandardOutput $fdLog -RedirectStandardError "$env:TEMP\fd_nr_err.txt" -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 2000
"native right-click at (960,350)"
[NR]::RightClick(960, 350)
Start-Sleep -Milliseconds 1500
"dismiss with click at (300,700) (over fence area overlap)"
[NR]::Click(300, 700)
Start-Sleep -Milliseconds 1500
$p.WaitForExit(20000) | Out-Null
Get-Content $fdLog
$f.Close()
