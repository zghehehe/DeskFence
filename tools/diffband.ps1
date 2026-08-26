# Diff two PNGs, report only pixels with x >= MinX, clustered by 20px row bands.
# ASCII only.
param(
  [Parameter(Mandatory=$true)][string]$A,
  [Parameter(Mandatory=$true)][string]$B,
  [int]$MinX = 1150
)
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
$bands = @{}
$minx2 = $w; $maxx2 = -1
for ($y = 0; $y -lt $h; $y++) {
  $row = $y * $bdA.Stride
  for ($x = $MinX; $x -lt $w; $x++) {
    $o = $row + $x * 4
    $d = 0
    for ($c = 0; $c -lt 3; $c++) {
      $dd = [Math]::Abs($ba[$o+$c] - $bb[$o+$c])
      if ($dd -gt $d) { $d = $dd }
    }
    if ($d -gt 2) {
      $band = [Math]::Floor($y / 20) * 20
      if (-not $bands.ContainsKey($band)) { $bands[$band] = @(0, $w, -1) }
      $bands[$band][0]++
      if ($x -lt $bands[$band][1]) { $bands[$band][1] = $x }
      if ($x -gt $bands[$band][2]) { $bands[$band][2] = $x }
    }
  }
}
foreach ($k in ($bands.Keys | Sort-Object)) {
  $v = $bands[$k]
  "y={0}-{1}: {2} px, x {3}-{4}" -f $k, ($k+19), $v[0], $v[1], $v[2]
}
$ia.Dispose(); $ib.Dispose()
