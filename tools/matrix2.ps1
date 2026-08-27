# Final matrix: {tray|triangle} x Ns after restart.
# Verified flow: real menu click -> probe MENU OPEN + measure rect -> real
# click at a measured-blank point (outside menu AND outside fences) ->
# probe MENU CLOSED -> fence count still 5 (no menu item fired).
# ASCII only.
param(
  [ValidateSet('tray','triangle')][string]$Path = 'tray',
  [int]$WaitS = 10
)
$ErrorActionPreference = 'Continue'
function Stamp { return [DateTime]::Now.ToString("mm:ss.fff") }

taskkill /F /IM deskfence.exe 2>&1 | Out-Null
Start-Sleep -Milliseconds 1200
Start-Process -FilePath "C:\Users\z00897910\Desktop\DeskFence\target\release\deskfence.exe"
Start-Sleep -Seconds $WaitS

$n0 = (Get-Content "$env:APPDATA\DeskFence\run.log" | Measure-Object -Line).Lines
$fdLog = "$env:TEMP\fd_m.txt"
Remove-Item "$env:TEMP\fm2_ev*.png", $fdLog -ErrorAction SilentlyContinue
$p = Start-Process -FilePath "C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe" `
  -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\Users\z00897910\Desktop\DeskFence\tools\flashdet.ps1','-Seconds','13','-Prefix',"$env:TEMP\fm2" `
  -RedirectStandardOutput $fdLog -RedirectStandardError "$env:TEMP\fd_m_err.txt" -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds 2000

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class TX {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  public static IntPtr MenuHwnd() {
    StringBuilder sb = new StringBuilder(64);
    IntPtr res = IntPtr.Zero;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "#32768" && IsWindowVisible(h)) res = h;
      return true;
    }, IntPtr.Zero);
    return res;
  }
  public static int CountBigFences() {
    StringBuilder sb = new StringBuilder(64);
    int n = 0;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "DeskFenceFence" && IsWindowVisible(h)) {
        RECT r; GetWindowRect(h, out r);
        if (r.R - r.L > 100) n++;
      }
      return true;
    }, IntPtr.Zero);
    return n;
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
[void][TX]::SetProcessDPIAware()

function Find-Icon {
  $root = [System.Windows.Automation.AutomationElement]::RootElement
  $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, "DeskFence")
  $hits = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
  foreach ($h in $hits) {
    $r = $h.Current.BoundingRectangle
    if ($r.X -gt 1200 -and $r.Y -gt 800) {
      return ([int]($r.X + $r.Width / 2)), ([int]($r.Y + $r.Height / 2))
    }
  }
  return 0, 0
}

"$(Stamp) [$Path +$WaitS] START  fences=$([TX]::CountBigFences())"
if ($Path -eq 'tray') {
  $bx = 0; $by = 0
  foreach ($try in 1..3) {
    [TX]::Click(1395, 1244)
    Start-Sleep -Milliseconds 1100
    $bx, $by = Find-Icon
    "try ${try}: icon at $bx,$by"
    if ($bx -ne 0) { break }
  }
  if ($bx -eq 0) { "ERROR: icon not found"; exit 1 }
  [TX]::Click($bx, $by)
} else {
  [TX]::Click(228, 14)
}
Start-Sleep -Milliseconds 1200
$m = [TX]::MenuHwnd()
if ($m -eq [IntPtr]::Zero) { "$(Stamp) ERROR: menu did not open"; exit 1 }
$mr = New-Object TX+RECT
[void][TX]::GetWindowRect($m, [ref]$mr)
"$(Stamp) menu OPEN rect=($($mr.L),$($mr.T))-($($mr.R),$($mr.B))"
# verified blank point: screen center-right band, outside menu rect (with
# margin) and outside all fences (fences end at x=700) and above taskbar
$px = 960; $py = 350
if ($px -gt ($mr.L - 60) -and $px -lt ($mr.R + 60) -and $py -gt ($mr.T - 60) -and $py -lt ($mr.B + 60)) { $py = 150 }
"$(Stamp) blank click at ($px,$py)"
[TX]::Click($px, $py)
Start-Sleep -Milliseconds 900
$mstate = if ([TX]::MenuHwnd() -eq [IntPtr]::Zero) { "CLOSED" } else { "STILL-OPEN" }
"$(Stamp) menu after blank: $mstate  fences=$([TX]::CountBigFences()) (expect CLOSED + 5)"
Start-Sleep -Milliseconds 1300

$p.WaitForExit(20000) | Out-Null
"--- flashdet ---"
Get-Content $fdLog
$delta = Get-Content "$env:APPDATA\DeskFence\run.log" | Select-Object -Skip $n0
"--- repairs: $(($delta | Select-String 'z-chain repair').Count) ---"
$delta | Select-String "z-chain repair" | Select-Object -First 3
