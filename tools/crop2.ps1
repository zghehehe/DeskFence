# Crop a region from two PNGs for visual comparison. ASCII only.
param(
  [string]$A = "C:\Users\z00897910\Desktop\DeskFence\tools\before_ink.png",
  [string]$B = "C:\Users\z00897910\Desktop\DeskFence\tools\after_ink.png",
  [int]$X = 760, [int]$Y = 170, [int]$W = 380, [int]$H = 160
)
Add-Type -AssemblyName System.Drawing
$r = New-Object System.Drawing.Rectangle $X, $Y, $W, $H
foreach ($pair in @(@("a", $A), @("b", $B))) {
  $name = $pair[0]; $path = $pair[1]
  $src = [System.Drawing.Bitmap]::FromFile($path)
  $dst = New-Object System.Drawing.Bitmap $W, $H
  $g = [System.Drawing.Graphics]::FromImage($dst)
  $g.DrawImage($src, (New-Object System.Drawing.Rectangle 0, 0, $W, $H), $r, [System.Drawing.GraphicsUnit]::Pixel)
  $g.Dispose()
  $out = "C:\Users\z00897910\Desktop\DeskFence\tools\crop_$name.png"
  $dst.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
  $src.Dispose(); $dst.Dispose()
  Write-Output "saved $out"
}
