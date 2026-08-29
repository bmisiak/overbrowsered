[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+\.\d+$')]
    [string] $Version,

    [string] $PackageDirectory,

    [string] $OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$pcDirectory = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $PackageDirectory) {
    $PackageDirectory = Join-Path $pcDirectory 'target\msix'
}
if (-not $OutputPath) {
    $OutputPath = Join-Path $PackageDirectory "Overbrowsered-$Version.msixbundle"
}

$expectedPackages = @('x64', 'arm64') | ForEach-Object {
    Join-Path $PackageDirectory "Overbrowsered-$Version-$_.msix"
}
foreach ($package in $expectedPackages) {
    if (-not (Test-Path $package -PathType Leaf)) {
        throw "Required architecture package was not found: $package"
    }
}

$makeAppx = Get-Command makeappx.exe -ErrorAction SilentlyContinue
if (-not $makeAppx) {
    $makeAppx = Get-Item 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\makeappx.exe' -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending |
        Select-Object -First 1
}
if (-not $makeAppx) {
    throw 'makeappx.exe was not found. Install the Windows SDK and try again.'
}

$outputDirectory = Split-Path -Parent $OutputPath
if ($outputDirectory) {
    New-Item -ItemType Directory -Force $outputDirectory | Out-Null
}
if (Test-Path $OutputPath) {
    Remove-Item -Force $OutputPath
}

$makeAppxPath = if ($makeAppx -is [System.Management.Automation.ApplicationInfo]) {
    $makeAppx.Source
} else {
    $makeAppx.FullName
}
$stagingDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("overbrowsered-msixbundle-" + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory $stagingDirectory | Out-Null
try {
    Copy-Item $expectedPackages $stagingDirectory
    & $makeAppxPath bundle /o /d $stagingDirectory /p $OutputPath
    if ($LASTEXITCODE -ne 0) {
        throw "MakeAppx bundle failed with exit code $LASTEXITCODE"
    }
} finally {
    Remove-Item -Recurse -Force $stagingDirectory
}

Write-Host "Created $OutputPath"
