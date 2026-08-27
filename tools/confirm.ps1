# Final confirmation: chain state + click behavior with/without sunk junk.
# ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class FC {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  public delegate bool CB(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(CB cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int ht, uint f);
  public static IntPtr progman = IntPtr.Zero;
  public static void Find() {
    StringBuilder sb = new StringBuilder(64);
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      GetClassName(h, sb, 64);
      if (sb.ToString() == "Progman" && IsWindowVisible(h)) progman = h;
      return true;
    }, IntPtr.Zero);
  }
  public static string Chain() {
    StringBuilder sb = new StringBuilder(64);
    var wins = new System.Collections.Generic.List<IntPtr>();
    EnumWindows(delegate(IntPtr h, IntPtr l) { wins.Add(h); return true; }, IntPtr.Zero);
    int idx = wins.IndexOf(progman);
    var res = new System.Text.StringBuilder();
    for (int j = idx - 1; j >= System.Math.Max(0, idx - 9); j--) {
      GetClassName(wins[j], sb, 64);
      RECT r; GetWindowRect(wins[j], out r);
      string tag = sb.ToString().Length > 14 ? sb.ToString().Substring(0, 14) : sb.ToString();
      res.AppendFormat("[{0}]{1}{2} ", idx - j, tag, IsIconic(wins[j]) ? "(MIN)" : "");
    }
    return res.ToString();
  }
  public static void Click(int x, int y) {
    SetCursorPos(x, y);
    System.Threading.Thread.Sleep(120);
    mouse_event(2, 0, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(50);
    mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
}
"@
[void][FC]::SetProcessDPIAware()
function Stamp { return [DateTime]::Now.ToString("mm:ss.fff") }
[FC]::Find()
$n0 = (Get-Content "$env:APPDATA\DeskFence\run.log" | Measure-Object -Line).Lines

"=== state 1: current chain ==="
[FC]::Chain()
"$(Stamp) triangle click + blank click (round A)"
[FC]::Click(228, 14); Start-Sleep -Milliseconds 900
[FC]::Click(1500, 640); Start-Sleep -Milliseconds 1500
$d = Get-Content "$env:APPDATA\DeskFence\run.log" | Select-Object -Skip $n0
"repairs in round A: $(($d | Select-String 'z-chain repair').Count)"
[FC]::Chain()

"=== state 2: MinimizeAll (sinks minimized junk between host and fences) ==="
(New-Object -ComObject Shell.Application).MinimizeAll()
Start-Sleep -Milliseconds 900
[FC]::Chain()
$n1 = (Get-Content "$env:APPDATA\DeskFence\run.log" | Measure-Object -Line).Lines
"$(Stamp) triangle click + blank click (round B, junk sunk)"
[FC]::Click(228, 14); Start-Sleep -Milliseconds 900
[FC]::Click(1500, 640); Start-Sleep -Milliseconds 1500
$d2 = Get-Content "$env:APPDATA\DeskFence\run.log" | Select-Object -Skip $n1
"repairs in round B: $(($d2 | Select-String 'z-chain repair').Count)"
$d2 | Select-String "z-chain repair" | Select-Object -First 4
[FC]::Chain()
