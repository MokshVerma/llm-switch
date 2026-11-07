# Generates assets/icon.icns (macOS) by drawing the toggle-switch icon at each required size.
# Run from anywhere:  powershell -File packaging\make-icns.ps1
# ICNS entries may hold PNG data directly, so no macOS tools (iconutil) are needed.
Add-Type -AssemblyName System.Drawing

function New-RoundedRect($x, $y, $w, $h, $r) {
    $p = New-Object Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    $p
}

# macOS icons sit inside a transparent margin: the visible tile is 824/1024 of the canvas.
function Draw-Icon($size) {
    $bmp = New-Object Drawing.Bitmap $size, $size
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([Drawing.Color]::Transparent)
    $s = [single]$size
    $tile = $s * 824 / 1024
    $off = ($s - $tile) / 2
    $top = [Drawing.PointF]::new(0, $off)
    $bottom = [Drawing.PointF]::new(0, $off + $tile)
    $bg = New-Object Drawing.Drawing2D.LinearGradientBrush $top, $bottom, ([Drawing.Color]::FromArgb(255, 44, 48, 66)), ([Drawing.Color]::FromArgb(255, 22, 24, 34))
    $g.FillPath($bg, (New-RoundedRect $off $off $tile $tile ($tile * 0.2237)))
    $pw = $tile * 0.68; $ph = $tile * 0.36
    $px = $off + ($tile - $pw) / 2; $py = $off + ($tile - $ph) / 2
    $g.FillPath((New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(255, 60, 190, 90))), (New-RoundedRect $px $py $pw $ph ($ph / 2)))
    $k = $ph * 0.76; $kx = $px + $pw - $k - ($ph - $k) / 2; $ky = $py + ($ph - $k) / 2
    $g.FillEllipse([Drawing.Brushes]::White, $kx, $ky, $k, $k)
    $g.Dispose()
    $bmp
}

function Get-Png($size) {
    $bmp = Draw-Icon $size
    $ms = New-Object IO.MemoryStream
    $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    , $ms.ToArray()
}

$png = @{}
foreach ($s in 16, 32, 64, 128, 256, 512, 1024) { $png[$s] = Get-Png $s }

# (ICNS type, pixel size) - the @2x types reuse the next size up.
$entries = @(
    @('icp4', 16), @('icp5', 32), @('icp6', 64),
    @('ic07', 128), @('ic08', 256), @('ic09', 512), @('ic10', 1024),
    @('ic11', 32), @('ic12', 64), @('ic13', 256), @('ic14', 512)
)

function Write-BE32($w, [uint32]$v) {
    $w.Write([byte](($v -shr 24) -band 255)); $w.Write([byte](($v -shr 16) -band 255))
    $w.Write([byte](($v -shr 8) -band 255)); $w.Write([byte]($v -band 255))
}

$body = New-Object IO.MemoryStream
$bw = New-Object IO.BinaryWriter $body
foreach ($e in $entries) {
    $data = $png[$e[1]]
    $bw.Write([Text.Encoding]::ASCII.GetBytes($e[0]))
    Write-BE32 $bw ([uint32]($data.Length + 8))
    $bw.Write($data)
}
$bw.Flush()

$out = New-Object IO.MemoryStream
$ow = New-Object IO.BinaryWriter $out
$ow.Write([Text.Encoding]::ASCII.GetBytes('icns'))
Write-BE32 $ow ([uint32]($body.Length + 8))
$ow.Write($body.ToArray())
$ow.Flush()

$target = Join-Path (Split-Path $PSScriptRoot -Parent) 'assets\icon.icns'
[IO.File]::WriteAllBytes($target, $out.ToArray())
"wrote $target ($($out.Length) bytes)"
