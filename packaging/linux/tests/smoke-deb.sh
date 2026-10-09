#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} != --allow-system-changes ]]; then
  echo "usage: $0 --allow-system-changes [PACKAGE.deb]" >&2
  echo "This test installs and purges xsoc on the current system." >&2
  exit 2
fi
shift

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(cd -- "$script_dir/../../.." && pwd)
package=${1:-}
if [[ -z $package ]]; then
  packages=()
  while IFS= read -r -d '' candidate; do
    packages+=("$candidate")
  done < <(find "$repository_root/dist" -maxdepth 1 -type f \
    -name 'xsoc_*_amd64.deb' -print0)
  if (( ${#packages[@]} != 1 )); then
    echo "error: expected exactly one xsoc amd64 DEB in $repository_root/dist, found ${#packages[@]}" >&2
    if (( ${#packages[@]} > 0 )); then
      printf '  %s\n' "${packages[@]}" >&2
    fi
    exit 1
  fi
  package=${packages[0]}
fi
[[ -n $package && -f $package ]]

sudo dpkg -i "$package"
systemctl enable --now xsoc.service
systemctl is-enabled --quiet xsoc.service
systemctl is-active --quiet xsoc.service
sudo touch /var/lib/xsoc/release-lifecycle-marker

sudo dpkg --remove xsoc
[[ ! -e /usr/bin/xsoc ]]
sudo test -e /var/lib/xsoc/release-lifecycle-marker
sudo test -e /etc/xsoc/config.json

sudo dpkg -i "$package"
systemctl is-active --quiet xsoc.service
sudo dpkg --purge xsoc
sudo test ! -e /var/lib/xsoc
sudo test ! -e /etc/xsoc
! getent passwd xsoc
! getent group xsoc
