#!/bin/sh
set -eu

die() {
  echo "host-monitor package build: $*" >&2
  exit 1
}

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# 客户端源码统一位于 clients/；打包器必须回到工作区根解析 Cargo 与 config/。
repository_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
cd "$repository_root"

package_id=$(cargo pkgid --locked -p host-monitor) || die "cannot resolve the Client Cargo package"
case "$package_id" in
  *@*) package_version=${package_id##*@} ;;
  *) die "unexpected cargo pkgid output: $package_id" ;;
esac
case "$package_version" in
  *[!0-9.]*|*.*.*.*|.*|*.) die "Client version is not strict MAJOR.MINOR.PATCH: $package_version" ;;
esac
[ "$(printf '%s' "$package_version" | awk -F. 'NF == 3 && $1 != "" && $2 != "" && $3 != "" { print "yes" }')" = yes ] ||
  die "Client version is not strict MAJOR.MINOR.PATCH: $package_version"

# Every lifecycle helper rejects state owned by another package version. Bind
# those literals to Cargo here so the documented direct builder cannot emit a
# package that only discovers the mismatch during install or removal.
for lifecycle_script in \
  packaging/linux/postinstall.sh \
  packaging/linux/preremove.sh \
  packaging/linux/postremove.sh \
  packaging/linux/purge-local-state.sh
do
  lifecycle_version=$(sed -n 's/^package_version=\([0-9][0-9.]*\)$/\1/p' "$lifecycle_script")
  [ "$lifecycle_version" = "$package_version" ] ||
    die "$lifecycle_script package_version does not match Cargo $package_version"
done

client_binary=target/release/host-monitor
[ -x "$client_binary" ] || die "Client binary is missing or not executable: $client_binary"
package_arch=${NFPM_ARCH:-amd64}
case "$package_arch" in
  amd64)
    expected_elf_machine='Advanced Micro Devices X86-64'
    rpm_arch=x86_64
    ;;
  arm64)
    expected_elf_machine=AArch64
    rpm_arch=aarch64
    ;;
  *) die "unsupported Client Linux package architecture: $package_arch" ;;
esac
command -v readelf >/dev/null 2>&1 ||
  die "required binary inspection command is unavailable: readelf"
elf_machine=$(
  LC_ALL=C readelf -h -- "$client_binary" 2>/dev/null |
    awk -F: '
      /^[[:space:]]*Machine:/ {
        value = $2
        sub(/^[[:space:]]+/, "", value)
        sub(/[[:space:]]+$/, "", value)
        found += 1
        machine = value
      }
      END {
        if (found != 1 || machine == "") exit 1
        print machine
      }
    '
) || die "Client package payload is not a readable ELF binary: $client_binary"
[ "$elf_machine" = "$expected_elf_machine" ] ||
  die "Client package payload architecture $elf_machine does not match $package_arch"
LC_ALL=C readelf -p .host_monitor.version -- "$client_binary" 2>/dev/null |
  awk -v expected="host-monitor $package_version" '
    /^[[:space:]]*\[[[:space:]]*[[:xdigit:]]+\][[:space:]]+/ {
      value = $0
      sub(/^[[:space:]]*\[[[:space:]]*[[:xdigit:]]+\][[:space:]]+/, "", value)
      sub(/[[:space:]]+$/, "", value)
      found += 1
      if (value == expected) matched += 1
    }
    END { exit found == 1 && matched == 1 ? 0 : 1 }
  ' || die "Client package payload ELF version marker does not match Cargo $package_version"

config_version=$(sed -n 's/^  "application_version": "\([^"]*\)",$/\1/p' config/host-monitor.json.example)
[ "$config_version" = "$package_version" ] ||
  die "host-monitor.json.example version $config_version does not match Client $package_version"

nfpm_bin=${NFPM_BIN:-nfpm}
command -v "$nfpm_bin" >/dev/null 2>&1 || [ -x "$nfpm_bin" ] ||
  die "nFPM is unavailable: $nfpm_bin"

mkdir -p dist
VERSION="$package_version" NFPM_ARCH="$package_arch" "$nfpm_bin" package \
  --config packaging/nfpm.yaml --packager deb \
  --target "dist/host-monitor_${package_version}_${package_arch}.deb"
VERSION="$package_version" NFPM_ARCH="$package_arch" "$nfpm_bin" package \
  --config packaging/nfpm.yaml --packager rpm \
  --target "dist/host-monitor-${package_version}.${rpm_arch}.rpm"
