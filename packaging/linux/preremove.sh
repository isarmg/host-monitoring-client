#!/bin/sh
set -eu

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

service_name=host-monitor.service
package_version=0.9.15

systemd_is_running() {
  [ -d /run/systemd/system ]
}

die() {
  echo "host-monitor preremove: $*" >&2
  exit 1
}

stop_for_current_reinstall() {
  if systemd_is_running; then
    command -v systemctl >/dev/null 2>&1 || die "systemd is running but systemctl is unavailable"
    systemctl stop "$service_name"
  fi
}

disable_for_remove() {
  if systemd_is_running; then
    command -v systemctl >/dev/null 2>&1 || {
      echo "host-monitor preremove: systemd is running but systemctl is unavailable" >&2
      exit 1
    }
    systemctl disable --now "$service_name"
  fi
}

# Debian uses the literal `upgrade <new-version>` ABI even when reinstalling
# the exact same package. Accept only the current package version. RPM uses a positive remaining
# instance count for replacement; the new postinstall has already validated
# the exact current ownership markers before the pre-remove scriptlet can run.
case "${1:-}" in
  upgrade)
    [ "$#" -eq 2 ] && [ "$2" = "$package_version" ] ||
      die "cross-version replacement is unsupported; purge before installing another version"
    stop_for_current_reinstall
    ;;
  *[!0-9]*|'')
    disable_for_remove
    ;;
  *[1-9]*)
    :
    ;;
  *)
    disable_for_remove
    ;;
esac
