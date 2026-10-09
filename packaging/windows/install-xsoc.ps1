#Requires -RunAsAdministrator
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$Msi,
    [switch]$Quiet
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Msi = (Resolve-Path -LiteralPath $Msi).Path
$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.OpenDatabase($Msi, 0)
function Read-MsiProperty([string]$Name) {
    $view = $db.OpenView("SELECT ``Value`` FROM ``Property`` WHERE ``Property``='$Name'")
    [void]$view.Execute(); $row = $view.Fetch(); $value = $row.StringData(1); [void]$view.Close(); return $value
}
if ((Read-MsiProperty 'ProductName') -ne 'xsoc' -or (Read-MsiProperty 'Manufacturer') -ne 'xsos') { throw 'Expected the official xsoc MSI.' }
$version = Read-MsiProperty 'ProductVersion'
$product = Read-MsiProperty 'ProductCode'
$logRoot = Join-Path $env:TEMP ('xsoc-install-' + [guid]::NewGuid())
$null = New-Item -ItemType Directory -Path $logRoot
function Invoke-Installer([string]$Arguments,[string]$Name,[bool]$UseFullUi = $false) {
    $log = Join-Path $logRoot ($Name + '.log')
    $ui = if ($UseFullUi) { '' } else { ' /qn' }
    for ($attempt = 0; $attempt -lt 12; $attempt++) {
        $p = Start-Process msiexec.exe -ArgumentList ($Arguments + $ui + ' /norestart /l*v "' + $log + '"') -Wait -PassThru
        if ($p.ExitCode -ne 1618) { break }
        Start-Sleep -Seconds 5
    }
    if ($p.ExitCode -notin @(0,3010)) { throw "Windows Installer error $($p.ExitCode). Diagnostic log: $log" }
    if ($p.ExitCode -eq 3010) { Write-Warning 'Windows requires a restart to finish replacing files.' }
}
# The rebuilt release line starts at 1.0.0. Earlier registrations require an
# explicit export/uninstall/reinstall rather than an implicit state migration.
$entries = @(Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*' | Where-Object {
    $properties = $_.PSObject.Properties.Name
    $properties -contains 'DisplayName' -and $properties -contains 'Publisher' -and
    $properties -contains 'WindowsInstaller' -and $properties -contains 'DisplayVersion' -and
    $_.DisplayName -eq 'xsoc' -and $_.Publisher -eq 'xsos' -and $_.WindowsInstaller -eq 1
})
foreach ($entry in $entries) {
    if ($entry.PSChildName -notmatch '^\{[0-9A-Fa-f-]{36}\}$') { throw 'Invalid legacy MSI product registration.' }
    if ([version]$entry.DisplayVersion -lt [version]'1.0.0') { throw 'Export and uninstall the pre-1.0 installation before installing the new baseline.' }
    if ([version]$entry.DisplayVersion -gt [version]$version) { throw 'A newer version is installed.' }
}
$repair = if (@($entries | ForEach-Object { $_.PSChildName }) -contains $product) { ' REINSTALL=ALL REINSTALLMODE=amus' } else { '' }
Invoke-Installer ('/i "' + $Msi + '"' + $repair) 'install' (-not $Quiet)
$installLocation = (Get-ItemProperty -LiteralPath 'HKLM:\Software\xsos\xsoc' -Name InstallLocation).InstallLocation
if ([string]::IsNullOrWhiteSpace($installLocation)) { throw "Installer did not record its selected installation directory. Logs: $logRoot" }
& (Join-Path $installLocation 'xsoc.exe') --version
if ($LASTEXITCODE -ne 0) { throw "Installed executable verification failed. Logs: $logRoot" }
Write-Host "Installation/repair completed successfully. Existing Client state was retained. Logs: $logRoot"
