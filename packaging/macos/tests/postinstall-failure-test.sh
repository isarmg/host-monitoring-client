#!/bin/sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
postinstall="$script_dir/../scripts/postinstall"
preinstall="$script_dir/../scripts/preinstall"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/host-monitoring-macos-postinstall.XXXXXX")"

cleanup() {
  rm -rf "$test_root"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

fail() {
  echo "macOS postinstall failure test: $*" >&2
  exit 1
}

assert_calls_in_order_after() {
  calls_file="$1"
  anchor_call="$2"
  shift 2
  previous_line="$(
    awk -v expected="$anchor_call" '$0 == expected { found = NR } END { if (found) print found }' \
      "$calls_file"
  )"
  [ -n "$previous_line" ] || fail "missing launchctl call anchor: $anchor_call"
  for expected_call in "$@"; do
    next_line="$(
      awk -v expected="$expected_call" -v after="$previous_line" \
        'NR > after && $0 == expected { print NR; exit }' "$calls_file"
    )"
    [ -n "$next_line" ] ||
      fail "missing ordered launchctl call after line $previous_line: $expected_call"
    previous_line="$next_line"
  done
}

make_case() {
  case_dir="$1"
  case_bin="$case_dir/bin"
  case_state_parent="$case_dir/Library/Application Support"
  case_state="$case_dir/Library/Application Support/host-monitor"
  case_ownership="$case_dir/var/db/host-monitor"
  case_log="$case_dir/var/log/host-monitor.log"
  case_client_binary="$case_dir/usr/local/libexec/host-monitor"
  case_client_command="$case_dir/usr/local/bin/host-monitor"
  case_logrotate_helper="$case_dir/usr/local/libexec/host-monitor-logrotate"
  case_client_plist="$case_dir/Library/LaunchDaemons/org.sarmg.hostmonitor.plist"
  case_logrotate_plist="$case_dir/Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist"
  case_share="$case_dir/usr/local/share/host-monitor"
  case_package_config="$case_share/host-monitor.json.example"
  case_newsyslog_config="$case_share/newsyslog.conf"
  case_uninstall_helper="$case_share/uninstall.sh"
  mkdir -p "$case_bin" "$case_state_parent" "$case_dir/dscl/Groups" "$case_dir/dscl/Users" \
    "$case_dir/launch" "$(dirname -- "$case_log")" "$(dirname -- "$case_client_plist")" \
    "$(dirname -- "$case_client_binary")" "$(dirname -- "$case_client_command")" "$case_share"
  {
    printf '{\n'
    printf '  "application_version": "0.9.4",\n'
    printf '  "server_url": null\n'
    printf '}\n'
  } >"$case_package_config"
  cat >"$case_client_binary" <<'EOF'
#!/bin/sh
[ "${1:-}" = --version ] || exit 64
printf 'host-monitor @HOST_MONITOR_PACKAGE_VERSION@\n'
EOF
  cat >"$case_logrotate_helper" <<'EOF'
#!/bin/sh
exit 0
EOF
  cat >"$case_client_plist" <<'EOF'
<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>org.sarmg.hostmonitor</string></dict></plist>
EOF
  cat >"$case_logrotate_plist" <<'EOF'
<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>org.sarmg.hostmonitor.logrotate</string></dict></plist>
EOF
  printf '/var/log/host-monitor.log _hostmonitor:_hostmonitor 600 7 10240 * JN\n' \
    >"$case_newsyslog_config"
  cat >"$case_uninstall_helper" <<'EOF'
#!/bin/sh
exit 0
EOF
  chmod 0755 "$case_client_binary" "$case_logrotate_helper" "$case_uninstall_helper"
  ln -s ../libexec/host-monitor "$case_client_command"

  sed \
    -e "s|^PATH=.*|PATH=\"$case_bin:/bin:/usr/bin:/sbin:/usr/sbin\"|" \
    -e "s|^state=.*|state=\"$case_state\"|" \
    -e "s|^state_parent=.*|state_parent=\"$case_state_parent\"|" \
    -e "s|^log=.*|log=\"$case_log\"|" \
    -e "s|^client_binary=.*|client_binary=\"$case_client_binary\"|" \
    -e "s|^client_command=.*|client_command=\"$case_client_command\"|" \
    -e "s|^logrotate_helper=.*|logrotate_helper=\"$case_logrotate_helper\"|" \
    -e "s|^client_plist=.*|client_plist=\"$case_client_plist\"|" \
    -e "s|^logrotate_plist=.*|logrotate_plist=\"$case_logrotate_plist\"|" \
    -e "s|^ownership_dir=.*|ownership_dir=\"$case_ownership\"|" \
    "$postinstall" >"$case_dir/postinstall"
  chmod +x "$case_dir/postinstall"

  sed \
    -e "s|^PATH=.*|PATH=\"$case_bin:/bin:/usr/bin:/sbin:/usr/sbin\"|" \
    -e "s|^client_binary=.*|client_binary=\"$case_client_binary\"|" \
    -e "s|^client_command=.*|client_command=\"$case_client_command\"|" \
    -e "s|^logrotate_helper=.*|logrotate_helper=\"$case_logrotate_helper\"|" \
    -e "s|^client_plist=.*|client_plist=\"$case_client_plist\"|" \
    -e "s|^logrotate_plist=.*|logrotate_plist=\"$case_logrotate_plist\"|" \
    -e "s|^log_dir=.*|log_dir=\"${case_log%/*}\"|" \
    -e "s|^state_parent=.*|state_parent=\"$case_state_parent\"|" \
    "$preinstall" >"$case_dir/preinstall"
  chmod +x "$case_dir/preinstall"

  cat >"$case_bin/id" <<'EOF'
#!/bin/sh
if [ "${1:-}" = -u ]; then
  printf '0\n'
  exit 0
fi
exec /usr/bin/id "$@"
EOF

  cat >"$case_bin/install" <<'EOF'
#!/bin/sh
destination=
for argument do
  destination="$argument"
done
[ -n "$destination" ] || exit 64
mkdir -p "$destination"
if [ "$destination" = "$CASE_ROOT/Library/Application Support/host-monitor" ]; then
  printf '0:0:700\n' >"$FILE_STATE/metadata.state-dir"
fi
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
  "$CASE_ROOT/usr")
    metadata="${STAT_SYSTEM_USR:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local")
    metadata="${STAT_LOCAL_PREFIX:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/libexec")
    metadata="${STAT_LIBEXEC_DIR:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/bin")
    metadata="${STAT_COMMAND_DIR:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/share")
    metadata="${STAT_SHARE_PARENT:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/share/host-monitor")
    metadata="${STAT_SHARE_DIR:-0:0:755}"
    ;;
  "$CASE_ROOT/Library/LaunchDaemons")
    metadata="${STAT_LAUNCH_DAEMON_DIR:-0:0:755}"
    ;;
  "$CASE_ROOT/Library")
    metadata="${STAT_LIBRARY_ROOT:-0:0:755}"
    ;;
  "$CASE_ROOT/Library/Application Support")
    metadata="${STAT_STATE_PARENT:-0:80:755}"
    ;;
  "$CASE_ROOT/usr/local/libexec/host-monitor")
    metadata="${STAT_CLIENT_BINARY:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/bin/host-monitor")
    metadata="${STAT_CLIENT_COMMAND:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/libexec/host-monitor-logrotate")
    metadata="${STAT_LOGROTATE_HELPER:-0:0:755}"
    ;;
  "$CASE_ROOT/Library/LaunchDaemons/org.sarmg.hostmonitor.plist")
    metadata="${STAT_CLIENT_PLIST:-0:0:644}"
    ;;
  "$CASE_ROOT/Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist")
    metadata="${STAT_LOGROTATE_PLIST:-0:0:644}"
    ;;
  "$CASE_ROOT/usr/local/share/host-monitor/newsyslog.conf")
    metadata="${STAT_NEWSYSLOG_CONFIG:-0:0:644}"
    ;;
  "$CASE_ROOT/usr/local/share/host-monitor/host-monitor.json.example")
    metadata="${STAT_PACKAGE_CONFIG:-0:0:644}"
    ;;
  "$CASE_ROOT/usr/local/share/host-monitor/uninstall.sh")
    metadata="${STAT_UNINSTALL_HELPER:-0:0:755}"
    ;;
  "$CASE_ROOT/var/db/host-monitor")
    metadata="${STAT_OWNERSHIP_DIR:-0:0:700}"
    ;;
  "$CASE_ROOT/var/db/host-monitor/account-ownership"|\
  "$CASE_ROOT/var/db/host-monitor"/.account-ownership.*)
    metadata="${STAT_OWNERSHIP_MARKER:-0:0:600}"
    ;;
  "$CASE_ROOT/Library/Application Support/host-monitor")
    if [ -n "${STAT_STATE_ROOT:-}" ]; then
      metadata="$STAT_STATE_ROOT"
    elif [ -f "$FILE_STATE/metadata.state-dir" ]; then
      IFS= read -r metadata <"$FILE_STATE/metadata.state-dir"
    else
      metadata="0:0:755"
    fi
    ;;
  "$CASE_ROOT/Library/Application Support/host-monitor/config.json")
    if [ -n "${STAT_CONFIG:-}" ]; then
      metadata="$STAT_CONFIG"
    elif [ -f "$FILE_STATE/metadata.retained-config" ]; then
      IFS= read -r metadata <"$FILE_STATE/metadata.retained-config"
    elif [ -f "$FILE_STATE/metadata.config-temp" ]; then
      # Fresh config is published as a hard link to the already-secured temp
      # inode, so the final path inherits the same metadata.
      IFS= read -r metadata <"$FILE_STATE/metadata.config-temp"
    else
      metadata="0:0:600"
    fi
    ;;
  "$CASE_ROOT/Library/Application Support/host-monitor"/.config.json.*)
    if [ -n "${STAT_CONFIG_TEMP:-}" ]; then
      metadata="$STAT_CONFIG_TEMP"
    elif [ -f "$FILE_STATE/metadata.config-temp" ]; then
      IFS= read -r metadata <"$FILE_STATE/metadata.config-temp"
    else
      metadata="0:0:600"
    fi
    ;;
  "$CASE_ROOT/var/log")
    metadata="${STAT_LOG_DIR:-0:0:755}"
    ;;
  "$CASE_ROOT/var/log/host-monitor.log")
    if [ -n "${STAT_LOG_FILE:-}" ]; then
      metadata="$STAT_LOG_FILE"
    elif [ -f "$FILE_STATE/metadata.log-file" ]; then
      IFS= read -r metadata <"$FILE_STATE/metadata.log-file"
    else
      metadata="0:0:600"
    fi
    ;;
  *) exit 65 ;;
esac
case "$metadata" in
  *:*:*:*) printf '%s\n' "$metadata" ;;
  *)
    uid="${metadata%%:*}"
    remainder="${metadata#*:}"
    gid="${remainder%%:*}"
    mode="${remainder#*:}"
    printf '%s:%s:%s:%s\n' "$uid" "$gid" "${STAT_SPECIAL_MODE:-0}" "$mode"
    ;;
esac
EOF

  cat >"$case_bin/ls" <<'EOF'
#!/bin/sh
if [ "${1:-}" != -lde ] || [ "$#" -ne 2 ]; then
  exec /bin/ls "$@"
fi
path="$2"
acl_key=
permissions=-rw-------
acl_requested=0
deny_acl_requested=0
tricky_allow_acl_requested=0
ls_failure=0
case "$path" in
  "$CASE_ROOT/usr")
    acl_key=system-usr
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/usr/local")
    acl_key=local-prefix
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/usr/local/libexec")
    acl_key=libexec-dir
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/usr/local/bin")
    acl_key=command-dir
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/usr/local/share")
    acl_key=share-parent
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/usr/local/share/host-monitor")
    acl_key=share-dir
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/Library/LaunchDaemons")
    acl_key=launch-daemon-dir
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/Library")
    acl_key=library-root
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/Library/Application Support")
    acl_key=state-parent
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/usr/local/libexec/host-monitor") acl_key=client-binary ;;
  "$CASE_ROOT/usr/local/bin/host-monitor")
    acl_key=client-command
    permissions=lrwxr-xr-x
    ;;
  "$CASE_ROOT/usr/local/libexec/host-monitor-logrotate") acl_key=logrotate-helper ;;
  "$CASE_ROOT/Library/LaunchDaemons/org.sarmg.hostmonitor.plist") acl_key=client-plist ;;
  "$CASE_ROOT/Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist")
    acl_key=logrotate-plist
    ;;
  "$CASE_ROOT/usr/local/share/host-monitor/newsyslog.conf") acl_key=newsyslog-config ;;
  "$CASE_ROOT/usr/local/share/host-monitor/host-monitor.json.example") acl_key=package-config ;;
  "$CASE_ROOT/usr/local/share/host-monitor/uninstall.sh") acl_key=uninstall-helper ;;
  "$CASE_ROOT/var/log")
    acl_key=log-dir
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/Library/Application Support/host-monitor")
    acl_key=state-dir
    permissions=drwx------
    ;;
  "$CASE_ROOT/Library/Application Support/host-monitor/config.json")
    acl_key=retained-config
    ;;
  "$CASE_ROOT/Library/Application Support/host-monitor"/.config.json.*)
    acl_key=config-temp
    ;;
  "$CASE_ROOT/var/log/host-monitor.log")
    acl_key=log-file
    ;;
  "$CASE_ROOT/var/db/host-monitor")
    acl_key=ownership-dir
    permissions=drwx------
    acl_requested="${ACL_OWNERSHIP_DIR:-0}"
    ls_failure="${FAIL_LS_OWNERSHIP_DIR:-0}"
    ;;
  "$CASE_ROOT/var/db/host-monitor/account-ownership")
    acl_key=ownership-marker
    acl_requested="${ACL_OWNERSHIP_MARKER:-0}"
    ls_failure="${FAIL_LS_OWNERSHIP_MARKER:-0}"
    ;;
  "$CASE_ROOT/var/db/host-monitor"/.account-ownership.*)
    acl_key=ownership-temp
    acl_requested="${ACL_OWNERSHIP_TEMP:-0}"
    ls_failure="${FAIL_LS_OWNERSHIP_TEMP:-0}"
    ;;
  *) exit 65 ;;
esac
[ "${ACL_PATH:-}" = "$acl_key" ] && acl_requested=1
[ "${DENY_ACL_PATH:-}" = "$acl_key" ] && deny_acl_requested=1
[ "${TRICKY_ALLOW_ACL_PATH:-}" = "$acl_key" ] && tricky_allow_acl_requested=1
[ "${FAIL_LS_PATH:-}" = "$acl_key" ] && ls_failure=1
if [ -e "$FILE_STATE/mutable-checks-active" ]; then
  [ "${POST_BOOTOUT_ACL_PATH:-}" = "$acl_key" ] && acl_requested=1
  [ "${POST_BOOTOUT_FAIL_LS_PATH:-}" = "$acl_key" ] && ls_failure=1
fi
[ "$ls_failure" -eq 0 ] || exit 71
if [ "$acl_requested" -eq 1 ] && [ ! -e "$FILE_STATE/acl-cleared.$acl_key" ]; then
  printf '%s+ 1 root wheel 0 Jan 1 00:00 %s\n' "$permissions" "$path"
  printf ' 0: user:untrusted allow read,write\n'
elif [ "$deny_acl_requested" -eq 1 ]; then
  # macOS displays @ instead of + when an object has both xattrs and an ACL;
  # `ls -e` still emits the ACL entries below the first line.
  printf '%s@ 1 root wheel 0 Jan 1 00:00 %s\n' "$permissions" "$path"
  printf ' 0: group:everyone deny delete\n'
elif [ "$tricky_allow_acl_requested" -eq 1 ]; then
  printf '%s@ 1 root wheel 0 Jan 1 00:00 %s\n' "$permissions" "$path"
  printf ' 0: User deny Name allow add_file,delete_child,writesecurity\n'
else
  printf '%s 1 root wheel 0 Jan 1 00:00 %s\n' "$permissions" "$path"
fi
EOF

  cat >"$case_bin/chmod" <<'EOF'
#!/bin/sh
if [ "${1:-}" != -N ]; then
  requested_mode="$1"
  shift
  /bin/chmod "$requested_mode" "$@" || exit $?
  normalized_mode="${requested_mode#0}"
  for path do
    metadata_key=
    default_owner=
    case "$path" in
      "$CASE_ROOT/Library/Application Support/host-monitor")
        metadata_key=state-dir
        default_owner=0:0
        ;;
      "$CASE_ROOT/Library/Application Support/host-monitor/config.json")
        metadata_key=retained-config
        default_owner=0:0
        ;;
      "$CASE_ROOT/Library/Application Support/host-monitor"/.config.json.*)
        metadata_key=config-temp
        default_owner=0:0
        ;;
      "$CASE_ROOT/var/log/host-monitor.log")
        metadata_key=log-file
        default_owner=0:0
        ;;
    esac
    [ -n "$metadata_key" ] || continue
    current_owner="$default_owner"
    if [ -f "$FILE_STATE/metadata.$metadata_key" ]; then
      IFS= read -r current_metadata <"$FILE_STATE/metadata.$metadata_key"
      current_owner="${current_metadata%:*}"
    fi
    printf '%s:%s\n' "$current_owner" "$normalized_mode" >"$FILE_STATE/metadata.$metadata_key"
  done
  exit 0
fi
count=0
if [ -f "$FILE_STATE/chmod-n-count" ]; then
  IFS= read -r count <"$FILE_STATE/chmod-n-count"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$FILE_STATE/chmod-n-count"
if [ -n "${FAIL_CHMOD_N_AT:-}" ] && [ "$count" -eq "$FAIL_CHMOD_N_AT" ]; then
  exit 72
fi
case "$2" in
  "$CASE_ROOT/var/db/host-monitor") acl_key=ownership-dir ;;
  "$CASE_ROOT/var/db/host-monitor/account-ownership") acl_key=ownership-marker ;;
  "$CASE_ROOT/var/db/host-monitor"/.account-ownership.*) acl_key=ownership-temp ;;
  "$CASE_ROOT/Library/Application Support/host-monitor") acl_key=state-dir ;;
  "$CASE_ROOT/Library/Application Support/host-monitor/config.json") acl_key=retained-config ;;
  "$CASE_ROOT/Library/Application Support/host-monitor"/.config.json.*) acl_key=config-temp ;;
  "$CASE_ROOT/var/log/host-monitor.log") acl_key=log-file ;;
  *) exit 65 ;;
esac
: >"$FILE_STATE/acl-cleared.$acl_key"
EOF

  cat >"$case_bin/plutil" <<'EOF'
#!/bin/sh
[ "${1:-}" = -lint ] && [ "$#" -eq 2 ] || exit 64
[ -f "$2" ] && grep -F '<plist ' "$2" >/dev/null
EOF

  cat >"$case_bin/chown" <<'EOF'
#!/bin/sh
count=0
if [ -f "$FILE_STATE/chown-count" ]; then
  IFS= read -r count <"$FILE_STATE/chown-count"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$FILE_STATE/chown-count"
printf '%s\n' "$*" >>"$FILE_STATE/chown-calls"
if [ -n "${FAIL_CHOWN_AT:-}" ] && [ "$count" -eq "$FAIL_CHOWN_AT" ]; then
  exit 71
fi
owner="$1"
shift
case "$owner" in
  root:wheel|0:0) numeric_owner=0:0 ;;
  _hostmonitor:_hostmonitor|450:450) numeric_owner=450:450 ;;
  *:*) numeric_owner="$owner" ;;
  *) exit 64 ;;
esac
for path do
  metadata_key=
  default_mode=
  case "$path" in
    "$CASE_ROOT/Library/Application Support/host-monitor")
      metadata_key=state-dir
      default_mode=700
      ;;
    "$CASE_ROOT/Library/Application Support/host-monitor/config.json")
      metadata_key=retained-config
      default_mode=600
      ;;
    "$CASE_ROOT/Library/Application Support/host-monitor"/.config.json.*)
      metadata_key=config-temp
      default_mode=600
      ;;
    "$CASE_ROOT/var/log/host-monitor.log")
      metadata_key=log-file
      default_mode=600
      ;;
  esac
  [ -n "$metadata_key" ] || continue
  current_mode="$default_mode"
  if [ -f "$FILE_STATE/metadata.$metadata_key" ]; then
    IFS= read -r current_metadata <"$FILE_STATE/metadata.$metadata_key"
    current_mode="${current_metadata##*:}"
  fi
  printf '%s:%s\n' "$numeric_owner" "$current_mode" >"$FILE_STATE/metadata.$metadata_key"
done
exit 0
EOF

  cat >"$case_bin/pgrep" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$FILE_STATE/pgrep-calls"
[ "$#" -eq 3 ] && { [ "$1" = -u ] || [ "$1" = -U ]; } &&
  [ "$3" = . ] || exit 64
process_marker="$LAUNCH_STATE/process-${1#-}.org.sarmg.hostmonitor"
if [ "${PGREP_HAS_PROCESS:-0}" -eq 1 ] || [ -e "$process_marker" ]; then
  printf '4242\n'
  exit 0
fi
exit 1
EOF

  cat >"$case_bin/pkill" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$FILE_STATE/pkill-calls"
[ "$#" -eq 4 ] && [ "$1" = -KILL ] &&
  { [ "$2" = -u ] || [ "$2" = -U ]; } && [ "$4" = . ] || exit 64
process_marker="$LAUNCH_STATE/process-${2#-}.org.sarmg.hostmonitor"
[ -e "$process_marker" ] || exit 1
if [ "${PKILL_LEAVES_PROCESS:-0}" -ne 1 ]; then
  rm -f "$process_marker"
fi
exit 0
EOF

  cat >"$case_bin/sleep" <<'EOF'
#!/bin/sh
exit 0
EOF

  cat >"$case_bin/pkgutil" <<'EOF'
#!/bin/sh
if [ "${PKGUTIL_INSTALLED:-0}" -eq 1 ]; then
  printf 'package-id: org.sarmg.hostmonitor\n'
  printf 'version: @HOST_MONITOR_PACKAGE_VERSION@\n'
  exit 0
fi
exit 1
EOF

  cat >"$case_bin/dscl" <<'EOF'
#!/bin/sh
set -eu

next_count() {
  counter="$1"
  count=0
  if [ -f "$counter" ]; then
    IFS= read -r count <"$counter"
  fi
  count=$((count + 1))
  printf '%s\n' "$count" >"$counter"
  printf '%s\n' "$count"
}

[ "${1:-}" = . ] || exit 64
operation="${2:-}"
case "$operation" in
  -list)
    record_type="$3"
    field="${4:-}"
    directory="$DSCL_STATE$record_type"
    for record_directory in "$directory"/*; do
      [ -d "$record_directory" ] || continue
      record_name="${record_directory##*/}"
      if [ -n "$field" ]; then
        [ -f "$record_directory/$field" ] || continue
        value="$(sed -n '1p' "$record_directory/$field")"
        printf '%s %s\n' "$record_name" "$value"
      else
        printf '%s\n' "$record_name"
      fi
    done
    ;;
  -create)
    count="$(next_count "$DSCL_STATE/create-count")"
    if [ -n "${FAIL_DSCL_CREATE_AT:-}" ] && [ "$count" -eq "$FAIL_DSCL_CREATE_AT" ]; then
      exit 72
    fi
    record="$3"
    record_directory="$DSCL_STATE$record"
    mkdir -p "$record_directory"
    if [ -n "${4:-}" ]; then
      printf '%s\n' "$5" >"$record_directory/$4"
    fi
    ;;
  -read)
    count="$(next_count "$DSCL_STATE/read-count")"
    if [ -n "${FAIL_DSCL_READ_AT:-}" ] && [ "$count" -eq "$FAIL_DSCL_READ_AT" ]; then
      exit 73
    fi
    record="$3"
    record_directory="$DSCL_STATE$record"
    [ -d "$record_directory" ] || exit 1
    if [ -n "${4:-}" ]; then
      [ -f "$record_directory/$4" ] || exit 1
      printf '%s: %s\n' "$4" "$(sed -n '1p' "$record_directory/$4")"
    fi
    ;;
  -delete)
    record="$3"
    record_directory="$DSCL_STATE$record"
    rm -rf "$record_directory"
    ;;
  *) exit 64 ;;
esac
EOF

  cat >"$case_bin/launchctl" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$LAUNCH_STATE/calls"
case "${1:-}" in
  enable|disable)
    count_file="$LAUNCH_STATE/$1-count"
    count=0
    if [ -f "$count_file" ]; then
      IFS= read -r count <"$count_file"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$count_file"
    if [ "$1" = disable ] && [ -n "${FAIL_DISABLE_FROM:-}" ] &&
      [ "$count" -ge "$FAIL_DISABLE_FROM" ]; then
      exit 78
    fi
    target="$2"
    label="${target#system/}"
    if [ "$1" = enable ]; then
      rm -f "$LAUNCH_STATE/disabled.$label"
    else
      : >"$LAUNCH_STATE/disabled.$label"
    fi
    ;;
  bootstrap)
    count=0
    if [ -f "$LAUNCH_STATE/bootstrap-count" ]; then
      IFS= read -r count <"$LAUNCH_STATE/bootstrap-count"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$LAUNCH_STATE/bootstrap-count"
    if [ -n "${FAIL_BOOTSTRAP_AT:-}" ] && [ "$count" -eq "$FAIL_BOOTSTRAP_AT" ]; then
      if [ "${FAIL_BOOTSTRAP_STARTS_CLIENT:-0}" = 1 ]; then
        : >"$LAUNCH_STATE/loaded.org.sarmg.hostmonitor"
        : >"$LAUNCH_STATE/process-u.org.sarmg.hostmonitor"
        : >"$LAUNCH_STATE/process-U.org.sarmg.hostmonitor"
      fi
      exit 74
    fi
    plist="$3"
    label="${plist##*/}"
    label="${label%.plist}"
    [ ! -e "$LAUNCH_STATE/disabled.$label" ] || exit 77
    : >"$LAUNCH_STATE/loaded.$label"
    if [ "$label" = org.sarmg.hostmonitor ]; then
      : >"$LAUNCH_STATE/process-u.$label"
      : >"$LAUNCH_STATE/process-U.$label"
    fi
    ;;
  bootout)
    count=0
    if [ -f "$LAUNCH_STATE/bootout-count" ]; then
      IFS= read -r count <"$LAUNCH_STATE/bootout-count"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$LAUNCH_STATE/bootout-count"
    if [ -n "${FAIL_BOOTOUT_AT:-}" ] && [ "$count" -eq "$FAIL_BOOTOUT_AT" ]; then
      exit 75
    fi
    target="$2"
    label="${target#system/}"
    rm -f "$LAUNCH_STATE/loaded.$label"
    if [ "$label" = org.sarmg.hostmonitor ] &&
      [ "${BOOTOUT_LEAVES_CLIENT_PROCESS:-0}" -ne 1 ]; then
      rm -f "$LAUNCH_STATE/process-u.$label" "$LAUNCH_STATE/process-U.$label"
    fi
    if [ "$label" = org.sarmg.hostmonitor.logrotate ] &&
      [ "${HELPER_RESTORES_CLIENT_ON_EXIT:-0}" -eq 1 ]; then
      : >"$LAUNCH_STATE/loaded.org.sarmg.hostmonitor"
      : >"$LAUNCH_STATE/process-u.org.sarmg.hostmonitor"
      : >"$LAUNCH_STATE/process-U.org.sarmg.hostmonitor"
    fi
    if [ "$label" = org.sarmg.hostmonitor ]; then
      : >"$FILE_STATE/mutable-checks-active"
      : >"$LAUNCH_STATE/client-bootout-completed"
    fi
    if [ "$label" = org.sarmg.hostmonitor ] &&
      [ "${RACE_CONFIG_SYMLINK_AFTER_CLIENT_BOOTOUT:-0}" -eq 1 ] &&
      [ ! -e "$FILE_STATE/race-config-injected" ]; then
      config="$CASE_ROOT/Library/Application Support/host-monitor/config.json"
      foreign="$CASE_ROOT/raced-config-target.json"
      [ -f "$config" ] && [ ! -L "$config" ] || exit 76
      mv "$config" "$foreign"
      ln -s "$foreign" "$config"
      : >"$FILE_STATE/race-config-injected"
    fi
    ;;
  kill)
    [ "$2" = SIGKILL ] || exit 64
    target="$3"
    label="${target#system/}"
    if [ "$label" = org.sarmg.hostmonitor ] &&
      [ "${LAUNCHCTL_KILL_LEAVES_CLIENT_PROCESS:-0}" -ne 1 ]; then
      rm -f "$LAUNCH_STATE/process-u.$label" "$LAUNCH_STATE/process-U.$label"
    fi
    ;;
  print)
    count=0
    if [ -f "$LAUNCH_STATE/print-count" ]; then
      IFS= read -r count <"$LAUNCH_STATE/print-count"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$LAUNCH_STATE/print-count"
    if [ -n "${FAIL_PRINT_AT:-}" ] && [ "$count" -eq "$FAIL_PRINT_AT" ]; then
      exit 79
    fi
    if [ "${FAIL_PRINT_AFTER_CLIENT_BOOTOUT:-0}" -eq 1 ] &&
      [ -e "$LAUNCH_STATE/client-bootout-completed" ] &&
      [ ! -e "$LAUNCH_STATE/post-bootout-print-failed" ]; then
      : >"$LAUNCH_STATE/post-bootout-print-failed"
      exit 79
    fi
    target="$2"
    label="${target#system/}"
    if [ -f "$LAUNCH_STATE/loaded.$label" ]; then
      exit 0
    fi
    printf 'Could not find service "%s" in domain for system\n' "$label" >&2
    exit 113
    ;;
  *) exit 64 ;;
esac
EOF

  chmod +x "$case_bin/id" "$case_bin/install" "$case_bin/stat" "$case_bin/ls" \
    "$case_bin/chmod" "$case_bin/chown" \
    "$case_bin/dscl" "$case_bin/launchctl" "$case_bin/pgrep" \
    "$case_bin/pkill" "$case_bin/sleep" \
    "$case_bin/pkgutil" "$case_bin/plutil"
}

run_postinstall() {
  run_case="$1"
  shift
  env \
    DSCL_STATE="$run_case/dscl" \
    FILE_STATE="$run_case" \
    LAUNCH_STATE="$run_case/launch" \
    CASE_ROOT="$run_case" \
    "$@" \
    "$run_case/postinstall"
}

start_prior_client() {
  rm -f "$case_dir/launch/disabled.org.sarmg.hostmonitor"
  touch "$case_dir/launch/loaded.org.sarmg.hostmonitor" "$case_dir/launch/process-u.org.sarmg.hostmonitor" "$case_dir/launch/process-U.org.sarmg.hostmonitor"
}

run_preinstall() {
  run_case="$1"
  shift
  env \
    FILE_STATE="$run_case" \
    LAUNCH_STATE="$run_case/launch" \
    CASE_ROOT="$run_case" \
    "$@" \
    "$run_case/preinstall"
}

reset_fault_counters() {
  reset_case="$1"
  rm -f "$reset_case/dscl/create-count" "$reset_case/dscl/read-count" \
    "$reset_case/chown-count" "$reset_case/chown-calls" \
    "$reset_case/chmod-n-count" "$reset_case"/acl-cleared.* \
    "$reset_case/pgrep-calls" "$reset_case/pkill-calls" \
    "$reset_case/race-config-injected" \
    "$reset_case/mutable-checks-active" \
    "$reset_case/launch/bootstrap-count" "$reset_case/launch/bootout-count" \
    "$reset_case/launch/enable-count" "$reset_case/launch/disable-count" \
    "$reset_case/launch/print-count" \
    "$reset_case/launch/client-bootout-completed" \
    "$reset_case/launch/post-bootout-print-failed"
}

assert_mock_metadata() {
  metadata_case="$1"
  expected_metadata="$2"
  metadata_path="$3"
  actual_metadata="$(
    env CASE_ROOT="$metadata_case" FILE_STATE="$metadata_case" \
      "$metadata_case/bin/stat" -f '%u:%g:%Mp:%Lp' "$metadata_path"
  )"
  [ "$actual_metadata" = "$expected_metadata" ] ||
    fail "unexpected metadata on $metadata_path: expected $expected_metadata, got $actual_metadata"
}

assert_mock_no_acl() {
  acl_case="$1"
  acl_path="$2"
  acl_listing="$(
    env CASE_ROOT="$acl_case" FILE_STATE="$acl_case" \
      "$acl_case/bin/ls" -lde "$acl_path"
  )"
  acl_line_count="$(printf '%s\n' "$acl_listing" | wc -l | tr -d '[:space:]')"
  [ "$acl_line_count" = 1 ] || fail "unexpected extended ACL on $acl_path"
  acl_permissions="${acl_listing%% *}"
  case "$acl_permissions" in
    ''|*+) fail "unexpected extended ACL marker on $acl_path" ;;
  esac
}

write_ownership_marker_ids() {
  marker_case="$1"
  marker_uid="$2"
  marker_primary_gid="$3"
  marker_group_gid="$4"
  mkdir -p "$marker_case/var/db/host-monitor"
  {
    printf 'format=@HOST_MONITOR_PACKAGE_VERSION@\n'
    printf 'user_created=1\n'
    printf 'user_uid=%s\n' "$marker_uid"
    printf 'user_primary_gid=%s\n' "$marker_primary_gid"
    printf 'group_created=1\n'
    printf 'group_gid=%s\n' "$marker_group_gid"
  } >"$marker_case/var/db/host-monitor/account-ownership"
}

write_valid_ownership_marker() {
  write_ownership_marker_ids "$1" 450 450 450
}

assert_payload_rejected_without_bootout() {
  payload_case="$1"
  payload_case_name="$2"
  shift 2
  : >"$payload_case/launch/loaded.org.sarmg.hostmonitor"
  : >"$payload_case/launch/loaded.org.sarmg.hostmonitor.logrotate"
  : >"$payload_case/launch/calls"
  if run_postinstall "$payload_case" "$@" >"$payload_case/failure.log" 2>&1; then
    fail "$payload_case_name unexpectedly succeeded"
  fi
  [ -e "$payload_case/launch/loaded.org.sarmg.hostmonitor" ] ||
    fail "$payload_case_name stopped the previously loaded Client"
  [ -e "$payload_case/launch/loaded.org.sarmg.hostmonitor.logrotate" ] ||
    fail "$payload_case_name stopped the previously loaded log-rotation helper"
  if grep -F 'bootout ' "$payload_case/launch/calls" >/dev/null 2>&1; then
    fail "$payload_case_name reached the launchd stop transaction"
  fi
  [ ! -d "$payload_case/dscl/Groups/_hostmonitor" ] ||
    fail "$payload_case_name created a service group before rejecting the payload"
  [ ! -d "$payload_case/dscl/Users/_hostmonitor" ] ||
    fail "$payload_case_name created a service user before rejecting the payload"
}

assert_preinstall_rejected_without_mutation() {
  preinstall_case="$1"
  preinstall_case_name="$2"
  shift 2
  : >"$preinstall_case/launch/calls"
  if run_preinstall "$preinstall_case" "$@" >"$preinstall_case/preinstall-failure.log" 2>&1; then
    fail "$preinstall_case_name preinstall unexpectedly succeeded"
  fi
  [ ! -s "$preinstall_case/launch/calls" ] ||
    fail "$preinstall_case_name preinstall called launchd"
  [ ! -e "$preinstall_case/var/db/host-monitor" ] ||
    fail "$preinstall_case_name preinstall mutated ownership bookkeeping"
  [ ! -d "$preinstall_case/dscl/Groups/_hostmonitor" ] ||
    fail "$preinstall_case_name preinstall created a service group"
  [ ! -d "$preinstall_case/dscl/Users/_hostmonitor" ] ||
    fail "$preinstall_case_name preinstall created a service user"
}

# Installer replaces files before postinstall. Every start-critical payload
# check must therefore fail while the old jobs remain loaded; rollback cannot
# recover the old files after they have already been replaced.
payload_failure=missing-client
while [ -n "$payload_failure" ]; do
  case_dir="$test_root/payload-$payload_failure"
  make_case "$case_dir"
  payload_environment=
  case "$payload_failure" in
    missing-client)
      rm "$case_dir/usr/local/libexec/host-monitor"
      next_payload_failure=redirected-client
      ;;
    redirected-client)
      rm "$case_dir/usr/local/libexec/host-monitor"
      ln -s /bin/true "$case_dir/usr/local/libexec/host-monitor"
      next_payload_failure=client-mode
      ;;
    client-mode)
      payload_environment=STAT_CLIENT_BINARY=0:0:700
      next_payload_failure=client-version
      ;;
    client-version)
      sed 's/host-monitor @HOST_MONITOR_PACKAGE_VERSION@/host-monitor 0.0.0/' \
        "$case_dir/usr/local/libexec/host-monitor" >"$case_dir/client.invalid"
      mv "$case_dir/client.invalid" "$case_dir/usr/local/libexec/host-monitor"
      chmod 0755 "$case_dir/usr/local/libexec/host-monitor"
      next_payload_failure=missing-helper
      ;;
    missing-helper)
      rm "$case_dir/usr/local/libexec/host-monitor-logrotate"
      next_payload_failure=helper-syntax
      ;;
    helper-syntax)
      printf '#!/bin/sh\nif then\n' >"$case_dir/usr/local/libexec/host-monitor-logrotate"
      chmod 0755 "$case_dir/usr/local/libexec/host-monitor-logrotate"
      next_payload_failure=helper-mode
      ;;
    helper-mode)
      payload_environment=STAT_LOGROTATE_HELPER=501:20:755
      next_payload_failure=missing-client-plist
      ;;
    missing-client-plist)
      rm "$case_dir/Library/LaunchDaemons/org.sarmg.hostmonitor.plist"
      next_payload_failure=invalid-client-plist
      ;;
    invalid-client-plist)
      printf 'not a plist\n' >"$case_dir/Library/LaunchDaemons/org.sarmg.hostmonitor.plist"
      next_payload_failure=client-plist-mode
      ;;
    client-plist-mode)
      payload_environment=STAT_CLIENT_PLIST=0:0:666
      next_payload_failure=missing-logrotate-plist
      ;;
    missing-logrotate-plist)
      rm "$case_dir/Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist"
      next_payload_failure=invalid-logrotate-plist
      ;;
    invalid-logrotate-plist)
      printf 'not a plist\n' >"$case_dir/Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist"
      next_payload_failure=missing-newsyslog
      ;;
    missing-newsyslog)
      rm "$case_dir/usr/local/share/host-monitor/newsyslog.conf"
      next_payload_failure=newsyslog-symlink
      ;;
    newsyslog-symlink)
      rm "$case_dir/usr/local/share/host-monitor/newsyslog.conf"
      ln -s /etc/hosts "$case_dir/usr/local/share/host-monitor/newsyslog.conf"
      next_payload_failure=newsyslog-mode
      ;;
    newsyslog-mode)
      payload_environment=STAT_NEWSYSLOG_CONFIG=0:0:666
      next_payload_failure=missing-package-config
      ;;
    missing-package-config)
      rm "$case_dir/usr/local/share/host-monitor/host-monitor.json.example"
      next_payload_failure=package-config-symlink
      ;;
    package-config-symlink)
      rm "$case_dir/usr/local/share/host-monitor/host-monitor.json.example"
      ln -s /etc/hosts "$case_dir/usr/local/share/host-monitor/host-monitor.json.example"
      next_payload_failure=package-config-mode
      ;;
    package-config-mode)
      payload_environment=STAT_PACKAGE_CONFIG=0:0:600
      next_payload_failure=package-config-version
      ;;
    package-config-version)
      sed 's/0.9.4/0.0.0/' \
        "$case_dir/usr/local/share/host-monitor/host-monitor.json.example" \
        >"$case_dir/config.example.invalid"
      mv "$case_dir/config.example.invalid" \
        "$case_dir/usr/local/share/host-monitor/host-monitor.json.example"
      next_payload_failure=missing-uninstall-helper
      ;;
    missing-uninstall-helper)
      rm "$case_dir/usr/local/share/host-monitor/uninstall.sh"
      next_payload_failure=uninstall-helper-symlink
      ;;
    uninstall-helper-symlink)
      rm "$case_dir/usr/local/share/host-monitor/uninstall.sh"
      ln -s /bin/true "$case_dir/usr/local/share/host-monitor/uninstall.sh"
      next_payload_failure=uninstall-helper-mode
      ;;
    uninstall-helper-mode)
      payload_environment=STAT_UNINSTALL_HELPER=0:0:700
      next_payload_failure=uninstall-helper-syntax
      ;;
    uninstall-helper-syntax)
      printf '#!/bin/sh\nif then\n' >"$case_dir/usr/local/share/host-monitor/uninstall.sh"
      chmod 0755 "$case_dir/usr/local/share/host-monitor/uninstall.sh"
      next_payload_failure=command-not-link
      ;;
    command-not-link)
      rm "$case_dir/usr/local/bin/host-monitor"
      : >"$case_dir/usr/local/bin/host-monitor"
      next_payload_failure=command-target
      ;;
    command-target)
      rm "$case_dir/usr/local/bin/host-monitor"
      ln -s ../libexec/not-host-monitor "$case_dir/usr/local/bin/host-monitor"
      next_payload_failure=command-mode
      ;;
    command-mode)
      payload_environment=STAT_CLIENT_COMMAND=501:20:755
      next_payload_failure=
      ;;
    *) fail "unknown payload failure case: $payload_failure" ;;
  esac
  if [ -n "$payload_environment" ]; then
    assert_payload_rejected_without_bootout "$case_dir" "$payload_failure" \
      "$payload_environment"
  else
    assert_payload_rejected_without_bootout "$case_dir" "$payload_failure"
  fi
  payload_failure="$next_payload_failure"
done

# Reject an unsafe existing path in preinstall, before Installer can make the
# same path appear safe by applying payload metadata.
while IFS='|' read -r privileged_case privileged_environment; do
  [ -n "$privileged_case" ] || continue
  case_dir="$test_root/preinstall-$privileged_case"
  make_case "$case_dir"
  assert_preinstall_rejected_without_mutation "$case_dir" "$privileged_case" \
    "$privileged_environment"
done <<'EOF'
local-prefix-owner|STAT_LOCAL_PREFIX=501:20:755
system-usr-acl|ACL_PATH=system-usr
local-prefix-acl|ACL_PATH=local-prefix
libexec-mode|STAT_LIBEXEC_DIR=0:0:777
libexec-acl|ACL_PATH=libexec-dir
command-dir-acl|ACL_PATH=command-dir
share-parent-acl|ACL_PATH=share-parent
share-dir-acl|ACL_PATH=share-dir
library-root-acl|ACL_PATH=library-root
library-root-tricky-allow|TRICKY_ALLOW_ACL_PATH=library-root
state-parent-owner|STAT_STATE_PARENT=501:80:755
state-parent-mode|STAT_STATE_PARENT=0:80:777
state-parent-acl|ACL_PATH=state-parent
state-parent-tricky-allow|TRICKY_ALLOW_ACL_PATH=state-parent
launch-daemon-dir-acl|ACL_PATH=launch-daemon-dir
log-dir-acl|ACL_PATH=log-dir
client-binary-acl|ACL_PATH=client-binary
client-binary-acl-inspection|FAIL_LS_PATH=client-binary
client-command-acl|ACL_PATH=client-command
logrotate-helper-acl|ACL_PATH=logrotate-helper
client-plist-acl|ACL_PATH=client-plist
logrotate-plist-acl|ACL_PATH=logrotate-plist
newsyslog-acl|ACL_PATH=newsyslog-config
package-config-acl|ACL_PATH=package-config
uninstall-helper-acl|ACL_PATH=uninstall-helper
EOF

case_dir="$test_root/preinstall-libexec-symlink"
make_case "$case_dir"
mv "$case_dir/usr/local/libexec" "$case_dir/foreign-libexec"
ln -s "$case_dir/foreign-libexec" "$case_dir/usr/local/libexec"
assert_preinstall_rejected_without_mutation "$case_dir" libexec-symlink

case_dir="$test_root/preinstall-client-symlink"
make_case "$case_dir"
rm "$case_dir/usr/local/libexec/host-monitor"
ln -s /bin/true "$case_dir/usr/local/libexec/host-monitor"
assert_preinstall_rejected_without_mutation "$case_dir" client-symlink

case_dir="$test_root/preinstall-safe-shared-modes"
make_case "$case_dir"
run_preinstall "$case_dir" \
  STAT_LOCAL_PREFIX=0:0:751 \
  STAT_LIBEXEC_DIR=0:0:711 \
  STAT_COMMAND_DIR=0:0:705 \
  STAT_SHARE_PARENT=0:0:745 \
  DENY_ACL_PATH=library-root \
  >"$case_dir/preinstall.log" 2>&1 ||
  fail 'preinstall rejected safe shared modes or a deny-only system ACL'

case_dir="$test_root/preinstall-missing-private-share"
make_case "$case_dir"
rm -rf "$case_dir/usr/local/share/host-monitor"
run_preinstall "$case_dir" >"$case_dir/preinstall.log" 2>&1 ||
  fail 'preinstall rejected a missing package-private descendant'

# Postinstall independently verifies the extracted root-owned payload and its
# complete replaceable path chain before account creation or launchd cutover.
while IFS='|' read -r privileged_case privileged_environment; do
  [ -n "$privileged_case" ] || continue
  case_dir="$test_root/privileged-$privileged_case"
  make_case "$case_dir"
  assert_payload_rejected_without_bootout "$case_dir" "$privileged_case" \
    "$privileged_environment"
  [ ! -e "$case_dir/var/db/host-monitor" ] ||
    fail "$privileged_case mutated ownership bookkeeping before rejecting the path"
done <<'EOF'
local-prefix-owner|STAT_LOCAL_PREFIX=501:20:755
system-usr-acl|ACL_PATH=system-usr
local-prefix-acl|ACL_PATH=local-prefix
libexec-mode|STAT_LIBEXEC_DIR=0:0:777
libexec-acl|ACL_PATH=libexec-dir
command-dir-acl|ACL_PATH=command-dir
share-parent-acl|ACL_PATH=share-parent
share-dir-acl|ACL_PATH=share-dir
library-root-acl|ACL_PATH=library-root
library-root-tricky-allow|TRICKY_ALLOW_ACL_PATH=library-root
state-parent-owner|STAT_STATE_PARENT=501:80:755
state-parent-mode|STAT_STATE_PARENT=0:80:777
state-parent-acl|ACL_PATH=state-parent
state-parent-tricky-allow|TRICKY_ALLOW_ACL_PATH=state-parent
launch-daemon-dir-acl|ACL_PATH=launch-daemon-dir
log-dir-acl|ACL_PATH=log-dir
client-binary-acl|ACL_PATH=client-binary
client-binary-acl-inspection|FAIL_LS_PATH=client-binary
client-command-acl|ACL_PATH=client-command
logrotate-helper-acl|ACL_PATH=logrotate-helper
client-plist-acl|ACL_PATH=client-plist
logrotate-plist-acl|ACL_PATH=logrotate-plist
newsyslog-acl|ACL_PATH=newsyslog-config
package-config-acl|ACL_PATH=package-config
uninstall-helper-acl|ACL_PATH=uninstall-helper
EOF

case_dir="$test_root/privileged-libexec-symlink"
make_case "$case_dir"
mv "$case_dir/usr/local/libexec" "$case_dir/foreign-libexec"
ln -s "$case_dir/foreign-libexec" "$case_dir/usr/local/libexec"
assert_payload_rejected_without_bootout "$case_dir" libexec-symlink

case_dir="$test_root/privileged-deny-only-system-acl"
make_case "$case_dir"
run_postinstall "$case_dir" DENY_ACL_PATH=library-root \
  >"$case_dir/postinstall.log" 2>&1 ||
  fail 'postinstall rejected a deny-only ACL on the system Library root'
[ -f "$case_dir/usr/local/share/host-monitor/host-monitor.json.example" ] ||
  fail 'successful install lost the root-owned package config template'
[ -f "$case_dir/Library/Application Support/host-monitor/config.json" ] ||
  fail 'successful install did not initialize the retained config'
[ ! -e "$case_dir/Library/Application Support/host-monitor/host-monitor.json.example" ] ||
  fail 'successful install copied the package template into mutable service state'
assert_mock_metadata "$case_dir" 0:0:0:644 \
  "$case_dir/usr/local/share/host-monitor/host-monitor.json.example"
assert_mock_metadata "$case_dir" 450:450:0:700 \
  "$case_dir/Library/Application Support/host-monitor"
assert_mock_metadata "$case_dir" 450:450:0:600 \
  "$case_dir/Library/Application Support/host-monitor/config.json"
assert_mock_metadata "$case_dir" 450:450:0:600 \
  "$case_dir/var/log/host-monitor.log"
assert_mock_no_acl "$case_dir" \
  "$case_dir/usr/local/share/host-monitor/host-monitor.json.example"
assert_mock_no_acl "$case_dir" \
  "$case_dir/Library/Application Support/host-monitor"
assert_mock_no_acl "$case_dir" \
  "$case_dir/Library/Application Support/host-monitor/config.json"
assert_mock_no_acl "$case_dir" "$case_dir/var/log/host-monitor.log"
grep -Fx "0:0 $case_dir/Library/Application Support/host-monitor" \
  "$case_dir/chown-calls" >/dev/null ||
  fail 'successful install did not lock state as root before mutation'
grep -Fx "450:450 $case_dir/Library/Application Support/host-monitor" \
  "$case_dir/chown-calls" >/dev/null ||
  fail 'successful install did not return state ownership to the service account'
grep -Fx 'disable system/org.sarmg.hostmonitor' "$case_dir/launch/calls" >/dev/null ||
  fail 'successful install did not disable Client autoload during mutable-state validation'
[ -e "$case_dir/launch/disabled.org.sarmg.hostmonitor" ] &&
  [ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] ||
  fail 'fresh install must leave Client stopped and disabled'

assert_recoverable() {
  recovery_case="$1"
  reset_fault_counters "$recovery_case"
  run_postinstall "$recovery_case" >"$recovery_case/recovery.log" 2>&1 ||
    fail "a clean rerun did not recover $recovery_case"
  [ -d "$recovery_case/dscl/Groups/_hostmonitor" ] || fail "group missing after recovery"
  [ -d "$recovery_case/dscl/Users/_hostmonitor" ] || fail "user missing after recovery"
  grep -Fx 'group_created=1' "$recovery_case/var/db/host-monitor/account-ownership" >/dev/null ||
    fail "group ownership was not committed after recovery"
  grep -Fx 'user_created=1' "$recovery_case/var/db/host-monitor/account-ownership" >/dev/null ||
    fail "user ownership was not committed after recovery"
}

# Security-sensitive roots must be rejected before postinstall creates an
# account or runs any state/log chown as root.
case_dir="$test_root/unsafe-ownership-symlink"
make_case "$case_dir"
mkdir -p "$case_dir/foreign-ownership" "$(dirname -- "$case_dir/var/db/host-monitor")"
: >"$case_dir/foreign-ownership/sentinel"
ln -s "$case_dir/foreign-ownership" "$case_dir/var/db/host-monitor"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked ownership directory unexpectedly succeeded'
fi
[ -e "$case_dir/foreign-ownership/sentinel" ] || fail 'ownership symlink target was modified'

case_dir="$test_root/unsafe-ownership-mode"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/host-monitor"
if run_postinstall "$case_dir" STAT_OWNERSHIP_DIR=501:20:777 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'foreign ownership directory metadata unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-ownership-special-mode"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/host-monitor"
if run_postinstall "$case_dir" STAT_OWNERSHIP_DIR=0:0:1:700 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'ownership directory with special permission bits unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-ownership-acl"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/host-monitor"
if run_postinstall "$case_dir" ACL_OWNERSHIP_DIR=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'ownership directory with an extended ACL unexpectedly succeeded'
fi
[ ! -e "$case_dir/acl-cleared.ownership-dir" ] ||
  fail 'postinstall normalized an ACL on a pre-existing ownership directory'

case_dir="$test_root/unsafe-ownership-ls-failure"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/host-monitor"
if run_postinstall "$case_dir" FAIL_LS_OWNERSHIP_DIR=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'ownership directory ACL inspection failure unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-new-ownership-acl-clear"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_CHMOD_N_AT=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'new ownership directory ACL-clear failure unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-marker-mode"
make_case "$case_dir"
write_valid_ownership_marker "$case_dir"
if run_postinstall "$case_dir" STAT_OWNERSHIP_MARKER=0:0:0:666 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'writable ownership marker unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-marker-acl"
make_case "$case_dir"
write_valid_ownership_marker "$case_dir"
if run_postinstall "$case_dir" ACL_OWNERSHIP_MARKER=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'ownership marker with an extended ACL unexpectedly succeeded'
fi
[ ! -e "$case_dir/acl-cleared.ownership-marker" ] ||
  fail 'postinstall normalized an ACL on a pre-existing ownership marker'

case_dir="$test_root/unsafe-marker-symlink"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/host-monitor"
: >"$case_dir/foreign-marker"
ln -s "$case_dir/foreign-marker" "$case_dir/var/db/host-monitor/account-ownership"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked ownership marker unexpectedly succeeded'
fi

for unsafe_marker_case in uid-zero uid-above-range gid-zero gid-above-range; do
  case_dir="$test_root/unsafe-marker-$unsafe_marker_case"
  make_case "$case_dir"
  case "$unsafe_marker_case" in
    uid-zero) write_ownership_marker_ids "$case_dir" 0 450 450 ;;
    uid-above-range) write_ownership_marker_ids "$case_dir" 451 450 450 ;;
    gid-zero) write_ownership_marker_ids "$case_dir" 450 0 0 ;;
    gid-above-range) write_ownership_marker_ids "$case_dir" 450 451 451 ;;
  esac
  if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
    fail "$unsafe_marker_case ownership marker unexpectedly succeeded"
  fi
  [ ! -e "$case_dir/pkill-calls" ] ||
    fail "$unsafe_marker_case ownership marker reached broad process cleanup"
  if grep -F 'bootout ' "$case_dir/launch/calls" >/dev/null 2>&1; then
    fail "$unsafe_marker_case ownership marker reached the launchd transaction"
  fi
done

case_dir="$test_root/unsafe-state-symlink"
make_case "$case_dir"
mkdir -p "$case_dir/foreign-state"
: >"$case_dir/foreign-state/sentinel"
rm -rf "$case_dir/Library/Application Support/host-monitor"
ln -s "$case_dir/foreign-state" "$case_dir/Library/Application Support/host-monitor"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked state directory unexpectedly succeeded'
fi
[ -e "$case_dir/foreign-state/sentinel" ] || fail 'state symlink target was modified'

case_dir="$test_root/unsafe-state-owner"
make_case "$case_dir"
mkdir -p "$case_dir/Library/Application Support/host-monitor"
write_valid_ownership_marker "$case_dir"
if run_postinstall "$case_dir" STAT_STATE_ROOT=501:20:755 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'foreign state ownership unexpectedly succeeded'
fi
[ ! -d "$case_dir/dscl/Groups/_hostmonitor" ] ||
  fail 'foreign state ownership was rejected only after creating an account'
[ ! -s "$case_dir/chown-calls" ] || fail 'foreign state ownership reached chown'

case_dir="$test_root/unsafe-unbound-root-state"
make_case "$case_dir"
mkdir -p "$case_dir/Library/Application Support/host-monitor"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'pre-existing root-owned state without an ownership proof unexpectedly succeeded'
fi
[ ! -d "$case_dir/dscl/Groups/_hostmonitor" ] ||
  fail 'unbound root-owned state was rejected only after creating an account'
[ ! -s "$case_dir/chown-calls" ] || fail 'unbound root-owned state reached chown'
[ ! -e "$case_dir/var/db/host-monitor" ] ||
  fail 'unbound root-owned state mutated ownership bookkeeping before rejection'

case_dir="$test_root/mutable-retained-config-symlink"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish state for the retained-config symlink case'
start_prior_client
reset_fault_counters "$case_dir"
rm "$case_dir/Library/Application Support/host-monitor/config.json"
cp "$case_dir/usr/local/share/host-monitor/host-monitor.json.example" \
  "$case_dir/foreign-config.json"
ln -s "$case_dir/foreign-config.json" \
  "$case_dir/Library/Application Support/host-monitor/config.json"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked retained config unexpectedly succeeded'
fi
[ -f "$case_dir/foreign-config.json" ] ||
  fail 'symlinked retained config target was modified'

case_dir="$test_root/stale-package-config"
make_case "$case_dir"
sed 's/0.9.4/0.0.0/' \
  "$case_dir/usr/local/share/host-monitor/host-monitor.json.example" \
  >"$case_dir/config.example.stale"
mv "$case_dir/config.example.stale" \
  "$case_dir/usr/local/share/host-monitor/host-monitor.json.example"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'stale package config unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-log-dir"
make_case "$case_dir"
if run_postinstall "$case_dir" STAT_LOG_DIR=0:0:777 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'world-writable log directory unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-log-dir-acl"
make_case "$case_dir"
if run_postinstall "$case_dir" ACL_PATH=log-dir \
  >"$case_dir/failure.log" 2>&1; then
  fail 'log directory with an extended ACL unexpectedly succeeded'
fi

case_dir="$test_root/mutable-log-symlink"
make_case "$case_dir"
: >"$case_dir/foreign-log"
ln -s "$case_dir/foreign-log" "$case_dir/var/log/host-monitor.log"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked log file unexpectedly succeeded'
fi

# None of the immutable-root validation failures above may have reached account creation.
# Mutable retained config/log checks intentionally happen later, after the old
# service has been stopped and its state directory has been locked.
for unsafe_case in "$test_root"/unsafe-* "$test_root"/stale-package-config; do
  [ ! -d "$unsafe_case/dscl/Groups/_hostmonitor" ] ||
    fail "unsafe root case created a service group: $unsafe_case"
  [ ! -d "$unsafe_case/dscl/Users/_hostmonitor" ] ||
    fail "unsafe root case created a service user: $unsafe_case"
  [ ! -s "$unsafe_case/chown-calls" ] ||
    fail "unsafe root case reached a privileged chown: $unsafe_case"
done

# Four group attributes followed by eight user attributes. Every failed dscl
# creation must remove only the incomplete record and leave a clean rerun path.
failure=1
while [ "$failure" -le 12 ]; do
  case_dir="$test_root/create-$failure"
  make_case "$case_dir"
  if run_postinstall "$case_dir" FAIL_DSCL_CREATE_AT="$failure" \
    >"$case_dir/failure.log" 2>&1; then
    fail "dscl create failure $failure unexpectedly succeeded"
  fi
  [ ! -d "$case_dir/dscl/Users/_hostmonitor" ] ||
    fail "dscl create failure $failure left a partial user"
  if [ "$failure" -le 4 ]; then
    [ ! -d "$case_dir/dscl/Groups/_hostmonitor" ] ||
      fail "dscl create failure $failure left a partial group"
    [ ! -e "$case_dir/var/db/host-monitor/account-ownership" ] ||
      fail "dscl create failure $failure left an ownership claim"
  else
    [ -d "$case_dir/dscl/Groups/_hostmonitor" ] ||
      fail "user creation failure $failure removed the committed group"
    grep -Fx 'user_created=0' "$case_dir/var/db/host-monitor/account-ownership" >/dev/null ||
      fail "user creation failure $failure corrupted the group-only marker"
  fi
  assert_recoverable "$case_dir"
  failure=$((failure + 1))
done

# Verification is part of the same transaction as record construction.
failure=1
while [ "$failure" -le 3 ]; do
  case_dir="$test_root/read-$failure"
  make_case "$case_dir"
  if run_postinstall "$case_dir" FAIL_DSCL_READ_AT="$failure" \
    >"$case_dir/failure.log" 2>&1; then
    fail "dscl verification failure $failure unexpectedly succeeded"
  fi
  [ ! -d "$case_dir/dscl/Users/_hostmonitor" ] ||
    fail "dscl verification failure $failure left a partial user"
  if [ "$failure" -eq 1 ]; then
    [ ! -d "$case_dir/dscl/Groups/_hostmonitor" ] ||
      fail "group verification failure left a partial group"
  else
    [ -d "$case_dir/dscl/Groups/_hostmonitor" ] ||
      fail "user verification failure removed the committed group"
  fi
  assert_recoverable "$case_dir"
  failure=$((failure + 1))
done

# Marker publication failures used to strand an unowned same-name record.
failure=1
while [ "$failure" -le 2 ]; do
  case_dir="$test_root/marker-$failure"
  make_case "$case_dir"
  if run_postinstall "$case_dir" FAIL_CHOWN_AT="$failure" \
    >"$case_dir/failure.log" 2>&1; then
    fail "marker publication failure $failure unexpectedly succeeded"
  fi
  [ ! -d "$case_dir/dscl/Users/_hostmonitor" ] ||
    fail "marker publication failure $failure left a partial user"
  if [ "$failure" -eq 1 ]; then
    [ ! -d "$case_dir/dscl/Groups/_hostmonitor" ] ||
      fail "group marker failure left a partial group"
  else
    [ -d "$case_dir/dscl/Groups/_hostmonitor" ] ||
      fail "user marker failure removed the committed group"
  fi
  assert_recoverable "$case_dir"
  failure=$((failure + 1))
done

case_dir="$test_root/marker-acl-clear-failure"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_CHMOD_N_AT=2 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'marker ACL-clear failure unexpectedly succeeded'
fi
[ ! -d "$case_dir/dscl/Groups/_hostmonitor" ] ||
  fail 'marker ACL-clear failure left a partial group'
[ ! -e "$case_dir/var/db/host-monitor/account-ownership" ] ||
  fail 'marker ACL-clear failure published an ownership claim'
if find "$case_dir/var/db/host-monitor" -name '.account-ownership.*' -print -quit |
  grep . >/dev/null; then
  fail 'marker ACL-clear failure left a temporary marker'
fi
assert_recoverable "$case_dir"

# A same-directory rename is the marker transaction's commit point. No fallible
# final-path ACL check may trigger account rollback after publication; the real
# macOS smoke test validates the published path independently.
case_dir="$test_root/marker-rename-commit"
make_case "$case_dir"
run_postinstall "$case_dir" FAIL_LS_OWNERSHIP_MARKER=1 \
  >"$case_dir/commit.log" 2>&1 ||
  fail 'postinstall performed a fallible marker check after the rename commit'
[ -d "$case_dir/dscl/Groups/_hostmonitor" ] &&
  [ -d "$case_dir/dscl/Users/_hostmonitor" ] ||
  fail 'marker rename commit test lost a service account'
grep -Fx 'user_created=1' "$case_dir/var/db/host-monitor/account-ownership" >/dev/null ||
  fail 'marker rename commit test did not publish the completed proof'

# A retained config or log is controlled by the service account between
# installations. ACL checks therefore belong inside the root-owned state lock,
# after both old jobs are stopped. Reject unexpected ACLs and fail closed when
# ACL metadata cannot be inspected; never normalize an untrusted leaf in place.
mutable_fault=state-acl
while [ -n "$mutable_fault" ]; do
  case_dir="$test_root/mutable-$mutable_fault"
  make_case "$case_dir"
  run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
    fail "could not establish jobs for mutable $mutable_fault"
start_prior_client
  reset_fault_counters "$case_dir"
  : >"$case_dir/launch/calls"
  mutable_expect_lock=1
  case "$mutable_fault" in
    state-acl)
      mutable_environment=POST_BOOTOUT_ACL_PATH=state-dir
      mutable_acl_key=state-dir
      mutable_expect_lock=0
      next_mutable_fault=state-acl-inspection
      ;;
    state-acl-inspection)
      mutable_environment=POST_BOOTOUT_FAIL_LS_PATH=state-dir
      mutable_acl_key=
      mutable_expect_lock=0
      next_mutable_fault=config-acl
      ;;
    config-acl)
      mutable_environment=POST_BOOTOUT_ACL_PATH=retained-config
      mutable_acl_key=retained-config
      next_mutable_fault=config-acl-inspection
      ;;
    config-acl-inspection)
      mutable_environment=POST_BOOTOUT_FAIL_LS_PATH=retained-config
      mutable_acl_key=
      next_mutable_fault=log-acl
      ;;
    log-acl)
      mutable_environment=POST_BOOTOUT_ACL_PATH=log-file
      mutable_acl_key=log-file
      next_mutable_fault=log-acl-inspection
      ;;
    log-acl-inspection)
      mutable_environment=POST_BOOTOUT_FAIL_LS_PATH=log-file
      mutable_acl_key=
      next_mutable_fault=
      ;;
    *) fail "unknown mutable ACL fault: $mutable_fault" ;;
  esac
  if run_postinstall "$case_dir" "$mutable_environment" \
    >"$case_dir/failure.log" 2>&1; then
    fail "mutable $mutable_fault unexpectedly succeeded"
  fi
  grep -Fx 'bootout system/org.sarmg.hostmonitor.logrotate' \
    "$case_dir/launch/calls" >/dev/null ||
    fail "mutable $mutable_fault was checked before stopping the helper"
  grep -Fx 'bootout system/org.sarmg.hostmonitor' \
    "$case_dir/launch/calls" >/dev/null ||
    fail "mutable $mutable_fault was checked before stopping the Client"
  grep -Fx 'disable system/org.sarmg.hostmonitor' \
    "$case_dir/launch/calls" >/dev/null ||
    fail "mutable $mutable_fault did not persist the fail-closed Client state"
  if [ "$mutable_expect_lock" -eq 1 ]; then
    grep -Fx "0:0 $case_dir/Library/Application Support/host-monitor" \
      "$case_dir/chown-calls" >/dev/null ||
      fail "mutable $mutable_fault was checked before locking state"
  elif grep -Fx "0:0 $case_dir/Library/Application Support/host-monitor" \
    "$case_dir/chown-calls" >/dev/null 2>&1; then
    fail "mutable $mutable_fault reached chown before state ACL validation"
  fi
  if [ -n "$mutable_acl_key" ] &&
    [ -e "$case_dir/acl-cleared.$mutable_acl_key" ]; then
    fail "mutable $mutable_fault normalized an untrusted leaf ACL"
  fi
  mutable_fault="$next_mutable_fault"
done

# Simulate the old Client replacing config.json immediately after its bootout.
# The root lock must contain the state directory and reject the new symlink
# without following it through any privileged config operation.
case_dir="$test_root/mutable-config-race"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish jobs for the retained-config race case'
start_prior_client
config_path="$case_dir/Library/Application Support/host-monitor/config.json"
config_checksum="$(cksum "$config_path" | awk '{ print $1 ":" $2 }')"
reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
if run_postinstall "$case_dir" RACE_CONFIG_SYMLINK_AFTER_CLIENT_BOOTOUT=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'post-bootout retained-config symlink race unexpectedly succeeded'
fi
[ -L "$config_path" ] || fail 'retained-config race injection did not occur'
[ -f "$case_dir/raced-config-target.json" ] ||
  fail 'retained-config race lost the original config inode'
raced_checksum="$(cksum "$case_dir/raced-config-target.json" | awk '{ print $1 ":" $2 }')"
[ "$raced_checksum" = "$config_checksum" ] ||
  fail 'a privileged operation modified the raced config target'
grep -Fx 'bootout system/org.sarmg.hostmonitor' "$case_dir/launch/calls" >/dev/null ||
  fail 'retained-config race was not injected after Client bootout'
grep -Fx -- '-u 450 .' "$case_dir/pgrep-calls" >/dev/null ||
  fail 'postinstall did not check for lingering service processes after bootout'
grep -Fx -- '-U 450 .' "$case_dir/pgrep-calls" >/dev/null ||
  fail 'postinstall did not check the real service UID after bootout'
grep -Fx "0:0 $case_dir/Library/Application Support/host-monitor" \
  "$case_dir/chown-calls" >/dev/null ||
  fail 'postinstall did not lock state before rejecting the raced config'
if grep -F " $config_path" "$case_dir/chown-calls" >/dev/null 2>&1; then
  fail 'postinstall ran privileged chown on the raced config leaf'
fi

# A detached old process under the service UID would still be able to mutate
# state after launchd bootout. Refuse the lock transaction, then kill and verify
# both effective-UID and real-UID views during the unsafe rollback.
case_dir="$test_root/mutable-lingering-process"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish jobs for the lingering-process case'
start_prior_client
reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
if run_postinstall "$case_dir" BOOTOUT_LEAVES_CLIENT_PROCESS=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'postinstall accepted a lingering service process after bootout'
fi
grep -Fx -- '-u 450 .' "$case_dir/pgrep-calls" >/dev/null ||
  fail 'lingering-process case did not inspect the service UID'
grep -Fx -- '-KILL -u 450 .' "$case_dir/pkill-calls" >/dev/null ||
  fail 'unsafe rollback did not kill the old effective-UID process'
grep -Fx -- '-KILL -U 450 .' "$case_dir/pkill-calls" >/dev/null ||
  fail 'unsafe rollback did not kill the old real-UID process'
[ ! -e "$case_dir/launch/process-u.org.sarmg.hostmonitor" ] &&
  [ ! -e "$case_dir/launch/process-U.org.sarmg.hostmonitor" ] ||
  fail 'unsafe rollback left an old service-UID process alive'
[ -e "$case_dir/launch/disabled.org.sarmg.hostmonitor" ] &&
  [ -e "$case_dir/launch/disabled.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'unsafe rollback did not leave both labels disabled'
if grep -Fx "0:0 $case_dir/Library/Application Support/host-monitor" \
  "$case_dir/chown-calls" >/dev/null 2>&1; then
  fail 'postinstall locked state while a service process could still mutate it'
fi

# Fault injection leaves a partial Client job during failed helper registration;
# rollback must still remove it, although normal fresh installs never start Client.
case_dir="$test_root/bootstrap-2"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 FAIL_BOOTSTRAP_STARTS_CLIENT=1 >"$case_dir/failure.log" 2>&1; then
  fail "second launchctl bootstrap failure unexpectedly succeeded"
fi
[ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] ||
  fail "Client remained loaded after helper bootstrap failed"
[ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor.logrotate" ] ||
  fail "failed helper bootstrap left a loaded helper"
grep -Fx 'bootout system/org.sarmg.hostmonitor' "$case_dir/launch/calls" >/dev/null ||
  fail "postinstall did not boot out the Client after helper bootstrap failed"
[ -d "$case_dir/dscl/Groups/_hostmonitor" ] && [ -d "$case_dir/dscl/Users/_hostmonitor" ] ||
  fail "a launchd failure removed fully committed service accounts"
assert_recoverable "$case_dir"

# A bootstrap failure combined with a failed first rollback bootout must still
# disable KeepAlive first, force-stop the Client, retry bootout, and remove both
# effective-UID and real-UID residual processes before returning failure.
case_dir="$test_root/bootstrap-rollback-bootout-failure"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 FAIL_BOOTSTRAP_STARTS_CLIENT=1 FAIL_BOOTOUT_AT=1 \
  BOOTOUT_LEAVES_CLIENT_PROCESS=1 LAUNCHCTL_KILL_LEAVES_CLIENT_PROCESS=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'combined bootstrap/rollback-bootout failure unexpectedly succeeded'
fi
[ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] ||
  fail 'rollback bootout retry left the Client label loaded'
[ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'combined rollback failure left the helper label loaded'
[ ! -e "$case_dir/launch/process-u.org.sarmg.hostmonitor" ] &&
  [ ! -e "$case_dir/launch/process-U.org.sarmg.hostmonitor" ] ||
  fail 'combined rollback failure left a service-UID process running'
[ -e "$case_dir/launch/disabled.org.sarmg.hostmonitor" ] &&
  [ -e "$case_dir/launch/disabled.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'combined rollback failure did not persistently disable both labels'
assert_calls_in_order_after "$case_dir/launch/calls" \
  "bootstrap system $case_dir/Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist" \
  'disable system/org.sarmg.hostmonitor.logrotate' \
  'disable system/org.sarmg.hostmonitor' \
  'print system/org.sarmg.hostmonitor.logrotate' \
  'print system/org.sarmg.hostmonitor' \
  'bootout system/org.sarmg.hostmonitor' \
  'print system/org.sarmg.hostmonitor' \
  'kill SIGKILL system/org.sarmg.hostmonitor' \
  'bootout system/org.sarmg.hostmonitor' \
  'print system/org.sarmg.hostmonitor'
grep -Fx -- '-KILL -u 450 .' "$case_dir/pkill-calls" >/dev/null ||
  fail 'rollback did not kill the effective-UID residual process'
grep -Fx -- '-KILL -U 450 .' "$case_dir/pkill-calls" >/dev/null ||
  fail 'rollback did not kill the real-UID residual process'

# A nonzero print caused by launchd/RPC inspection failure is not proof that a
# label is absent. Even if a later retry confirms absence, report the rollback
# as incomplete rather than silently treating the failed inspection as success.
case_dir="$test_root/bootstrap-rollback-print-failure"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 FAIL_BOOTSTRAP_STARTS_CLIENT=1 \
  FAIL_PRINT_AFTER_CLIENT_BOOTOUT=1 >"$case_dir/failure.log" 2>&1; then
  fail 'rollback launchd inspection failure unexpectedly succeeded'
fi
grep -F 'Could not inspect system/org.sarmg.hostmonitor (launchctl status 79)' \
  "$case_dir/failure.log" >/dev/null ||
  fail 'rollback launchd inspection failure was mistaken for an absent label'
grep -F 'host-monitor rollback cleanup is incomplete' "$case_dir/failure.log" >/dev/null ||
  fail 'rollback launchd inspection failure was not marked incomplete'
[ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] ||
  fail 'inspection-failure retry left the Client label loaded'
[ -e "$case_dir/launch/disabled.org.sarmg.hostmonitor" ] &&
  [ -e "$case_dir/launch/disabled.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'inspection-failure rollback did not leave both labels disabled'

# If even repeated pkill cannot remove a residual process, rollback must say
# that cleanup is incomplete instead of claiming the failed cutover is closed.
case_dir="$test_root/bootstrap-rollback-residual-process"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 FAIL_BOOTSTRAP_STARTS_CLIENT=1 FAIL_BOOTOUT_AT=1 \
  BOOTOUT_LEAVES_CLIENT_PROCESS=1 LAUNCHCTL_KILL_LEAVES_CLIENT_PROCESS=1 \
  PKILL_LEAVES_PROCESS=1 >"$case_dir/failure.log" 2>&1; then
  fail 'unkillable rollback process case unexpectedly succeeded'
fi
grep -F 'host-monitor rollback cleanup is incomplete' "$case_dir/failure.log" >/dev/null ||
  fail 'unkillable rollback process was not reported as incomplete cleanup'
[ -e "$case_dir/launch/process-u.org.sarmg.hostmonitor" ] &&
  [ -e "$case_dir/launch/process-U.org.sarmg.hostmonitor" ] ||
  fail 'unkillable rollback-process fixture did not retain both UID views'

# Persistent disable errors are also incomplete cleanup even if forced bootout
# removes the current process: reboot safety has not been established.
case_dir="$test_root/bootstrap-rollback-disable-failure"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 FAIL_BOOTSTRAP_STARTS_CLIENT=1 FAIL_DISABLE_FROM=4 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'rollback disable failure unexpectedly succeeded'
fi
grep -F 'host-monitor rollback cleanup is incomplete' "$case_dir/failure.log" >/dev/null ||
  fail 'rollback disable failure was not reported as incomplete cleanup'
[ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] ||
  fail 'rollback disable failure left the current Client loaded'

# preinstall deliberately leaves the previous jobs running. postinstall stops
# them only after immutable validation succeeds. Once the replacement Client
# has been allowed to access mutable state, a partial bootstrap failure is
# fail-closed: half-registered jobs are removed and prior jobs stay stopped.
case_dir="$test_root/reinstall-postinstall-failure"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish the initial loaded launchd jobs'
start_prior_client
reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
run_preinstall "$case_dir" PKGUTIL_INSTALLED=1 >"$case_dir/preinstall.log" 2>&1 ||
  fail 'same-version preinstall unexpectedly failed'
[ -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] ||
  fail 'preinstall stopped the previous Client before payload validation'
[ -e "$case_dir/launch/loaded.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'preinstall stopped the previous helper before payload validation'
if grep -F 'bootout ' "$case_dir/launch/calls" >/dev/null; then
  fail 'preinstall issued a launchd bootout'
fi

# A replacement that fails payload validation must likewise leave both old
# jobs untouched because postinstall has not reached its stop transaction.
sed 's/0.9.4/0.0.0/' \
  "$case_dir/usr/local/share/host-monitor/host-monitor.json.example" \
  >"$case_dir/config.example.invalid"
mv "$case_dir/config.example.invalid" \
  "$case_dir/usr/local/share/host-monitor/host-monitor.json.example"
if run_postinstall "$case_dir" >"$case_dir/validation-failure.log" 2>&1; then
  fail 'replacement with an invalid package config unexpectedly succeeded'
fi
[ -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] ||
  fail 'payload validation failure stopped the previous Client'
[ -e "$case_dir/launch/loaded.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'payload validation failure stopped the previous helper'
if grep -F 'bootout ' "$case_dir/launch/calls" >/dev/null; then
  fail 'payload validation failure reached the launchd stop transaction'
fi
sed 's/0.0.0/0.9.4/' \
  "$case_dir/usr/local/share/host-monitor/host-monitor.json.example" \
  >"$case_dir/config.example.restored"
mv "$case_dir/config.example.restored" \
  "$case_dir/usr/local/share/host-monitor/host-monitor.json.example"

reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 FAIL_BOOTSTRAP_STARTS_CLIENT=1 \
  >"$case_dir/postinstall-failure.log" 2>&1; then
  fail 'replacement helper bootstrap failure unexpectedly succeeded'
fi
[ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] ||
  fail 'failed replacement restarted the Client without revalidating mutable state'
[ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'failed replacement restarted the helper without revalidating mutable state'
grep -Fx 'disable system/org.sarmg.hostmonitor' "$case_dir/launch/calls" >/dev/null ||
  fail 'failed replacement did not persistently disable the Client label'

# The rotation helper may have the Client temporarily stopped and restore it
# from its exit trap. postinstall must decide whether to stop Client only after
# helper bootout has completed, not from the stale pre-helper snapshot.
case_dir="$test_root/reinstall-helper-restores-client"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish jobs for the helper restore race case'
start_prior_client
reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
rm -f "$case_dir/launch/loaded.org.sarmg.hostmonitor"
run_postinstall "$case_dir" HELPER_RESTORES_CLIENT_ON_EXIT=1 \
  >"$case_dir/reinstall.log" 2>&1 ||
  fail 'postinstall did not contain the helper Client-restore race'
grep -Fx 'bootout system/org.sarmg.hostmonitor.logrotate' "$case_dir/launch/calls" >/dev/null ||
  fail 'helper restore race did not stop the helper'
grep -Fx 'bootout system/org.sarmg.hostmonitor' "$case_dir/launch/calls" >/dev/null ||
  fail 'helper restore race left the restored Client running during state mutation'
[ ! -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] &&
  [ -e "$case_dir/launch/disabled.org.sarmg.hostmonitor" ] &&
  [ -e "$case_dir/launch/loaded.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'helper restore race did not leave the validated Client stopped'

# A launchd inspection error after normal bootout is not an absent-label
# result. Fail before locking mutable state, then restore only from later
# explicit absent results in the safe rollback path.
case_dir="$test_root/reinstall-post-bootout-print-failure"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish jobs for the normal print-failure case'
start_prior_client
reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
if run_postinstall "$case_dir" FAIL_PRINT_AFTER_CLIENT_BOOTOUT=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'normal cutover launchd inspection failure unexpectedly succeeded'
fi
grep -F 'Could not inspect system/org.sarmg.hostmonitor (launchctl status 79)' \
  "$case_dir/failure.log" >/dev/null ||
  fail 'normal cutover inspection error was mistaken for an absent Client'
if grep -Fx "0:0 $case_dir/Library/Application Support/host-monitor" \
  "$case_dir/chown-calls" >/dev/null 2>&1; then
  fail 'normal cutover inspection failure reached the mutable state lock'
fi
[ -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] &&
  [ -e "$case_dir/launch/loaded.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'safe rollback did not restore jobs after the cutover inspection failed'

# The stop phase inside postinstall is also failure-atomic: if stopping the
# second job fails, the validated helper stopped first is re-registered.
case_dir="$test_root/postinstall-bootout-failure"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish jobs for the postinstall bootout rollback case'
start_prior_client
reset_fault_counters "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTOUT_AT=2 \
  >"$case_dir/postinstall-failure.log" 2>&1; then
  fail 'second postinstall bootout failure unexpectedly succeeded'
fi
[ -e "$case_dir/launch/loaded.org.sarmg.hostmonitor" ] ||
  fail 'postinstall bootout failure lost the previous Client job'
[ -e "$case_dir/launch/loaded.org.sarmg.hostmonitor.logrotate" ] ||
  fail 'postinstall bootout failure did not re-register the helper job'

echo 'macOS postinstall failure-atomicity tests: ok'
