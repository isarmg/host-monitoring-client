#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
packaging_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)

for script in \
  "$packaging_dir/build-pkg.sh" \
  "$packaging_dir/scripts/preinstall" \
  "$packaging_dir/scripts/postinstall" \
  "$packaging_dir/uninstall.sh" \
  "$packaging_dir/host-monitor-logrotate" \
  "$script_dir/account-safety-test.sh" \
  "$script_dir/postinstall-failure-test.sh" \
  "$script_dir/uninstall-proof-test.sh"
do
  sh -n "$script"
done

if grep -Eq '\$client_command"[[:space:]]+setup|setup[[:space:]]+--interactive' \
  "$packaging_dir/scripts/postinstall"
then
  echo "postinstall must not run interactive setup inside the PKG transaction" >&2
  exit 1
fi

command -v plutil >/dev/null 2>&1 || {
  echo "validate-packaging.sh requires macOS plutil" >&2
  exit 1
}
plutil -lint "$packaging_dir/org.sarmg.hostmonitor.plist"
plutil -lint "$packaging_dir/org.sarmg.hostmonitor.logrotate.plist"
python3 - "$packaging_dir/Distribution.xml" <<'PY'
import sys
import xml.etree.ElementTree as ET

document = ET.parse(sys.argv[1]).getroot()
options = document.find("options")
declared = "" if options is None else options.get("hostArchitectures", "")
architectures = {item.strip() for item in declared.split(",") if item.strip()}
if architectures != {"arm64"}:
    raise SystemExit(
        f"Distribution hostArchitectures must be exactly arm64; found {declared!r}"
    )
PY
sh "$script_dir/account-safety-test.sh"
sh "$script_dir/postinstall-failure-test.sh"
sh "$script_dir/uninstall-proof-test.sh"
