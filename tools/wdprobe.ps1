$ErrorActionPreference='Continue'
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WDP {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint c);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, string cap);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
    public const uint GW_HWNDPREV = 3;
    public const uint GW_HWNDNEXT = 2;
}
"@
[void][WDP]::SetProcessDpiAwarenessContext([IntPtr](-4))

# host = WorkerW/Progman that has SHELLDLL_DefView child
$cands=New-Object System.Collections.ArrayList
$cb=[WDP+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][WDP]::GetClassName($h,$sb,64)
    if ($sb.ToString() -eq 'WorkerW' -or $sb.ToString() -eq 'Progman') { [void]$cands.Add($h) }
    return $true }
[void][WDP]::EnumWindows($cb,[IntPtr]::Zero)
$hostH=[IntPtr]::Zero; $hostCls=''
foreach ($w in $cands) {
    $sb=New-Object System.Text.StringBuilder 64
    [void][WDP]::GetClassName($w,$sb,64)
    if ([WDP]::FindWindowExW($w,[IntPtr]::Zero,'SHELLDLL_DefView',$null) -ne [IntPtr]::Zero) { $hostH=$w; $hostCls=$sb.ToString(); break }
}
Write-Output ("host=0x{0:X} cls={1}" -f $hostH.ToInt64(),$hostCls)

$dfpid=(Get-Process deskfence -ErrorAction SilentlyContinue | Select-Object -First 1).Id
if (-not $dfpid) { Write-Output "deskfence.exe not running"; exit 1 }

# fences = DeskFenceFence windows owned by current deskfence pid
$fences=New-Object System.Collections.ArrayList
$cb2=[WDP+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][WDP]::GetClassName($h,$sb,64)
    if ($sb.ToString() -eq 'DeskFenceFence') {
        $opid=0
        [void][WDP]::GetWindowThreadProcessId($h,[ref]$opid)
        if ($opid -eq $dfpid) { [void]$fences.Add($h) }
    }
    return $true }
[void][WDP]::EnumWindows($cb2,[IntPtr]::Zero)
Write-Output ("fences(pid={0}): {1}" -f $dfpid,$fences.Count)

function ShortCls($h) {
    $sb=New-Object System.Text.StringBuilder 64
    [void][WDP]::GetClassName($h,$sb,64)
    return $sb.ToString()
}

# returns string describing each fence: above-host step / below-host step / vis / iconic
function Snap {
    $out=@()
    # first window above host (context)
    $above=[WDP]::GetWindow($hostH,[WDP]::GW_HWNDPREV)
    $below=[WDP]::GetWindow($hostH,[WDP]::GW_HWNDNEXT)
    $ctx = ("aboveHost={0} belowHost={1}" -f (ShortCls $above),(ShortCls $below))
    foreach ($f in $fences) {
        $step=-1; $where='MISSING'
        $cur=[WDP]::GetWindow($hostH,[WDP]::GW_HWNDPREV); $i=0
        while ($cur -ne [IntPtr]::Zero -and $i -lt 1500) {
            $i++
            if ($cur -eq $f) { $step=$i; $where='ABOVE'; break }
            $cur=[WDP]::GetWindow($cur,[WDP]::GW_HWNDPREV)
        }
        if ($step -lt 0) {
            $cur=[WDP]::GetWindow($hostH,[WDP]::GW_HWNDNEXT); $i=0
            while ($cur -ne [IntPtr]::Zero -and $i -lt 400) {
                $i++
                if ($cur -eq $f) { $step=$i; $where='BELOW'; break }
                $cur=[WDP]::GetWindow($cur,[WDP]::GW_HWNDNEXT)
            }
        }
        $vis=[WDP]::IsWindowVisible($f); $ico=[WDP]::IsIconic($f)
        $out += ("{0}:{1}{2}/v{3}i{4}" -f ('0x{0:X}' -f $f.ToInt64()),$where,$step,[int]$vis,[int]$ico)
    }
    return "$ctx || " + ($out -join ' ')
}

$script:shotn=0
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
function Shot($tag) {
    $script:shotn++
    $b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp=New-Object System.Drawing.Bitmap($b.Width,$b.Height)
    $g=[System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size)
    $p=Join-Path $env:TEMP ("wdprobe_{0}_{1}.png" -f $tag,$script:shotn)
    $bmp.Save($p,[System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Output ("  [shot] " + $p)
}

$t0=Get-Date
$prev=''
$round=0
foreach ($phase in @('HIDE','SHOW')) {
    Write-Output ("=== phase {0} (ToggleDesktop) ===" -f $phase)
    $shell=New-Object -ComObject Shell.Application
    $shell.ToggleDesktop()
    $sw=[System.Diagnostics.Stopwatch]::StartNew()
    $nextShot=0
    while ($sw.ElapsedMilliseconds -lt 12000) {
        $s=Snap
        if ($s -ne $prev) {
            Write-Output ("  +{0,5}ms {1}" -f $sw.ElapsedMilliseconds,$s)
            $prev=$s
        }
        if (($phase -eq 'HIDE' -and $sw.ElapsedMilliseconds -gt 400 -and $nextShot -eq 0) -or
            ($phase -eq 'HIDE' -and $sw.ElapsedMilliseconds -gt 3000 -and $nextShot -eq 1)) {
            Shot $phase; $nextShot++
        }
        Start-Sleep -Milliseconds 120
    }
    Start-Sleep -Milliseconds 500
}
Write-Output ("done in {0:F1}s" -f ((Get-Date)-$t0).TotalSeconds)
