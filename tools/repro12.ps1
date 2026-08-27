# Reproduce issue-1: real window over fence area + menu interactions,
# monitoring fence z positions at every step. ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class FF {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  public static IntPtr tray = IntPtr.Zero;
  public static void Find() {
    StringBuilder sb = new StringBuilder(64);
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "DeskFenceTray" && IsWindowVisible(h)) tray = h;
      return true;
    }, IntPtr.Zero);
  }
  public static void Click(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(140);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(60);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
}
"@
[void][FF]::SetProcessDpiAwarenessContext([IntPtr](-4))
function Stamp { return [DateTime]::Now.ToString("mm:ss.fff") }

# reuse fenceloc logic inline (positions of all fences + host index)
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class FD2 {
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  public static string Dump() {
    var sb = new StringBuilder(64);
    var wins = new System.Collections.Generic.List<IntPtr>();
    EnumWindows(delegate(IntPtr h, IntPtr l) { wins.Add(h); return true; }, IntPtr.Zero);
    int pi = -1;
    for (int i = 0; i < wins.Count; i++) { GetClassName(wins[i], sb, 64); if (sb.ToString() == "Progman") { pi = i; break; } }
    var res = new System.Text.StringBuilder("progman@" + pi + " ");
    for (int i = 0; i < wins.Count; i++) {
      GetClassName(wins[i], sb, 64);
      if (sb.ToString() == "DeskFenceFence") {
        RECT r; GetWindowRect(wins[i], out r);
        if (r.R - r.L > 100) res.Append("[" + (r.L) + "," + (r.T) + "]@" + i + " ");
      }
    }
    return res.ToString();
  }
}
"@
[void][FD2]::SetProcessDpiAwarenessContext([IntPtr](-4))
function Z { "$(Stamp) z: $([FD2]::Dump())" }

$n0 = (Get-Content "$env:APPDATA\DeskFence\run.log" | Measure-Object -Line).Lines
$fdLog = "$env:TEMP\fd_rep.txt"
Remove-Item "$env:TEMP\fr2_ev*.png", $fdLog -ErrorAction SilentlyContinue
$p = Start-Process -FilePath "C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" `
  -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\Users\z00897910\Desktop\DeskFence\tools\flashdet.ps1','-Seconds','25','-Prefix',"$env:TEMP\fr2" `
  -RedirectStandardOutput $fdLog -RedirectStandardError "$env:TEMP\fd_rep_err.txt" -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 1500

# real app window over fence area (fences span x 0..644)
$f = New-Object System.Windows.Forms.Form
$f.Text = "ZFLOAT-TEST"
$f.StartPosition = "Manual"
$f.Left = 8; $f.Top = 8; $f.Width = 900; $f.Height = 1100
$f.BackColor = [System.Drawing.Color]::FromArgb(40,40,46)
$f.Show()
[System.Windows.Forms.Application]::DoEvents()
Start-Sleep -Milliseconds 800
Z
"$(Stamp) app window created over fence area"

# menu cycle 1: triangle
[FF]::Click(228, 14)
Start-Sleep -Milliseconds 1000
"$(Stamp) after triangle click: " + (Z)
[FF]::Click(960, 350)
Start-Sleep -Milliseconds 1000
"$(Stamp) after blank click: " + (Z)

# menu cycle 2: tray via PostMessage
[FF]::Find()
[void][FF]::PostMessageW([FF]::tray, 0x8001, [IntPtr]::Zero, [IntPtr]0x0205)
Start-Sleep -Milliseconds 1000
"$(Stamp) after tray menu: " + (Z)
[FF]::Click(960, 350)
Start-Sleep -Milliseconds 1000
"$(Stamp) after blank click: " + (Z)

# wait for tick cycle and re-check repeatedly
foreach ($i in 1..4) {
  Start-Sleep -Milliseconds 1000
  Z
}
$f.Close()
$p.WaitForExit(15000) | Out-Null
"--- flashdet ---"
Get-Content $fdLog
"--- repairs ---"
Get-Content "$env:APPDATA\DeskFence\run.log" | Select-Object -Skip $n0 | Select-String "z-chain repair|zen|desktop_state" | Select-Object -First 6
