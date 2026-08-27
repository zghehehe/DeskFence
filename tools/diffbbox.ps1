# Diff two PNGs: report diff count, bbox, and 160px hot cells. ASCII only.
param([Parameter(Mandatory=$true)][string]$A, [Parameter(Mandatory=$true)][string]$B)
Add-Type -AssemblyName System.Drawing
$ia = [System.Drawing.Bitmap]::FromFile($A)
$ib = [System.Drawing.Bitmap]::FromFile($B)
$w = $ia.Width; $h = $ia.Height
$bdA = $ia.LockBits((New-Object System.Drawing.Rectangle 0,0,$w,$h), [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$bdB = $ib.LockBits((New-Object System.Drawing.Rectangle 0,0,$w,$h), [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$len = $bdA.Stride * $h
$ba = New-Object byte[] $len
$bb = New-Object byte[] $len
[System.Runtime.InteropServices.Marshal]::Copy($bdA.Scan0, $ba, 0, $len)
[System.Runtime.InteropServices.Marshal]::Copy($bdB.Scan0, $bb, 0, $len)
$ia.UnlockBits($bdA); $ib.UnlockBits($bdB)
$minx = $w; $miny = $h; $maxx = -1; $maxy = -1; $n = 0; $cells = @{}
for ($y = 0; $y -lt $h; $y += 4) {
  $row = $y * $bdA.Stride
  for ($x = 0; $x -lt $w; $x += 4) {
    $o = $row + $x * 4
    $d = [Math]::Abs($ba[$o]-$bb[$o])
    $g = [Math]::Abs($ba[$o+1]-$bb[$o+1])
    $r = [Math]::Abs($ba[$o+2]-$bb[$o+2])
    $m = [Math]::Max($d, [Math]::Max($g, $r))
    if ($m -gt 2) {
      $n++
      if ($x -lt $minx) {$minx=$x}; if ($x -gt $maxx) {$maxx=$x}
      if ($y -lt $miny) {$miny=$y}; if ($y -gt $maxy) {$maxy=$y}
      $cx = [Math]::Floor($x/160); $cy = [Math]::Floor($y/160)
      $k = "$cx,$cy"
      if (-not $cells.ContainsKey($k)) { $cells[$k] = 0 }
      $cells[$k]++
    }
  }
}
"diff_px(sampled)=$n bbox=($minx,$miny)-($maxx,$maxy)"
$cells.GetEnumerator() | Where-Object { $_.Value -gt 300 } | Sort-Object Value -Descending | Select-Object -First 8 | ForEach-Object { "  cell($($_.Key)) = $($_.Value)" }
$ia.Dispose(); $ib.Dispose()
