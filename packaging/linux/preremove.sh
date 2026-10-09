#!/bin/sh
set -eu

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

service_name=xsoc.service
package_version=1.0.0

systemd_is_running() {
  [ -d /run/systemd/system ]
}

die() {
  echo "xsoc preremove: $*" >&2
  exit 1
}

stop_for_upgrade() {
  if systemd_is_running; then
    command -v systemctl >/dev/null 2>&1 || die "systemd is running but systemctl is unavailable"
    systemctl stop "$service_name"
  fi
}

disable_for_remove() {
  if systemd_is_running; then
    command -v systemctl >/dev/null 2>&1 || {
      echo "xsoc preremove: systemd is running but systemctl is unavailable" >&2
      exit 1
    }
    systemctl disable --now "$service_name"
  fi
}

is_package_version() {
  case "$1" in
    ''|*[!0-9.]*|.*|*.|*..*|*.*.*.*) return 1 ;;
    *.*.*) return 0 ;;
    *) return 1 ;;
  esac
}

# Debian retries with the new prerm when an older prerm rejects an upgrade.
# Both upgrade ABIs stop the process without disabling administrator startup
# policy. The new postinstall independently validates state and account identity.
# RPM replacement uses a positive remaining-instance count after postinstall.
case "${1:-}" in
  upgrade)
    [ "$#" -eq 2 ] && is_package_version "$2" || die "invalid upgrade version"
    stop_for_upgrade
    ;;
  failed-upgrade)
    # dpkg before 1.18.5 supplied only old-version; newer dpkg supplies both.
    { [ "$#" -eq 2 ] || [ "$#" -eq 3 ]; } && is_package_version "$2" ||
      die "invalid failed-upgrade arguments"
    if [ "$#" -eq 3 ]; then
      [ "$3" = "$package_version" ] || die "failed-upgrade target does not match this package"
    fi
    stop_for_upgrade
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
