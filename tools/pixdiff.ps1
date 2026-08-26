# Pixel diff between two same-size screenshots.
# Pure ASCII. Usage: -A a.png -B b.png
param(
  [Parameter(Mandatory=$true)][string]$A,
  [Parameter(Mandatory=$true)][string]$B
)
Add-Type -AssemblyName System.Drawing

$ia = [System.Drawing.Bitmap]::FromFile($A)
$ib = [System.Drawing.Bitmap]::FromFile($B)
if ($ia.Width -ne $ib.Width -or $ia.Height -ne $ib.Height) {
  throw "size mismatch: $($ia.Width)x$($ia.Height) vs $($ib.Width)x$($ib.Height)"
}
$w = $ia.Width; $h = $ia.Height

$bdA = $ia.LockBits((New-Object System.Drawing.Rectangle 0,0,$w,$h), [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$bdB = $ib.LockBits((New-Object System.Drawing.Rectangle 0,0,$w,$h), [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$len = $bdA.Stride * $h
$ba = New-Object byte[] $len
$bb = New-Object byte[] $len
[System.Runtime.InteropServices.Marshal]::Copy($bdA.Scan0, $ba, 0, $len)
[System.Runtime.InteropServices.Marshal]::Copy($bdB.Scan0, $bb, 0, $len)
$ia.UnlockBits($bdA); $ib.UnlockBits($bdB)

$c2 = 0; $c8 = 0; $c32 = 0; $maxd = 0; $ndiff = 0
$minx = $w; $miny = $h; $maxx = -1; $maxy = -1
$rowhist = @{}
for ($y = 0; $y -lt $h; $y++) {
  $row = $y * $bdA.Stride
  $rowDiff = 0
  for ($x = 0; $x -lt $w; $x++) {
    $o = $row + $x * 4
    $d = 0
    for ($c = 0; $c -lt 3; $c++) {
      $dd = [Math]::Abs($ba[$o+$c] - $bb[$o+$c])
      if ($dd -gt $d) { $d = $dd }
    }
    if ($d -gt 0) {
      $ndiff++
      $rowDiff++
      if ($d -gt $maxd) { $maxd = $d }
      if ($d -gt 2) { $c2++ }
      if ($d -gt 8) { $c8++ }
      if ($d -gt 32) { $c32++ }
      if ($x -lt $minx) { $minx = $x }
      if ($x -gt $maxx) { $maxx = $x }
      if ($y -lt $miny) { $miny = $y }
      if ($y -gt $maxy) { $maxy = $y }
    }
  }
  if ($rowDiff -gt 0) { $rowhist[$y] = $rowDiff }
}
$total = $w * $h
"total={0} px, nonzero-diff={1} ({2:N3}%), >2: {3}, >8: {4}, >32: {5}, maxdiff={6}" -f $total, $ndiff, (100.0*$ndiff/$total), $c2, $c8, $c32, $maxd
if ($maxx -ge 0) { "bbox of diffs: ({0},{1})-({2},{3})" -f $minx, $miny, $maxx, $maxy }
# rows with most diffs
$top = $rowhist.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 8
foreach ($t in $top) { "row y={0}: {1} diff px" -f $t.Key, $t.Value }
$ia.Dispose(); $ib.Dispose()
