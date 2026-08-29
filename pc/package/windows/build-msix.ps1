[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+\.\d+$')]
    [string] $Version,

    [Parameter(Mandatory = $true)]
    [string] $IdentityName,

    [Parameter(Mandatory = $true)]
    [string] $Publisher,

    [Parameter(Mandatory = $true)]
    [string] $PublisherDisplayName,

    [ValidateSet('x64', 'arm64')]
    [string] $Architecture = 'x64',

    [string] $CertificateThumbprint
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$packageDirectory = $PSScriptRoot
$pcDirectory = (Resolve-Path (Join-Path $packageDirectory '..\..')).Path
$targetByArchitecture = @{
    x64   = 'x86_64-pc-windows-msvc'
    arm64 = 'aarch64-pc-windows-msvc'
}
$rustTarget = $targetByArchitecture[$Architecture]
$outputDirectory = Join-Path $pcDirectory 'target\msix'
$layoutDirectory = Join-Path $outputDirectory "layout-$Architecture"
$outputPackage = Join-Path $outputDirectory "Overbrowsered-$Version-$Architecture.msix"

function Resolve-Command([string] $name, [string] $sdkPattern) {
    $command = Get-Command $name -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }
    $sdkCommand = Get-Item $sdkPattern -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if ($sdkCommand) {
        return $sdkCommand.FullName
    }
    throw "$name was not found. Install the Windows SDK and try again."
}

$cargo = (Get-Command cargo.exe -ErrorAction Stop).Source
$makeAppx = Resolve-Command 'makeappx.exe' 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\makeappx.exe'

Push-Location $pcDirectory
try {
    & $cargo build --locked --release --target $rustTarget
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

if (Test-Path $layoutDirectory) {
    Remove-Item -Recurse -Force $layoutDirectory
}
New-Item -ItemType Directory -Force (Join-Path $layoutDirectory 'Assets') | Out-Null

Copy-Item (Join-Path $pcDirectory "target\$rustTarget\release\overbrowsered.exe") $layoutDirectory
Copy-Item (Join-Path $packageDirectory 'Assets\*.png') (Join-Path $layoutDirectory 'Assets')

function Escape-XmlAttribute([string] $value) {
    return [System.Security.SecurityElement]::Escape($value)
}

$manifest = Get-Content (Join-Path $packageDirectory 'AppxManifest.xml.in') -Raw
$manifest = $manifest.Replace('@IDENTITY_NAME@', (Escape-XmlAttribute $IdentityName))
$manifest = $manifest.Replace('@PUBLISHER@', (Escape-XmlAttribute $Publisher))
$manifest = $manifest.Replace('@PUBLISHER_DISPLAY_NAME@', (Escape-XmlAttribute $PublisherDisplayName))
$manifest = $manifest.Replace('@VERSION@', $Version)
$manifest = $manifest.Replace('@ARCHITECTURE@', $Architecture)
[System.IO.File]::WriteAllText(
    (Join-Path $layoutDirectory 'AppxManifest.xml'),
    $manifest,
    [System.Text.UTF8Encoding]::new($false)
)

New-Item -ItemType Directory -Force $outputDirectory | Out-Null
if (Test-Path $outputPackage) {
    Remove-Item -Force $outputPackage
}
& $makeAppx pack /o /d $layoutDirectory /p $outputPackage
if ($LASTEXITCODE -ne 0) {
    throw "MakeAppx failed with exit code $LASTEXITCODE"
}

if ($CertificateThumbprint) {
    $signTool = Resolve-Command 'signtool.exe' 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\signtool.exe'
    & $signTool sign /fd SHA256 /sha1 $CertificateThumbprint $outputPackage
    if ($LASTEXITCODE -ne 0) {
        throw "SignTool failed with exit code $LASTEXITCODE"
    }
}

Write-Host "Created $outputPackage"
