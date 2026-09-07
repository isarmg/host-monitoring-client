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

command -v plutil >/dev/null 2>&1 || {
  echo "validate-packaging.sh requires macOS plutil" >&2
  exit 1
}
plutil -lint "$packaging_dir/org.sarmg.hostmonitor.plist"
plutil -lint "$packaging_dir/org.sarmg.hostmonitor.logrotate.plist"
sh "$script_dir/account-safety-test.sh"
sh "$script_dir/postinstall-failure-test.sh"
sh "$script_dir/uninstall-proof-test.sh"
