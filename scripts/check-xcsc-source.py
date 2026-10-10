#!/usr/bin/env python3
"""Delegate client boundary verification to the exact locked official xcsc."""
from pathlib import Path
import json
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--locked", "--all-features", "--format-version", "1",
], cwd=ROOT))
packages = [package for package in metadata["packages"] if package["name"] == "xcsc"]
if len(packages) != 1:
    raise SystemExit("expected exactly one locked xcsc support package")
package = packages[0]
source = package.get("source") or ""
match = re.fullmatch(r"git\+https://github\.com/isarmg/xcsc\.git\?rev=([0-9a-f]{40})#([0-9a-f]{40})", source)
if match is None or match[1] != match[2]:
    raise SystemExit("xcsc must come from one immutable official Git source")
client_source = Path(package["manifest_path"]).resolve().parent
subprocess.run([
    sys.executable, str(client_source / "scripts/check-xcsc.py"),
    "--product-root", str(ROOT),
], cwd=ROOT, check=True)
