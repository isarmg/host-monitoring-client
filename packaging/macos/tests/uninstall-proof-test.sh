#!/bin/sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
uninstaller="$script_dir/../uninstall.sh"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/host-monitoring-macos-uninstall.XXXXXX")"

cleanup() {
  rm -rf "$test_root"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

fail() {
  echo "macOS uninstall ownership-proof test: $*" >&2
  exit 1
}

write_marker() {
  marker_case="$1"
  marker_user_created="$2"
  marker_group_created="$3"
  if [ "$marker_user_created" -eq 1 ]; then
    marker_uid=450
    marker_primary_gid=450
  else
    marker_uid=-
    marker_primary_gid=-
  fi
  if [ "$marker_group_created" -eq 1 ]; then
    marker_gid=450
  else
    marker_gid=-
  fi
  {
    printf 'format=@HOST_MONITOR_PACKAGE_VERSION@\n'
    printf 'user_created=%s\n' "$marker_user_created"
    printf 'user_uid=%s\n' "$marker_uid"
    printf 'user_primary_gid=%s\n' "$marker_primary_gid"
    printf 'group_created=%s\n' "$marker_group_created"
    printf 'group_gid=%s\n' "$marker_gid"
  } >"$marker_case/var/db/host-monitor/account-ownership"
}

make_case() {
  case_root="$1"
  case_bin="$case_root/bin"
  case_state="$case_root/Library/Application Support/host-monitor"
  case_log="$case_root/var/log/host-monitor.log"
  case_share="$case_root/usr/local/share/host-monitor"
  case_ownership="$case_root/var/db/host-monitor"
  mkdir -p "$case_bin" "$case_state" "$(dirname -- "$case_log")" "$case_share" \
    "$case_ownership" "$case_root/Library/LaunchDaemons" \
    "$case_root/usr/local/libexec" "$case_root/usr/local/bin"
  : >"$case_state/state-sentinel"
  : >"$case_log"

  sed \
    -e "s|^PATH=.*|PATH=\"$case_bin:/bin:/usr/bin:/sbin:/usr/sbin\"|" \
    -e "s|/Library/Application Support/host-monitor|$case_state|g" \
    -e "s|/var/log/host-monitor.log|$case_log|g" \
    -e "s|/usr/local/share/host-monitor|$case_share|g" \
    -e "s|/var/db/host-monitor|$case_ownership|g" \
    -e "s|/Library/LaunchDaemons|$case_root/Library/LaunchDaemons|g" \
    -e "s|/usr/local/libexec|$case_root/usr/local/libexec|g" \
    -e "s|/usr/local/bin|$case_root/usr/local/bin|g" \
    "$uninstaller" >"$case_share/uninstall.sh"
  chmod +x "$case_share/uninstall.sh"

  cat >"$case_bin/id" <<'EOF'
#!/bin/sh
if [ "${1:-}" = -u ]; then
  printf '0\n'
  exit 0
fi
exec /usr/bin/id "$@"
EOF

  cat >"$case_bin/launchctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$CASE_ROOT/launchctl-calls"
case "${1:-}" in
  print)
    if [ "${FAIL_LAUNCHCTL_PRINT:-0}" -eq 1 ]; then
      printf 'launchd RPC inspection failed\n' >&2
      exit 79
    fi
    printf 'Could not find service "%s" in domain for system\n' \
      "${2#system/}" >&2
    exit 113
    ;;
  *) exit 0 ;;
esac
EOF

  cat >"$case_bin/stat" <<'EOF'
#!/bin/sh
format=
path=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -f)
      shift
      format="$1"
      ;;
    *) path="$1" ;;
  esac
  shift
done
[ "$format" = '%u:%g:%Mp:%Lp' ] || exit 64
case "$path" in
  "$CASE_ROOT/var/db/host-monitor") metadata="${STAT_OWNERSHIP_DIR:-0:0:0:700}" ;;
  "$CASE_ROOT/var/db/host-monitor/account-ownership"|\
  "$CASE_ROOT/var/db/host-monitor"/.account-ownership.*)
    metadata="${STAT_OWNERSHIP_MARKER:-0:0:0:600}"
    ;;
  *) exit 65 ;;
esac
printf '%s\n' "$metadata"
EOF

  cat >"$case_bin/ls" <<'EOF'
#!/bin/sh
[ "${1:-}" = -lde ] && [ "$#" -eq 2 ] || exec /bin/ls "$@"
path="$2"
permissions=-rw-------
case "$path" in
  "$CASE_ROOT/var/db/host-monitor")
    permissions=drwx------
    acl_present="${ACL_OWNERSHIP_DIR:-0}"
    ls_failure="${FAIL_LS_OWNERSHIP_DIR:-0}"
    ;;
  "$CASE_ROOT/var/db/host-monitor/account-ownership")
    acl_present="${ACL_OWNERSHIP_MARKER:-0}"
    ls_failure="${FAIL_LS_OWNERSHIP_MARKER:-0}"
    ;;
  "$CASE_ROOT/var/db/host-monitor"/.account-ownership.*)
    acl_present="${ACL_OWNERSHIP_TEMP:-0}"
    ls_failure="${FAIL_LS_OWNERSHIP_TEMP:-0}"
    ;;
  *) exit 65 ;;
esac
[ "$ls_failure" -eq 0 ] || exit 71
if [ "$acl_present" -eq 1 ]; then
  printf '%s+ 1 root wheel 0 Jan 1 00:00 %s\n' "$permissions" "$path"
  printf ' 0: user:untrusted allow read,write\n'
else
  printf '%s 1 root wheel 0 Jan 1 00:00 %s\n' "$permissions" "$path"
fi
EOF

  cat >"$case_bin/chown" <<'EOF'
#!/bin/sh
[ "${FAIL_CHOWN:-0}" -eq 0 ]
EOF

  cat >"$case_bin/chmod" <<'EOF'
#!/bin/sh
if [ "${1:-}" = -N ]; then
  [ "${FAIL_CHMOD_N:-0}" -eq 0 ]
  exit
fi
exec /bin/chmod "$@"
EOF

  cat >"$case_bin/rmdir" <<'EOF'
#!/bin/sh
if [ "$#" -eq 1 ] && [ "$1" = "$CASE_ROOT/var/db/host-monitor" ] &&
  [ "${FAIL_OWNERSHIP_RMDIR:-0}" -eq 1 ]; then
  exit 73
fi
exec /bin/rmdir "$@"
EOF

  cat >"$case_bin/pkgutil" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$CASE_ROOT/pkgutil-calls"
case "${1:-}" in
  --pkgs=*)
    [ "$1" = '--pkgs=^org[.]sarmg[.]hostmonitor$' ] || exit 64
    case "${PKGUTIL_RECEIPT_STATE:-present}" in
      present) printf 'org.sarmg.hostmonitor\n' ;;
      absent) ;;
      error)
        printf 'package database unavailable\n' >&2
        exit 74
        ;;
      unexpected) printf 'org.sarmg.hostmonitor.helper\n' ;;
      *) exit 64 ;;
    esac
    ;;
  --forget) exit 0 ;;
  *) exit 64 ;;
esac
EOF

  cat >"$case_bin/dscl" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" = . ] || exit 64
operation="${2:-}"
case "$operation" in
  -list)
    record_type="$3"
    field="${4:-}"
    case "$record_type:$field" in
      /Users:)
        [ "${USER_STATE:-present}" != unknown ] || exit 71
        [ "${USER_STATE:-present}" != present ] || printf '_hostmonitor\n'
        ;;
      /Groups:)
        [ "${GROUP_STATE:-present}" != unknown ] || exit 72
        [ "${GROUP_STATE:-present}" != present ] || printf '_hostmonitor\n'
        ;;
      /Users:PrimaryGroupID)
        [ "${FAIL_GROUP_USAGE:-0}" -eq 0 ] || exit 73
        if [ "${GROUP_IN_USE:-0}" -eq 1 ]; then
          printf 'alice 450\n'
        else
          printf 'alice 999\n'
        fi
        ;;
      *) exit 64 ;;
    esac
    ;;
  -read)
    record="$3"
    field="${4:-}"
    case "$record:$field" in
      /Users/_hostmonitor:RealName) printf 'RealName: host-monitor\n' ;;
      /Users/_hostmonitor:UniqueID) printf 'UniqueID: 450\n' ;;
      /Users/_hostmonitor:PrimaryGroupID) printf 'PrimaryGroupID: 450\n' ;;
      /Users/_hostmonitor:UserShell) printf 'UserShell: /usr/bin/false\n' ;;
      /Users/_hostmonitor:NFSHomeDirectory) printf 'NFSHomeDirectory: /var/empty\n' ;;
      /Groups/_hostmonitor:RealName) printf 'RealName: host-monitor\n' ;;
      /Groups/_hostmonitor:PrimaryGroupID) printf 'PrimaryGroupID: 450\n' ;;
      /Groups/_hostmonitor:GeneratedUID)
        printf 'GeneratedUID: 01234567-89AB-CDEF-0123-456789ABCDEF\n'
        ;;
      /Groups/_hostmonitor:)
        printf 'RealName: host-monitor\n'
        printf 'PrimaryGroupID: 450\n'
        printf 'GeneratedUID: 01234567-89AB-CDEF-0123-456789ABCDEF\n'
        ;;
      *) exit 1 ;;
    esac
    ;;
  -delete)
    record="$3"
    printf '%s\n' "$record" >>"$CASE_ROOT/dscl-deletes"
    case "$record" in
      /Users/_hostmonitor) [ "${FAIL_DELETE_USER:-0}" -eq 0 ] ;;
      /Groups/_hostmonitor) [ "${FAIL_DELETE_GROUP:-0}" -eq 0 ] ;;
      *) exit 64 ;;
    esac
    ;;
  *) exit 64 ;;
esac
EOF

  chmod +x "$case_bin/id" "$case_bin/launchctl" "$case_bin/stat" "$case_bin/ls" \
    "$case_bin/chown" "$case_bin/chmod" "$case_bin/rmdir" "$case_bin/pkgutil" \
    "$case_bin/dscl"
}

run_case() {
  run_root="$1"
  shift
  set +e
  env CASE_ROOT="$run_root" "$@" \
    "$run_root/usr/local/share/host-monitor/uninstall.sh" --purge --yes \
    >"$run_root/output.log" 2>&1
  run_status="$?"
  set -e
}

assert_incomplete() {
  incomplete_root="$1"
  [ "$run_status" -eq 2 ] || fail "expected exit 2 for $incomplete_root, got $run_status"
  [ -e "$incomplete_root/usr/local/share/host-monitor/uninstall.sh" ] ||
    fail "incomplete purge removed its maintenance helper: $incomplete_root"
  if grep -Fx -- '--forget org.sarmg.hostmonitor' "$incomplete_root/pkgutil-calls" >/dev/null 2>&1; then
    fail "incomplete purge forgot its package receipt: $incomplete_root"
  fi
}

case_root="$test_root/launchd-inspection-failure"
make_case "$case_root"
write_marker "$case_root" 1 1
for package_path in \
  "$case_root/Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist" \
  "$case_root/Library/LaunchDaemons/org.sarmg.hostmonitor.plist" \
  "$case_root/usr/local/libexec/host-monitor-logrotate" \
  "$case_root/usr/local/libexec/host-monitor" \
  "$case_root/usr/local/share/host-monitor/newsyslog.conf" \
  "$case_root/usr/local/share/host-monitor/host-monitor.json.example"
do
  : >"$package_path"
done
run_case "$case_root" FAIL_LAUNCHCTL_PRINT=1
[ "$run_status" -eq 1 ] ||
  fail "launchd inspection failure returned $run_status instead of 1"
for preserved_path in \
  "$case_root/Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist" \
  "$case_root/Library/LaunchDaemons/org.sarmg.hostmonitor.plist" \
  "$case_root/usr/local/libexec/host-monitor-logrotate" \
  "$case_root/usr/local/libexec/host-monitor" \
  "$case_root/Library/Application Support/host-monitor/state-sentinel" \
  "$case_root/usr/local/share/host-monitor/uninstall.sh"
do
  [ -e "$preserved_path" ] ||
    fail "launchd inspection failure removed $preserved_path"
done
if grep -F 'bootout ' "$case_root/launchctl-calls" >/dev/null 2>&1; then
  fail 'launchd inspection failure attempted to boot out an unknown job state'
fi
[ ! -e "$case_root/dscl-deletes" ] ||
  fail 'launchd inspection failure deleted the dedicated account'
if grep -Fx -- '--forget org.sarmg.hostmonitor' "$case_root/pkgutil-calls" >/dev/null 2>&1; then
  fail 'launchd inspection failure forgot the package receipt'
fi
grep -F 'Could not inspect system/org.sarmg.hostmonitor.logrotate (launchctl status 79)' \
  "$case_root/output.log" >/dev/null ||
  fail 'launchd inspection failure was not diagnosed'

case_root="$test_root/valid-owned"
make_case "$case_root"
write_marker "$case_root" 1 1
run_case "$case_root"
[ "$run_status" -eq 0 ] || fail "valid owned purge returned $run_status"
grep -Fx '/Users/_hostmonitor' "$case_root/dscl-deletes" >/dev/null ||
  fail 'valid proof did not delete the owned user'
grep -Fx '/Groups/_hostmonitor' "$case_root/dscl-deletes" >/dev/null ||
  fail 'valid proof did not delete the owned group'
grep -Fx -- '--forget org.sarmg.hostmonitor' "$case_root/pkgutil-calls" >/dev/null ||
  fail 'complete purge did not forget the receipt'
[ ! -e "$case_root/usr/local/share/host-monitor/uninstall.sh" ] ||
  fail 'complete purge retained the helper'
[ ! -e "$case_root/var/db/host-monitor" ] ||
  fail 'complete purge retained an empty ownership directory'

case_root="$test_root/receipt-absent"
make_case "$case_root"
write_marker "$case_root" 0 0
run_case "$case_root" PKGUTIL_RECEIPT_STATE=absent
[ "$run_status" -eq 0 ] || fail "absent receipt purge returned $run_status"
[ ! -e "$case_root/usr/local/share/host-monitor/uninstall.sh" ] ||
  fail 'absent receipt retained the maintenance helper'
if grep -Fx -- '--forget org.sarmg.hostmonitor' "$case_root/pkgutil-calls" >/dev/null 2>&1; then
  fail 'absent receipt was unnecessarily forgotten'
fi

case_root="$test_root/receipt-query-failure"
make_case "$case_root"
write_marker "$case_root" 0 0
run_case "$case_root" PKGUTIL_RECEIPT_STATE=error
[ "$run_status" -eq 2 ] || fail "receipt query failure returned $run_status instead of 2"
[ -e "$case_root/usr/local/share/host-monitor/uninstall.sh" ] ||
  fail 'receipt query failure removed the maintenance helper'
if grep -Fx -- '--forget org.sarmg.hostmonitor' "$case_root/pkgutil-calls" >/dev/null 2>&1; then
  fail 'receipt query failure forgot an unknown receipt'
fi
grep -F 'pkgutil status 74' "$case_root/output.log" >/dev/null ||
  fail 'receipt query failure did not report the pkgutil status'

case_root="$test_root/receipt-unexpected-output"
make_case "$case_root"
write_marker "$case_root" 0 0
run_case "$case_root" PKGUTIL_RECEIPT_STATE=unexpected
[ "$run_status" -eq 2 ] || fail "unexpected receipt output returned $run_status instead of 2"
[ -e "$case_root/usr/local/share/host-monitor/uninstall.sh" ] ||
  fail 'unexpected receipt output removed the maintenance helper'
if grep -Fx -- '--forget org.sarmg.hostmonitor' "$case_root/pkgutil-calls" >/dev/null 2>&1; then
  fail 'unexpected receipt output forgot an unverified receipt'
fi
grep -F 'pkgutil returned unexpected output' "$case_root/output.log" >/dev/null ||
  fail 'unexpected receipt output was not diagnosed'

case_root="$test_root/missing-proof-present"
make_case "$case_root"
run_case "$case_root"
assert_incomplete "$case_root"
[ ! -e "$case_root/dscl-deletes" ] || fail 'missing proof deleted a same-name account'
[ -d "$case_root/var/db/host-monitor" ] || fail 'missing proof removed bookkeeping during incomplete purge'

case_root="$test_root/invalid-directory-acl"
make_case "$case_root"
write_marker "$case_root" 1 1
cp "$case_root/var/db/host-monitor/account-ownership" "$case_root/marker.before"
run_case "$case_root" ACL_OWNERSHIP_DIR=1
assert_incomplete "$case_root"
[ ! -e "$case_root/dscl-deletes" ] || fail 'untrusted ownership directory authorized account deletion'
cmp "$case_root/marker.before" "$case_root/var/db/host-monitor/account-ownership" >/dev/null ||
  fail 'untrusted ownership directory marker was modified'

case_root="$test_root/invalid-marker-mode"
make_case "$case_root"
write_marker "$case_root" 1 1
run_case "$case_root" STAT_OWNERSHIP_MARKER=0:0:0:666
assert_incomplete "$case_root"
[ ! -e "$case_root/dscl-deletes" ] || fail 'writable marker authorized account deletion'
[ -e "$case_root/var/db/host-monitor/account-ownership" ] ||
  fail 'invalid marker was removed'

case_root="$test_root/invalid-proof-accounts-absent"
make_case "$case_root"
write_marker "$case_root" 1 1
run_case "$case_root" USER_STATE=absent GROUP_STATE=absent ACL_OWNERSHIP_MARKER=1
assert_incomplete "$case_root"
[ -e "$case_root/var/db/host-monitor/account-ownership" ] ||
  fail 'invalid proof was removed after accounts were absent'

case_root="$test_root/missing-proof-accounts-absent"
make_case "$case_root"
run_case "$case_root" USER_STATE=absent GROUP_STATE=absent
[ "$run_status" -eq 0 ] || fail "idempotent missing-proof purge returned $run_status"
[ ! -e "$case_root/var/db/host-monitor" ] ||
  fail 'idempotent missing-proof purge retained an empty trusted directory'
grep -Fx -- '--forget org.sarmg.hostmonitor' "$case_root/pkgutil-calls" >/dev/null ||
  fail 'idempotent complete purge did not forget the receipt'

case_root="$test_root/missing-proof-enumeration-unknown"
make_case "$case_root"
run_case "$case_root" USER_STATE=unknown GROUP_STATE=absent
assert_incomplete "$case_root"
[ ! -e "$case_root/dscl-deletes" ] || fail 'unknown account state caused deletion'

case_root="$test_root/user-delete-failure"
make_case "$case_root"
write_marker "$case_root" 1 1
run_case "$case_root" FAIL_DELETE_USER=1
assert_incomplete "$case_root"
grep -Fx '/Users/_hostmonitor' "$case_root/dscl-deletes" >/dev/null ||
  fail 'user delete failure was not exercised'
if grep -Fx '/Groups/_hostmonitor' "$case_root/dscl-deletes" >/dev/null 2>&1; then
  fail 'group was deleted after its owned user could not be removed'
fi
grep -Fx 'user_created=1' "$case_root/var/db/host-monitor/account-ownership" >/dev/null ||
  fail 'user delete failure lost the user ownership proof'

case_root="$test_root/group-in-use"
make_case "$case_root"
write_marker "$case_root" 1 1
run_case "$case_root" GROUP_IN_USE=1
assert_incomplete "$case_root"
grep -Fx 'user_created=0' "$case_root/var/db/host-monitor/account-ownership" >/dev/null ||
  fail 'partial purge did not record the completed user deletion'
grep -Fx 'group_created=1' "$case_root/var/db/host-monitor/account-ownership" >/dev/null ||
  fail 'partial purge lost the remaining group ownership proof'

case_root="$test_root/valid-preexisting"
make_case "$case_root"
write_marker "$case_root" 0 0
run_case "$case_root"
[ "$run_status" -eq 0 ] || fail "valid pre-existing account purge returned $run_status"
[ ! -e "$case_root/dscl-deletes" ] || fail 'pre-existing accounts were deleted'
[ ! -e "$case_root/var/db/host-monitor" ] ||
  fail 'completed pre-existing account purge retained bookkeeping'

case_root="$test_root/unexpected-bookkeeping-entry"
make_case "$case_root"
write_marker "$case_root" 0 0
: >"$case_root/var/db/host-monitor/.account-ownership.stale"
run_case "$case_root"
assert_incomplete "$case_root"
[ -e "$case_root/var/db/host-monitor/account-ownership" ] ||
  fail 'unexpected bookkeeping entry caused the completed marker to be lost'
[ -e "$case_root/var/db/host-monitor/.account-ownership.stale" ] ||
  fail 'unexpected bookkeeping entry was modified'

case_root="$test_root/ownership-rmdir-failure"
make_case "$case_root"
write_marker "$case_root" 0 0
run_case "$case_root" FAIL_OWNERSHIP_RMDIR=1
assert_incomplete "$case_root"
grep -Fx 'user_created=0' "$case_root/var/db/host-monitor/account-ownership" >/dev/null ||
  fail 'ownership-directory removal failure did not restore the user proof'
grep -Fx 'group_created=0' "$case_root/var/db/host-monitor/account-ownership" >/dev/null ||
  fail 'ownership-directory removal failure did not restore the group proof'

echo 'macOS uninstall ownership-proof tests: ok'
