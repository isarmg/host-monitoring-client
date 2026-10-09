#!/usr/bin/env python3
"""Compare Rust bindings with locally supplied official SDK headers (no downloads).

Usage: python3 scripts/verify-windows-gpu-abi.py /path/to/headers
Requires a 64-bit host, c++, rustc; see docs/windows-gpu-vendors.md for revisions.
"""
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile

headers = Path(sys.argv[1]).resolve()
bindings = Path(__file__).resolve().parents[1] / "src/collectors/windows_vendor"
# C type -> (Rust type, C field -> Rust field). Includes every telemetry group boundary.
intel = {
    "ctl_init_args_t": ("InitArgs", {"AppVersion": "app_version", "ApplicationUID": "application_uid"}),
    "ctl_device_adapter_properties_t": ("DeviceProperties", {
        "pDeviceID": "device_id", "device_type": "device_type", "driver_version": "driver_version",
        "pci_vendor_id": "pci_vendor_id", "name": "name", "num_xe_cores": "num_xe_cores", "reserved": "reserved"}),
    "ctl_oc_telemetry_item_t": ("Item", {"bSupported": "supported", "units": "units", "type": "data_type", "value": "value"}),
    "ctl_psu_info_t": ("Psu", {"energyCounter": "energy", "voltage": "voltage"}),
    "ctl_power_telemetry_t": ("PowerTelemetry", {
        "timeStamp": "timestamp", "gpuEnergyCounter": "gpu_energy", "gpuVoltage": "gpu_voltage",
        "gpuCurrentClockFrequency": "gpu_clock", "gpuCurrentTemperature": "gpu_temperature",
        "globalActivityCounter": "global_activity", "gpuPowerLimited": "gpu_limited",
        "vramEnergyCounter": "vram_energy", "vramVoltage": "vram_voltage",
        "vramCurrentClockFrequency": "vram_clock", "vramCurrentTemperature": "vram_temperature",
        "vramPowerLimited": "vram_limited", "totalCardEnergyCounter": "board_energy",
        "psu": "psu", "fanSpeed": "fans", "gpuVrTemp": "gpu_vr_temperature",
        "gpuEffectiveClock": "effective_clock", "vramWriteBandwidth": "vram_write_bandwidth"}),
}
amd = {
    "ISystem.h": {
        "IADLXSystemVtbl": ("System", {"GetGPUs": "get_gpus", "GetPerformanceMonitoringServices": "get_performance"}),
        "IADLXGPUListVtbl": ("GpuList", {"Size": "size", "Begin": "begin", "At_GPUList": "at_gpu"}),
        "IADLXGPUVtbl": ("Gpu", {"Name": "name", "TotalVRAM": "total_vram", "UniqueId": "unique_id"}),
    },
    "ISystem2.h": {"IADLXGPU2Vtbl": ("Gpu2", {"LUID": "luid"})},
    "IPerformanceMonitoring.h": {
        "IADLXPerformanceMonitoringServicesVtbl": ("Performance", {
            "GetCurrentGPUMetrics": "current_gpu_metrics", "GetSupportedGPUMetrics": "supported_gpu_metrics"}),
        "IADLXGPUMetricsVtbl": ("Metrics", {
            "GPUUsage": "usage", "GPUClockSpeed": "clock", "GPUVRAMClockSpeed": "memory_clock",
            "GPUTemperature": "temperature", "GPUHotspotTemperature": "hotspot", "GPUPower": "power",
            "GPUTotalBoardPower": "board_power", "GPUFanSpeed": "fan", "GPUVRAM": "vram", "GPUVoltage": "voltage"}),
        "IADLXGPUMetricsSupportVtbl": ("Support", {"IsSupportedGPUUsage": "supported"}),
    },
}
cpp = ['#include "igcl_api.h"', '#include <cstdio>', 'int main() { static_assert(sizeof(void*) == 8);']
rust = [f'#[path={json.dumps(str(bindings / (name + "_abi.rs")))}] mod {name};' for name in ("igcl", "adlx")]
rust += ['fn main() { assert_eq!(std::mem::size_of::<usize>(), 8);']
for c_type, (r_type, fields) in intel.items():
    cpp.append(f'printf("{c_type} %zu\\n", sizeof({c_type}));')
    rust.append(f'println!("{c_type} {{}}", std::mem::size_of::<igcl::{r_type}>());')
    for c_field, r_field in fields.items():
        key = f"{c_type}.{c_field}"
        cpp.append(f'printf("{key} %zu\\n", offsetof({c_type}, {c_field}));')
        rust.append(f'println!("{key} {{}}", std::mem::offset_of!(igcl::{r_type}, {r_field}));')
cpp.append('}')
expected_amd = {}
for filename, types in amd.items():
    source = (headers / filename).read_text()
    for c_type, (r_type, fields) in types.items():
        body = re.search(r"typedef struct " + c_type + r"\s*\{(.*?)\}", source, re.S).group(1)
        slots = re.findall(r"ADLX_STD_CALL\s*\*\s*(\w+)\s*\)", body)
        for c_field, r_field in fields.items():
            key = f"{c_type}.{c_field}"
            expected_amd[key] = str(slots.index(c_field) * 8)
            rust.append(f'println!("{key} {{}}", std::mem::offset_of!(adlx::{r_type}, {r_field}));')
rust.append('}')
with tempfile.TemporaryDirectory(prefix="host-gpu-abi-") as directory:
    work = Path(directory)
    (work / "check.cpp").write_text("\n".join(cpp))
    (work / "check.rs").write_text("\n".join(rust))
    subprocess.run(["c++", "-I", str(headers), str(work / "check.cpp"), "-o", str(work / "cpp")], check=True)
    subprocess.run(["rustc", "--edition=2024", "-A", "dead_code", str(work / "check.rs"), "-o", str(work / "rust")], check=True)
    def values(executable):
        return dict(line.split() for line in subprocess.check_output([str(executable)], text=True).splitlines())
    expected = values(work / "cpp") | expected_amd
    actual = values(work / "rust")
    mismatches = {key: (value, actual.get(key)) for key, value in expected.items() if actual.get(key) != value}
    if mismatches:
        raise SystemExit(f"ABI mismatch (SDK, Rust): {mismatches}")
    print(f"Verified {len(expected)} SDK sizes, field offsets and ADLX vtable slots against Rust.")
