#!/usr/bin/env python3
"""Reject release executables that cannot prove their exact source revision."""
import json
import os
from pathlib import Path
import platform
import subprocess


ROOT = Path(__file__).resolve().parents[1]
if platform.system() == "Windows":
    binary = ROOT / "target/x86_64-pc-windows-msvc/release/host-monitor.exe"
else:
    binary = ROOT / "target/release/host-monitor"

expected = os.environ.get("HOST_MONITOR_BUILD_SHA")
if not expected or len(expected) != 40:
    raise SystemExit("HOST_MONITOR_BUILD_SHA is missing or invalid")
result = subprocess.run(
    [str(binary), "version", "--format", "json"],
    check=True,
    capture_output=True,
    text=True,
)
identity = json.loads(result.stdout)
if identity.get("commit") != expected or identity.get("commit") == "unknown":
    raise SystemExit(f"binary source identity mismatch: {identity.get('commit')!r}")
print(f"verified executable source identity: {expected}")
