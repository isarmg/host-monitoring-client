[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string] $ProductVersion,
    [string] $ArtifactDirectory,
    [string] $LogDirectory,
    [string] $MaintenanceExecutable
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Destructive MSI lifecycle tests are restricted to disposable GitHub-hosted runners.'
}
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
if ([string]::IsNullOrWhiteSpace($MaintenanceExecutable)) {
    $MaintenanceExecutable = Join-Path $repositoryRoot `
        'target\x86_64-pc-windows-msvc\release\xsoc-maintenance.exe'
}
$MaintenanceExecutable = (Resolve-Path -LiteralPath $MaintenanceExecutable).Path
if ([string]::IsNullOrWhiteSpace($ArtifactDirectory)) {
    $ArtifactDirectory = Join-Path $repositoryRoot "dist"
}
else {
    $ArtifactDirectory = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath(
        $ArtifactDirectory
    )
}
if ([string]::IsNullOrWhiteSpace($LogDirectory)) {
    $temporaryRoot = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
        [IO.Path]::GetTempPath()
    }
    else {
        $env:RUNNER_TEMP
    }
    $LogDirectory = Join-Path $temporaryRoot "xsoc-msi-logs"
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "The MSI lifecycle smoke test requires an elevated Windows session."
}

$installedRoot = Join-Path $env:ProgramFiles "xsoc"
$installedTray = Join-Path $installedRoot "xsoc-tray.exe"
$stateRoot = Join-Path $env:ProgramData "xsoc"
$trayRunKey = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run"
$trayRunName = "xsoc-tray"
$commonPrograms = [Environment]::GetFolderPath(
    [Environment+SpecialFolder]::CommonPrograms
)
$trayShortcut = Join-Path $commonPrograms "xsoc.lnk"
$stateMarker = ".xsoc-managed-1.0.0"
$installJournal = Join-Path $env:ProgramData `
    "xsoc.install-journal-$ProductVersion"
$uninstallJournal = Join-Path $env:ProgramData `
    "xsoc.uninstall-journal-$ProductVersion"
$purgeQuarantine = Join-Path $env:ProgramData `
    "xsoc.purge-quarantine-$ProductVersion"
$maintenanceDiagnostic = Join-Path $env:ProgramData `
    "xsoc.maintenance-diagnostic-$ProductVersion.txt"
$maximumMaintenanceDiagnosticBytes = 64KB
$logs = $LogDirectory
New-Item -ItemType Directory -Force $logs | Out-Null

if (-not ("Xsoc.MsiNativeMethods" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace Xsoc {
    public static class MsiNativeMethods {
        [DllImport("msi.dll", EntryPoint = "MsiGetShortcutTargetW",
            CharSet = CharSet.Unicode, ExactSpelling = true)]
        public static extern uint MsiGetShortcutTarget(
            string shortcutTarget,
            StringBuilder productCode,
            StringBuilder featureId,
            StringBuilder componentCode);

        [DllImport("msi.dll", EntryPoint = "MsiGetComponentPathW",
            CharSet = CharSet.Unicode, ExactSpelling = true)]
        public static extern int MsiGetComponentPath(
            string productCode,
            string componentCode,
            StringBuilder path,
            ref uint pathLength);
    }
}
"@
}

function Get-MaintenanceDiagnosticItem {
    try {
        return (Get-Item -LiteralPath $maintenanceDiagnostic -Force -ErrorAction Stop)
    }
    catch [Management.Automation.ItemNotFoundException] {
        return $null
    }
}

function Assert-MaintenanceDiagnosticAbsent([string]$Context) {
    if ($null -ne (Get-MaintenanceDiagnosticItem)) {
        throw "$Context found an unexpected maintenance diagnostic: $maintenanceDiagnostic"
    }
}

function Read-AndRemoveMaintenanceDiagnostic {
    $item = Get-MaintenanceDiagnosticItem
    if ($null -eq $item) {
        return $null
    }

    if ($item.PSIsContainer -or
        (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "Maintenance diagnostic is not a regular non-reparse file: $maintenanceDiagnostic"
    }
    if ($item.Length -gt $maximumMaintenanceDiagnosticBytes) {
        throw ("Maintenance diagnostic exceeds the {0}-byte limit: {1}" -f `
            $maximumMaintenanceDiagnosticBytes, $maintenanceDiagnostic)
    }

    $acl = Get-Acl -LiteralPath $maintenanceDiagnostic
    $ownerSid = $acl.GetOwner(
        [Security.Principal.SecurityIdentifier]
    ).Value
    if ($ownerSid -ne "S-1-5-18" -or -not $acl.AreAccessRulesProtected) {
        throw "Maintenance diagnostic is not SYSTEM-owned with a protected DACL."
    }

    $rules = @($acl.Access)
    $expectedSids = @{
        "S-1-5-18" = $true
        "S-1-5-32-544" = $true
    }
    if ($rules.Count -ne $expectedSids.Count) {
        throw "Maintenance diagnostic DACL does not contain exactly SYSTEM and Administrators."
    }
    foreach ($rule in $rules) {
        $sid = $rule.IdentityReference.Translate(
            [Security.Principal.SecurityIdentifier]
        ).Value
        if (-not $expectedSids.ContainsKey($sid) -or
            $rule.AccessControlType -ne `
                [Security.AccessControl.AccessControlType]::Allow -or
            [int]$rule.FileSystemRights -ne 0x1f01ff -or
            $rule.InheritanceFlags -ne `
                [Security.AccessControl.InheritanceFlags]::None -or
            $rule.PropagationFlags -ne `
                [Security.AccessControl.PropagationFlags]::None -or
            $rule.IsInherited) {
            throw "Maintenance diagnostic contains an unexpected DACL entry for $sid."
        }
        $expectedSids.Remove($sid) | Out-Null
    }
    if ($expectedSids.Count -ne 0) {
        throw "Maintenance diagnostic is missing a required SYSTEM or Administrators DACL entry."
    }

    $stream = [IO.File]::Open(
        $maintenanceDiagnostic,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        [IO.FileShare]::None
    )
    try {
        if ($stream.Length -gt $maximumMaintenanceDiagnosticBytes) {
            throw ("Maintenance diagnostic exceeds the {0}-byte limit: {1}" -f `
                $maximumMaintenanceDiagnosticBytes, $maintenanceDiagnostic)
        }
        $bytes = [byte[]]::new($maximumMaintenanceDiagnosticBytes + 1)
        $length = 0
        while ($length -lt $bytes.Length) {
            $read = $stream.Read($bytes, $length, $bytes.Length - $length)
            if ($read -eq 0) { break }
            $length += $read
        }
        if ($length -gt $maximumMaintenanceDiagnosticBytes) {
            throw ("Maintenance diagnostic exceeds the {0}-byte limit: {1}" -f `
                $maximumMaintenanceDiagnosticBytes, $maintenanceDiagnostic)
        }
        $strictUtf8 = [Text.UTF8Encoding]::new($false, $true)
        $content = $strictUtf8.GetString($bytes, 0, $length)
    }
    finally {
        $stream.Dispose()
    }

    if ([string]::IsNullOrWhiteSpace($content)) {
        throw "Maintenance diagnostic is empty: $maintenanceDiagnostic"
    }
    $fields = @($content -split "`n", 4)
    $knownCommands = @(
        "prepare-install", "apply-install", "rollback-install", "commit-install",
        "preflight-uninstall", "rollback-uninstall-preflight", "preserve-state",
        "rollback-uninstall", "commit-uninstall", "prepare-purge", "rollback-purge",
        "commit-purge"
    )
    if ($fields.Count -ne 4 -or
        $fields[0] -cne "format=xsoc-maintenance-diagnostic-v1" -or
        $fields[1] -cne "version=$ProductVersion" -or
        -not $fields[2].StartsWith("command=", [StringComparison]::Ordinal) -or
        $fields[2].Substring("command=".Length) -cnotin $knownCommands -or
        -not $fields[3].StartsWith("error-chain=", [StringComparison]::Ordinal) -or
        [string]::IsNullOrWhiteSpace($fields[3].Substring("error-chain=".Length))) {
        throw "Maintenance diagnostic has an invalid fixed header or error chain."
    }
    Remove-Item -LiteralPath $maintenanceDiagnostic -Force
    Assert-MaintenanceDiagnosticAbsent "Maintenance diagnostic cleanup"
    return $content
}

function Write-MaintenanceDiagnostic([string]$Content) {
    foreach ($line in ($Content -split "`r`n|`n|`r")) {
        # The fixed prefix prevents diagnostic text from being interpreted as a
        # GitHub Actions workflow command even when a cause begins with `::`.
        Write-Host ("[maintenance] {0}" -f $line)
    }
}

function Invoke-Msi {
    param(
        [Parameter(Mandatory = $true)][ValidateSet('/i', '/x')][string]$Operation,
        [Parameter(Mandatory = $true)][string]$Package,
        [Parameter(Mandatory = $true)][string]$Name,
        [string]$Properties = "",
        [switch]$ExpectFailure
    )
    Assert-MaintenanceDiagnosticAbsent "Before MSI operation '$Name'"
    $log = Join-Path $logs "${Name}.log"
    $arguments = ("${Operation} `"${Package}`" ${Properties} " +
        "XSOC_MAINTENANCE_DIAGNOSTICS=1 /qn /norestart /l*v `"${log}`"")
    $process = Start-Process -FilePath msiexec.exe -ArgumentList $arguments -Wait -PassThru
    $diagnostic = Read-AndRemoveMaintenanceDiagnostic
    $succeeded = $process.ExitCode -in @(0, 3010)
    if ($succeeded -and $null -ne $diagnostic) {
        Write-MaintenanceDiagnostic $diagnostic
        throw "MSI operation '$Name' succeeded after a maintenance helper reported failure."
    }
    if ($ExpectFailure -and $succeeded) {
        throw "MSI operation '$Name' unexpectedly succeeded. Log: $log"
    }
    if ($ExpectFailure -and -not $succeeded -and $null -eq $diagnostic) {
        throw "MSI operation '$Name' failed without the required maintenance diagnostic. Log: $log"
    }
    if (-not $ExpectFailure -and -not $succeeded) {
        if ($null -ne $diagnostic) {
            Write-MaintenanceDiagnostic $diagnostic
        }
        $failureContext = @(Select-String -LiteralPath $log `
            -SimpleMatch "Return value 3" -Context 80, 20)
        if ($failureContext.Count -eq 0) {
            Get-Content -LiteralPath $log -Tail 240
        }
        else {
            foreach ($match in $failureContext) {
                $match.Context.PreContext
                $match.Line
                $match.Context.PostContext
            }
        }
        throw "MSI operation '$Name' failed with exit code $($process.ExitCode). Log: $log"
    }
}

function Assert-ServiceRunning {
    $service = Get-Service -Name "xsoc" -ErrorAction Stop
    $service.WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running,
        [TimeSpan]::FromSeconds(30))
    $definition = Get-CimInstance Win32_Service -Filter "Name='xsoc'"
    if ($definition.StartMode -ne "Manual" -or
        $definition.StartName -ne "NT AUTHORITY\LocalService" -or
        $definition.PathName -notmatch '--windows-service run --config') {
        throw "Installed SCM service definition is not the expected xsoc service."
    }
    $serviceKey = "HKLM:\SYSTEM\CurrentControlSet\Services\xsoc"
    if ((Get-ItemPropertyValue -LiteralPath $serviceKey -Name ServiceSidType) -ne 1) {
        throw "xsoc does not have an unrestricted service SID."
    }
    if ((Get-ItemPropertyValue -LiteralPath $serviceKey `
        -Name FailureActionsOnNonCrashFailures) -ne 1) {
        throw "xsoc does not enable failure actions for non-crash failures."
    }
}

function Assert-StateAcl {
    $serviceSid = (New-Object System.Security.Principal.NTAccount(
        "NT SERVICE", "xsoc"
    )).Translate([System.Security.Principal.SecurityIdentifier]).Value
    $protectedPaths = @($stateRoot)
    foreach ($child in @(
        $stateMarker, "config.json", "release-lifecycle-marker"
    )) {
        $candidate = Join-Path $stateRoot $child
        if (Test-Path -LiteralPath $candidate) { $protectedPaths += $candidate }
    }
    foreach ($protectedPath in $protectedPaths) {
        $acl = Get-Acl -LiteralPath $protectedPath
        $ownerSid = $acl.GetOwner(
            [System.Security.Principal.SecurityIdentifier]
        ).Value
        if ($ownerSid -ne "S-1-5-18" -or -not $acl.AreAccessRulesProtected) {
            throw "$protectedPath does not have SYSTEM ownership and a protected DACL."
        }

        $rules = @($acl.Access)
        $expectedRights = @{
            "S-1-5-18" = 0x1f01ff
            "S-1-5-32-544" = 0x1f01ff
            "S-1-3-4" = 0x00020000
        }
        $expectedRights[$serviceSid] = 0x001301bf
        $expectedInheritance = if ((Get-Item -LiteralPath $protectedPath).PSIsContainer) {
            [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor `
                [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
        }
        else {
            [System.Security.AccessControl.InheritanceFlags]::None
        }
        if ($rules.Count -ne $expectedRights.Count) {
            throw "$protectedPath has an unexpected managed-state ACE count."
        }
        foreach ($rule in $rules) {
            $sid = $rule.IdentityReference.Translate(
                [System.Security.Principal.SecurityIdentifier]
            ).Value
            if (-not $expectedRights.ContainsKey($sid) -or
                $sid -eq "S-1-5-19" -or
                $rule.AccessControlType -ne `
                    [System.Security.AccessControl.AccessControlType]::Allow -or
                [int]$rule.FileSystemRights -ne $expectedRights[$sid] -or
                $rule.InheritanceFlags -ne $expectedInheritance -or
                $rule.PropagationFlags -ne `
                    [System.Security.AccessControl.PropagationFlags]::None -or
                $rule.IsInherited) {
                throw ("$protectedPath has an unexpected managed-state ACE for $sid " +
                    "(rights=$([int]$rule.FileSystemRights), " +
                    "inheritance=$($rule.InheritanceFlags), " +
                    "propagation=$($rule.PropagationFlags), " +
                    "inherited=$($rule.IsInherited)).")
            }
            $expectedRights.Remove($sid) | Out-Null
        }
        if ($expectedRights.Count -ne 0) {
            throw "$protectedPath is missing a required managed-state ACE."
        }
    }
}

function Assert-TrayIntegration {
    if ((Test-Path -LiteralPath $installedTray) -or
        (Test-Path -LiteralPath $trayShortcut) -or
        (Get-ItemProperty -LiteralPath $trayRunKey -Name $trayRunName -ErrorAction SilentlyContinue)) {
        throw "Removed tray integration remains installed."
    }
}


function Get-XsocArpEntries {
    $uninstallRoot = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
    return @(Get-ChildItem -LiteralPath $uninstallRoot | Get-ItemProperty |
        Where-Object {
            $displayName = $_.PSObject.Properties["DisplayName"]
            $null -ne $displayName -and $displayName.Value -eq "xsoc"
        })
}

function Assert-ArpVersion([string]$ExpectedVersion) {
    $entries = @(Get-XsocArpEntries)
    if ($entries.Count -ne 1 -or $entries[0].DisplayVersion -ne $ExpectedVersion) {
        throw "Apps & Features does not contain exactly xsoc $ExpectedVersion."
    }
    if ($entries[0].Publisher -ne 'sarmg') {
        throw 'Apps & Features does not identify the current sarmg publisher.'
    }
}

function Assert-InstallLocation([string]$ExpectedRoot) {
    $location = (Get-ItemProperty -LiteralPath 'HKLM:\Software\sarmg\xsoc' `
        -Name InstallLocation -ErrorAction Stop).InstallLocation
    if ([string]::IsNullOrWhiteSpace($location) -or $location -ine $ExpectedRoot) {
        throw "The current Client registry namespace did not retain the selected installation path: $location"
    }
}

function Get-InstalledPathEntryCount {
    $expected = $installedRoot.TrimEnd('\')
    $machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    return @($machinePath -split ';' | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_) -and $_.Trim().TrimEnd('\') -ieq $expected
    }).Count
}

function Assert-MachinePathInstalled {
    if ((Get-InstalledPathEntryCount) -ne 1) {
        throw "Machine PATH does not contain exactly one xsoc installation directory."
    }
}

function Assert-MachinePathAbsent {
    if ((Get-InstalledPathEntryCount) -ne 0) {
        throw "Machine PATH retained the xsoc installation directory."
    }
}

function Assert-ClientCompletelyAbsent {
    if ((Test-Path -LiteralPath $installedRoot) -or
        (Test-Path -LiteralPath $stateRoot) -or
        (Test-Path -LiteralPath $installJournal) -or
        (Test-Path -LiteralPath $uninstallJournal) -or
        (Test-Path -LiteralPath $purgeQuarantine) -or
        (Test-Path -LiteralPath $maintenanceDiagnostic) -or
        (Test-Path -LiteralPath $trayShortcut) -or
        (Get-ItemProperty -LiteralPath $trayRunKey -Name $trayRunName `
            -ErrorAction SilentlyContinue) -or
        (Get-Service -Name "xsoc" -ErrorAction SilentlyContinue) -or
        (Get-Process -Name "xsoc-tray" -ErrorAction SilentlyContinue)) {
        throw "Client installation or a protected transaction artifact survived purge."
    }
    $entries = @(Get-XsocArpEntries)
    if ($entries.Count -ne 0) {
        throw "Apps & Features still contains xsoc."
    }
    if (Get-ItemProperty -LiteralPath 'HKLM:\Software\sarmg\xsoc' `
        -Name InstallLocation -ErrorAction SilentlyContinue) {
        throw 'The Client installation directory registration survived purge.'
    }
    Assert-MachinePathAbsent
}

function Read-RuntimeLogBounded([string]$Path) {
    $maximumBytes = 8388608
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read,
        [IO.FileShare]::ReadWrite)
    try {
        if ($stream.Length -gt $maximumBytes) { throw 'Runtime log exceeds its physical slot budget' }
        # A zero-length lease has no data to read while its advisory lock is held.
        if ($stream.Length -eq 0) { return ,([byte[]]::new(0)) }
        $buffer = [byte[]]::new($maximumBytes + 1)
        $length = 0
        while ($length -lt $buffer.Length) {
            $read = $stream.Read($buffer, $length, $buffer.Length - $length)
            if ($read -eq 0) { break }
            $length += $read
        }
        if ($length -gt $maximumBytes) { throw 'Runtime log exceeded bounded read budget' }
        $result = [byte[]]::new($length)
        [Array]::Copy($buffer, $result, $length)
        return ,$result
    } finally { $stream.Dispose() }
}

function Get-RuntimeLogHash([string]$Path) {
    $bytes = Read-RuntimeLogBounded $Path
    $algorithm = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($algorithm.ComputeHash($bytes))).Replace('-', '') }
    finally { $algorithm.Dispose() }
}

function Write-FixedStateAclEvidence([string]$Stage) {
    foreach ($name in @('.', 'maintenance.lock', '.credential-state.lock', 'client-token')) {
        $path = if ($name -eq '.') { $stateRoot } else { Join-Path $stateRoot $name }
        $exists = Test-Path -LiteralPath $path
        if ($exists) {
            $acl = Get-Acl -LiteralPath $path
            [pscustomobject]@{
                fixture_stage=$Stage; object_name=$name; exists=$true
                owner=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
                protected=$acl.AreAccessRulesProtected; ace_count=@($acl.Access).Count
                sddl=$acl.Sddl
            } | ConvertTo-Json -Compress | Write-Host
        } else {
            [pscustomobject]@{ fixture_stage=$Stage; object_name=$name; exists=$false } |
                ConvertTo-Json -Compress | Write-Host
        }
    }
}

function Assert-SystemOwnedPrivateFileAcl([string]$Path, [string]$ServiceSid, [bool]$Protected) {
    $item = Get-Item -LiteralPath $Path -Force
    $acl = Get-Acl -LiteralPath $Path
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
        $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne 'S-1-5-18' -or
        $acl.AreAccessRulesProtected -ne $Protected) { throw 'Known fixture file has unexpected type, owner or protection' }
    $expected = @{ 'S-1-5-18'=0x1f01ff; 'S-1-5-32-544'=0x1f01ff; 'S-1-3-4'=0x20000 }
    $expected[$ServiceSid] = 0x1301bf
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    if ($rules.Count -ne 4) { throw 'Known fixture file does not have exactly four private roles' }
    foreach ($rule in $rules) {
        $sid = $rule.IdentityReference.Value
        if (-not $expected.ContainsKey($sid) -or [int]$rule.FileSystemRights -ne $expected[$sid] -or
            $rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or
            $rule.InheritanceFlags -ne [Security.AccessControl.InheritanceFlags]::None -or
            $rule.PropagationFlags -ne [Security.AccessControl.PropagationFlags]::None -or
            $rule.IsInherited -eq $Protected) { throw 'Known fixture file has an unexpected exact ACE' }
        $expected.Remove($sid)
    }
    if ($expected.Count -ne 0) { throw 'Known fixture file is missing a required role' }
}

function Assert-RuntimeLogsAcl([string]$ServiceSid) {
    $directory = Join-Path $stateRoot 'logs'
    $items = @(Get-ChildItem -LiteralPath $directory -Force)
    if ($items.Count -gt 6) { throw 'Runtime log namespace exceeds six fixed entries' }
    $paths = @($directory)
    foreach ($item in $items) {
        if ($item.PSIsContainer -or
            ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
            $item.Length -gt 8388608 -or
            ($item.Name -ne '.xsoc.jsonl.writer.lock' -and
             $item.Name -ne 'xsoc.jsonl' -and
             $item.Name -notmatch '^xsoc\.jsonl\.[1-4]$')) {
            throw 'Runtime log namespace contains an unexpected object'
        }
        $paths += $item.FullName
    }
    foreach ($path in $paths) {
        $item = Get-Item -LiteralPath $path -Force
        $acl = Get-Acl -LiteralPath $path
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or
            $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne 'S-1-5-19' -or
            $acl.AreAccessRulesProtected) {
            throw 'Runtime logs must be service-owned and inherit only their private anchor'
        }
        $expected = @{ 'S-1-5-18'=0x1f01ff; 'S-1-5-32-544'=0x1f01ff; 'S-1-3-4'=0x20000 }
        $expected[$ServiceSid] = 0x1301bf
        $inheritance = if ($item.PSIsContainer) {
            [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
                [Security.AccessControl.InheritanceFlags]::ObjectInherit
        } else { [Security.AccessControl.InheritanceFlags]::None }
        $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
        if ($rules.Count -ne 4) { throw 'Runtime log ACL must have exactly four effective roles' }
        foreach ($rule in $rules) {
            $sid = $rule.IdentityReference.Value
            if (-not $expected.ContainsKey($sid) -or
                [int]$rule.FileSystemRights -ne $expected[$sid] -or
                $rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or
                $rule.InheritanceFlags -ne $inheritance -or
                $rule.PropagationFlags -ne [Security.AccessControl.PropagationFlags]::None -or
                -not $rule.IsInherited) { throw 'Runtime log grants unexpected rights or inheritance' }
            $expected.Remove($sid)
        }
        if ($expected.Count -ne 0) { throw 'Runtime log is missing a required exact role' }
    }
}

function Assert-PreservedStateAcl([string]$RetiredServiceSid) {
    $protectedPaths = @($stateRoot)
    $logDirectory = Join-Path $stateRoot 'logs'
    $businessObjects = @(Get-ChildItem -LiteralPath $stateRoot -Force -Recurse)
    if ($businessObjects.Count -gt 128) { throw 'Controlled preserved-state fixture exceeds its bounded path budget' }
    foreach ($item in $businessObjects) {
        if ($item.FullName -eq $logDirectory -or
            $item.FullName.StartsWith($logDirectory + '\', [StringComparison]::OrdinalIgnoreCase)) { continue }
        $protectedPaths += $item.FullName
    }
    foreach ($protectedPath in $protectedPaths) {
        $acl = Get-Acl -LiteralPath $protectedPath
        $ownerSid = $acl.GetOwner(
            [System.Security.Principal.SecurityIdentifier]
        ).Value
        if ($ownerSid -ne "S-1-5-18" -or -not $acl.AreAccessRulesProtected) {
            throw "$protectedPath does not have SYSTEM ownership and a protected DACL."
        }

        $rules = @($acl.Access)
        $expectedRights = @{
            "S-1-5-18" = 0x1f01ff
            "S-1-5-32-544" = 0x1f01ff
            "S-1-3-4" = 0x00020000
        }
        $expectedInheritance = if ((Get-Item -LiteralPath $protectedPath).PSIsContainer) {
            [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor `
                [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
        }
        else {
            [System.Security.AccessControl.InheritanceFlags]::None
        }
        $preservedLogAnchor = $protectedPath -eq $stateRoot -and
            (Test-Path -LiteralPath (Join-Path $stateRoot 'logs'))
        if ($preservedLogAnchor) { $expectedRights[$RetiredServiceSid] = 0x001301bf }
        if ($rules.Count -ne $expectedRights.Count) {
            throw "$protectedPath has an unexpected preserved-state ACE count."
        }
        foreach ($rule in $rules) {
            $sid = $rule.IdentityReference.Translate(
                [System.Security.Principal.SecurityIdentifier]
            ).Value
            if (-not $expectedRights.ContainsKey($sid) -or
                ($sid -eq $RetiredServiceSid -and -not $preservedLogAnchor) -or $sid -eq "S-1-5-19" -or
                $rule.AccessControlType -ne `
                    [System.Security.AccessControl.AccessControlType]::Allow -or
                [int]$rule.FileSystemRights -ne $expectedRights[$sid] -or
                $rule.InheritanceFlags -ne $expectedInheritance -or
                $rule.PropagationFlags -ne `
                    [System.Security.AccessControl.PropagationFlags]::None -or
                $rule.IsInherited) {
                throw ("$protectedPath has an unexpected preserved-state ACE for $sid " +
                    "(rights=$([int]$rule.FileSystemRights), " +
                    "inheritance=$($rule.InheritanceFlags), " +
                    "propagation=$($rule.PropagationFlags), " +
                    "inherited=$($rule.IsInherited)).")
            }
            $expectedRights.Remove($sid) | Out-Null
        }
        if ($expectedRights.Count -ne 0) {
            throw "$protectedPath is missing a required preserved-state ACE."
        }
    }
}

$existingArpEntries = @(Get-XsocArpEntries)
$runningTrayProcesses = @(Get-Process -Name "xsoc-tray" `
    -ErrorAction SilentlyContinue)
Assert-MaintenanceDiagnosticAbsent "Before MSI lifecycle smoke test"
if ((Test-Path -LiteralPath $installedRoot) -or
    (Test-Path -LiteralPath $stateRoot) -or
    (Test-Path -LiteralPath $installJournal) -or
    (Test-Path -LiteralPath $uninstallJournal) -or
    (Test-Path -LiteralPath $purgeQuarantine) -or
    (Test-Path -LiteralPath $trayShortcut) -or
    (Get-ItemProperty -LiteralPath $trayRunKey -Name $trayRunName `
        -ErrorAction SilentlyContinue) -or
    (Get-Service -Name "xsoc" -ErrorAction SilentlyContinue) -or
    $existingArpEntries.Count -ne 0 -or $runningTrayProcesses.Count -ne 0) {
    throw ("The disposable runner contains an existing xsoc installation, " +
        "MSI/Apps & Features registration, running tray process, or transaction artifact.")
}

$currentPackages = @(Get-ChildItem -LiteralPath $ArtifactDirectory -Filter "*.msi")
if ($currentPackages.Count -ne 1) {
    throw "Expected exactly one release MSI; found $($currentPackages.Count)."
}
$currentMsi = $currentPackages[0].FullName

# An unowned pre-existing state root must never be handed to LocalService.
New-Item -ItemType Directory -Path $stateRoot | Out-Null
$preseed = Join-Path $stateRoot "attacker-controlled.json"
Set-Content -LiteralPath $preseed -Value "must remain untrusted"
Invoke-Msi /i $currentMsi "reject-preseeded-state" -ExpectFailure
if (-not (Test-Path -LiteralPath $preseed -PathType Leaf)) {
    throw "Rejected install modified the untrusted pre-existing state tree."
}
Remove-Item -LiteralPath $stateRoot -Recurse -Force

# A foreign same-name SCM service must likewise remain untouched.
New-Service -Name "xsoc" -BinaryPathName "$env:SystemRoot\System32\cmd.exe /c exit 0" `
    -StartupType Manual | Out-Null
$foreignPath = (Get-CimInstance Win32_Service -Filter "Name='xsoc'").PathName
Invoke-Msi /i $currentMsi "reject-foreign-service" -ExpectFailure
$remainingForeignPath = (Get-CimInstance Win32_Service -Filter "Name='xsoc'").PathName
if ($remainingForeignPath -ne $foreignPath) {
    throw "Rejected install modified the foreign SCM service."
}
& sc.exe delete xsoc | Out-Host
if ($LASTEXITCODE -ne 0) { throw "Could not remove the smoke-test foreign service." }
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    if (-not (Get-Service -Name "xsoc" -ErrorAction SilentlyContinue)) { break }
    Start-Sleep -Milliseconds 500
}
if (Get-Service -Name "xsoc" -ErrorAction SilentlyContinue) {
    throw "Smoke-test foreign service was not deleted."
}

# Reproduce the reported machine: correctly registered LocalService, but its
# executable and state were removed by a previous uninstall.
$orphanCommand = '"' + (Join-Path $installedRoot 'xsoc.exe') + '" --windows-service run --config "' + (Join-Path $stateRoot 'config.json') + '"'
$orphan = Invoke-CimMethod -ClassName Win32_Service -MethodName Create -Arguments @{
    Name = 'xsoc'; DisplayName = 'xsoc'; PathName = $orphanCommand
    ServiceType = [byte]16; ErrorControl = [byte]1; StartMode = 'Automatic'
    DesktopInteract = $false; StartName = 'NT AUTHORITY\LocalService'
}
if ($orphan.ReturnValue -ne 0) { throw "Could not create orphan-service regression fixture: $($orphan.ReturnValue)" }
Invoke-Msi /i $currentMsi "repair-orphan-service"
Invoke-Msi /x $currentMsi "purge-orphan-fixture" "PURGE=1"
Assert-ClientCompletelyAbsent

Invoke-Msi /i $currentMsi "fresh-install"
Assert-InstallLocation $installedRoot
Assert-MachinePathInstalled
if ((Get-Service xsoc).Status -ne "Stopped") { throw "Fresh install must not start before pairing" }
$bundledSmartctl = Join-Path $installedRoot 'smartmontools\bin\smartctl.exe'
$bundledSmartSource = Join-Path $installedRoot 'smartmontools\smartmontools-7.5-source.tar.gz'
foreach ($bundledFile in @($bundledSmartctl, $bundledSmartSource)) {
    if (-not (Test-Path -LiteralPath $bundledFile -PathType Leaf)) {
        throw "Bundled smartmontools payload is missing: $bundledFile"
    }
}
$smartVersion = & $bundledSmartctl --version 2>&1 | Out-String
if ($LASTEXITCODE -ne 0 -or $smartVersion -notmatch 'smartctl 7\.5') {
    throw "Bundled smartctl is not executable or has the wrong version: $smartVersion"
}
Write-FixedStateAclEvidence 'fresh-install'
# Synthetic offline identity for SCM/ACL acceptance only. The installed private
# state root is fresh and the service is stopped; no remote registration occurs.
Assert-StateAcl
$fixtureIdentity = [Guid]::NewGuid().ToString()
$fixtureGeneration = [Guid]::NewGuid().ToString()
$fixtureRequest = [Guid]::NewGuid().ToString()
$fixtureEndpoint = 'https://127.0.0.1:9/api/v1/xsoc/report'
$fixtureTime = [DateTime]::UtcNow.ToString('o')
$fixtureFiles = @{
    'host-id' = $fixtureIdentity
    'client-token' = ('a' * 64)
    'active-binding.json' = (@{ version='1.0.0'; generation=$fixtureGeneration; request_id=$fixtureRequest; instance_id=$fixtureIdentity; report_endpoint=$fixtureEndpoint } | ConvertTo-Json -Compress)
    'auth-state.json' = (@{ version='1.0.0'; status='authorized'; reason='offline native service fixture'; changed_at=$fixtureTime } | ConvertTo-Json -Compress)
    'pairing-state.json' = (@{ phase='active'; version='1.0.0'; generation=$fixtureGeneration; request_id=$fixtureRequest; instance_id=$fixtureIdentity; report_endpoint=$fixtureEndpoint; activation_url='https://127.0.0.1:9/activate'; completed_at=$fixtureTime } | ConvertTo-Json -Compress)
}
$fixtureServiceSid = (New-Object Security.Principal.NTAccount('NT SERVICE', 'xsoc')).Translate([Security.Principal.SecurityIdentifier]).Value
# These are five synthetic files in a fresh, stopped test installation. Assign
# their owner first, then set the complete private DACL instead of retaining a
# partial ACL from the test account's file creation or owner reassignment.
$fixtureFileDacl = 'D:P(A;;RC;;;OW)(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1301bf;;;' + $fixtureServiceSid + ')'
foreach ($entry in $fixtureFiles.GetEnumerator()) {
    $path = Join-Path $stateRoot $entry.Key
    if (Test-Path -LiteralPath $path) { throw 'Refusing to overwrite an existing fixture identity' }
    [IO.File]::WriteAllText($path, $entry.Value, [Text.UTF8Encoding]::new($false))
    $fixtureBytesHash = Get-RuntimeLogHash $path
    & icacls.exe $path /setowner '*S-1-5-18' /Q | Out-Null
    if ($LASTEXITCODE -ne 0 -or
        (Get-Acl -LiteralPath $path).GetOwner([Security.Principal.SecurityIdentifier]).Value -ne 'S-1-5-18') {
        throw 'Offline service fixture must retain the installed SYSTEM owner'
    }
    $privateFixtureAcl = Get-Acl -LiteralPath $path
    $privateFixtureAcl.SetSecurityDescriptorSddlForm($fixtureFileDacl,
        [Security.AccessControl.AccessControlSections]::Access)
    Set-Acl -LiteralPath $path -AclObject $privateFixtureAcl
    Assert-SystemOwnedPrivateFileAcl $path $fixtureServiceSid $true
    if ((Get-RuntimeLogHash $path) -ne $fixtureBytesHash) {
        throw 'Assigning offline fixture owner and exact private DACL changed its bytes'
    }
}
Write-Host 'Offline SCM fixture verified five SYSTEM-owned protected exact-role files and unchanged bytes.'
# Isolate the root-negative fixture from unrelated owner/inheritance propagation.
# First prove the fresh SYSTEM-created lock is exactly private and record its
# original inherited state; then protect only these known fixture-owned leaves.
$isolatedLocks = @{}
Write-FixedStateAclEvidence 'before-root-isolation'
foreach ($name in @('maintenance.lock', '.credential-state.lock')) {
    $path = Join-Path $stateRoot $name
    if (-not (Test-Path -LiteralPath $path)) { continue }
    $acl = Get-Acl -LiteralPath $path
    Assert-SystemOwnedPrivateFileAcl $path $fixtureServiceSid $acl.AreAccessRulesProtected
    $bytesHash = Get-RuntimeLogHash $path
    $acl.SetAccessRuleProtection($true, $true)
    Set-Acl -LiteralPath $path -AclObject $acl
    Assert-SystemOwnedPrivateFileAcl $path $fixtureServiceSid $true
    if ((Get-RuntimeLogHash $path) -ne $bytesHash) { throw 'Fixture isolation changed known lock bytes' }
    $isolatedLocks[$path] = @{ Sddl=(Get-Acl -LiteralPath $path).Sddl; Hash=$bytesHash }
}
Write-FixedStateAclEvidence 'before-unsafe-root'
# The built-in account alone must not authorize product credential state.
# A real SCM start with an extra LocalService grant must fail before creating logs.
$safeStateSddl = (Get-Acl -LiteralPath $stateRoot).Sddl
$unsafeStateAcl = Get-Acl -LiteralPath $stateRoot
$unsafeStateAcl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
    [Security.Principal.SecurityIdentifier]::new('S-1-5-19'),
    [Security.AccessControl.FileSystemRights]::FullControl,
    [Security.AccessControl.InheritanceFlags]::None,
    [Security.AccessControl.PropagationFlags]::None,
    [Security.AccessControl.AccessControlType]::Allow
))
try {
    Set-Acl -LiteralPath $stateRoot -AclObject $unsafeStateAcl
    Write-FixedStateAclEvidence 'unsafe-root'
    try { Start-Service xsoc -ErrorAction Stop } catch { }
    $unsafeService = Get-Service xsoc
    $unsafeService.WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped,
        [TimeSpan]::FromSeconds(30))
    if (Test-Path -LiteralPath (Join-Path $stateRoot 'logs')) {
        throw 'SCM accepted a broad built-in account grant before private logging preflight'
    }
}
finally {
    $restoredStateAcl = Get-Acl -LiteralPath $stateRoot
    $restoredStateAcl.SetSecurityDescriptorSddlForm($safeStateSddl)
    Set-Acl -LiteralPath $stateRoot -AclObject $restoredStateAcl
}
Write-FixedStateAclEvidence 'restored-root'
foreach ($entry in $isolatedLocks.GetEnumerator()) {
    Assert-SystemOwnedPrivateFileAcl $entry.Key $fixtureServiceSid $true
    if ((Get-Acl -LiteralPath $entry.Key).Sddl -ne $entry.Value.Sddl -or
        (Get-RuntimeLogHash $entry.Key) -ne $entry.Value.Hash) {
        throw 'Root-negative fixture changed unrelated known lock owner, DACL or bytes'
    }
}
Assert-StateAcl
$credentialPath = Join-Path $stateRoot 'client-token'
$safeCredentialSddl = (Get-Acl -LiteralPath $credentialPath).Sddl
$safeCredentialHash = (Get-FileHash -LiteralPath $credentialPath).Hash
$unsafeCredentialAcl = Get-Acl -LiteralPath $credentialPath
$unsafeCredentialAcl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
    [Security.Principal.SecurityIdentifier]::new('S-1-5-19'),
    [Security.AccessControl.FileSystemRights]::FullControl,
    [Security.AccessControl.InheritanceFlags]::None,
    [Security.AccessControl.PropagationFlags]::None,
    [Security.AccessControl.AccessControlType]::Allow
))
Write-FixedStateAclEvidence 'before-unsafe-credential'
try {
    Set-Acl -LiteralPath $credentialPath -AclObject $unsafeCredentialAcl
    Write-FixedStateAclEvidence 'unsafe-credential'
    try { Start-Service xsoc -ErrorAction Stop } catch { }
    $unsafeService = Get-Service xsoc
    $unsafeService.WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped,
        [TimeSpan]::FromSeconds(30))
    if ((Get-FileHash -LiteralPath $credentialPath).Hash -ne $safeCredentialHash) {
        throw 'Rejected credential permissions caused a credential rewrite'
    }
}
finally {
    $restoredCredentialAcl = Get-Acl -LiteralPath $credentialPath
    $restoredCredentialAcl.SetSecurityDescriptorSddlForm($safeCredentialSddl)
    Set-Acl -LiteralPath $credentialPath -AclObject $restoredCredentialAcl
}
Write-FixedStateAclEvidence 'restored-credential'
foreach ($entry in $isolatedLocks.GetEnumerator()) {
    Assert-SystemOwnedPrivateFileAcl $entry.Key $fixtureServiceSid $true
    if ((Get-Acl -LiteralPath $entry.Key).Sddl -ne $entry.Value.Sddl -or
        (Get-RuntimeLogHash $entry.Key) -ne $entry.Value.Hash) {
        throw 'Credential-negative fixture changed unrelated known lock owner, DACL or bytes'
    }
}
Assert-StateAcl
try { Start-Service xsoc -ErrorAction Stop } catch {
    & sc.exe queryex xsoc
    $runtimeLog = Join-Path $stateRoot 'logs\xsoc.jsonl'
    if (Test-Path -LiteralPath $runtimeLog -PathType Leaf) {
        foreach ($record in @(Get-Content -LiteralPath $runtimeLog -Tail 8)) {
            $diagnostic = $record | ConvertFrom-Json
            [pscustomobject]@{
                event = $diagnostic.event
                level = $diagnostic.level
                error_code = if ($diagnostic.PSObject.Properties['error_code']) {
                    $diagnostic.error_code
                } else { $null }
            } | ConvertTo-Json -Compress | Write-Host
        }
    }
    foreach ($name in @('maintenance.lock', '.credential-state.lock')) {
        $lockPath = Join-Path $stateRoot $name
        if (Test-Path -LiteralPath $lockPath) {
            Get-Item -LiteralPath $lockPath | Select-Object Name, Attributes
            Write-Host (Get-Acl -LiteralPath $lockPath).Sddl
        }
    }
    Write-Host (Get-Acl -LiteralPath $stateRoot).Sddl
    throw
}
Assert-ServiceRunning
Assert-StateAcl
Assert-TrayIntegration
Assert-ArpVersion $ProductVersion
Assert-MachinePathInstalled

# Exercise the installed CLI against the real LocalService process. The fixture
# endpoint is deliberately offline; IPC reachability is not delivery health.
$client = Join-Path $installedRoot 'xsoc.exe'
$configPath = Join-Path $stateRoot 'config.json'
$before = @{}
foreach ($name in @($fixtureFiles.Keys) + @('config.json')) {
    $before[$name] = (Get-FileHash -LiteralPath (Join-Path $stateRoot $name)).Hash
}
$statusText = & $client status --config $configPath --format json --non-interactive --timeout 10s
if ($LASTEXITCODE -ne 0) { throw 'Installed CLI status failed' }
$status = $statusText | ConvertFrom-Json
if (-not $status.ok -or -not $status.result.runtime.available -or
    $status.result.runtime.binding_generation -ne $fixtureIdentity -or
    $status.result.service.state -ne 'running' -or $status.result.health -ne 'unknown') {
    @{ ipc_available=$status.result.runtime.available; binding_matches=($status.result.runtime.binding_generation -eq $fixtureIdentity); service=$status.result.service.state; health=$status.result.health } | ConvertTo-Json -Compress | Write-Host
    throw 'Installed CLI did not report the authenticated runtime and unconfirmed delivery health'
}
$logsText = & $client logs --config $configPath --event 'xsoc.windows.started' --tail 10 --format json --non-interactive
if ($LASTEXITCODE -ne 0) { throw 'Private runtime log query failed' }
$queriedLogs = $logsText | ConvertFrom-Json
if (-not $queriedLogs.ok -or $queriedLogs.result.source -ne 'private-runtime-log' -or $queriedLogs.result.retention_bytes -ne 41943040 -or @($queriedLogs.result.entries).Count -lt 1) { throw 'SCM runtime diagnostics were not persisted and queried' }
foreach ($entry in $queriedLogs.result.entries) {
    if ($entry.event -ne 'xsoc.windows.started' -or $entry.timestamp -notmatch 'Z$') { throw 'Typed event/time query contract failed' }
}
$logServiceSid = (New-Object Security.Principal.NTAccount('NT SERVICE', 'xsoc')).Translate([Security.Principal.SecurityIdentifier]).Value
Assert-RuntimeLogsAcl $logServiceSid
$followText = & $client logs --config $configPath --follow --format ndjson --timeout 2s --non-interactive
if ($LASTEXITCODE -ne 0 -or -not $followText) { throw 'Typed private runtime log follow failed' }
foreach ($line in $followText) { if (-not ($line | ConvertFrom-Json).ok) { throw 'Runtime log follow returned a failure record' } }
$revision = $status.result.config.stored_revision
$busyText = & $client config apply --config $configPath --file $configPath --expected-revision $revision --format json --non-interactive
if ($LASTEXITCODE -ne 5 -or ($busyText | ConvertFrom-Json).error.code -ne 'busy') {
    throw 'Running service did not exclude CLI maintenance'
}
foreach ($name in $before.Keys) {
    if ((Get-FileHash -LiteralPath (Join-Path $stateRoot $name)).Hash -ne $before[$name]) {
        throw 'Readonly CLI or rejected maintenance changed persisted configuration or identity'
    }
}
$stopText = & $client service stop --format json --non-interactive --timeout 20s
if ($LASTEXITCODE -ne 0 -or -not ($stopText | ConvertFrom-Json).ok) { throw 'CLI service stop failed' }
Start-Sleep -Seconds 3
if ((Get-Service xsoc).Status -ne 'Stopped') { throw 'Explicitly stopped service restarted' }
$applyText = & $client config apply --config $configPath --file $configPath --expected-revision $revision --format json --non-interactive
if ($LASTEXITCODE -ne 0 -or -not ($applyText | ConvertFrom-Json).result.committed) {
    throw 'Stopped service configuration commit failed'
}
$startText = & $client service start --format json --non-interactive --timeout 20s
if ($LASTEXITCODE -ne 0 -or -not ($startText | ConvertFrom-Json).ok) { throw 'CLI service restart after administrator commit failed' }
Assert-ServiceRunning
$updatedText = & $client status --config $configPath --format json --non-interactive
if ($LASTEXITCODE -ne 0 -or -not ($updatedText | ConvertFrom-Json).result.runtime.available) {
    throw 'LocalService could not read administrator-committed configuration'
}
$enableText = & $client service enable --format json --non-interactive
if ($LASTEXITCODE -ne 0 -or ($enableText | ConvertFrom-Json).result.startup -ne 'automatic') {
    throw 'CLI did not enable automatic startup'
}
$disableText = & $client service disable --format json --non-interactive
if ($LASTEXITCODE -ne 0 -or ($disableText | ConvertFrom-Json).result.startup -ne 'manual') {
    throw 'CLI did not restore manual startup'
}
Assert-ServiceRunning
Write-Host 'Installed CLI readonly IPC, maintenance exclusion and service-account configuration access passed.'

# Stop a live producer at the native Prepare boundary and then simulate MSI
# rollback. The ACL path set must stay frozen and rollback must restore the
# original running state without replacing credentials or configuration.
$prepareHashes = @{}
$prepareAcls = @{}
foreach ($name in @($fixtureFiles.Keys) + @('config.json')) {
    $path = Join-Path $stateRoot $name
    $prepareHashes[$name] = (Get-FileHash -LiteralPath $path).Hash
    $prepareAcls[$path] = (Get-Acl -LiteralPath $path).Sddl
}
foreach ($path in @($stateRoot, (Join-Path $stateRoot $stateMarker))) {
    $prepareAcls[$path] = (Get-Acl -LiteralPath $path).Sddl
}
Assert-RuntimeLogsAcl $logServiceSid
$prepareLogRoot = Join-Path $stateRoot 'logs'
$prepareLogLeaves = @(Get-ChildItem -LiteralPath $prepareLogRoot -Force)
if ($prepareLogLeaves.Count -gt 6) { throw 'Runtime log snapshot exceeded its fixed namespace' }
foreach ($path in @($prepareLogRoot) + @($prepareLogLeaves | ForEach-Object FullName)) {
    $prepareAcls[$path] = (Get-Acl -LiteralPath $path).Sddl
}
$prepare = Start-Process -FilePath $MaintenanceExecutable -ArgumentList @(
    'prepare-install', ('"{0}"' -f $client), '1'
) -Wait -PassThru
if ($prepare.ExitCode -ne 0) { throw 'Live-service install preparation failed' }
Assert-MaintenanceDiagnosticAbsent 'live-service prepare'
if ((Get-Service xsoc).Status -ne 'Stopped') {
    throw 'Install preparation captured ACLs while the producer was still active'
}
$snapshot = Get-Content -LiteralPath (Join-Path $installJournal 'snapshot.json') -Raw |
    ConvertFrom-Json
if (-not $snapshot.original_service_running) {
    throw 'Install preparation lost the original running state needed by rollback'
}
$preparedPaths = @(Get-ChildItem -LiteralPath $stateRoot -Force -Recurse |
    ForEach-Object FullName | Sort-Object)
Start-Sleep -Seconds 3
$stablePaths = @(Get-ChildItem -LiteralPath $stateRoot -Force -Recurse |
    ForEach-Object FullName | Sort-Object)
if (@(Compare-Object $preparedPaths $stablePaths).Count -ne 0) {
    throw 'State paths changed after install preparation stopped the producer'
}
$rollback = Start-Process -FilePath $MaintenanceExecutable -ArgumentList @(
    'rollback-install', ('"{0}"' -f $client), '1'
) -Wait -PassThru
if ($rollback.ExitCode -ne 0) { throw 'Live-service install rollback failed' }
Assert-MaintenanceDiagnosticAbsent 'live-service rollback'
Assert-ServiceRunning
# Administrator configuration commits legitimately create an administrator-owned
# private replacement file. Rollback preserves its original owner and DACL;
# the exact factory installer ACL was already checked before that CLI commit.
foreach ($path in $prepareAcls.Keys) {
    if ((Get-Acl -LiteralPath $path).Sddl -ne $prepareAcls[$path]) {
        throw 'Install rollback did not restore the original state owner and ACL'
    }
}
Assert-RuntimeLogsAcl $logServiceSid
if (Test-Path -LiteralPath $installJournal) { throw 'Install rollback retained a completed journal' }
foreach ($name in $prepareHashes.Keys) {
    if ((Get-FileHash -LiteralPath (Join-Path $stateRoot $name)).Hash -ne $prepareHashes[$name]) {
        throw 'Install preparation or rollback changed configuration or identity'
    }
}
Write-Host 'Live-service preparation froze state paths and rollback restored the running service.'

$marker = Join-Path $stateRoot "release-lifecycle-marker"
Set-Content -LiteralPath $marker -Value "must survive ordinary uninstall"

$installedServiceSid = (New-Object System.Security.Principal.NTAccount(
    "NT SERVICE", "xsoc"
)).Translate([System.Security.Principal.SecurityIdentifier]).Value
# Keep the producer running for the real uninstall gate. The controlled fixture
# must stay far below its 8MiB rotation boundary; stop legitimately appends a
# typed record, so active preservation is an exact old-prefix check first.
$preUninstallLogHashes = @{}
$preUninstallLogNames = @()
$preUninstallActive = $null
$fixtureLogBytes = 0L
foreach ($entry in @(Get-ChildItem -LiteralPath (Join-Path $stateRoot 'logs') -Force)) {
    $fixtureLogBytes += $entry.Length
    $preUninstallLogNames += $entry.Name
    if ($entry.Name -eq 'xsoc.jsonl') {
        $preUninstallActive = Read-RuntimeLogBounded $entry.FullName
    } else {
        $preUninstallLogHashes[$entry.FullName] = (Get-RuntimeLogHash $entry.FullName)
    }
}
if ($null -eq $preUninstallActive -or $fixtureLogBytes -ge 1048576) {
    throw 'Controlled fixture logs do not have the required margin below the 8MiB rotation boundary'
}
Invoke-Msi /x $currentMsi "preserve-uninstall"

if (Get-Service -Name "xsoc" -ErrorAction SilentlyContinue) {
    throw "Client service survived ordinary uninstall."
}
if (Test-Path -LiteralPath $installedRoot) {
    throw "Program directory survived ordinary uninstall."
}
if ((Test-Path -LiteralPath $trayShortcut) -or
    (Get-ItemProperty -LiteralPath $trayRunKey -Name $trayRunName `
        -ErrorAction SilentlyContinue)) {
    throw "Tray startup integration survived ordinary uninstall."
}
if (-not (Test-Path -LiteralPath $marker -PathType Leaf)) {
    throw "Ordinary uninstall removed Client state."
}
if ((Test-Path -LiteralPath $installJournal) -or
    (Test-Path -LiteralPath $uninstallJournal) -or
    (Test-Path -LiteralPath $purgeQuarantine)) {
    throw "Ordinary uninstall left a transaction journal or purge quarantine."
}
if (@(Get-XsocArpEntries).Count -ne 0) {
    throw "Apps & Features still contains xsoc after ordinary uninstall."
}
Assert-PreservedStateAcl $installedServiceSid
Assert-RuntimeLogsAcl $installedServiceSid
$postUninstallLogs = @(Get-ChildItem -LiteralPath (Join-Path $stateRoot 'logs') -Force)
if (@(Compare-Object ($preUninstallLogNames | Sort-Object) ($postUninstallLogs.Name | Sort-Object)).Count -ne 0 -or
    ($postUninstallLogs | Measure-Object -Property Length -Sum).Sum -ge 1048576) {
    throw 'Uninstall changed fixed log paths or exhausted the non-rotating fixture budget'
}
$postUninstallActive = Read-RuntimeLogBounded (Join-Path $stateRoot 'logs\xsoc.jsonl')
if ($postUninstallActive.Length -lt $preUninstallActive.Length) { throw 'Uninstall truncated existing active log bytes' }
for ($index = 0; $index -lt $preUninstallActive.Length; $index++) {
    if ($postUninstallActive[$index] -ne $preUninstallActive[$index]) { throw 'Uninstall changed existing active log prefix bytes' }
}
foreach ($entry in $preUninstallLogHashes.GetEnumerator()) {
    if ((Get-RuntimeLogHash $entry.Key) -ne $entry.Value) { throw 'Preserve uninstall changed retained archive or lease bytes' }
}
# The producer is now absent. This full byte snapshot must remain exact through
# reinstall/repair before another explicit SCM start appends runtime records.
$preservedLogHashes = @{}
foreach ($entry in $postUninstallLogs) {
    $preservedLogHashes[$entry.FullName] = (Get-RuntimeLogHash $entry.FullName)
}

# Reinstall must preserve the canonical 1.0.0 ownership marker byte for byte.
$preservedMarkerHash = (Get-FileHash -LiteralPath (Join-Path $stateRoot $stateMarker)).Hash
Invoke-Msi /i $currentMsi "reinstall"
Assert-InstallLocation $installedRoot
if ((Get-FileHash -LiteralPath (Join-Path $stateRoot $stateMarker)).Hash -ne $preservedMarkerHash) {
    throw 'Reinstall changed the canonical persistent-state ownership marker'
}
Assert-RuntimeLogsAcl $installedServiceSid
foreach ($entry in $preservedLogHashes.GetEnumerator()) {
    if ((Get-RuntimeLogHash $entry.Key) -ne $entry.Value) { throw 'Reinstall changed retained runtime log bytes' }
}
$expectedExeHash = (Get-FileHash (Join-Path $installedRoot 'xsoc.exe')).Hash
Set-Content -LiteralPath (Join-Path $installedRoot 'xsoc.exe') -Value 'damaged payload'
& (Join-Path $PSScriptRoot "../install-xsoc.ps1") -Msi $currentMsi -Quiet
Assert-InstallLocation $installedRoot
if ((Get-FileHash (Join-Path $installedRoot 'xsoc.exe')).Hash -ne $expectedExeHash) { throw 'Repair did not force replacement of damaged executable' }
if ((Get-Content -LiteralPath (Join-Path $stateRoot 'host-id') -Raw) -ne $fixtureIdentity) { throw 'Repair changed device identity' }
Assert-RuntimeLogsAcl $installedServiceSid
foreach ($entry in $preservedLogHashes.GetEnumerator()) {
    if ((Get-RuntimeLogHash $entry.Key) -ne $entry.Value) { throw 'Repair changed retained runtime log bytes before SCM restart' }
}

try { Start-Service xsoc -ErrorAction Stop } catch {
    & sc.exe queryex xsoc
    throw
}
Assert-ServiceRunning
Assert-StateAcl
Assert-RuntimeLogsAcl $installedServiceSid
Assert-TrayIntegration
Assert-ArpVersion $ProductVersion
Assert-MachinePathInstalled
Invoke-Msi /x $currentMsi "purge-uninstall" "PURGE=1"
Assert-ClientCompletelyAbsent
Assert-MaintenanceDiagnosticAbsent "After MSI lifecycle smoke test"

# A non-default program directory must flow through MSI, SCM and every native
# maintenance validation without weakening the fixed ProgramData state root.
$customInstallRoot = Join-Path $env:ProgramFiles "xsoc-custom-$ProductVersion"
Invoke-Msi /i $currentMsi "custom-path-install" "INSTALLFOLDER=`"$customInstallRoot`""
Assert-InstallLocation $customInstallRoot
if (-not (Test-Path -LiteralPath (Join-Path $customInstallRoot 'xsoc.exe') -PathType Leaf)) {
    throw 'Custom installation directory did not receive the Client executable.'
}
if (-not (Test-Path -LiteralPath (Join-Path $customInstallRoot 'smartmontools\bin\smartctl.exe') -PathType Leaf)) {
    throw 'Custom installation directory did not receive bundled smartctl.'
}
$customImagePath = (Get-CimInstance Win32_Service -Filter "Name='xsoc'").PathName
if (-not $customImagePath.StartsWith("`"$customInstallRoot\xsoc.exe`"", [StringComparison]::OrdinalIgnoreCase)) {
    throw "Service ImagePath did not use the selected installation directory: $customImagePath"
}
Set-Content -LiteralPath (Join-Path $stateRoot 'host-id') -Value $fixtureIdentity -NoNewline
Invoke-Msi /x $currentMsi "custom-path-preserve-uninstall"
Invoke-Msi /i $currentMsi "custom-path-retain-state" "INSTALLFOLDER=`"$customInstallRoot`""
Assert-InstallLocation $customInstallRoot
if ((Get-Content -LiteralPath (Join-Path $stateRoot 'host-id') -Raw) -ne $fixtureIdentity) {
    throw 'Reinstall with the fixed feature set changed device identity.'
}
Invoke-Msi /x $currentMsi "custom-path-purge" "PURGE=1"
if (Test-Path -LiteralPath $customInstallRoot) {
    throw 'Custom installation directory survived uninstall.'
}
Assert-ClientCompletelyAbsent

# The rebuilt release line begins at 1.0.0. Exercise its installer entry point
# without depending on retired repository names or removed historical releases.
Invoke-Msi /i $currentMsi 'install-baseline'
[IO.File]::WriteAllText((Join-Path $stateRoot 'host-id'), $fixtureIdentity, [Text.UTF8Encoding]::new($false))
$baselineConfigHash = (Get-FileHash -LiteralPath (Join-Path $stateRoot 'config.json')).Hash
& (Join-Path $PSScriptRoot '../install-xsoc.ps1') -Msi $currentMsi -Quiet
Assert-ArpVersion $ProductVersion
Assert-MachinePathInstalled
Assert-StateAcl
if ((Get-Content -LiteralPath (Join-Path $stateRoot 'host-id') -Raw) -ne $fixtureIdentity) { throw 'Baseline repair changed device identity' }
if ((Get-FileHash -LiteralPath (Join-Path $stateRoot 'config.json')).Hash -ne $baselineConfigHash) { throw 'Baseline repair changed persisted configuration' }
Invoke-Msi /x $currentMsi 'purge-baseline-repair' 'PURGE=1'
Assert-ClientCompletelyAbsent
