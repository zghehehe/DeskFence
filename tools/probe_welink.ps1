# Read-only probe: window attributes + process elevation for a hwnd.
param([int]$H)
$ErrorActionPreference='Continue'
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public class WP1 {
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
[DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int i);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
public struct RECT { public int L; public int T; public int R; public int B; }
[DllImport("advapi32.dll")] public static extern bool OpenProcessToken(IntPtr p, uint access, out IntPtr tok);
[DllImport("advapi32.dll")] public static extern bool GetTokenInformation(IntPtr tok, int cls, out int info, int len, out int retLen);
[DllImport("advapi32.dll")] public static extern bool CloseHandle(IntPtr h);
}
"@
$h=[IntPtr]$H
if(-not [WP1]::IsWindow($h)){ "0x{0:x}: not a window" -f $H; exit }
$sb=New-Object System.Text.StringBuilder 64
[void][WP1]::GetClassName($h,$sb,64)
$p=0; [void][WP1]::GetWindowThreadProcessId($h,[ref]$p)
$ex = [WP1]::GetWindowLongW($h, -20)
$r = New-Object WP1+RECT
[void][WP1]::GetWindowRect($h,[ref]$r)
"hwnd=0x{0:x} class={1} pid={2} vis={3} exstyle=0x{4:x} topmostbit={5} rect=({6},{7})-({8},{9})" -f `
  $H,$sb.ToString(),$p,[WP1]::IsWindowVisible($h),($ex -band 0xFFFFFFFF),[bool]($ex -band 8),$r.L,$r.T,$r.R,$r.B
# elevation: TokenElevation class = 20
$proc = Get-Process -Id $p -ErrorAction SilentlyContinue
if ($proc) {
  "proc=$($proc.ProcessName) start=$($proc.StartTime) path=$($proc.Path)"
  $tok=[IntPtr]::Zero
  if ([WP1]::OpenProcessToken($proc.Handle, 0x0008, [ref]$tok)) {
    $elev=0; $rl=0
    if ([WP1]::GetTokenInformation($tok, 20, [ref]$elev, 4, [ref]$rl)) {
      "elevated=$([bool]$elev)"
    } else { "elevation query failed err=$([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
    [void][WP1]::CloseHandle($tok)
  } else { "OpenProcessToken failed err=$([Runtime.InteropServices.Marshal]::GetLastWin32Error()) (likely higher IL)" }
}
