[CmdletBinding()]
param()

Set-StrictMode -Version 2.0
$ErrorActionPreference = "Stop"

$packagingRoot = Split-Path -Parent $PSScriptRoot
$wixRoot = Join-Path $packagingRoot "wix"
$packagePath = Join-Path $wixRoot "Package.wxs"
$projectPath = Join-Path $wixRoot "HostMonitor.Installer.wixproj"
$buildPath = Join-Path $wixRoot "build-msi.cmd"
$clientRoot = Split-Path -Parent (Split-Path -Parent $packagingRoot)
$workspaceRoot = $clientRoot
$workspacePath = Join-Path $workspaceRoot "Cargo.toml"
$helperPath = Join-Path $clientRoot "src\bin\host-monitor-maintenance.rs"
$mainPath = Join-Path $clientRoot "src\main.rs"
$helperSourceRoot = Join-Path $clientRoot "src\windows\maintenance"

foreach ($required in @(
    $packagePath, $projectPath, $buildPath, $workspacePath,
    $helperPath, $mainPath
)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required WiX packaging file is missing: $required"
    }
}
foreach ($requiredSourceRoot in @($helperSourceRoot)) {
    if (-not (Test-Path -LiteralPath $requiredSourceRoot -PathType Container)) {
        throw "Required Windows Client source tree is missing: $requiredSourceRoot"
    }
}

function Get-SourceBundle {
    param(
        [Parameter(Mandatory = $true)][string]$EntryPath,
        [Parameter(Mandatory = $true)][string]$SourceRoot
    )

    # The Windows binaries deliberately keep tiny entrypoints and place their
    # implementation in module trees. Packaging invariants must inspect the
    # complete compiled/text-included source, not just the entrypoint wrapper.
    $parts = @(
        Get-Content -LiteralPath $EntryPath -Raw -Encoding UTF8
        Get-ChildItem -LiteralPath $SourceRoot -Recurse -File |
            Sort-Object -Property FullName |
            ForEach-Object {
                Get-Content -LiteralPath $_.FullName -Raw -Encoding UTF8
            }
    )
    return ($parts -join "`n")
}

[xml]$package = Get-Content -LiteralPath $packagePath -Raw
[xml]$project = Get-Content -LiteralPath $projectPath -Raw
$packageText = Get-Content -LiteralPath $packagePath -Raw
$projectText = Get-Content -LiteralPath $projectPath -Raw
$buildText = Get-Content -LiteralPath $buildPath -Raw
$workspaceText = Get-Content -LiteralPath $workspacePath -Raw
$helperEntryText = Get-Content -LiteralPath $helperPath -Raw -Encoding UTF8
$helperText = Get-SourceBundle -EntryPath $helperPath -SourceRoot $helperSourceRoot
$mainText = Get-Content -LiteralPath $mainPath -Raw

foreach ($currentVersionBinding in @(
    'env!("CARGO_PKG_VERSION")',
    'application_version: String',
    'const SNAPSHOT_FORMAT: u32 = 2'
)) {
    if (-not $helperText.Contains($currentVersionBinding)) {
        throw "Windows state markers and transaction journals must be bound to the current package version."
    }
}
foreach ($outOfScopeUpgradeMechanism in @('TaskScheduler', 'ScheduledTask')) {
    if ($helperText.Contains($outOfScopeUpgradeMechanism)) {
        throw "The current-only maintenance helper contains an upgrade mechanism: $outOfScopeUpgradeMechanism"
    }
}

$guiSubsystemAttribute = '#![cfg_attr(windows, windows_subsystem = "windows")]'
if (-not $helperEntryText.StartsWith($guiSubsystemAttribute)) {
    throw "The MSI maintenance helper must use the Windows GUI subsystem to avoid flashing console windows."
}
if ([regex]::Matches($helperEntryText, [regex]::Escape($guiSubsystemAttribute)).Count -ne 1) {
    throw "The MSI maintenance helper must declare the Windows GUI subsystem exactly once."
}
if ($mainText -match 'windows_subsystem') {
    throw "The interactive Client executable must not inherit the maintenance helper's GUI subsystem."
}

function Assert-Contains {
    param(
        [Parameter(Mandatory = $true)][string]$Text,
        [Parameter(Mandatory = $true)][string]$Expected,
        [Parameter(Mandatory = $true)][string]$Message
    )
    if (-not $Text.Contains($Expected)) {
        throw $Message
    }
}

$namespace = New-Object System.Xml.XmlNamespaceManager($package.NameTable)
$namespace.AddNamespace("w", "http://wixtoolset.org/schemas/v4/wxs")
$namespace.AddNamespace("util", "http://wixtoolset.org/schemas/v4/wxs/util")

function Select-One {
    param([Parameter(Mandatory = $true)][string]$XPath)
    $nodes = @($package.SelectNodes($XPath, $namespace))
    if ($nodes.Count -ne 1) {
        throw "Expected exactly one WiX node for ${XPath}; found $($nodes.Count)."
    }
    return $nodes[0]
}

function Assert-Equal {
    param(
        [AllowNull()]$Actual,
        [AllowNull()]$Expected,
        [Parameter(Mandatory = $true)][string]$Message
    )
    if ([Convert]::ToString($Actual) -cne [Convert]::ToString($Expected)) {
        throw "${Message} Expected '$Expected', found '$Actual'."
    }
}

$product = Select-One "/w:Wix/w:Package"
Assert-Equal $product.Scope "perMachine" "The MSI must be per-machine."
Assert-Equal $product.InstallerVersion "500" "The MSI must target MSI 5.0."
Assert-Equal ($product.GetAttribute("UpgradeCode")) "B4A341EC-D4B2-419F-A00B-E8E504DE9798" `
    "Upgrade family must stay stable."
$infoUrl = Select-One "/w:Wix/w:Package/w:Property[@Id='ARPURLINFOABOUT']"
Assert-Equal $infoUrl.Value "https://github.com/isarmg/host-monitoring-client" `
    "The Apps & Features project URL must identify the current repository."

$upgrade = Select-One "/w:Wix/w:Package/w:MajorUpgrade"
Assert-Equal $upgrade.Schedule "afterInstallInitialize" "Upgrade removal must be transactional."
$service = Select-One "//w:ServiceInstall[@Name='host-monitor']"
Assert-Equal $service.DisplayName "host-monitor" "Unexpected service display name."
Assert-Equal $service.Type "ownProcess" "The Client must be an own-process service."
Assert-Equal $service.Start "demand" "Service startup requires an administrator action."
Assert-Equal $service.Account "NT AUTHORITY\LocalService" `
    "The service must run as LocalService."
Assert-Equal $service.Arguments `
    '--windows-service run --config "[CommonAppDataFolder]host-monitor\config.json"' `
    "The SCM entrypoint and fixed config path drifted."

$serviceControl = Select-One "//w:ServiceControl[@Name='host-monitor']"
Assert-Equal $serviceControl.GetAttribute("Start") "" "Installation must not start an unpaired service."
Assert-Equal $serviceControl.Stop "both" "MSI must stop the service transactionally."
Assert-Equal $serviceControl.Remove "uninstall" "MSI must unregister the service on uninstall."
Assert-Equal $serviceControl.Wait "yes" "MSI must wait for SCM operations."

$failurePolicy = Select-One "//util:ServiceConfig"
foreach ($attribute in @(
    "FirstFailureActionType",
    "SecondFailureActionType",
    "ThirdFailureActionType"
)) {
    Assert-Equal $failurePolicy.GetAttribute($attribute) "restart" `
        "All service failures must request restart."
}
Assert-Equal $failurePolicy.RestartServiceDelayInSeconds "60" `
    "Unexpected service restart delay."

$purgeProperty = Select-One "//w:Property[@Id='PURGE']"
Assert-Equal $purgeProperty.Secure "yes" "PURGE must survive the client/server MSI boundary."
$diagnosticsProperty = Select-One "//w:Property[@Id='HOST_MONITORING_MAINTENANCE_DIAGNOSTICS']"
Assert-Equal $diagnosticsProperty.Secure "yes" `
    "The maintenance diagnostics switch must survive the client/server MSI boundary."
Assert-Equal $diagnosticsProperty.GetAttribute("Value") "1" `
    "Failed setup must leave protected diagnostics by default."
$diagnosticsLaunches = @($package.SelectNodes(
    "//w:Launch[contains(@Condition, 'HOST_MONITORING_MAINTENANCE_DIAGNOSTICS')]",
    $namespace
))
if ($diagnosticsLaunches.Count -ne 1) {
    throw "Expected exactly one maintenance diagnostics value gate; found $($diagnosticsLaunches.Count)."
}
$diagnosticsLaunch = Select-One `
    '//w:Launch[@Condition=''NOT HOST_MONITORING_MAINTENANCE_DIAGNOSTICS OR HOST_MONITORING_MAINTENANCE_DIAGNOSTICS = "1"'']'
Assert-Contains $diagnosticsLaunch.Message "accepts only the value 1" `
    "The diagnostics gate must explain that only an exact value of 1 is accepted."

$testOnlyProperties = @($package.SelectNodes(
    "//w:Property[starts-with(@Id, 'HOST_MONITORING_TEST_')]",
    $namespace
))
if ($testOnlyProperties.Count -ne 0) {
    throw "The current-only MSI must not carry upgrade fault-injection properties."
}

if (@($package.SelectNodes("//w:DirectoryRef[@Id='STATEDIRECTORY']/w:Component/w:CreateFolder", $namespace)).Count -ne 0) {
    throw "MSI must not own/recreate the mutable state directory; the native helper owns its transaction."
}

$actions = @($package.SelectNodes("//w:CustomAction", $namespace))
$expectedActions = [ordered]@{
    "RollbackClientInstall" = @("rollback-install [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "rollback", "check")
    "PrepareClientInstall" = @("prepare-install [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "deferred", "check")
    "ApplyClientInstall" = @("apply-install [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "deferred", "check")
    "CommitClientInstall" = @("commit-install [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "commit", "ignore")
    "RollbackUninstallPreflight" = @("rollback-uninstall-preflight [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "rollback", "check")
    "PreflightClientUninstall" = @("preflight-uninstall [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "deferred", "check")
    "RollbackPreservedState" = @("rollback-uninstall [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "rollback", "check")
    "PreserveClientState" = @("preserve-state [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "deferred", "check")
    "CommitPreservedState" = @("commit-uninstall [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "commit", "ignore")
    "RollbackPurgedState" = @("rollback-purge [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "rollback", "check")
    "PreparePurgedState" = @("prepare-purge [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "deferred", "check")
    "CommitPurgedState" = @("commit-purge [HOST_MONITORING_MAINTENANCE_DIAGNOSTICS]", "commit", "ignore")
}
$nativeActions = @($actions | Where-Object {
    $_.GetAttribute("BinaryRef") -eq "HostMonitorMaintenance.exe"
})
if ($nativeActions.Count -ne $expectedActions.Count) {
    throw "Expected exactly $($expectedActions.Count) native lifecycle custom actions; found $($nativeActions.Count)."
}
if ($actions.Count -ne ($expectedActions.Count + 1)) {
    throw "Expected the native lifecycle actions plus the first-run setup action; found $($actions.Count)."
}

$setupAction = Select-One "//w:CustomAction[@Id='LaunchInteractiveSetup']"
Assert-Equal $setupAction.FileRef "ClientExecutable" `
    "First-run setup must execute the installed Client executable."
Assert-Equal $setupAction.ExeCommand "setup --interactive" `
    "First-run setup must use the interactive CLI contract."
Assert-Equal $setupAction.Execute "immediate" `
    "First-run setup must run after the committed MSI transaction."
Assert-Equal $setupAction.Impersonate "yes" `
    "First-run setup must run in the invoking administrator's interactive context."
Assert-Equal $setupAction.Return "ignore" `
    "Pairing failure must preserve the committed installation for setup resume."
if (-not [string]::IsNullOrEmpty($setupAction.GetAttribute("BinaryRef"))) {
    throw "First-run setup must execute the installed Client file, not an embedded helper."
}

foreach ($entry in $expectedActions.GetEnumerator()) {
    $action = Select-One "//w:CustomAction[@Id='$($entry.Key)']"
    Assert-Equal $action.ExeCommand $entry.Value[0] `
        "The command bound to custom action $($entry.Key) drifted."
    Assert-Equal $action.Execute $entry.Value[1] `
        "The execution phase for custom action $($entry.Key) drifted."
    Assert-Equal $action.Return $entry.Value[2] `
        "The return policy for custom action $($entry.Key) drifted."
    Assert-Equal $action.BinaryRef "HostMonitorMaintenance.exe" `
        "Lifecycle actions must use only the embedded native helper."
    Assert-Equal $action.Impersonate "no" `
        "Privileged lifecycle actions must run in the system install context."
    if ($action.ExeCommand -match '(?i)powershell|cmd(?:\.exe)?|schtasks(?:\.exe)?|sc(?:\.exe)?') {
        throw "Lifecycle action invokes a command shell or inbox CLI: $($action.Id)"
    }
}

if (@($actions | Where-Object {
    -not [string]::IsNullOrEmpty($_.GetAttribute("Error"))
}).Count -ne 0) {
    throw "The MSI must not carry a Type 19 upgrade fault-injection action."
}

$helperCommandMatches = [regex]::Matches(
    $helperText,
    '"(?<command>[a-z][a-z-]+)"\s*=>\s*[a-z_]+\(&paths\)'
)
$helperCommands = @($helperCommandMatches | ForEach-Object { $_.Groups["command"].Value } | Sort-Object -Unique)
$authoredCommands = @(
    $expectedActions.GetEnumerator() |
        ForEach-Object { ($_.Value[0] -split ' ', 2)[0] } |
        Sort-Object -Unique
)
$commandDifference = @(Compare-Object -ReferenceObject $authoredCommands -DifferenceObject $helperCommands)
if ($helperCommands.Count -ne $expectedActions.Count -or $commandDifference.Count -ne 0) {
    throw "MSI/helper command sets differ. Authored: $($authoredCommands -join ', '); helper: $($helperCommands -join ', ')."
}

Assert-Contains $packageText 'Condition="NOT RollbackDisabled"' `
    "Transactional current-version lifecycle changes must reject policy-disabled rollback."
$installCondition = 'NOT REMOVE~="ALL"'
$preflightCondition = 'REMOVE~="ALL"'
$preserveCondition = 'REMOVE~="ALL" AND NOT (PURGE = "1")'
$purgeCondition = 'REMOVE~="ALL" AND PURGE = "1"'
$expectedSequence = [ordered]@{
    "RollbackClientInstall" = @("Before", "PrepareClientInstall", $installCondition)
    "PrepareClientInstall" = @("Before", "StopServices", $installCondition)
    "ApplyClientInstall" = @("After", "InstallServices", $installCondition)
    "CommitClientInstall" = @("After", "ApplyClientInstall", $installCondition)
    "RollbackUninstallPreflight" = @("Before", "PreflightClientUninstall", $preflightCondition)
    "PreflightClientUninstall" = @("Before", "StopServices", $preflightCondition)
    "RollbackPreservedState" = @("After", "StopServices", $preserveCondition)
    "PreserveClientState" = @("After", "RollbackPreservedState", $preserveCondition)
    "CommitPreservedState" = @("After", "PreserveClientState", $preserveCondition)
    "RollbackPurgedState" = @("After", "StopServices", $purgeCondition)
    "PreparePurgedState" = @("After", "RollbackPurgedState", $purgeCondition)
    "CommitPurgedState" = @("After", "PreparePurgedState", $purgeCondition)
    "LaunchInteractiveSetup" = @("After", "InstallFinalize", 'NOT Installed AND NOT REMOVE~="ALL" AND UILevel >= 4')
}
$sequenceActions = @($package.SelectNodes("//w:InstallExecuteSequence/w:Custom", $namespace))
if ($sequenceActions.Count -ne $expectedSequence.Count) {
    throw "Every lifecycle action must be sequenced exactly once; found $($sequenceActions.Count) sequence rows."
}
foreach ($entry in $expectedSequence.GetEnumerator()) {
    $sequence = Select-One "//w:InstallExecuteSequence/w:Custom[@Action='$($entry.Key)']"
    $relation = $entry.Value[0]
    $opposite = if ($relation -eq "Before") { "After" } else { "Before" }
    Assert-Equal $sequence.GetAttribute($relation) $entry.Value[1] `
        "The $relation anchor for $($entry.Key) drifted."
    Assert-Equal $sequence.GetAttribute($opposite) "" `
        "The $($entry.Key) sequence row must use exactly one relative anchor."
    Assert-Equal $sequence.Condition $entry.Value[2] `
        "The execution condition for $($entry.Key) drifted."
}

if ($packageText -match '(?i)WixQuietExec|CAQuietExec') {
    throw "The MSI authoring must not use command-shell custom actions."
}
Assert-Contains $projectText 'WixToolset.Sdk/4.0.6' `
    "The WiX SDK version must be pinned for reproducible builds."
$warningsAsErrors = @($project.Project.PropertyGroup.TreatWarningsAsErrors)
if ($warningsAsErrors.Count -ne 1) {
    throw "Expected exactly one WiX TreatWarningsAsErrors setting; found $($warningsAsErrors.Count)."
}
Assert-Equal $warningsAsErrors[0] "true" `
    "All unsuppressed WiX warnings must remain build errors."
$workspaceVersionMatches = [regex]::Matches(
    $workspaceText,
    '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"\s*$'
)
if ($workspaceVersionMatches.Count -ne 1) {
    throw "Expected exactly one strict workspace package version; found $($workspaceVersionMatches.Count)."
}
$defaultProductVersions = @($project.Project.PropertyGroup.ProductVersion)
if ($defaultProductVersions.Count -ne 1) {
    throw "Expected exactly one default WiX ProductVersion; found $($defaultProductVersions.Count)."
}
Assert-Equal $defaultProductVersions[0].InnerText `
    $workspaceVersionMatches[0].Groups["version"].Value `
    "The default WiX ProductVersion must match the host-monitor workspace package version."
$expectedPayloads = [ordered]@{
    ClientExe = '$(MSBuildThisFileDirectory)..\..\..\target\x86_64-pc-windows-msvc\release\host-monitor.exe'
    MaintenanceExe = '$(MSBuildThisFileDirectory)..\..\..\target\x86_64-pc-windows-msvc\release\host-monitor-maintenance.exe'
}
foreach ($propertyName in $expectedPayloads.Keys) {
    $payloadNodes = @($project.Project.PropertyGroup.$propertyName)
    if ($payloadNodes.Count -ne 1) {
        throw "Expected exactly one WiX default payload path for $propertyName."
    }
    Assert-Equal $payloadNodes[0].InnerText $expectedPayloads[$propertyName] `
        "The WiX default payload path must resolve from the current repository layout."
}
Assert-Contains $projectText "'^\d+\.\d+\.\d+$'" `
    "The build must enforce a strict three-field MSI version."
Assert-Contains $projectText '.Major) &gt; 255' `
    "The MSI major version field range must be checked."
Assert-Contains $projectText '.Minor) &gt; 255' `
    "The MSI minor version field range must be checked."
Assert-Contains $projectText '.Build) &gt; 65535' `
    "The MSI build version field range must be checked."
Assert-Contains $projectText 'ConsoleToMSBuild="true"' `
    "The WiX project must execute ClientExe to bind the MSI version to its binary."
Assert-Contains $projectText "'`$(DetectedClientVersion)' != 'host-monitor `$(ProductVersion)'" `
    "Direct MSBuild callers must fail when ProductVersion differs from ClientExe --version."
if ($buildText -match '(?i)powershell(?:\.exe)?|pwsh(?:\.exe)?') {
    throw "The MSI build entrypoint must not require PowerShell."
}
Assert-Contains $buildText '"%CLIENT_EXE%" --version' `
    "The command-line MSI build entrypoint must read the Client binary version."
Assert-Contains $buildText 'host-monitor %PRODUCT_VERSION%' `
    "The command-line MSI build entrypoint must reject a binary/version mismatch."
if ($buildText.Contains('1.2.3')) {
    throw "The current-only MSI build documentation must not advertise an arbitrary version."
}

if ($packageText -match 'TrayExe|ClientTrayComponent|WixUnelevatedShellExec|LaunchClientTray|<Shortcut ') {
    throw "Removed UI payload or interactive entry remains in MSI."
}
$legacyRun = Select-One "//w:RemoveRegistryValue[@Id='RemoveLegacyTrayRun']"
Assert-Equal $legacyRun.Name "host-monitor-tray" "Legacy cleanup must name only this product's Run entry."
Assert-Equal $legacyRun.Root "HKLM" "Legacy Run cleanup must use the installed hive."
$legacyFile = Select-One "//w:RemoveFile[@Id='RemoveLegacyTrayFile']"
Assert-Equal $legacyFile.Name "host-monitor-tray.exe" "Legacy cleanup must not use wildcard paths."
Assert-Equal $legacyFile.On "install" "Legacy UI cleanup belongs to installation."

Write-Host "WiX MSI authoring passed current-only lifecycle, CLI, service, rollback, and purge checks."
