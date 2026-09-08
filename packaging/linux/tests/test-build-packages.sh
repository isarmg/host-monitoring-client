#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
packaging_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/host-monitor-package-builder-test.XXXXXX")
fixture_root=$test_root/repository
mock_bin=$test_root/bin
package_version=0.9.8
execution_marker=$test_root/payload-executed
nfpm_log=$test_root/nfpm.log
builder_output=$test_root/builder.output

cleanup() {
  case "$test_root" in
    "${TMPDIR:-/tmp}"/host-monitor-package-builder-test.*)
      rm -rf -- "$test_root"
      ;;
    *)
      echo "refusing to remove unexpected test path: $test_root" >&2
      ;;
  esac
}
trap cleanup EXIT HUP INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_absent() {
  [ ! -e "$1" ] || fail "expected path to be absent: $1"
}

assert_output_contains() {
  grep -F -- "$1" "$builder_output" >/dev/null || {
    sed -n '1,120p' "$builder_output" >&2
    fail "builder output does not contain: $1"
  }
}

mkdir -p \
  "$fixture_root/packaging/linux" \
  "$fixture_root/config" \
  "$fixture_root/target/release" \
  "$mock_bin"
cp "$packaging_dir/build-packages.sh" \
  "$fixture_root/packaging/linux/build-packages.sh"
cp "$packaging_dir/../nfpm.yaml" "$fixture_root/packaging/nfpm.yaml"
for lifecycle_script in postinstall.sh preremove.sh postremove.sh purge-local-state.sh; do
  printf 'package_version=%s\n' "$package_version" \
    >"$fixture_root/packaging/linux/$lifecycle_script"
done
cat >"$fixture_root/config/host-monitor.json.example" <<EOF
{
  "application_version": "0.9.4",
  "fixture": true
}
EOF

cat >"$mock_bin/cargo" <<EOF
#!/bin/sh
[ "\$*" = "pkgid --locked -p host-monitor" ] || exit 64
printf '%s\n' 'path+file:///fixture/client#host-monitor@$package_version'
EOF

cat >"$mock_bin/readelf" <<'EOF'
#!/bin/sh
mode=
payload=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -h) mode=header ;;
    -p)
      shift
      [ "${1:-}" = .host_monitor.version ] || exit 65
      mode=version
      ;;
    --) ;;
    *) payload=$1 ;;
  esac
  shift
done
[ -n "$payload" ] && [ -f "$payload" ] || exit 66
case "$mode" in
  header)
    machine=$(sed -n 's/^# fixture-machine=//p' "$payload")
    [ -n "$machine" ] || exit 67
    printf 'ELF Header:\n  Machine:                           %s\n' "$machine"
    ;;
  version)
    version=$(sed -n 's/^# fixture-version=//p' "$payload")
    [ -n "$version" ] || exit 68
    printf "String dump of section '.host_monitor.version':\n  [     0]  %s\n" "$version"
    ;;
  *) exit 69 ;;
esac
EOF

cat >"$mock_bin/nfpm" <<'EOF'
#!/bin/sh
printf 'VERSION=%s NFPM_ARCH=%s %s\n' "$VERSION" "$NFPM_ARCH" "$*" >>"$NFPM_LOG"
EOF
chmod 0755 "$mock_bin/cargo" "$mock_bin/readelf" "$mock_bin/nfpm"

write_payload() {
  payload_path=$1
  payload_machine=$2
  payload_version=$3
  cat >"$payload_path" <<EOF
#!/bin/sh
: >"\${EXECUTION_MARKER:?}"
exit 91
# fixture-machine=$payload_machine
# fixture-version=$payload_version
EOF
  chmod 0755 "$payload_path"
}

reset_case() {
  : >"$nfpm_log"
  : >"$builder_output"
  rm -f -- "$execution_marker"
  test_client_binary_override=
}

run_builder() {
  requested_arch=$1
  (
    cd "$fixture_root"
    PATH="$mock_bin:/usr/bin:/bin"
    EXECUTION_MARKER=$execution_marker
    NFPM_LOG=$nfpm_log
    NFPM_ARCH=$requested_arch
    CLIENT_BINARY=$test_client_binary_override
    export PATH EXECUTION_MARKER NFPM_LOG NFPM_ARCH CLIENT_BINARY
    sh packaging/linux/build-packages.sh
  ) >"$builder_output" 2>&1
}

assert_success() {
  requested_arch=$1
  if ! run_builder "$requested_arch"; then
    sed -n '1,120p' "$builder_output" >&2
    fail "builder rejected the valid $requested_arch fixture"
  fi
  assert_absent "$execution_marker"
  [ "$(wc -l <"$nfpm_log" | tr -d ' ')" = 2 ] ||
    fail "valid $requested_arch build did not invoke nFPM exactly twice"
}

fixed_payload=$fixture_root/target/release/host-monitor

reset_case
write_payload "$fixed_payload" 'Advanced Micro Devices X86-64' "host-monitor $package_version"
assert_success amd64
grep -F -- "host-monitor_${package_version}_amd64.deb" "$nfpm_log" >/dev/null ||
  fail 'amd64 DEB target was not requested'
grep -F -- "host-monitor-${package_version}.x86_64.rpm" "$nfpm_log" >/dev/null ||
  fail 'amd64 RPM target did not use the RPM architecture name'

reset_case
write_payload "$fixed_payload" AArch64 "host-monitor $package_version"
assert_success arm64
grep -F -- "host-monitor-${package_version}.aarch64.rpm" "$nfpm_log" >/dev/null ||
  fail 'arm64 RPM target did not use the RPM architecture name'

reset_case
write_payload "$fixed_payload" 'Advanced Micro Devices X86-64' "host-monitor $package_version"
if run_builder arm64; then
  fail 'builder accepted an amd64 payload for an arm64 package'
fi
assert_output_contains 'does not match arm64'
[ ! -s "$nfpm_log" ] || fail 'architecture mismatch reached nFPM'
assert_absent "$execution_marker"

reset_case
write_payload "$fixed_payload" 'Advanced Micro Devices X86-64' 'host-monitor 0.3.3'
alternate_payload=$test_root/alternate-client
write_payload "$alternate_payload" 'Advanced Micro Devices X86-64' "host-monitor $package_version"
test_client_binary_override=$alternate_payload
if run_builder amd64; then
  fail 'CLIENT_BINARY redirected validation away from the fixed nFPM payload'
fi
assert_output_contains 'ELF version marker does not match Cargo'
[ ! -s "$nfpm_log" ] || fail 'stale fixed payload reached nFPM'
assert_absent "$execution_marker"

reset_case
write_payload "$fixed_payload" 'Advanced Micro Devices X86-64' "host-monitor $package_version"
if run_builder riscv64; then
  fail 'builder accepted an unsupported package architecture'
fi
assert_output_contains 'unsupported Client Linux package architecture: riscv64'
[ ! -s "$nfpm_log" ] || fail 'unsupported architecture reached nFPM'

for stale_lifecycle_script in \
  postinstall.sh preremove.sh postremove.sh purge-local-state.sh
do
  reset_case
  printf 'package_version=0.3.3\n' \
    >"$fixture_root/packaging/linux/$stale_lifecycle_script"
  if run_builder amd64; then
    fail "builder accepted stale $stale_lifecycle_script package_version"
  fi
  assert_output_contains \
    "packaging/linux/$stale_lifecycle_script package_version does not match Cargo"
  [ ! -s "$nfpm_log" ] || fail "stale $stale_lifecycle_script reached nFPM"
  printf 'package_version=%s\n' "$package_version" \
    >"$fixture_root/packaging/linux/$stale_lifecycle_script"
done

echo 'Client Linux package builder tests passed'
