# The NSIS installer's header (150x57) and sidebar (164x314) bitmaps, drawn from Scout's
# icon shape (app-icon.svg: a #3490f7 rounded square, a white play triangle) in the app's
# Dark theme colours (app.css). 24-bit BMP, as NSIS wants; PNG copies for looking at.
# Usage: powershell -File tools/nsis-art.ps1 -Out apps/klip-app/src-tauri/icons -Preview <folder>
param([string]$Out, [string]$Preview)
Add-Type -AssemblyName System.Drawing

function RoundedRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
  $p = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = 2 * $r
  $p.AddArc($x, $y, $d, $d, 180, 90)
  $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
  $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
  $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
  $p.CloseFigure()
  return $p
}

# The icon at size $s with its top-left at ($x, $y): the SVG's 896-box, rx 25 %.
function DrawIcon($g, [float]$x, [float]$y, [float]$s) {
  $blue = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(0x34, 0x90, 0xF7))
  $white = [System.Drawing.Brushes]::White
  $g.FillPath($blue, (RoundedRect $x $y $s $s ($s * 0.25)))
  $pts = @(
    (New-Object System.Drawing.PointF ($x + $s * 0.366), ($y + $s * 0.263)),
    (New-Object System.Drawing.PointF ($x + $s * 0.759), ($y + $s * 0.5)),
    (New-Object System.Drawing.PointF ($x + $s * 0.366), ($y + $s * 0.737))
  )
  $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::White), ($s * 0.054)
  $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
  $g.FillPolygon($white, $pts)
  $g.DrawPolygon($pen, $pts)
  $pen.Dispose(); $blue.Dispose()
}

function Canvas([int]$w, [int]$h) {
  $bmp = New-Object System.Drawing.Bitmap $w, $h, ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  return @($bmp, $g)
}

function Save($bmp, [string]$name) {
  $bmp.Save((Join-Path $Out "$name.bmp"), [System.Drawing.Imaging.ImageFormat]::Bmp)
  $bmp.Save((Join-Path $Preview "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
}

$textStrong = [System.Drawing.Color]::FromArgb(0xF2, 0xF2, 0xF2)
$textMuted = [System.Drawing.Color]::FromArgb(0x92, 0x92, 0x92)
$accent = [System.Drawing.Color]::FromArgb(0x1F, 0x6F, 0xE0)
$panel = [System.Drawing.Color]::FromArgb(0x1D, 0x1D, 0x1D)
$gutter = [System.Drawing.Color]::FromArgb(0x0C, 0x0C, 0x0C)
$centre = New-Object System.Drawing.StringFormat
$centre.Alignment = [System.Drawing.StringAlignment]::Center
$centre.LineAlignment = [System.Drawing.StringAlignment]::Center

# Sidebar 164x314: a gutter-to-panel gradient, the icon, the name, the one-line pitch, an accent line.
$bmp, $g = Canvas 164 314
$grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.Rectangle 0, 0, 164, 314), $gutter, $panel, ([System.Drawing.Drawing2D.LinearGradientMode]::Vertical)
$g.FillRectangle($grad, 0, 0, 164, 314)
DrawIcon $g 38 58 88
$name = New-Object System.Drawing.Font 'Segoe UI', 26, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
$g.DrawString('Scout', $name, (New-Object System.Drawing.SolidBrush $textStrong), (New-Object System.Drawing.RectangleF 0, 176, 164, 34), $centre)
$pitch = New-Object System.Drawing.Font 'Segoe UI', 12, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
$g.DrawString('Clips of your games', $pitch, (New-Object System.Drawing.SolidBrush $textMuted), (New-Object System.Drawing.RectangleF 0, 210, 164, 18), $centre)
$g.FillRectangle((New-Object System.Drawing.SolidBrush $accent), 62, 240, 40, 2)
$g.Dispose(); Save $bmp 'nsis-sidebar'; $bmp.Dispose()

# Header 150x57: the panel colour, the icon and the name, as a badge at the left of the header bar.
$bmp, $g = Canvas 150 57
$g.Clear($panel)
DrawIcon $g 12 11 36
$h = New-Object System.Drawing.Font 'Segoe UI', 20, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
$left = New-Object System.Drawing.StringFormat
$left.LineAlignment = [System.Drawing.StringAlignment]::Center
$g.DrawString('Scout', $h, (New-Object System.Drawing.SolidBrush $textStrong), (New-Object System.Drawing.RectangleF 56, 0, 94, 57), $left)
$g.Dispose(); Save $bmp 'nsis-header'; $bmp.Dispose()
"drawn"
