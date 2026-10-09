$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$result = @{ memory = @(); pnp = @(); monitors = @(); sound = @(); errors = @{} }
function Read-Inventory($key, [scriptblock]$query) {
    try { $result[$key] = @(& $query) }
    catch {
        $kind = 'transient'
        if ($_.Exception.HResult -eq -2147024891 -or $_.Exception.HResult -eq -2147217405) { $kind = 'permission_denied' }
        $result.errors[$key] = $kind
    }
}
Read-Inventory 'memory' {
    Get-CimInstance Win32_PhysicalMemory | Select-Object -First 64 DeviceLocator, BankLabel, Tag, PartNumber, Manufacturer, Version, Capacity, SMBIOSMemoryType, FormFactor, Speed, ConfiguredClockSpeed
}
Read-Inventory 'pnp' {
    Get-CimInstance Win32_PnPEntity -Filter "Present = TRUE AND (PNPClass = 'USB' OR PNPClass = 'Bluetooth' OR PNPClass = 'MEDIA' OR PNPClass = 'AudioEndpoint' OR PNPClass = 'Monitor' OR Name LIKE '%Thunderbolt%' OR Name LIKE '%USB4%')" |
        Select-Object -First 1024 PNPDeviceID, Name, Manufacturer, PNPClass, HardwareID, Service
}
Read-Inventory 'monitors' {
    Get-CimInstance -Namespace root/wmi WmiMonitorID -Filter 'Active = TRUE' |
        Select-Object -First 64 InstanceName, ManufacturerName, ProductCodeID, UserFriendlyName
}
Read-Inventory 'sound' {
    Get-CimInstance Win32_SoundDevice | Select-Object -First 256 PNPDeviceID, Name, Manufacturer
}
$result | ConvertTo-Json -Depth 6 -Compress
