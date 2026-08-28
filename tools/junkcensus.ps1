$ErrorActionPreference='Continue'
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class JC {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint c);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, string cap);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int i);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int cb);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
    public const uint GW_HWNDPREV = 3;
}
"@
[void][JC]::SetProcessDpiAwarenessContext([IntPtr](-4))
$cands=New-Object System.Collections.ArrayList
$cb=[JC+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][JC]::GetClassName($h,$sb,64)
    if ($sb.ToString() -eq 'WorkerW' -or $sb.ToString() -eq 'Progman') { [void]$cands.Add($h) }
    return $true }
[void][JC]::EnumWindows($cb,[IntPtr]::Zero)
$hostH=[IntPtr]::Zero
foreach ($w in $cands) {
    if ([JC]::FindWindowExW($w,[IntPtr]::Zero,'SHELLDLL_DefView',$null) -ne [IntPtr]::Zero) { $hostH=$w; break }
}
Write-Output ("host=0x{0:X}" -f $hostH.ToInt64())
$vx=0;$vy=0;$vw=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds.Width
Add-Type -AssemblyName System.Windows.Forms
$vs=[System.Windows.Forms.SystemInformation]::VirtualScreen
$vx=$vs.X;$vy=$vs.Y;$vw=$vs.Width;$vh=$vs.Height

$rows=New-Object System.Collections.ArrayList
$w=[JC]::GetWindow($hostH,[JC]::GW_HWNDPREV); $step=0
while ($w -ne [IntPtr]::Zero -and $step -lt 500) {
    $step++
    $sb=New-Object System.Text.StringBuilder 64
    [void][JC]::GetClassName($w,$sb,64); $cls=$sb.ToString()
    $r=New-Object JC+RECT; [void][JC]::GetWindowRect($w,[ref]$r)
    $opid=0; [void][JC]::GetWindowThreadProcessId($w,[ref]$opid)
    $pn=''; try { $pn=(Get-Process -Id $opid -ErrorAction Stop).ProcessName } catch {}
    $vis=[JC]::IsWindowVisible($w); $ico=[JC]::IsIconic($w)
    $clv=0; [void][JC]::DwmGetWindowAttribute($w,14,[ref]$clv,4)
    $ex=[JC]::GetWindowLongW($w,-20)
    $tiny=($r.R-$r.L) -le 2 -or ($r.B-$r.T) -le 2
    $off=($r.R -le $vx) -or ($r.B -le $vy) -or ($r.L -ge ($vx+$vw)) -or ($r.T -ge ($vy+$vh))
    [void]$rows.Add([pscustomobject]@{step=$step;h=$w;cls=$cls;pid=$opid;pn=$pn;vis=$vis;ico=$ico;cloak=$clv;ex=$ex;tiny=$tiny;off=$off;L=$r.L;T=$r.T;R=$r.R;B=$r.B})
    $w=[JC]::GetWindow($w,[JC]::GW_HWNDPREV)
}
$total=$rows.Count
$junkIconic=($rows | Where-Object {$_.ico}).Count
$junkHidden=($rows | Where-Object {-not $_.vis}).Count
$junkCloak=($rows | Where-Object {$_.cloak -ne 0}).Count
$junkTiny=($rows | Where-Object {$_.tiny}).Count
$junkOff=($rows | Where-Object {$_.off}).Count
$live=$rows | Where-Object { $_.vis -and -not $_.ico -and $_.cloak -eq 0 -and -not $_.tiny -and -not $_.off -and $_.cls -notin @('DeskFenceFence','Shell_TrayWnd') }
Write-Output ("band depth walked: {0} steps (500 cap)" -f $total)
Write-Output ("junk: iconic={0} hidden={1} cloaked={2} tiny={3} offscreen={4}" -f $junkIconic,$junkHidden,$junkCloak,$junkTiny,$junkOff)
Write-Output ("LIVE visible-on-screen foreign windows between host and fence stack:")
foreach ($l in $live) { Write-Output ("  step{0} {1} pid={2}({3}) APPWINDOW={4} TOPMOST={5} rect=({6},{7})-({8},{9})" -f $l.step,$l.cls,$l.pid,$l.pn,((($l.ex -band 0x40000)) -ne 0),((($l.ex -band 8)) -ne 0),$l.L,$l.T,$l.R,$l.B) }
$fsteps=($rows | Where-Object {$_.cls -eq 'DeskFenceFence'} | Select-Object -First 8)
Write-Output "fences in this walk:"
foreach ($f in $fsteps) { Write-Output ("  step{0} h=0x{1:X} APPWINDOW={2}" -f $f.step,$f.h.ToInt64(),((($f.ex -band 0x40000)) -ne 0)) }
Write-Output ""
Write-Output "top junk producers (class,process) among non-live windows:"
$rows | Where-Object { $_.cls -ne 'DeskFenceFence' } | Group-Object {$_.cls+' @ '+$_.pn} | Sort-Object Count -Descending | Select-Object -First 12 | ForEach-Object { "  {0,3}x {1}" -f $_.Count,$_.Name }
