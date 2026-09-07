#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} != --allow-system-changes ]]; then
  echo "usage: $0 --allow-system-changes [PACKAGE.deb]" >&2
  echo "This test installs and purges host-monitor on the current system." >&2
  exit 2
fi
shift

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "$script_dir/../../../.." && pwd)
package=${1:-}
if [[ -z $package ]]; then
  packages=()
  while IFS= read -r -d '' candidate; do
    packages+=("$candidate")
  done < <(find "$repository_root/dist" -maxdepth 1 -type f \
    -name 'host-monitor_*_amd64.deb' -print0)
  if (( ${#packages[@]} != 1 )); then
    echo "error: expected exactly one host-monitor amd64 DEB in $repository_root/dist, found ${#packages[@]}" >&2
    if (( ${#packages[@]} > 0 )); then
      printf '  %s\n' "${packages[@]}" >&2
    fi
    exit 1
  fi
  package=${packages[0]}
fi
[[ -n $package && -f $package ]]

sudo dpkg -i "$package"
systemctl is-enabled --quiet host-monitor.service
systemctl is-active --quiet host-monitor.service
sudo touch /var/lib/host-monitor/release-lifecycle-marker

sudo dpkg --remove host-monitor
[[ ! -e /usr/bin/host-monitor ]]
sudo test -e /var/lib/host-monitor/release-lifecycle-marker
sudo test -e /etc/host-monitor/config.json

sudo dpkg -i "$package"
systemctl is-active --quiet host-monitor.service
sudo dpkg --purge host-monitor
sudo test ! -e /var/lib/host-monitor
sudo test ! -e /etc/host-monitor
! getent passwd host-monitor
! getent group host-monitor
