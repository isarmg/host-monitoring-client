[CmdletBinding()]
param(
    [string] $ReleaseRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($ReleaseRoot)) {
    $repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
    $ReleaseRoot = Join-Path $repositoryRoot "target\x86_64-pc-windows-msvc\release"
}

function Assert-PeSubsystem {
    param(
        [Parameter(Mandatory = $true)]
        [string] $LiteralPath,
        [Parameter(Mandatory = $true)]
        [int] $ExpectedSubsystem
    )

    if (-not (Test-Path -LiteralPath $LiteralPath -PathType Leaf)) {
        throw "PE file does not exist: $LiteralPath"
    }
    $bytes = [System.IO.File]::ReadAllBytes($LiteralPath)
    if ($bytes.Length -lt 64) {
        throw "PE file is too short to contain a DOS header: $LiteralPath"
    }

    $peOffset = [System.BitConverter]::ToInt32($bytes, 0x3c)
    if ($peOffset -lt 0 -or $peOffset -gt ($bytes.Length - 24)) {
        throw "PE header offset is outside the file: $LiteralPath"
    }
    if ([System.BitConverter]::ToUInt32($bytes, $peOffset) -ne 0x00004550) {
        throw "PE signature is invalid: $LiteralPath"
    }

    $optionalHeaderSize = [System.BitConverter]::ToUInt16($bytes, $peOffset + 20)
    $optionalHeaderOffset = $peOffset + 24
    if ($optionalHeaderSize -lt 70 -or
        $optionalHeaderOffset -gt ($bytes.Length - $optionalHeaderSize)) {
        throw "PE optional header is truncated: $LiteralPath"
    }
    $magic = [System.BitConverter]::ToUInt16($bytes, $optionalHeaderOffset)
    if ($magic -ne 0x010b -and $magic -ne 0x020b) {
        throw "PE optional header has an unsupported magic value: $LiteralPath"
    }

    $actualSubsystem = [System.BitConverter]::ToUInt16($bytes, $optionalHeaderOffset + 68)
    if ($actualSubsystem -ne $ExpectedSubsystem) {
        throw "Unexpected PE subsystem for ${LiteralPath}: expected $ExpectedSubsystem, found $actualSubsystem."
    }
    Write-Host "Verified PE subsystem $actualSubsystem for $LiteralPath"
}

Assert-PeSubsystem `
    -LiteralPath (Join-Path $ReleaseRoot "host-monitor.exe") `
    -ExpectedSubsystem 3
Assert-PeSubsystem `
    -LiteralPath (Join-Path $ReleaseRoot "host-monitor-maintenance.exe") `
    -ExpectedSubsystem 2
Assert-PeSubsystem `
    -LiteralPath (Join-Path $ReleaseRoot "host-monitor-tray.exe") `
    -ExpectedSubsystem 2
