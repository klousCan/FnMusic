Add-Type -AssemblyName System.Drawing
$root = Split-Path $PSScriptRoot -Parent
$assetDir = Join-Path $root 'assets'
[IO.Directory]::CreateDirectory($assetDir) | Out-Null
$images = @()
foreach ($size in @(16, 24, 32, 48, 64, 128, 256)) {
    $bitmap = [Drawing.Bitmap]::new($size, $size)
    $g = [Drawing.Graphics]::FromImage($bitmap)
    $g.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.ScaleTransform($size / 256.0, $size / 256.0)
    $path = [Drawing.Drawing2D.GraphicsPath]::new()
    $path.AddArc(8,8,88,88,180,90)
    $path.AddArc(160,8,88,88,270,90)
    $path.AddArc(160,160,88,88,0,90)
    $path.AddArc(8,160,88,88,90,90)
    $path.CloseFigure()
    $brush = [Drawing.SolidBrush]::new([Drawing.ColorTranslator]::FromHtml('#FAF8F3'))
    $g.FillPath($brush,$path)
    $border = [Drawing.Pen]::new([Drawing.ColorTranslator]::FromHtml('#2A2A2A'),6)
    $g.DrawPath($border,$path)
    $border.Dispose()
    # Centre the painted SVG bounds (approximately 25,24), not its
    # asymmetric viewBox, within the square application-icon tile.
    $g.TranslateTransform(128,128)
    $g.ScaleTransform(4.7,4.7)
    $g.TranslateTransform(-25,-24)
    $pen = [Drawing.Pen]::new([Drawing.ColorTranslator]::FromHtml('#2A2A2A'),3.5)
    $pen.StartCap = $pen.EndCap = [Drawing.Drawing2D.LineCap]::Round
    $note = [Drawing.SolidBrush]::new([Drawing.ColorTranslator]::FromHtml('#E76F51'))
    foreach ($center in @(@(16,32),@(34,28))) {
        $state = $g.Save()
        $g.TranslateTransform($center[0],$center[1])
        $g.RotateTransform(-15)
        $g.FillEllipse($note,-8,-6,16,12)
        $g.DrawEllipse($pen,-8,-6,16,12)
        $g.Restore($state)
    }
    $pen.Width = 3
    $g.DrawLine($pen,22,14,40,10)
    $g.DrawLine($pen,40,10,40,28)
    $note.Dispose()
    $memory = [IO.MemoryStream]::new()
    $bitmap.Save($memory,[Drawing.Imaging.ImageFormat]::Png)
    $images += ,@{Size=$size;Bytes=$memory.ToArray()}
    if ($size -eq 256) { [IO.File]::WriteAllBytes((Join-Path $assetDir 'icon.png'),$memory.ToArray()) }
    $memory.Dispose(); $pen.Dispose(); $brush.Dispose(); $path.Dispose(); $g.Dispose(); $bitmap.Dispose()
}
$stream = [IO.MemoryStream]::new()
$writer = [IO.BinaryWriter]::new($stream)
$writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($entry in $images) {
    $dimension = if ($entry.Size -eq 256) { 0 } else { $entry.Size }
    $writer.Write([byte]$dimension); $writer.Write([byte]$dimension)
    $writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]32)
    $writer.Write([uint32]$entry.Bytes.Length); $writer.Write([uint32]$offset)
    $offset += $entry.Bytes.Length
}
foreach ($entry in $images) { $writer.Write([byte[]]$entry.Bytes) }
[IO.File]::WriteAllBytes((Join-Path $assetDir 'icon.ico'),$stream.ToArray())
$writer.Dispose(); $stream.Dispose()
