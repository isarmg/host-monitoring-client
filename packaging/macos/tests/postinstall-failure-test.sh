#!/bin/sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
postinstall="$script_dir/../scripts/postinstall"
preinstall="$script_dir/../scripts/preinstall"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/xsoc-macos-postinstall.XXXXXX")"

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
  case_state="$case_dir/Library/Application Support/xsoc"
  case_ownership="$case_dir/var/db/xsoc"
  case_log="$case_dir/var/log/xsoc.log"
  case_client_binary="$case_dir/usr/local/libexec/xsoc"
  case_client_command="$case_dir/usr/local/bin/xsoc"
  case_logrotate_helper="$case_dir/usr/local/libexec/xsoc-logrotate"
  case_client_plist="$case_dir/Library/LaunchDaemons/org.sarmg.xsoc.plist"
  case_logrotate_plist="$case_dir/Library/LaunchDaemons/org.sarmg.xsoc.logrotate.plist"
  case_share="$case_dir/usr/local/share/xsoc"
  case_package_config="$case_share/xsoc.json.example"
  case_newsyslog_config="$case_share/newsyslog.conf"
  case_uninstall_helper="$case_share/uninstall.sh"
  mkdir -p "$case_bin" "$case_state_parent" "$case_dir/dscl/Groups" "$case_dir/dscl/Users" \
    "$case_dir/launch" "$(dirname -- "$case_log")" "$(dirname -- "$case_client_plist")" \
    "$(dirname -- "$case_client_binary")" "$(dirname -- "$case_client_command")" "$case_share"
  {
    printf '{\n'
    printf '  "application_version": "1.0.0",\n'
    printf '  "server_url": null\n'
    printf '}\n'
  } >"$case_package_config"
  cat >"$case_client_binary" <<'EOF'
#!/bin/sh
[ "${1:-}" = --version ] || exit 64
printf 'xsoc @XSOC_PACKAGE_VERSION@\n'
EOF
  cat >"$case_logrotate_helper" <<'EOF'
#!/bin/sh
exit 0
EOF
  cat >"$case_client_plist" <<'EOF'
<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>org.sarmg.xsoc</string></dict></plist>
EOF
  cat >"$case_logrotate_plist" <<'EOF'
<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>org.sarmg.xsoc.logrotate</string></dict></plist>
EOF
  printf '/var/log/xsoc.log _xsoc:_xsoc 600 7 10240 * JN\n' \
    >"$case_newsyslog_config"
  cat >"$case_uninstall_helper" <<'EOF'
#!/bin/sh
exit 0
EOF
  mkdir -p "${case_client_binary%/*}/xsoc-smartmontools/bin" "${case_client_binary%/*}/xsoc-smartmontools/share" "$case_share/smartmontools"
  printf '#!/bin/sh\nexit 0\n' > "${case_client_binary%/*}/xsoc-smartmontools/bin/smartctl"
  chmod 0755 "${case_client_binary%/*}/xsoc-smartmontools/bin/smartctl"
  touch "${case_client_binary%/*}/xsoc-smartmontools/share/drivedb.h"
  for smart_info in COPYING smartmontools-7.5.tar.gz build-smartmontools.sh BUILD-INFO.txt; do
    touch "$case_share/smartmontools/$smart_info"
  done
  chmod 0755 "$case_client_binary" "$case_logrotate_helper" "$case_uninstall_helper"
  ln -s ../libexec/xsoc "$case_client_command"

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
if [ "$destination" = "$CASE_ROOT/Library/Application Support/xsoc" ]; then
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
  "$CASE_ROOT/usr/local/share/xsoc")
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
  "$CASE_ROOT/usr/local/libexec/xsoc")
    metadata="${STAT_CLIENT_BINARY:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/bin/xsoc")
    metadata="${STAT_CLIENT_COMMAND:-0:0:755}"
    ;;
  "$CASE_ROOT/usr/local/libexec/xsoc-logrotate")
    metadata="${STAT_LOGROTATE_HELPER:-0:0:755}"
    ;;
  "$CASE_ROOT/Library/LaunchDaemons/org.sarmg.xsoc.plist")
    metadata="${STAT_CLIENT_PLIST:-0:0:644}"
    ;;
  "$CASE_ROOT/Library/LaunchDaemons/org.sarmg.xsoc.logrotate.plist")
    metadata="${STAT_LOGROTATE_PLIST:-0:0:644}"
    ;;
  "$CASE_ROOT/usr/local/share/xsoc/newsyslog.conf")
    metadata="${STAT_NEWSYSLOG_CONFIG:-0:0:644}"
    ;;
  "$CASE_ROOT/usr/local/share/xsoc/xsoc.json.example")
    metadata="${STAT_PACKAGE_CONFIG:-0:0:644}"
    ;;
  "$CASE_ROOT/usr/local/share/xsoc/uninstall.sh")
    metadata="${STAT_UNINSTALL_HELPER:-0:0:755}"
    ;;
  "$CASE_ROOT/var/db/xsoc")
    metadata="${STAT_OWNERSHIP_DIR:-0:0:700}"
    ;;
  "$CASE_ROOT/var/db/xsoc/account-ownership"|\
  "$CASE_ROOT/var/db/xsoc"/.account-ownership.*)
    metadata="${STAT_OWNERSHIP_MARKER:-0:0:600}"
    ;;
  "$CASE_ROOT/Library/Application Support/xsoc")
    if [ -n "${STAT_STATE_ROOT:-}" ]; then
      metadata="$STAT_STATE_ROOT"
    elif [ -f "$FILE_STATE/metadata.state-dir" ]; then
      IFS= read -r metadata <"$FILE_STATE/metadata.state-dir"
    else
      metadata="0:0:755"
    fi
    ;;
  "$CASE_ROOT/Library/Application Support/xsoc/config.json")
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
  "$CASE_ROOT/Library/Application Support/xsoc"/.config.json.*)
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
  "$CASE_ROOT/var/log/xsoc.log")
    if [ -n "${STAT_LOG_FILE:-}" ]; then
      metadata="$STAT_LOG_FILE"
    elif [ -f "$FILE_STATE/metadata.log-file" ]; then
      IFS= read -r metadata <"$FILE_STATE/metadata.log-file"
    else
      metadata="0:0:600"
    fi
    ;;
  "$CASE_ROOT/usr/local/libexec/xsoc-smartmontools"|"$CASE_ROOT/usr/local/libexec/xsoc-smartmontools/bin"|"$CASE_ROOT/usr/local/libexec/xsoc-smartmontools/share"|"$CASE_ROOT/usr/local/share/xsoc/smartmontools") metadata="${STAT_SMART_DIRECTORY:-0:0:755}" ;;
  "$CASE_ROOT/usr/local/libexec/xsoc-smartmontools/bin/smartctl") metadata="${STAT_SMART_BINARY:-0:0:755}" ;;
  "$CASE_ROOT/usr/local/libexec/xsoc-smartmontools/share/drivedb.h"|"$CASE_ROOT/usr/local/share/xsoc/smartmontools/"*) metadata="0:0:644" ;;
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
  "$CASE_ROOT/usr/local/share/xsoc")
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
  "$CASE_ROOT/usr/local/libexec/xsoc") acl_key=client-binary ;;
  "$CASE_ROOT/usr/local/bin/xsoc")
    acl_key=client-command
    permissions=lrwxr-xr-x
    ;;
  "$CASE_ROOT/usr/local/libexec/xsoc-logrotate") acl_key=logrotate-helper ;;
  "$CASE_ROOT/Library/LaunchDaemons/org.sarmg.xsoc.plist") acl_key=client-plist ;;
  "$CASE_ROOT/Library/LaunchDaemons/org.sarmg.xsoc.logrotate.plist")
    acl_key=logrotate-plist
    ;;
  "$CASE_ROOT/usr/local/share/xsoc/newsyslog.conf") acl_key=newsyslog-config ;;
  "$CASE_ROOT/usr/local/share/xsoc/xsoc.json.example") acl_key=package-config ;;
  "$CASE_ROOT/usr/local/share/xsoc/uninstall.sh") acl_key=uninstall-helper ;;
  "$CASE_ROOT/usr/local/libexec/xsoc-smartmontools"|"$CASE_ROOT/usr/local/libexec/xsoc-smartmontools/bin"|"$CASE_ROOT/usr/local/libexec/xsoc-smartmontools/share"|"$CASE_ROOT/usr/local/share/xsoc/smartmontools")
    acl_key=smart-directory
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/usr/local/libexec/xsoc-smartmontools/bin/smartctl") acl_key=smart-binary ;;
  "$CASE_ROOT/usr/local/libexec/xsoc-smartmontools/share/drivedb.h"|"$CASE_ROOT/usr/local/share/xsoc/smartmontools/"*) acl_key=smart-source ;;
  "$CASE_ROOT/var/log")
    acl_key=log-dir
    permissions=drwxr-xr-x
    ;;
  "$CASE_ROOT/Library/Application Support/xsoc")
    acl_key=state-dir
    permissions=drwx------
    ;;
  "$CASE_ROOT/Library/Application Support/xsoc/config.json")
    acl_key=retained-config
    ;;
  "$CASE_ROOT/Library/Application Support/xsoc"/.config.json.*)
    acl_key=config-temp
    ;;
  "$CASE_ROOT/var/log/xsoc.log")
    acl_key=log-file
    ;;
  "$CASE_ROOT/var/db/xsoc")
    acl_key=ownership-dir
    permissions=drwx------
    acl_requested="${ACL_OWNERSHIP_DIR:-0}"
    ls_failure="${FAIL_LS_OWNERSHIP_DIR:-0}"
    ;;
  "$CASE_ROOT/var/db/xsoc/account-ownership")
    acl_key=ownership-marker
    acl_requested="${ACL_OWNERSHIP_MARKER:-0}"
    ls_failure="${FAIL_LS_OWNERSHIP_MARKER:-0}"
    ;;
  "$CASE_ROOT/var/db/xsoc"/.account-ownership.*)
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
      "$CASE_ROOT/Library/Application Support/xsoc")
        metadata_key=state-dir
        default_owner=0:0
        ;;
      "$CASE_ROOT/Library/Application Support/xsoc/config.json")
        metadata_key=retained-config
        default_owner=0:0
        ;;
      "$CASE_ROOT/Library/Application Support/xsoc"/.config.json.*)
        metadata_key=config-temp
        default_owner=0:0
        ;;
      "$CASE_ROOT/var/log/xsoc.log")
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
  "$CASE_ROOT/var/db/xsoc") acl_key=ownership-dir ;;
  "$CASE_ROOT/var/db/xsoc/account-ownership") acl_key=ownership-marker ;;
  "$CASE_ROOT/var/db/xsoc"/.account-ownership.*) acl_key=ownership-temp ;;
  "$CASE_ROOT/Library/Application Support/xsoc") acl_key=state-dir ;;
  "$CASE_ROOT/Library/Application Support/xsoc/config.json") acl_key=retained-config ;;
  "$CASE_ROOT/Library/Application Support/xsoc"/.config.json.*) acl_key=config-temp ;;
  "$CASE_ROOT/var/log/xsoc.log") acl_key=log-file ;;
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
  _xsoc:_xsoc|450:450) numeric_owner=450:450 ;;
  *:*) numeric_owner="$owner" ;;
  *) exit 64 ;;
esac
for path do
  metadata_key=
  default_mode=
  case "$path" in
    "$CASE_ROOT/Library/Application Support/xsoc")
      metadata_key=state-dir
      default_mode=700
      ;;
    "$CASE_ROOT/Library/Application Support/xsoc/config.json")
      metadata_key=retained-config
      default_mode=600
      ;;
    "$CASE_ROOT/Library/Application Support/xsoc"/.config.json.*)
      metadata_key=config-temp
      default_mode=600
      ;;
    "$CASE_ROOT/var/log/xsoc.log")
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
process_marker="$LAUNCH_STATE/process-${1#-}.org.sarmg.xsoc"
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
process_marker="$LAUNCH_STATE/process-${2#-}.org.sarmg.xsoc"
[ -e "$process_marker" ] || exit 1
if [ "${PKILL_LEAVES_PROCESS:-0}" -ne 1 ]; then
  rm -f "$process_marker"
fi
exit 0
EOF

  cat >"$case_bin/sleep" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$FILE_STATE/sleep-calls"
exit 0
EOF

  cat >"$case_bin/pkgutil" <<'EOF'
#!/bin/sh
if [ "${PKGUTIL_INSTALLED:-0}" -eq 1 ]; then
  printf 'package-id: org.sarmg.xsoc\n'
  printf 'version: @XSOC_PACKAGE_VERSION@\n'
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
        : >"$LAUNCH_STATE/loaded.org.sarmg.xsoc"
        : >"$LAUNCH_STATE/process-u.org.sarmg.xsoc"
        : >"$LAUNCH_STATE/process-U.org.sarmg.xsoc"
      fi
      exit 74
    fi
    plist="$3"
    label="${plist##*/}"
    label="${label%.plist}"
    [ ! -e "$LAUNCH_STATE/disabled.$label" ] || exit 77
    : >"$LAUNCH_STATE/loaded.$label"
    if [ "$label" = org.sarmg.xsoc ]; then
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
    delayed_absence="${BOOTOUT_DELAYED_ABSENCE_PRINTS:-0}"
    case "$delayed_absence" in
      ''|*[!0-9]*) exit 64 ;;
    esac
    if [ "$delayed_absence" -gt 0 ] && [ -e "$LAUNCH_STATE/loaded.$label" ]; then
      printf '%s\n' "$delayed_absence" >"$LAUNCH_STATE/delayed-absence.$label"
    else
      rm -f "$LAUNCH_STATE/loaded.$label"
      if [ "$label" = org.sarmg.xsoc ] &&
        [ "${BOOTOUT_LEAVES_CLIENT_PROCESS:-0}" -ne 1 ]; then
        rm -f "$LAUNCH_STATE/process-u.$label" "$LAUNCH_STATE/process-U.$label"
      fi
    fi
    if [ "$label" = org.sarmg.xsoc.logrotate ] &&
      [ "${HELPER_RESTORES_CLIENT_ON_EXIT:-0}" -eq 1 ]; then
      : >"$LAUNCH_STATE/loaded.org.sarmg.xsoc"
      : >"$LAUNCH_STATE/process-u.org.sarmg.xsoc"
      : >"$LAUNCH_STATE/process-U.org.sarmg.xsoc"
    fi
    if [ "$label" = org.sarmg.xsoc ]; then
      : >"$FILE_STATE/mutable-checks-active"
      : >"$LAUNCH_STATE/client-bootout-completed"
    fi
    if [ "$label" = org.sarmg.xsoc ] &&
      [ "${RACE_CONFIG_SYMLINK_AFTER_CLIENT_BOOTOUT:-0}" -eq 1 ] &&
      [ ! -e "$FILE_STATE/race-config-injected" ]; then
      config="$CASE_ROOT/Library/Application Support/xsoc/config.json"
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
    if [ "$label" = org.sarmg.xsoc ] &&
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
    delayed_marker="$LAUNCH_STATE/delayed-absence.$label"
    if [ -f "$delayed_marker" ]; then
      IFS= read -r delayed_remaining <"$delayed_marker"
      delayed_remaining=$((delayed_remaining - 1))
      if [ "$delayed_remaining" -gt 0 ]; then
        printf '%s\n' "$delayed_remaining" >"$delayed_marker"
      else
        rm -f "$delayed_marker" "$LAUNCH_STATE/loaded.$label"
        if [ "$label" = org.sarmg.xsoc ]; then
          rm -f "$LAUNCH_STATE/process-u.$label" "$LAUNCH_STATE/process-U.$label"
        fi
      fi
    fi
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
  rm -f "$case_dir/launch/disabled.org.sarmg.xsoc"
  touch "$case_dir/launch/loaded.org.sarmg.xsoc" "$case_dir/launch/process-u.org.sarmg.xsoc" "$case_dir/launch/process-U.org.sarmg.xsoc"
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
    "$reset_case/sleep-calls" \
    "$reset_case/race-config-injected" \
    "$reset_case/mutable-checks-active" \
    "$reset_case/launch/bootstrap-count" "$reset_case/launch/bootout-count" \
    "$reset_case/launch/enable-count" "$reset_case/launch/disable-count" \
    "$reset_case/launch/print-count" \
    "$reset_case/launch/client-bootout-completed" \
    "$reset_case/launch/post-bootout-print-failed"
  rm -f "$reset_case/launch"/delayed-absence.*
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
  marker_package_version="${5:-@XSOC_PACKAGE_VERSION@}"
  mkdir -p "$marker_case/var/db/xsoc"
  {
    printf 'format=%s\n' "$marker_package_version"
    printf 'user_created=1\n'
    printf 'user_uid=%s\n' "$marker_uid"
    printf 'user_primary_gid=%s\n' "$marker_primary_gid"
    printf 'group_created=1\n'
    printf 'group_gid=%s\n' "$marker_group_gid"
  } >"$marker_case/var/db/xsoc/account-ownership"
}

write_valid_ownership_marker() {
  write_ownership_marker_ids "$1" 450 450 450
}

assert_payload_rejected_without_bootout() {
  payload_case="$1"
  payload_case_name="$2"
  shift 2
  : >"$payload_case/launch/loaded.org.sarmg.xsoc"
  : >"$payload_case/launch/loaded.org.sarmg.xsoc.logrotate"
  : >"$payload_case/launch/calls"
  if run_postinstall "$payload_case" "$@" >"$payload_case/failure.log" 2>&1; then
    fail "$payload_case_name unexpectedly succeeded"
  fi
  [ -e "$payload_case/launch/loaded.org.sarmg.xsoc" ] ||
    fail "$payload_case_name stopped the previously loaded Client"
  [ -e "$payload_case/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
    fail "$payload_case_name stopped the previously loaded log-rotation helper"
  if grep -F 'bootout ' "$payload_case/launch/calls" >/dev/null 2>&1; then
    fail "$payload_case_name reached the launchd stop transaction"
  fi
  [ ! -d "$payload_case/dscl/Groups/_xsoc" ] ||
    fail "$payload_case_name created a service group before rejecting the payload"
  [ ! -d "$payload_case/dscl/Users/_xsoc" ] ||
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
  [ ! -e "$preinstall_case/var/db/xsoc" ] ||
    fail "$preinstall_case_name preinstall mutated ownership bookkeeping"
  [ ! -d "$preinstall_case/dscl/Groups/_xsoc" ] ||
    fail "$preinstall_case_name preinstall created a service group"
  [ ! -d "$preinstall_case/dscl/Users/_xsoc" ] ||
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
      rm "$case_dir/usr/local/libexec/xsoc"
      next_payload_failure=redirected-client
      ;;
    redirected-client)
      rm "$case_dir/usr/local/libexec/xsoc"
      ln -s /bin/true "$case_dir/usr/local/libexec/xsoc"
      next_payload_failure=client-mode
      ;;
    client-mode)
      payload_environment=STAT_CLIENT_BINARY=0:0:700
      next_payload_failure=client-version
      ;;
    client-version)
      sed 's/xsoc @XSOC_PACKAGE_VERSION@/xsoc 0.0.0/' \
        "$case_dir/usr/local/libexec/xsoc" >"$case_dir/client.invalid"
      mv "$case_dir/client.invalid" "$case_dir/usr/local/libexec/xsoc"
      chmod 0755 "$case_dir/usr/local/libexec/xsoc"
      next_payload_failure=missing-helper
      ;;
    missing-helper)
      rm "$case_dir/usr/local/libexec/xsoc-logrotate"
      next_payload_failure=helper-syntax
      ;;
    helper-syntax)
      printf '#!/bin/sh\nif then\n' >"$case_dir/usr/local/libexec/xsoc-logrotate"
      chmod 0755 "$case_dir/usr/local/libexec/xsoc-logrotate"
      next_payload_failure=helper-mode
      ;;
    helper-mode)
      payload_environment=STAT_LOGROTATE_HELPER=501:20:755
      next_payload_failure=missing-client-plist
      ;;
    missing-client-plist)
      rm "$case_dir/Library/LaunchDaemons/org.sarmg.xsoc.plist"
      next_payload_failure=invalid-client-plist
      ;;
    invalid-client-plist)
      printf 'not a plist\n' >"$case_dir/Library/LaunchDaemons/org.sarmg.xsoc.plist"
      next_payload_failure=client-plist-mode
      ;;
    client-plist-mode)
      payload_environment=STAT_CLIENT_PLIST=0:0:666
      next_payload_failure=missing-logrotate-plist
      ;;
    missing-logrotate-plist)
      rm "$case_dir/Library/LaunchDaemons/org.sarmg.xsoc.logrotate.plist"
      next_payload_failure=invalid-logrotate-plist
      ;;
    invalid-logrotate-plist)
      printf 'not a plist\n' >"$case_dir/Library/LaunchDaemons/org.sarmg.xsoc.logrotate.plist"
      next_payload_failure=missing-newsyslog
      ;;
    missing-newsyslog)
      rm "$case_dir/usr/local/share/xsoc/newsyslog.conf"
      next_payload_failure=newsyslog-symlink
      ;;
    newsyslog-symlink)
      rm "$case_dir/usr/local/share/xsoc/newsyslog.conf"
      ln -s /etc/hosts "$case_dir/usr/local/share/xsoc/newsyslog.conf"
      next_payload_failure=newsyslog-mode
      ;;
    newsyslog-mode)
      payload_environment=STAT_NEWSYSLOG_CONFIG=0:0:666
      next_payload_failure=missing-package-config
      ;;
    missing-package-config)
      rm "$case_dir/usr/local/share/xsoc/xsoc.json.example"
      next_payload_failure=package-config-symlink
      ;;
    package-config-symlink)
      rm "$case_dir/usr/local/share/xsoc/xsoc.json.example"
      ln -s /etc/hosts "$case_dir/usr/local/share/xsoc/xsoc.json.example"
      next_payload_failure=package-config-mode
      ;;
    package-config-mode)
      payload_environment=STAT_PACKAGE_CONFIG=0:0:600
      next_payload_failure=package-config-version
      ;;
    package-config-version)
      sed 's/1.0.0/0.0.0/' \
        "$case_dir/usr/local/share/xsoc/xsoc.json.example" \
        >"$case_dir/config.example.invalid"
      mv "$case_dir/config.example.invalid" \
        "$case_dir/usr/local/share/xsoc/xsoc.json.example"
      next_payload_failure=missing-uninstall-helper
      ;;
    missing-uninstall-helper)
      rm "$case_dir/usr/local/share/xsoc/uninstall.sh"
      next_payload_failure=uninstall-helper-symlink
      ;;
    uninstall-helper-symlink)
      rm "$case_dir/usr/local/share/xsoc/uninstall.sh"
      ln -s /bin/true "$case_dir/usr/local/share/xsoc/uninstall.sh"
      next_payload_failure=uninstall-helper-mode
      ;;
    uninstall-helper-mode)
      payload_environment=STAT_UNINSTALL_HELPER=0:0:700
      next_payload_failure=uninstall-helper-syntax
      ;;
    uninstall-helper-syntax)
      printf '#!/bin/sh\nif then\n' >"$case_dir/usr/local/share/xsoc/uninstall.sh"
      chmod 0755 "$case_dir/usr/local/share/xsoc/uninstall.sh"
      next_payload_failure=command-not-link
      ;;
    command-not-link)
      rm "$case_dir/usr/local/bin/xsoc"
      : >"$case_dir/usr/local/bin/xsoc"
      next_payload_failure=command-target
      ;;
    command-target)
      rm "$case_dir/usr/local/bin/xsoc"
      ln -s ../libexec/not-xsoc "$case_dir/usr/local/bin/xsoc"
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
smart-directory-owner|STAT_SMART_DIRECTORY=501:20:755
smart-directory-mode|STAT_SMART_DIRECTORY=0:0:777
smart-directory-acl|ACL_PATH=smart-directory
smart-binary-owner|STAT_SMART_BINARY=501:20:755
smart-binary-acl|ACL_PATH=smart-binary
smart-source-acl|ACL_PATH=smart-source
EOF

case_dir="$test_root/preinstall-libexec-symlink"
make_case "$case_dir"
mv "$case_dir/usr/local/libexec" "$case_dir/foreign-libexec"
ln -s "$case_dir/foreign-libexec" "$case_dir/usr/local/libexec"
assert_preinstall_rejected_without_mutation "$case_dir" libexec-symlink

case_dir="$test_root/preinstall-client-symlink"
make_case "$case_dir"
rm "$case_dir/usr/local/libexec/xsoc"
ln -s /bin/true "$case_dir/usr/local/libexec/xsoc"
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
rm -rf "$case_dir/usr/local/share/xsoc"
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
  [ ! -e "$case_dir/var/db/xsoc" ] ||
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
smart-directory-owner|STAT_SMART_DIRECTORY=501:20:755
smart-directory-mode|STAT_SMART_DIRECTORY=0:0:777
smart-directory-acl|ACL_PATH=smart-directory
smart-binary-owner|STAT_SMART_BINARY=501:20:755
smart-binary-acl|ACL_PATH=smart-binary
smart-source-acl|ACL_PATH=smart-source
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
[ -f "$case_dir/usr/local/share/xsoc/xsoc.json.example" ] ||
  fail 'successful install lost the root-owned package config template'
[ -f "$case_dir/Library/Application Support/xsoc/config.json" ] ||
  fail 'successful install did not initialize the retained config'
[ ! -e "$case_dir/Library/Application Support/xsoc/xsoc.json.example" ] ||
  fail 'successful install copied the package template into mutable service state'
assert_mock_metadata "$case_dir" 0:0:0:644 \
  "$case_dir/usr/local/share/xsoc/xsoc.json.example"
assert_mock_metadata "$case_dir" 450:450:0:700 \
  "$case_dir/Library/Application Support/xsoc"
assert_mock_metadata "$case_dir" 450:450:0:600 \
  "$case_dir/Library/Application Support/xsoc/config.json"
assert_mock_metadata "$case_dir" 450:450:0:600 \
  "$case_dir/var/log/xsoc.log"
assert_mock_no_acl "$case_dir" \
  "$case_dir/usr/local/share/xsoc/xsoc.json.example"
assert_mock_no_acl "$case_dir" \
  "$case_dir/Library/Application Support/xsoc"
assert_mock_no_acl "$case_dir" \
  "$case_dir/Library/Application Support/xsoc/config.json"
assert_mock_no_acl "$case_dir" "$case_dir/var/log/xsoc.log"
grep -Fx "0:0 $case_dir/Library/Application Support/xsoc" \
  "$case_dir/chown-calls" >/dev/null ||
  fail 'successful install did not lock state as root before mutation'
grep -Fx "450:450 $case_dir/Library/Application Support/xsoc" \
  "$case_dir/chown-calls" >/dev/null ||
  fail 'successful install did not return state ownership to the service account'
grep -Fx 'disable system/org.sarmg.xsoc' "$case_dir/launch/calls" >/dev/null ||
  fail 'successful install did not disable Client autoload during mutable-state validation'
[ -e "$case_dir/launch/disabled.org.sarmg.xsoc" ] &&
  [ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] ||
  fail 'fresh install must leave Client stopped and disabled'

# A package-version marker from any previous release is bookkeeping, not a
# persistent-state format. Validate its identity binding and replace it with
# the explicit marker schema instead of maintaining a release whitelist.
write_ownership_marker_ids "$case_dir" 450 450 450 0.9.23
start_prior_client
reset_fault_counters "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 \
  >"$case_dir/legacy-marker-failure.log" 2>&1; then
  fail 'legacy marker replacement failure unexpectedly succeeded'
fi
grep -Fx 'format=0.9.23' "$case_dir/var/db/xsoc/account-ownership" >/dev/null ||
  fail 'failed replacement did not preserve the legacy ownership marker'
reset_fault_counters "$case_dir"
run_postinstall "$case_dir" >"$case_dir/legacy-marker-upgrade.log" 2>&1 ||
  fail 'legacy package-version ownership marker blocked replacement'
grep -Fx 'marker_format=1' "$case_dir/var/db/xsoc/account-ownership" >/dev/null ||
  fail 'legacy ownership marker was not overwritten with marker schema 1'
grep -Fx 'last_package_version=@XSOC_PACKAGE_VERSION@' \
  "$case_dir/var/db/xsoc/account-ownership" >/dev/null ||
  fail 'rewritten ownership marker did not record the installed package version'
grep -Fx 'state_format=1.0.0' "$case_dir/var/db/xsoc/account-ownership" >/dev/null ||
  fail 'rewritten ownership marker did not separate the persistent-state format'
if grep -E '^format=' "$case_dir/var/db/xsoc/account-ownership" >/dev/null; then
  fail 'rewritten ownership marker retained the ambiguous legacy format field'
fi

# Current-schema markers from the 0.10 releases must also allow replacement.
for prior_package_version in 0.10.1 0.10.2; do
  sed "s/last_package_version=@XSOC_PACKAGE_VERSION@/last_package_version=$prior_package_version/" \
    "$case_dir/var/db/xsoc/account-ownership" >"$case_dir/prior-marker"
  mv "$case_dir/prior-marker" "$case_dir/var/db/xsoc/account-ownership"
  cp "$case_dir/Library/Application Support/xsoc/config.json" "$case_dir/config.before-upgrade"
  start_prior_client
  reset_fault_counters "$case_dir"
  run_postinstall "$case_dir" >"$case_dir/current-schema-upgrade.log" 2>&1 ||
    fail "compatible $prior_package_version marker blocked replacement"
  cmp "$case_dir/config.before-upgrade" "$case_dir/Library/Application Support/xsoc/config.json" ||
    fail 'cross-version upgrade changed existing config bytes'
  grep -Fx 'last_package_version=@XSOC_PACKAGE_VERSION@' \
    "$case_dir/var/db/xsoc/account-ownership" >/dev/null ||
    fail 'cross-version upgrade did not update software bookkeeping'
  grep -Fx 'user_uid=450' "$case_dir/var/db/xsoc/account-ownership" >/dev/null ||
    fail 'cross-version upgrade changed service identity'
done

assert_recoverable() {
  recovery_case="$1"
  reset_fault_counters "$recovery_case"
  run_postinstall "$recovery_case" >"$recovery_case/recovery.log" 2>&1 ||
    fail "a clean rerun did not recover $recovery_case"
  [ -d "$recovery_case/dscl/Groups/_xsoc" ] || fail "group missing after recovery"
  [ -d "$recovery_case/dscl/Users/_xsoc" ] || fail "user missing after recovery"
  grep -Fx 'group_created=1' "$recovery_case/var/db/xsoc/account-ownership" >/dev/null ||
    fail "group ownership was not committed after recovery"
  grep -Fx 'user_created=1' "$recovery_case/var/db/xsoc/account-ownership" >/dev/null ||
    fail "user ownership was not committed after recovery"
}

# Security-sensitive roots must be rejected before postinstall creates an
# account or runs any state/log chown as root.
case_dir="$test_root/unsafe-ownership-symlink"
make_case "$case_dir"
mkdir -p "$case_dir/foreign-ownership" "$(dirname -- "$case_dir/var/db/xsoc")"
: >"$case_dir/foreign-ownership/sentinel"
ln -s "$case_dir/foreign-ownership" "$case_dir/var/db/xsoc"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked ownership directory unexpectedly succeeded'
fi
[ -e "$case_dir/foreign-ownership/sentinel" ] || fail 'ownership symlink target was modified'

case_dir="$test_root/unsafe-ownership-mode"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/xsoc"
if run_postinstall "$case_dir" STAT_OWNERSHIP_DIR=501:20:777 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'foreign ownership directory metadata unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-ownership-special-mode"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/xsoc"
if run_postinstall "$case_dir" STAT_OWNERSHIP_DIR=0:0:1:700 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'ownership directory with special permission bits unexpectedly succeeded'
fi

case_dir="$test_root/unsafe-ownership-acl"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/xsoc"
if run_postinstall "$case_dir" ACL_OWNERSHIP_DIR=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'ownership directory with an extended ACL unexpectedly succeeded'
fi
[ ! -e "$case_dir/acl-cleared.ownership-dir" ] ||
  fail 'postinstall normalized an ACL on a pre-existing ownership directory'

case_dir="$test_root/unsafe-ownership-ls-failure"
make_case "$case_dir"
mkdir -p "$case_dir/var/db/xsoc"
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
mkdir -p "$case_dir/var/db/xsoc"
: >"$case_dir/foreign-marker"
ln -s "$case_dir/foreign-marker" "$case_dir/var/db/xsoc/account-ownership"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked ownership marker unexpectedly succeeded'
fi

case_dir="$test_root/invalid-marker-package-version"
make_case "$case_dir"
write_ownership_marker_ids "$case_dir" 450 450 450 0.10.bad
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'ownership marker with malformed package version unexpectedly succeeded'
fi
grep -F 'Invalid legacy ownership marker package version' "$case_dir/failure.log" >/dev/null ||
  fail 'invalid legacy marker package version was not diagnosed'

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
rm -rf "$case_dir/Library/Application Support/xsoc"
ln -s "$case_dir/foreign-state" "$case_dir/Library/Application Support/xsoc"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked state directory unexpectedly succeeded'
fi
[ -e "$case_dir/foreign-state/sentinel" ] || fail 'state symlink target was modified'

case_dir="$test_root/unsafe-state-owner"
make_case "$case_dir"
mkdir -p "$case_dir/Library/Application Support/xsoc"
write_valid_ownership_marker "$case_dir"
if run_postinstall "$case_dir" STAT_STATE_ROOT=501:20:755 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'foreign state ownership unexpectedly succeeded'
fi
[ ! -d "$case_dir/dscl/Groups/_xsoc" ] ||
  fail 'foreign state ownership was rejected only after creating an account'
[ ! -s "$case_dir/chown-calls" ] || fail 'foreign state ownership reached chown'

case_dir="$test_root/unsafe-unbound-root-state"
make_case "$case_dir"
mkdir -p "$case_dir/Library/Application Support/xsoc"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'pre-existing root-owned state without an ownership proof unexpectedly succeeded'
fi
[ ! -d "$case_dir/dscl/Groups/_xsoc" ] ||
  fail 'unbound root-owned state was rejected only after creating an account'
[ ! -s "$case_dir/chown-calls" ] || fail 'unbound root-owned state reached chown'
[ ! -e "$case_dir/var/db/xsoc" ] ||
  fail 'unbound root-owned state mutated ownership bookkeeping before rejection'

case_dir="$test_root/mutable-retained-config-symlink"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish state for the retained-config symlink case'
start_prior_client
reset_fault_counters "$case_dir"
rm "$case_dir/Library/Application Support/xsoc/config.json"
cp "$case_dir/usr/local/share/xsoc/xsoc.json.example" \
  "$case_dir/foreign-config.json"
ln -s "$case_dir/foreign-config.json" \
  "$case_dir/Library/Application Support/xsoc/config.json"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked retained config unexpectedly succeeded'
fi
[ -f "$case_dir/foreign-config.json" ] ||
  fail 'symlinked retained config target was modified'

case_dir="$test_root/stale-package-config"
make_case "$case_dir"
sed 's/1.0.0/0.0.0/' \
  "$case_dir/usr/local/share/xsoc/xsoc.json.example" \
  >"$case_dir/config.example.stale"
mv "$case_dir/config.example.stale" \
  "$case_dir/usr/local/share/xsoc/xsoc.json.example"
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
ln -s "$case_dir/foreign-log" "$case_dir/var/log/xsoc.log"
if run_postinstall "$case_dir" >"$case_dir/failure.log" 2>&1; then
  fail 'symlinked log file unexpectedly succeeded'
fi

# None of the immutable-root validation failures above may have reached account creation.
# Mutable retained config/log checks intentionally happen later, after the old
# service has been stopped and its state directory has been locked.
for unsafe_case in "$test_root"/unsafe-* "$test_root"/stale-package-config; do
  [ ! -d "$unsafe_case/dscl/Groups/_xsoc" ] ||
    fail "unsafe root case created a service group: $unsafe_case"
  [ ! -d "$unsafe_case/dscl/Users/_xsoc" ] ||
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
  [ ! -d "$case_dir/dscl/Users/_xsoc" ] ||
    fail "dscl create failure $failure left a partial user"
  if [ "$failure" -le 4 ]; then
    [ ! -d "$case_dir/dscl/Groups/_xsoc" ] ||
      fail "dscl create failure $failure left a partial group"
    [ ! -e "$case_dir/var/db/xsoc/account-ownership" ] ||
      fail "dscl create failure $failure left an ownership claim"
  else
    [ -d "$case_dir/dscl/Groups/_xsoc" ] ||
      fail "user creation failure $failure removed the committed group"
    grep -Fx 'user_created=0' "$case_dir/var/db/xsoc/account-ownership" >/dev/null ||
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
  [ ! -d "$case_dir/dscl/Users/_xsoc" ] ||
    fail "dscl verification failure $failure left a partial user"
  if [ "$failure" -eq 1 ]; then
    [ ! -d "$case_dir/dscl/Groups/_xsoc" ] ||
      fail "group verification failure left a partial group"
  else
    [ -d "$case_dir/dscl/Groups/_xsoc" ] ||
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
  [ ! -d "$case_dir/dscl/Users/_xsoc" ] ||
    fail "marker publication failure $failure left a partial user"
  if [ "$failure" -eq 1 ]; then
    [ ! -d "$case_dir/dscl/Groups/_xsoc" ] ||
      fail "group marker failure left a partial group"
  else
    [ -d "$case_dir/dscl/Groups/_xsoc" ] ||
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
[ ! -d "$case_dir/dscl/Groups/_xsoc" ] ||
  fail 'marker ACL-clear failure left a partial group'
[ ! -e "$case_dir/var/db/xsoc/account-ownership" ] ||
  fail 'marker ACL-clear failure published an ownership claim'
if find "$case_dir/var/db/xsoc" -name '.account-ownership.*' -print -quit |
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
[ -d "$case_dir/dscl/Groups/_xsoc" ] &&
  [ -d "$case_dir/dscl/Users/_xsoc" ] ||
  fail 'marker rename commit test lost a service account'
grep -Fx 'user_created=1' "$case_dir/var/db/xsoc/account-ownership" >/dev/null ||
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
  grep -Fx 'bootout system/org.sarmg.xsoc.logrotate' \
    "$case_dir/launch/calls" >/dev/null ||
    fail "mutable $mutable_fault was checked before stopping the helper"
  grep -Fx 'bootout system/org.sarmg.xsoc' \
    "$case_dir/launch/calls" >/dev/null ||
    fail "mutable $mutable_fault was checked before stopping the Client"
  grep -Fx 'disable system/org.sarmg.xsoc' \
    "$case_dir/launch/calls" >/dev/null ||
    fail "mutable $mutable_fault did not persist the fail-closed Client state"
  if [ "$mutable_expect_lock" -eq 1 ]; then
    grep -Fx "0:0 $case_dir/Library/Application Support/xsoc" \
      "$case_dir/chown-calls" >/dev/null ||
      fail "mutable $mutable_fault was checked before locking state"
  elif grep -Fx "0:0 $case_dir/Library/Application Support/xsoc" \
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
config_path="$case_dir/Library/Application Support/xsoc/config.json"
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
grep -Fx 'bootout system/org.sarmg.xsoc' "$case_dir/launch/calls" >/dev/null ||
  fail 'retained-config race was not injected after Client bootout'
grep -Fx -- '-u 450 .' "$case_dir/pgrep-calls" >/dev/null ||
  fail 'postinstall did not check for lingering service processes after bootout'
grep -Fx -- '-U 450 .' "$case_dir/pgrep-calls" >/dev/null ||
  fail 'postinstall did not check the real service UID after bootout'
grep -Fx "0:0 $case_dir/Library/Application Support/xsoc" \
  "$case_dir/chown-calls" >/dev/null ||
  fail 'postinstall did not lock state before rejecting the raced config'
if grep -F " $config_path" "$case_dir/chown-calls" >/dev/null 2>&1; then
  fail 'postinstall ran privileged chown on the raced config leaf'
fi

# A detached process that cannot be quiesced must still block the lock transaction.
case_dir="$test_root/mutable-lingering-process"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish jobs for the lingering-process case'
start_prior_client
reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
if run_postinstall "$case_dir" BOOTOUT_LEAVES_CLIENT_PROCESS=1 PKILL_LEAVES_PROCESS=1 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'postinstall accepted a lingering service process after bootout'
fi
grep -Fx -- '-u 450 .' "$case_dir/pgrep-calls" >/dev/null ||
  fail 'lingering-process case did not inspect the service UID'
grep -Fx -- '-KILL -u 450 .' "$case_dir/pkill-calls" >/dev/null ||
  fail 'unsafe rollback did not kill the old effective-UID process'
grep -Fx -- '-KILL -U 450 .' "$case_dir/pkill-calls" >/dev/null ||
  fail 'unsafe rollback did not kill the old real-UID process'
[ -e "$case_dir/launch/process-u.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/process-U.org.sarmg.xsoc" ] ||
  fail 'persistent-process fault did not exercise both UID views'
[ -e "$case_dir/launch/disabled.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/disabled.org.sarmg.xsoc.logrotate" ] ||
  fail 'unsafe rollback did not leave both labels disabled'
if grep -Fx "0:0 $case_dir/Library/Application Support/xsoc" \
  "$case_dir/chown-calls" >/dev/null 2>&1; then
  fail 'postinstall locked state while a service process could still mutate it'
fi

# Native hardware reporting starts macOS user agents outside the launchd job.
# A replacement can proceed only after cleanup has emptied both UID views.
case_dir="$test_root/quiesce-service-agents"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish jobs for service-agent cleanup'
start_prior_client
reset_fault_counters "$case_dir"
run_postinstall "$case_dir" BOOTOUT_LEAVES_CLIENT_PROCESS=1 \
  >"$case_dir/upgrade.log" 2>&1 ||
  fail 'upgrade rejected service-account agents that could be stopped'
for selector in -u -U; do
  grep -Fx -- "-KILL $selector 450 ." "$case_dir/pkill-calls" >/dev/null ||
    fail 'upgrade did not clean both service-account UID views'
done
[ ! -e "$case_dir/launch/process-u.org.sarmg.xsoc" ] &&
  [ ! -e "$case_dir/launch/process-U.org.sarmg.xsoc" ] ||
  fail 'upgrade left a service-account process alive'
grep -Fx "0:0 $case_dir/Library/Application Support/xsoc" \
  "$case_dir/chown-calls" >/dev/null ||
  fail 'upgrade did not reach the state lock after quiescing agents'

# Fault injection leaves a partial Client job during failed helper registration;
# rollback must still remove it, although normal fresh installs never start Client.
case_dir="$test_root/bootstrap-2"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 FAIL_BOOTSTRAP_STARTS_CLIENT=1 >"$case_dir/failure.log" 2>&1; then
  fail "second launchctl bootstrap failure unexpectedly succeeded"
fi
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] ||
  fail "Client remained loaded after helper bootstrap failed"
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
  fail "failed helper bootstrap left a loaded helper"
grep -Fx 'bootout system/org.sarmg.xsoc' "$case_dir/launch/calls" >/dev/null ||
  fail "postinstall did not boot out the Client after helper bootstrap failed"
[ -d "$case_dir/dscl/Groups/_xsoc" ] && [ -d "$case_dir/dscl/Users/_xsoc" ] ||
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
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] ||
  fail 'rollback bootout retry left the Client label loaded'
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
  fail 'combined rollback failure left the helper label loaded'
[ ! -e "$case_dir/launch/process-u.org.sarmg.xsoc" ] &&
  [ ! -e "$case_dir/launch/process-U.org.sarmg.xsoc" ] ||
  fail 'combined rollback failure left a service-UID process running'
[ -e "$case_dir/launch/disabled.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/disabled.org.sarmg.xsoc.logrotate" ] ||
  fail 'combined rollback failure did not persistently disable both labels'
assert_calls_in_order_after "$case_dir/launch/calls" \
  "bootstrap system $case_dir/Library/LaunchDaemons/org.sarmg.xsoc.logrotate.plist" \
  'disable system/org.sarmg.xsoc.logrotate' \
  'disable system/org.sarmg.xsoc' \
  'print system/org.sarmg.xsoc.logrotate' \
  'print system/org.sarmg.xsoc' \
  'bootout system/org.sarmg.xsoc' \
  'print system/org.sarmg.xsoc' \
  'kill SIGKILL system/org.sarmg.xsoc' \
  'bootout system/org.sarmg.xsoc' \
  'print system/org.sarmg.xsoc'
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
grep -F 'Could not inspect system/org.sarmg.xsoc (launchctl status 79)' \
  "$case_dir/failure.log" >/dev/null ||
  fail 'rollback launchd inspection failure was mistaken for an absent label'
grep -F 'xsoc rollback cleanup is incomplete' "$case_dir/failure.log" >/dev/null ||
  fail 'rollback launchd inspection failure was not marked incomplete'
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] ||
  fail 'inspection-failure retry left the Client label loaded'
[ -e "$case_dir/launch/disabled.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/disabled.org.sarmg.xsoc.logrotate" ] ||
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
grep -F 'xsoc rollback cleanup is incomplete' "$case_dir/failure.log" >/dev/null ||
  fail 'unkillable rollback process was not reported as incomplete cleanup'
[ -e "$case_dir/launch/process-u.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/process-U.org.sarmg.xsoc" ] ||
  fail 'unkillable rollback-process fixture did not retain both UID views'

# Persistent disable errors are also incomplete cleanup even if forced bootout
# removes the current process: reboot safety has not been established.
case_dir="$test_root/bootstrap-rollback-disable-failure"
make_case "$case_dir"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 FAIL_BOOTSTRAP_STARTS_CLIENT=1 FAIL_DISABLE_FROM=4 \
  >"$case_dir/failure.log" 2>&1; then
  fail 'rollback disable failure unexpectedly succeeded'
fi
grep -F 'xsoc rollback cleanup is incomplete' "$case_dir/failure.log" >/dev/null ||
  fail 'rollback disable failure was not reported as incomplete cleanup'
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] ||
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
run_preinstall "$case_dir" PKGUTIL_INSTALLED=1 PKGUTIL_VERSION=0.9.23 \
  >"$case_dir/preinstall.log" 2>&1 ||
  fail 'replacement preinstall rejected an older package receipt'
[ -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] ||
  fail 'preinstall stopped the previous Client before payload validation'
[ -e "$case_dir/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
  fail 'preinstall stopped the previous helper before payload validation'
if grep -F 'bootout ' "$case_dir/launch/calls" >/dev/null; then
  fail 'preinstall issued a launchd bootout'
fi

# A replacement that fails payload validation must likewise leave both old
# jobs untouched because postinstall has not reached its stop transaction.
sed 's/1.0.0/0.0.0/' \
  "$case_dir/usr/local/share/xsoc/xsoc.json.example" \
  >"$case_dir/config.example.invalid"
mv "$case_dir/config.example.invalid" \
  "$case_dir/usr/local/share/xsoc/xsoc.json.example"
if run_postinstall "$case_dir" >"$case_dir/validation-failure.log" 2>&1; then
  fail 'replacement with an invalid package config unexpectedly succeeded'
fi
[ -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] ||
  fail 'payload validation failure stopped the previous Client'
[ -e "$case_dir/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
  fail 'payload validation failure stopped the previous helper'
if grep -F 'bootout ' "$case_dir/launch/calls" >/dev/null; then
  fail 'payload validation failure reached the launchd stop transaction'
fi
sed 's/0.0.0/1.0.0/' \
  "$case_dir/usr/local/share/xsoc/xsoc.json.example" \
  >"$case_dir/config.example.restored"
mv "$case_dir/config.example.restored" \
  "$case_dir/usr/local/share/xsoc/xsoc.json.example"

reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
if run_postinstall "$case_dir" FAIL_BOOTSTRAP_AT=1 FAIL_BOOTSTRAP_STARTS_CLIENT=1 \
  >"$case_dir/postinstall-failure.log" 2>&1; then
  fail 'replacement helper bootstrap failure unexpectedly succeeded'
fi
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] ||
  fail 'failed replacement restarted the Client without revalidating mutable state'
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
  fail 'failed replacement restarted the helper without revalidating mutable state'
grep -Fx 'disable system/org.sarmg.xsoc' "$case_dir/launch/calls" >/dev/null ||
  fail 'failed replacement did not persistently disable the Client label'

# launchctl bootout can return before launchd finishes removing a job. A normal
# package upgrade must wait for both old labels instead of treating the short
# removal window as a failed cutover.
case_dir="$test_root/reinstall-asynchronous-bootout"
make_case "$case_dir"
run_postinstall "$case_dir" >"$case_dir/initial.log" 2>&1 ||
  fail 'could not establish jobs for the asynchronous bootout case'
start_prior_client
reset_fault_counters "$case_dir"
: >"$case_dir/launch/calls"
run_postinstall "$case_dir" BOOTOUT_DELAYED_ABSENCE_PRINTS=2 \
  >"$case_dir/reinstall.log" 2>&1 ||
  fail 'postinstall rejected launchd asynchronous job removal'
grep -Fx '1' "$case_dir/sleep-calls" >/dev/null ||
  fail 'postinstall did not wait for launchd job removal'
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/disabled.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
  fail 'asynchronous bootout did not leave the validated Client stopped'

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
rm -f "$case_dir/launch/loaded.org.sarmg.xsoc"
run_postinstall "$case_dir" HELPER_RESTORES_CLIENT_ON_EXIT=1 \
  >"$case_dir/reinstall.log" 2>&1 ||
  fail 'postinstall did not contain the helper Client-restore race'
grep -Fx 'bootout system/org.sarmg.xsoc.logrotate' "$case_dir/launch/calls" >/dev/null ||
  fail 'helper restore race did not stop the helper'
grep -Fx 'bootout system/org.sarmg.xsoc' "$case_dir/launch/calls" >/dev/null ||
  fail 'helper restore race left the restored Client running during state mutation'
[ ! -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/disabled.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
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
grep -F 'Could not inspect system/org.sarmg.xsoc (launchctl status 79)' \
  "$case_dir/failure.log" >/dev/null ||
  fail 'normal cutover inspection error was mistaken for an absent Client'
if grep -Fx "0:0 $case_dir/Library/Application Support/xsoc" \
  "$case_dir/chown-calls" >/dev/null 2>&1; then
  fail 'normal cutover inspection failure reached the mutable state lock'
fi
[ -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] &&
  [ -e "$case_dir/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
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
[ -e "$case_dir/launch/loaded.org.sarmg.xsoc" ] ||
  fail 'postinstall bootout failure lost the previous Client job'
[ -e "$case_dir/launch/loaded.org.sarmg.xsoc.logrotate" ] ||
  fail 'postinstall bootout failure did not re-register the helper job'

echo 'macOS postinstall failure-atomicity tests: ok'
