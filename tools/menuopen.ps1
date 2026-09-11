# Targeted: flashdet + ONE menu open. Analyze event-pair magnitude histogram.
# ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class MO {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  public static IntPtr tray = IntPtr.Zero;
  public static void Find() {
    StringBuilder sb = new StringBuilder(64);
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "DeskFenceTray" && IsWindowVisible(h)) tray = h;
      return true;
    }, IntPtr.Zero);
  }
}
"@
[void][MO]::SetProcessDpiAwarenessContext([IntPtr](-4))
[MO]::Find()
# real window over the fence area (the user's desktop scenario)
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object System.Windows.Forms.Form
$f.Text = "MO-TEST"
$f.StartPosition = "Manual"
$f.Left = 8; $f.Top = 8; $f.Width = 900; $f.Height = 1100
$f.BackColor = [System.Drawing.Color]::FromArgb(40,40,46)
$f.Show()
[System.Windows.Forms.Application]::DoEvents()
Start-Sleep -Milliseconds 1200
$fdLog = "$env:TEMP\fd_mo.txt"
Remove-Item "$env:TEMP\fmo_ev*.png", $fdLog -ErrorAction SilentlyContinue
$p = Start-Process -FilePath "C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" `
  -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\Users\z00897910\Desktop\DeskFence\tools\flashdet.ps1','-Seconds','10','-Prefix',"$env:TEMP\fmo" `
  -RedirectStandardOutput $fdLog -RedirectStandardError "$env:TEMP\fd_mo_err.txt" -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 2000
"opening tray menu once"
[void][MO]::PostMessageW([MO]::tray, 0x8001, [IntPtr]::Zero, [IntPtr]0x0205)
Start-Sleep -Milliseconds 3000
# close it
[void][MO]::PostMessageW([MO]::tray, 0x8001, [IntPtr]::Zero, [IntPtr]0x0205)
Start-Sleep -Milliseconds 3000
$p.WaitForExit(20000) | Out-Null
Get-Content $fdLog
$f.Close()
