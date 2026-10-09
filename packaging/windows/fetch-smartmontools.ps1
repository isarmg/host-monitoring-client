[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"

$version = "7.5"
$release = "RELEASE_7_5"
$baseUrl = "https://github.com/smartmontools/smartmontools/releases/download/$release"
$setupName = "smartmontools-$version.win32-setup.exe"
$sourceName = "smartmontools-$version.tar.gz"
$setupSha256 = "896337fcc253220614cf8cdbd5cf2321c5aa326a37a04160a672a281e6104c70"
$sourceSha256 = "690b83ca331378da9ea0d9d61008c4b22dde391387b9bbad7f29387f2595f76e"

$output = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $output -Force | Out-Null
$setup = Join-Path $output $setupName
$source = Join-Path $output $sourceName

function Receive-VerifiedFile {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string]$ExpectedSha256
    )
    $temporary = "$Destination.download-$PID"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $temporary
        $actual = (Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -cne $ExpectedSha256) {
            throw "Downloaded smartmontools artifact digest mismatch: expected $ExpectedSha256, found $actual"
        }
        Move-Item -LiteralPath $temporary -Destination $Destination -Force
    }
    finally {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    }
}

Receive-VerifiedFile -Uri "$baseUrl/$setupName" -Destination $setup -ExpectedSha256 $setupSha256
Receive-VerifiedFile -Uri "$baseUrl/$sourceName" -Destination $source -ExpectedSha256 $sourceSha256

$payload = Join-Path $output "payload"
if (Test-Path -LiteralPath $payload) {
    Remove-Item -LiteralPath $payload -Recurse -Force
}
New-Item -ItemType Directory -Path $payload | Out-Null

# The upstream NSIS package supports component selection. Extract only the x64
# smartctl runtime, drive database and license documentation; do not install smartd,
# services, PATH entries, shortcuts or an uninstaller on the build machine.
$process = Start-Process -FilePath $setup -WindowStyle Hidden -Wait -PassThru -ArgumentList @(
    "/S",
    "/SO", "x64,smartctl,drivedb,doc",
    "/D=$payload"
)
if ($process.ExitCode -ne 0) {
    throw "smartmontools payload extraction failed with exit code $($process.ExitCode)"
}

foreach ($required in @(
    (Join-Path $payload "bin\smartctl.exe"),
    (Join-Path $payload "bin\drivedb.h"),
    (Join-Path $payload "doc\COPYING.txt")
)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "smartmontools payload is incomplete: $required"
    }
}

$versionOutput = & (Join-Path $payload "bin\smartctl.exe") --version 2>&1 | Out-String
if ($LASTEXITCODE -ne 0 -or $versionOutput -notmatch "smartctl 7\.5") {
    throw "Unexpected bundled smartctl version: $versionOutput"
}

Write-Output $payload
