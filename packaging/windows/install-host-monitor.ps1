#Requires -RunAsAdministrator
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$Msi)
$ErrorActionPreference = 'Stop'
$Msi = (Resolve-Path -LiteralPath $Msi).Path
$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.OpenDatabase($Msi, 0)
function Read-MsiProperty([string]$Name) {
    $view = $db.OpenView("SELECT ``Value`` FROM ``Property`` WHERE ``Property``='$Name'")
    $view.Execute(); $row = $view.Fetch(); $value = $row.StringData(1); $view.Close(); return $value
}
if ((Read-MsiProperty 'ProductName') -ne 'host-monitor' -or (Read-MsiProperty 'Manufacturer') -ne 'Host Monitoring') { throw 'Expected the official host-monitor MSI.' }
$version = Read-MsiProperty 'ProductVersion'
$product = Read-MsiProperty 'ProductCode'
$logRoot = Join-Path $env:TEMP ('host-monitor-install-' + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $logRoot
function Invoke-Installer([string]$Arguments,[string]$Name) {
    $log = Join-Path $logRoot ($Name + '.log')
    for ($attempt = 0; $attempt -lt 12; $attempt++) {
        $p = Start-Process msiexec.exe -ArgumentList ($Arguments + ' /qn /norestart /l*v "' + $log + '"') -Wait -PassThru
        if ($p.ExitCode -ne 1618) { break }
        Start-Sleep -Seconds 5
    }
    if ($p.ExitCode -notin @(0,3010)) { throw "Windows Installer error $($p.ExitCode). Diagnostic log: $log" }
    if ($p.ExitCode -eq 3010) { Write-Warning 'Windows requires a restart to finish replacing files.' }
}
# Releases before 0.9.7 had no UpgradeCode. Remove their exact MSI registration
# through Windows Installer; its normal uninstall retains identity/configuration.
$entries = @(Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*' | Where-Object { $_.DisplayName -eq 'host-monitor' -and $_.Publisher -eq 'Host Monitoring' -and $_.WindowsInstaller -eq 1 })
foreach ($entry in $entries) {
    if ($entry.PSChildName -notmatch '^\{[0-9A-Fa-f-]{36}\}$') { throw 'Invalid legacy MSI product registration.' }
    if ([version]$entry.DisplayVersion -gt [version]$version) { throw 'A newer version is installed.' }
}
foreach ($entry in $entries) {
    if ($entry.PSChildName -ne $product -and [version]$entry.DisplayVersion -lt [version]'0.9.7') {
        Invoke-Installer ('/x ' + $entry.PSChildName) ('remove-' + $entry.DisplayVersion)
    }
}
$repair = if ($entries.PSChildName -contains $product) { ' REINSTALL=ALL REINSTALLMODE=amus' } else { '' }
Invoke-Installer ('/i "' + $Msi + '"' + $repair) 'install'
& (Join-Path $env:ProgramFiles 'host-monitor\host-monitor.exe') --version
if ($LASTEXITCODE -ne 0) { throw "Installed executable verification failed. Logs: $logRoot" }
Write-Host "Installation/repair complete. Configuration and queue retained. Logs: $logRoot"
