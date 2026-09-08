[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string] $ProductVersion,
    [string] $ArtifactDirectory,
    [string] $LogDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Destructive MSI lifecycle tests are restricted to disposable GitHub-hosted runners.'
}
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
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
    $LogDirectory = Join-Path $temporaryRoot "host-monitor-msi-logs"
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "The MSI lifecycle smoke test requires an elevated Windows session."
}

$installedRoot = Join-Path $env:ProgramFiles "host-monitor"
$installedTray = Join-Path $installedRoot "host-monitor-tray.exe"
$stateRoot = Join-Path $env:ProgramData "host-monitor"
$trayRunKey = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run"
$trayRunName = "host-monitor-tray"
$commonPrograms = [Environment]::GetFolderPath(
    [Environment+SpecialFolder]::CommonPrograms
)
$trayShortcut = Join-Path $commonPrograms "host-monitor.lnk"
$stateMarker = ".host-monitor-managed-$ProductVersion"
$installJournal = Join-Path $env:ProgramData `
    "host-monitor.install-journal-$ProductVersion"
$uninstallJournal = Join-Path $env:ProgramData `
    "host-monitor.uninstall-journal-$ProductVersion"
$purgeQuarantine = Join-Path $env:ProgramData `
    "host-monitor.purge-quarantine-$ProductVersion"
$maintenanceDiagnostic = Join-Path $env:ProgramData `
    "host-monitor.maintenance-diagnostic-$ProductVersion.txt"
$maximumMaintenanceDiagnosticBytes = 64KB
$logs = $LogDirectory
New-Item -ItemType Directory -Force $logs | Out-Null

if (-not ("HostMonitoring.MsiNativeMethods" -as [type])) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace HostMonitoring {
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
        $fields[0] -cne "format=host-monitor-maintenance-diagnostic-v1" -or
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
        "HOST_MONITORING_MAINTENANCE_DIAGNOSTICS=1 /qn /norestart /l*v `"${log}`"")
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
    $service = Get-Service -Name "host-monitor" -ErrorAction Stop
    $service.WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running,
        [TimeSpan]::FromSeconds(30))
    $definition = Get-CimInstance Win32_Service -Filter "Name='host-monitor'"
    if ($definition.StartMode -ne "Auto" -or
        $definition.StartName -ne "NT AUTHORITY\LocalService" -or
        $definition.PathName -notmatch '--windows-service run --config') {
        throw "Installed SCM service definition is not the expected host-monitor service."
    }
    $serviceKey = "HKLM:\SYSTEM\CurrentControlSet\Services\host-monitor"
    if ((Get-ItemPropertyValue -LiteralPath $serviceKey -Name ServiceSidType) -ne 1) {
        throw "host-monitor does not have an unrestricted service SID."
    }
    if ((Get-ItemPropertyValue -LiteralPath $serviceKey `
        -Name FailureActionsOnNonCrashFailures) -ne 1) {
        throw "host-monitor does not enable failure actions for non-crash failures."
    }
}

function Assert-StateAcl {
    $serviceSid = (New-Object System.Security.Principal.NTAccount(
        "NT SERVICE", "host-monitor"
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
    if (Get-Process -Name "host-monitor-tray" -ErrorAction SilentlyContinue) {
        throw "A quiet MSI install incorrectly launched an interactive tray process."
    }
    if (-not (Test-Path -LiteralPath $installedTray -PathType Leaf)) {
        throw "The installed tray companion is missing."
    }
    $expectedRun = '"{0}" --startup' -f $installedTray
    $actualRun = Get-ItemPropertyValue -LiteralPath $trayRunKey `
        -Name $trayRunName -ErrorAction Stop
    if ($actualRun -cne $expectedRun) {
        throw "The tray Run registration is not the fixed installed command."
    }
    if (-not (Test-Path -LiteralPath $trayShortcut -PathType Leaf)) {
        throw "The host-monitor Start menu shortcut is missing."
    }

    # Advertised MSI shortcuts store a Darwin descriptor instead of a normal shell-link
    # target, so WScript.Shell.TargetPath is not a reliable assertion. Ask Windows
    # Installer which installed component the actual shortcut advertises instead.
    $productCode = New-Object System.Text.StringBuilder 39
    $featureId = New-Object System.Text.StringBuilder 39
    $componentCode = New-Object System.Text.StringBuilder 39
    $shortcutResult = [HostMonitoring.MsiNativeMethods]::MsiGetShortcutTarget(
        $trayShortcut, $productCode, $featureId, $componentCode
    )
    if ($shortcutResult -ne 0 -or $featureId.ToString() -cne "ClientFeature" -or
        $componentCode.ToString() -ine "{882DF421-2758-42E4-95D4-730C2571803E}") {
        throw "The Start menu shortcut does not advertise the tray component."
    }

    $componentPath = New-Object System.Text.StringBuilder 32768
    [uint32]$componentPathLength = $componentPath.Capacity
    $componentState = [HostMonitoring.MsiNativeMethods]::MsiGetComponentPath(
        $productCode.ToString(), $componentCode.ToString(),
        $componentPath, [ref]$componentPathLength
    )
    if ($componentState -ne 3 -or -not [string]::Equals(
        [IO.Path]::GetFullPath($componentPath.ToString()),
        [IO.Path]::GetFullPath($installedTray),
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "The advertised tray component is not installed at the fixed path."
    }

    # The service/config remains isolated, but every interactive user must be able
    # to execute the tray image from the protected Program Files tree.
    $usersRules = @((Get-Acl -LiteralPath $installedTray).Access | Where-Object {
        $_.AccessControlType -eq `
            [System.Security.AccessControl.AccessControlType]::Allow -and
        $_.IdentityReference.Translate(
            [System.Security.Principal.SecurityIdentifier]
        ).Value -eq "S-1-5-32-545"
    })
    if ($usersRules.Count -ne 1 -or
        [int]$usersRules[0].FileSystemRights -ne 0x1200a9) {
        throw "The tray image does not grant BUILTIN\\Users exact read/execute access."
    }
}

function Start-TrayForRemovalSmoke {
    $process = Start-Process -FilePath $installedTray -ArgumentList "--startup" -PassThru
    Start-Sleep -Seconds 2
    $process.Refresh()
    if ($process.HasExited) {
        throw "The tray companion exited before the MSI shutdown smoke could run."
    }
    return $process
}

function Get-HostMonitorArpEntries {
    $uninstallRoot = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
    return @(Get-ChildItem -LiteralPath $uninstallRoot | Get-ItemProperty |
        Where-Object {
            $displayName = $_.PSObject.Properties["DisplayName"]
            $null -ne $displayName -and $displayName.Value -eq "host-monitor"
        })
}

function Assert-ArpVersion([string]$ExpectedVersion) {
    $entries = @(Get-HostMonitorArpEntries)
    if ($entries.Count -ne 1 -or $entries[0].DisplayVersion -ne $ExpectedVersion) {
        throw "Apps & Features does not contain exactly host-monitor $ExpectedVersion."
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
        (Get-Service -Name "host-monitor" -ErrorAction SilentlyContinue) -or
        (Get-Process -Name "host-monitor-tray" -ErrorAction SilentlyContinue)) {
        throw "Client installation or a protected transaction artifact survived purge."
    }
    $entries = @(Get-HostMonitorArpEntries)
    if ($entries.Count -ne 0) {
        throw "Apps & Features still contains host-monitor."
    }
}

function Assert-PreservedStateAcl([string]$RetiredServiceSid) {
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
        $expectedInheritance = if ((Get-Item -LiteralPath $protectedPath).PSIsContainer) {
            [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor `
                [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
        }
        else {
            [System.Security.AccessControl.InheritanceFlags]::None
        }
        if ($rules.Count -ne $expectedRights.Count) {
            throw "$protectedPath has an unexpected preserved-state ACE count."
        }
        foreach ($rule in $rules) {
            $sid = $rule.IdentityReference.Translate(
                [System.Security.Principal.SecurityIdentifier]
            ).Value
            if (-not $expectedRights.ContainsKey($sid) -or
                $sid -eq $RetiredServiceSid -or $sid -eq "S-1-5-19" -or
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

$existingArpEntries = @(Get-HostMonitorArpEntries)
$runningTrayProcesses = @(Get-Process -Name "host-monitor-tray" `
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
    (Get-Service -Name "host-monitor" -ErrorAction SilentlyContinue) -or
    $existingArpEntries.Count -ne 0 -or $runningTrayProcesses.Count -ne 0) {
    throw ("The disposable runner contains an existing host-monitor installation, " +
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
New-Service -Name "host-monitor" -BinaryPathName "$env:SystemRoot\System32\cmd.exe /c exit 0" `
    -StartupType Manual | Out-Null
$foreignPath = (Get-CimInstance Win32_Service -Filter "Name='host-monitor'").PathName
Invoke-Msi /i $currentMsi "reject-foreign-service" -ExpectFailure
$remainingForeignPath = (Get-CimInstance Win32_Service -Filter "Name='host-monitor'").PathName
if ($remainingForeignPath -ne $foreignPath) {
    throw "Rejected install modified the foreign SCM service."
}
& sc.exe delete host-monitor | Out-Host
if ($LASTEXITCODE -ne 0) { throw "Could not remove the smoke-test foreign service." }
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    if (-not (Get-Service -Name "host-monitor" -ErrorAction SilentlyContinue)) { break }
    Start-Sleep -Milliseconds 500
}
if (Get-Service -Name "host-monitor" -ErrorAction SilentlyContinue) {
    throw "Smoke-test foreign service was not deleted."
}

Invoke-Msi /i $currentMsi "fresh-install"
Assert-ServiceRunning
Assert-StateAcl
Assert-TrayIntegration
Assert-ArpVersion $ProductVersion
$marker = Join-Path $stateRoot "release-lifecycle-marker"
Set-Content -LiteralPath $marker -Value "must survive ordinary uninstall"

$installedServiceSid = (New-Object System.Security.Principal.NTAccount(
    "NT SERVICE", "host-monitor"
)).Translate([System.Security.Principal.SecurityIdentifier]).Value
$trayBeforeUninstall = Start-TrayForRemovalSmoke
Invoke-Msi /x $currentMsi "preserve-uninstall"
if (-not $trayBeforeUninstall.WaitForExit(30000)) {
    throw "MSI uninstall did not gracefully close the running tray before file removal."
}
if (Get-Service -Name "host-monitor" -ErrorAction SilentlyContinue) {
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
if (@(Get-HostMonitorArpEntries).Count -ne 0) {
    throw "Apps & Features still contains host-monitor after ordinary uninstall."
}
Assert-PreservedStateAcl $installedServiceSid

Invoke-Msi /i $currentMsi "reinstall"
Assert-ServiceRunning
Assert-StateAcl
Assert-TrayIntegration
Assert-ArpVersion $ProductVersion
Invoke-Msi /x $currentMsi "purge-uninstall" "PURGE=1"
Assert-ClientCompletelyAbsent
Assert-MaintenanceDiagnosticAbsent "After MSI lifecycle smoke test"
