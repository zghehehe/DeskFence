param([int]$H)
$ErrorActionPreference='Continue'
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public class WI1 {
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
}
"@
$h=[IntPtr]$H
if(-not [WI1]::IsWindow($h)){ "0x{0:x}: not a window (stale handle)" -f $H; exit }
$sb=New-Object System.Text.StringBuilder 64
[void][WI1]::GetClassName($h,$sb,64)
$p=0; [void][WI1]::GetWindowThreadProcessId($h,[ref]$p)
$pn=''; try { $pn=(Get-Process -Id $p -ErrorAction Stop).ProcessName } catch {}
"0x{0:x}: class={1} pid={2}({3}) vis={4}" -f $H,$sb.ToString(),$p,$pn,[WI1]::IsWindowVisible($h)
