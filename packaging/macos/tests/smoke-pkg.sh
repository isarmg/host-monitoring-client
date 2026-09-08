#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} != --allow-system-changes ]]; then
  echo "usage: $0 --allow-system-changes [PACKAGE.pkg]" >&2
  echo "This test installs and purges host-monitor on the current system." >&2
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
    -name 'host-monitor-*.pkg' -print0)
  if (( ${#packages[@]} != 1 )); then
    echo "expected exactly one host-monitor pkg in $repository_root/dist; found ${#packages[@]}" >&2
    printf '  %s\n' "${packages[@]}" >&2
    exit 1
  fi
  package=${packages[0]}
fi
[[ -n $package && -f $package ]]

assert_no_extended_acl() {
  local path=$1
  path_has_no_extended_acl "$path" || {
    echo "unexpected extended ACL on $path" >&2
    exit 1
  }
}

path_has_no_extended_acl() {
  local path=$1
  local listing first_line permissions line_count
  listing=$(sudo env LC_ALL=C ls -lde "$path") || return 1
  first_line=${listing%%$'\n'*}
  permissions=${first_line%% *}
  [[ -n $permissions && $permissions != *+ ]] || return 1
  line_count=$(printf '%s\n' "$listing" | wc -l | tr -d '[:space:]')
  [[ $line_count == 1 ]]
}

path_has_no_permissive_acl() {
  local path=$1
  local listing first_line permissions line acl_entries=0
  listing=$(sudo env LC_ALL=C ls -lde "$path") || return 1
  first_line=${listing%%$'\n'*}
  [[ -n $first_line ]] || return 1
  while IFS= read -r line; do
    [[ $line == "$first_line" ]] && continue
    [[ $line != *" allow "* ]] || return 1
    [[ $line =~ ^[[:space:]]*[0-9]+:.*[[:space:]]deny[[:space:]] ]] || return 1
    (( acl_entries += 1 ))
  done <<<"$listing"
  permissions=${first_line%% *}
  if [[ $permissions == *+ ]]; then
    (( acl_entries > 0 ))
  else
    return 0
  fi
}

shared_directory_is_secure() {
  local path=$1
  local metadata uid gid special mode remainder
  sudo test -d "$path" && sudo test ! -L "$path" || return 1
  metadata=$(sudo stat -f '%u:%g:%Mp:%Lp' "$path") || return 1
  uid=${metadata%%:*}
  remainder=${metadata#*:}
  gid=${remainder%%:*}
  remainder=${remainder#*:}
  special=${remainder%%:*}
  mode=${remainder#*:}
  [[ $uid == 0 && $gid == 0 && $special == 0 && $mode =~ ^7[0145][15]$ ]] || return 1
  path_has_no_extended_acl "$path"
}

assert_secure_shared_directory() {
  local path=$1
  shared_directory_is_secure "$path" || {
    echo "unsafe shared directory metadata or ACL on $path" >&2
    exit 1
  }
}

assert_secure_root_directory_any_group() {
  local path=$1
  local metadata uid special mode remainder
  sudo test -d "$path" && sudo test ! -L "$path" || {
    echo "expected a real root-owned directory at $path" >&2
    exit 1
  }
  metadata=$(sudo stat -f '%u:%g:%Mp:%Lp' "$path")
  uid=${metadata%%:*}
  remainder=${metadata#*:}
  remainder=${remainder#*:}
  special=${remainder%%:*}
  mode=${remainder#*:}
  [[ $uid == 0 && $special == 0 && $mode =~ ^7[0145][15]$ ]] || {
    echo "unsafe root-owned directory metadata on $path: $metadata" >&2
    exit 1
  }
  path_has_no_permissive_acl "$path" || {
    echo "unexpected permissive ACL on root-owned directory $path" >&2
    exit 1
  }
}

assert_path_metadata() {
  local expected=$1
  local path=$2
  local actual
  actual=$(sudo stat -f '%u:%g:%Mp:%Lp' "$path")
  [[ $actual == "$expected" ]] || {
    echo "unexpected metadata on $path: expected $expected, got $actual" >&2
    exit 1
  }
}

assert_trusted_path() {
  local expected=$1
  local path=$2
  assert_path_metadata "$expected" "$path"
  assert_no_extended_acl "$path"
}

assert_trusted_directory() {
  local expected=$1
  local path=$2
  sudo test -d "$path" && sudo test ! -L "$path" || {
    echo "expected a real directory at $path" >&2
    exit 1
  }
  assert_trusted_path "$expected" "$path"
}

assert_trusted_regular_file() {
  local expected=$1
  local path=$2
  sudo test -f "$path" && sudo test ! -L "$path" || {
    echo "expected a real regular file at $path" >&2
    exit 1
  }
  assert_trusted_path "$expected" "$path"
}

assert_system_root_directory() {
  local expected=$1
  local path=$2
  sudo test -d "$path" && sudo test ! -L "$path" || {
    echo "expected a real system directory at $path" >&2
    exit 1
  }
  assert_path_metadata "$expected" "$path"
  path_has_no_permissive_acl "$path" || {
    echo "unexpected permissive ACL on system directory $path" >&2
    exit 1
  }
}

assert_command_link() {
  [[ -L /usr/local/bin/host-monitor ]]
  [[ $(readlink /usr/local/bin/host-monitor) == ../libexec/host-monitor ]]
  assert_trusted_path 0:0:0:755 /usr/local/bin/host-monitor
}

assert_install_trust() {
  local directory
  assert_system_root_directory 0:0:0:755 /usr
  assert_system_root_directory 0:0:0:755 /Library
  for directory in \
    /usr/local \
    /usr/local/libexec \
    /usr/local/bin \
    /usr/local/share
  do
    assert_secure_shared_directory "$directory"
  done
  assert_trusted_directory 0:0:0:755 /usr/local/share/host-monitor
  assert_trusted_directory 0:0:0:755 /Library/LaunchDaemons
  assert_trusted_directory 0:0:0:755 /var/log
  assert_secure_root_directory_any_group '/Library/Application Support'
  assert_trusted_regular_file 0:0:0:755 /usr/local/libexec/host-monitor
  assert_trusted_regular_file 0:0:0:755 /usr/local/libexec/host-monitor-logrotate
  assert_trusted_regular_file 0:0:0:755 /usr/local/share/host-monitor/uninstall.sh
  assert_trusted_regular_file 0:0:0:644 /usr/local/share/host-monitor/newsyslog.conf
  assert_trusted_regular_file 0:0:0:644 \
    /usr/local/share/host-monitor/host-monitor.json.example
  assert_trusted_regular_file 0:0:0:644 /Library/LaunchDaemons/org.sarmg.hostmonitor.plist
  assert_trusted_regular_file 0:0:0:644 /Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist
  assert_command_link
}

assert_runtime_state_trust() {
  local expect_package_template=${1:-1}
  local service_uid service_gid
  service_uid=$(id -u _hostmonitor)
  service_gid=$(id -g _hostmonitor)
  assert_trusted_directory "$service_uid:$service_gid:0:700" \
    '/Library/Application Support/host-monitor'
  assert_trusted_regular_file "$service_uid:$service_gid:0:600" \
    '/Library/Application Support/host-monitor/config.json'
  assert_trusted_regular_file "$service_uid:$service_gid:0:600" \
    /var/log/host-monitor.log
  sudo test ! -e '/Library/Application Support/host-monitor/host-monitor.json.example' &&
    sudo test ! -L '/Library/Application Support/host-monitor/host-monitor.json.example' || {
    echo 'package config template leaked into service-writable state' >&2
    exit 1
  }
  if [[ $expect_package_template == 1 ]]; then
    grep -F '"state_dir": "/Library/Application Support/host-monitor"' \
      /usr/local/share/host-monitor/host-monitor.json.example >/dev/null || {
      echo 'installed package config template has the wrong state directory' >&2
      exit 1
    }
  fi
}

assert_preserved_uninstall_trust() {
  assert_secure_shared_directory /usr/local
  assert_secure_shared_directory /usr/local/bin
  assert_secure_shared_directory /usr/local/share
  assert_trusted_directory 0:0:0:755 /usr/local/share/host-monitor
  assert_trusted_regular_file 0:0:0:755 /usr/local/share/host-monitor/uninstall.sh
}

assert_ownership_proof() {
  assert_trusted_directory 0:0:0:700 /var/db/host-monitor
  assert_trusted_regular_file 0:0:0:600 /var/db/host-monitor/account-ownership
}

install_package() {
  if sudo installer -pkg "$package" -target /; then
    return 0
  fi
  echo 'macOS Installer failed; recent install log follows:' >&2
  sudo tail -n 240 /var/log/install.log >&2 || true
  return 1
}

# GitHub's hosted macOS image deliberately makes parts of /usr/local runner-owned
# and writable. First prove that preinstall rejects that state before extraction,
# then harden only the path components used by this destructive package smoke.
if [[ ${GITHUB_ACTIONS:-} == true && ${RUNNER_OS:-} == macOS ]]; then
  unsafe_host_path=0
  for directory in \
    /usr/local \
    /usr/local/libexec \
    /usr/local/bin \
    /usr/local/share
  do
    if [[ -e $directory || -L $directory ]] && ! shared_directory_is_secure "$directory"; then
      unsafe_host_path=1
    fi
  done
  if (( unsafe_host_path == 1 )); then
    for package_path in \
      /usr/local/libexec/host-monitor \
      /usr/local/libexec/host-monitor-logrotate \
      /usr/local/bin/host-monitor \
      /usr/local/share/host-monitor
    do
      [[ ! -e $package_path && ! -L $package_path ]]
    done
    if sudo installer -pkg "$package" -target /; then
      echo 'preinstall accepted an unsafe GitHub runner path' >&2
      exit 1
    fi
    ! pkgutil --pkg-info org.sarmg.hostmonitor >/dev/null 2>&1
  fi
  for directory in \
    /usr/local \
    /usr/local/libexec \
    /usr/local/bin \
    /usr/local/share
  do
    if [[ -L $directory || ( -e $directory && ! -d $directory ) ]]; then
      echo "refusing to harden redirected or non-directory CI path: $directory" >&2
      exit 1
    fi
    sudo install -d -m 0755 -o root -g wheel "$directory"
    sudo chmod -N "$directory"
    sudo chown root:wheel "$directory"
    sudo chmod 0755 "$directory"
    assert_secure_shared_directory "$directory"
  done
fi

package_payload=$(pkgutil --payload-files "$package")
if grep -E '(^|/)Library/Application Support/host-monitor(/|$)' \
  <<<"$package_payload" >/dev/null; then
  echo 'package payload contains service-writable Client state' >&2
  exit 1
fi
grep -E '(^|/)usr/local/share/host-monitor/host-monitor\.json\.example$' \
  <<<"$package_payload" >/dev/null || {
  echo 'package payload is missing the root-owned config template' >&2
  exit 1
}

install_package
sudo /usr/local/libexec/host-monitor service status --format json | python3 -c 'import json,sys; s=json.load(sys.stdin)["result"]; assert s["installed"] and s["state"] == "stopped"'
assert_install_trust
assert_runtime_state_trust
assert_ownership_proof
sudo touch '/Library/Application Support/host-monitor/release-lifecycle-marker'

install_package
sudo /usr/local/libexec/host-monitor service status --format json | python3 -c 'import json,sys; s=json.load(sys.stdin)["result"]; assert s["installed"] and s["state"] == "stopped"'
assert_install_trust
assert_runtime_state_trust
assert_ownership_proof
sudo /usr/local/share/host-monitor/uninstall.sh
[[ ! -e /usr/local/libexec/host-monitor ]]
sudo test -e '/Library/Application Support/host-monitor/release-lifecycle-marker'
[[ ! -e /usr/local/share/host-monitor/host-monitor.json.example ]]
assert_preserved_uninstall_trust
assert_runtime_state_trust 0
assert_ownership_proof

install_package
assert_install_trust
assert_runtime_state_trust
assert_ownership_proof
sudo /usr/local/share/host-monitor/uninstall.sh --purge --yes
sudo test ! -e '/Library/Application Support/host-monitor'
sudo test ! -e /var/db/host-monitor
! dscl . -read /Users/_hostmonitor >/dev/null 2>&1
! dscl . -read /Groups/_hostmonitor >/dev/null 2>&1
! pkgutil --pkg-info org.sarmg.hostmonitor >/dev/null 2>&1
