@echo off
setlocal EnableExtensions DisableDelayedExpansion

if "%~4"=="" goto :usage
if not "%~5"=="" goto :usage

set "PRODUCT_VERSION=%~1"
set "CLIENT_EXE=%~f2"
set "MAINTENANCE_EXE=%~f3"
set "TRAY_EXE=%~f4"
set "SCRIPT_ROOT=%~dp0"

if not exist "%CLIENT_EXE%" (
  echo Client executable not found: "%CLIENT_EXE%" 1>&2
  exit /b 2
)
if not exist "%MAINTENANCE_EXE%" (
  echo Maintenance executable not found: "%MAINTENANCE_EXE%" 1>&2
  exit /b 2
)
if not exist "%TRAY_EXE%" (
  echo Tray executable not found: "%TRAY_EXE%" 1>&2
  exit /b 2
)

set "DETECTED_CLIENT_VERSION="
for /f "delims=" %%V in ('"%CLIENT_EXE%" --version') do set "DETECTED_CLIENT_VERSION=%%V"
if not "%DETECTED_CLIENT_VERSION%"=="host-monitor %PRODUCT_VERSION%" (
  echo Product version %PRODUCT_VERSION% does not match Client executable version "%DETECTED_CLIENT_VERSION%". 1>&2
  exit /b 2
)

rem The project repeats the exact binary/version check so direct MSBuild callers cannot bypass it.
dotnet build "%SCRIPT_ROOT%HostMonitor.Installer.wixproj" ^
  --configuration Release ^
  --nologo ^
  -p:ProductVersion="%PRODUCT_VERSION%" ^
  -p:ClientExe="%CLIENT_EXE%" ^
  -p:MaintenanceExe="%MAINTENANCE_EXE%" ^
  -p:TrayExe="%TRAY_EXE%"
if errorlevel 1 exit /b %errorlevel%

echo MSI created below "%SCRIPT_ROOT%bin\x64\Release".
exit /b 0

:usage
echo Usage: build-msi.cmd VERSION CLIENT_EXE MAINTENANCE_EXE TRAY_EXE 1>&2
echo Example from the repository root: packaging\windows\wix\build-msi.cmd 0.9.3 target\x86_64-pc-windows-msvc\release\host-monitor.exe target\x86_64-pc-windows-msvc\release\host-monitor-maintenance.exe target\x86_64-pc-windows-msvc\release\host-monitor-tray.exe 1>&2
exit /b 2
