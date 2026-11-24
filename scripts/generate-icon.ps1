param(
    [string]$SourcePng
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$assetDir = Join-Path (Split-Path $PSScriptRoot -Parent) 'assets'
$appIcon = Join-Path $assetDir 'icon.png'

function ConvertTo-IconPng([Drawing.Bitmap]$Source, [int]$Size) {
    $bitmap = [Drawing.Bitmap]::new($Size, $Size, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [Drawing.Graphics]::FromImage($bitmap)
    $memory = [IO.MemoryStream]::new()
    try {
        $graphics.CompositingMode = [Drawing.Drawing2D.CompositingMode]::SourceCopy
        $graphics.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.PixelOffsetMode = [Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.DrawImage($Source, [Drawing.Rectangle]::new(0, 0, $Size, $Size))
        $bitmap.Save($memory, [Drawing.Imaging.ImageFormat]::Png)
        return ,$memory.ToArray()
    } finally {
        $memory.Dispose()
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

if ($SourcePng) {
    $source = [Drawing.Bitmap]::new((Resolve-Path -LiteralPath $SourcePng).Path)
    try {
        if ($source.Width -ne $source.Height -or $source.Width -lt 256) {
            throw 'The source icon must be square and at least 256 pixels wide.'
        }
        [byte[]]$png = ConvertTo-IconPng $source 256
    } finally {
        $source.Dispose()
    }
    [IO.File]::WriteAllBytes($appIcon, $png)
}

$source = [Drawing.Bitmap]::new($appIcon)
try {
    $images = foreach ($size in @(16, 24, 32, 48, 64, 128, 256)) {
        $bytes = if ($size -eq 256) { [IO.File]::ReadAllBytes($appIcon) } else { ConvertTo-IconPng $source $size }
        @{ Size = $size; Bytes = [byte[]]$bytes }
    }
} finally {
    $source.Dispose()
}

$stream = [IO.MemoryStream]::new()
$writer = [IO.BinaryWriter]::new($stream)
try {
    $writer.Write([uint16]0)
    $writer.Write([uint16]1)
    $writer.Write([uint16]$images.Count)
    $offset = 6 + 16 * $images.Count
    foreach ($entry in $images) {
        $dimension = if ($entry.Size -eq 256) { 0 } else { $entry.Size }
        $writer.Write([byte]$dimension)
        $writer.Write([byte]$dimension)
        $writer.Write([uint16]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]32)
        $writer.Write([uint32]$entry.Bytes.Length)
        $writer.Write([uint32]$offset)
        $offset += $entry.Bytes.Length
    }
    foreach ($entry in $images) {
        $writer.Write($entry.Bytes)
    }
    [IO.File]::WriteAllBytes((Join-Path $assetDir 'icon.ico'), $stream.ToArray())
} finally {
    $writer.Dispose()
    $stream.Dispose()
}
