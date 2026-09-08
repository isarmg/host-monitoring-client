#!/usr/bin/env python3
"""Check current-only version binding and collect native CI installers."""
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def main():
    os.chdir(ROOT)
    version = tomllib.loads(Path("Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    config = json.loads(Path("config/host-monitor.json.example").read_text(encoding="utf-8"))
    if config["application_version"] != "0.9.4":
        raise ValueError("configuration does not match the frozen 0.9.4 format")
    if os.environ.get("GITHUB_REF_TYPE") == "tag" and os.environ.get("GITHUB_REF_NAME") != f"v{version}":
        raise ValueError("release tag does not match Cargo")
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    if os.environ.get("GITHUB_SHA", commit) != commit:
        raise ValueError("checkout does not match CI source commit")
    subprocess.run(["git", "diff", "--exit-code", "HEAD"], check=True)
    if sys.argv[1:] == ["--check"]:
        print(f"release inputs verified: {version} {commit}")
        return
    if sys.argv[1:]:
        raise ValueError("usage: collect-release.py [--check]")
    system = platform.system()
    if system == "Windows":
        assets = list(Path("packaging/windows/wix/bin/x64/Release").rglob(f"host-monitor-{version}-x64.msi"))
        expected = 1
    elif system == "Linux":
        assets = [Path(f"dist/host-monitor_{version}_amd64.deb"), Path(f"dist/host-monitor-{version}.x86_64.rpm")]
        expected = 2
    elif system == "Darwin":
        assets = [Path(f"dist/host-monitor-{version}-macos-{platform.machine()}-unsigned.pkg")]
        expected = 1
    else:
        raise ValueError(f"unsupported platform: {system}")
    if len(assets) != expected or any(not p.is_file() or p.stat().st_size < 1024 for p in assets):
        raise ValueError(f"missing or invalid native installers: {assets}")
    output = Path("release-dist")
    output.mkdir(exist_ok=False)
    files = {}
    for asset in assets:
        shutil.copy2(asset, output / asset.name)
        files[asset.name] = hashlib.sha256(asset.read_bytes()).hexdigest()
    manifest = {"version": version, "commit": commit, "os": system, "arch": platform.machine(), "sha256": files,
                "signing": "unsigned (no Authenticode/Developer ID/notarization)",
                "ci_run": os.environ.get("GITHUB_RUN_ID")}
    (output / f"host-monitor-{version}-{system}-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")
    (output / f"SHA256SUMS-{system}").write_text("".join(f"{digest}  {name}\n" for name, digest in sorted(files.items())), encoding="utf-8", newline="\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
