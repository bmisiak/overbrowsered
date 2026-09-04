[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Drawing

$iconDirectory = $PSScriptRoot
$pcDirectory = Split-Path $iconDirectory -Parent
$sourcePath = Join-Path $iconDirectory 'app-1024.png'
$frontVectorPath = Join-Path $iconDirectory 'windows-window-front.svg'
$windowsSourcePath = Join-Path $iconDirectory 'app-windows-1024.png'
$windowsIconPath = Join-Path $iconDirectory 'app-windows.ico'
$assetDirectory = Join-Path $pcDirectory 'package\windows\Assets'

[xml] $frontVector = Get-Content $frontVectorPath -Raw
$circles = @($frontVector.svg.ChildNodes | Where-Object { $_.LocalName -eq 'circle' })
if ($circles.Count -ne 0) {
    throw 'The Windows front-window vector must not contain macOS window controls.'
}
$addressRect = @($frontVector.svg.ChildNodes | Where-Object {
    $_.LocalName -eq 'rect' -and $_.fill -eq '#c9d0e2'
})
if (@($addressRect).Count -ne 1) {
    throw 'The Windows front-window vector must contain one address bar.'
}

function New-ResizedPngBytes {
    param(
        [Parameter(Mandatory)] [System.Drawing.Image] $Source,
        [Parameter(Mandatory)] [int] $Size
    )

    $bitmap = [System.Drawing.Bitmap]::new($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $bitmap.SetResolution(96, 96)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
            $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
            $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
            $graphics.DrawImage($Source, [System.Drawing.Rectangle]::new(0, 0, $Size, $Size))
        }
        finally {
            $graphics.Dispose()
        }

        $stream = [System.IO.MemoryStream]::new()
        try {
            $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
            return ,$stream.ToArray()
        }
        finally {
            $stream.Dispose()
        }
    }
    finally {
        $bitmap.Dispose()
    }
}

function Write-Bytes {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [byte[]] $Bytes
    )

    [System.IO.File]::WriteAllBytes($Path, $Bytes)
}

$source = [System.Drawing.Bitmap]::new($sourcePath)
try {
    $windows = [System.Drawing.Bitmap]::new($source)
    try {
        # The source is the macOS Icon Composer export. Preserve its glass,
        # shadows, and window stack while replacing only the front toolbar.
        $addressSource = [System.Drawing.Rectangle]::new(360, 369, 470, 53)
        $addressPixels = $source.Clone($addressSource, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        try {
            for ($y = $addressSource.Top; $y -lt $addressSource.Bottom; $y++) {
                $toolbarColor = $source.GetPixel(350, $y)
                for ($x = 190; $x -lt 835; $x++) {
                    $windows.SetPixel($x, $y, $toolbarColor)
                }
            }

            $graphics = [System.Drawing.Graphics]::FromImage($windows)
            try {
                # The visible address bar is 460 px wide; this placement centers
                # it at x=512 after the macOS window controls are removed.
                $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceCopy
                $iconComposerScale = 460.0 / [double] $addressRect.width
                $addressX = [Math]::Round(99 + ([double] $addressRect.x * $iconComposerScale))
                $graphics.DrawImageUnscaled($addressPixels, $addressX - 5, 369)
            }
            finally {
                $graphics.Dispose()
            }
        }
        finally {
            $addressPixels.Dispose()
        }

        $windows.Save($windowsSourcePath, [System.Drawing.Imaging.ImageFormat]::Png)

        $assetSizes = [ordered]@{
            'Square150x150Logo.png' = 150
            'Square44x44Logo.png' = 44
            'StoreLogo.png' = 50
        }
        foreach ($asset in $assetSizes.GetEnumerator()) {
            Write-Bytes -Path (Join-Path $assetDirectory $asset.Key) `
                -Bytes (New-ResizedPngBytes -Source $windows -Size $asset.Value)
        }

        $iconSizes = 16, 24, 32, 48, 256
        $iconImages = @($iconSizes | ForEach-Object {
            [pscustomobject]@{
                Size = $_
                Bytes = New-ResizedPngBytes -Source $windows -Size $_
            }
        })

        $stream = [System.IO.MemoryStream]::new()
        $writer = [System.IO.BinaryWriter]::new($stream)
        try {
            $writer.Write([uint16] 0)
            $writer.Write([uint16] 1)
            $writer.Write([uint16] $iconImages.Count)

            $offset = 6 + (16 * $iconImages.Count)
            foreach ($image in $iconImages) {
                $dimension = if ($image.Size -eq 256) { 0 } else { $image.Size }
                $writer.Write([byte] $dimension)
                $writer.Write([byte] $dimension)
                $writer.Write([byte] 0)
                $writer.Write([byte] 0)
                $writer.Write([uint16] 1)
                $writer.Write([uint16] 32)
                $writer.Write([uint32] $image.Bytes.Length)
                $writer.Write([uint32] $offset)
                $offset += $image.Bytes.Length
            }
            foreach ($image in $iconImages) {
                $writer.Write($image.Bytes)
            }
            $writer.Flush()
            Write-Bytes -Path $windowsIconPath -Bytes $stream.ToArray()
        }
        finally {
            $writer.Dispose()
            $stream.Dispose()
        }
    }
    finally {
        $windows.Dispose()
    }
}
finally {
    $source.Dispose()
}

Write-Host "Generated Windows app icon and Store assets."

