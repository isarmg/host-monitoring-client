#!/bin/sh
set -eu

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

service_name=host-monitor.service
package_version=0.9.7
account_state_dir=/var/lib/host-monitor-package
managed_user_marker="$account_state_dir/managed-user"
managed_group_marker="$account_state_dir/managed-group"
purge_incomplete=0
recorded_user_uid=
recorded_user_primary_gid=
recorded_group_gid=

trusted_path_has_metadata() {
  metadata_path=$1
  expected_metadata=$2
  actual_metadata=$(stat -c '%u:%g:%a' -- "$metadata_path" 2>/dev/null) || return 1
  [ "$actual_metadata" = "$expected_metadata" ]
}

account_state_is_trusted() {
  if [ ! -e "$account_state_dir" ] && [ ! -L "$account_state_dir" ]; then
    return 0
  fi
  [ -d "$account_state_dir" ] &&
    [ ! -L "$account_state_dir" ] &&
    trusted_path_has_metadata "$account_state_dir" 0:0:700
}

marker_is_trusted() {
  marker_path=$1
  [ -f "$marker_path" ] &&
    [ ! -L "$marker_path" ] &&
    trusted_path_has_metadata "$marker_path" 0:0:600
}

systemd_daemon_reload() {
  if [ -d /run/systemd/system ]; then
    command -v systemctl >/dev/null 2>&1 || {
      echo "host-monitor postremove: systemd is running but systemctl is unavailable" >&2
      exit 1
    }
    systemctl daemon-reload
    # A removed or purged unit may no longer exist, so reset-failed is best effort.
    systemctl reset-failed "$service_name" >/dev/null 2>&1 || true
  fi
}

load_user_marker() {
  marker_format_seen=0
  marker_uid_seen=0
  marker_primary_gid_seen=0
  recorded_user_uid=
  recorded_user_primary_gid=
  while IFS= read -r marker_line || [ -n "$marker_line" ]; do
    case "$marker_line" in
      format=0.9.4|format=0.9.5|format=0.9.6|format="$package_version")
        [ "$marker_format_seen" -eq 0 ] || return 1
        marker_format_seen=1
        ;;
      uid=*)
        [ "$marker_uid_seen" -eq 0 ] || return 1
        recorded_user_uid=${marker_line#uid=}
        marker_uid_seen=1
        ;;
      primary_gid=*)
        [ "$marker_primary_gid_seen" -eq 0 ] || return 1
        recorded_user_primary_gid=${marker_line#primary_gid=}
        marker_primary_gid_seen=1
        ;;
      *) return 1 ;;
    esac
  done <"$managed_user_marker"
  [ "$marker_format_seen" -eq 1 ] && [ "$marker_uid_seen" -eq 1 ] &&
    [ "$marker_primary_gid_seen" -eq 1 ] || return 1
  case "$recorded_user_uid:$recorded_user_primary_gid" in
    *[!0-9:]*) return 1 ;;
    :*|*:) return 1 ;;
  esac
}

load_group_marker() {
  marker_format_seen=0
  marker_gid_seen=0
  recorded_group_gid=
  while IFS= read -r marker_line || [ -n "$marker_line" ]; do
    case "$marker_line" in
      format=0.9.4|format=0.9.5|format=0.9.6|format="$package_version")
        [ "$marker_format_seen" -eq 0 ] || return 1
        marker_format_seen=1
        ;;
      gid=*)
        [ "$marker_gid_seen" -eq 0 ] || return 1
        recorded_group_gid=${marker_line#gid=}
        marker_gid_seen=1
        ;;
      *) return 1 ;;
    esac
  done <"$managed_group_marker"
  [ "$marker_format_seen" -eq 1 ] && [ "$marker_gid_seen" -eq 1 ] || return 1
  case "$recorded_group_gid" in
    ''|*[!0-9]*) return 1 ;;
  esac
}

# Enumerate the account databases instead of treating every failed keyed lookup
# as "absent". Status 2 means the query was unavailable or ambiguous and all
# account deletion must fail closed.
lookup_user_entry() {
  passwd_listing=$(getent passwd 2>/dev/null) || return 2
  user_entry=
  user_match_count=0
  while IFS= read -r directory_entry || [ -n "$directory_entry" ]; do
    case "$directory_entry" in
      host-monitor:*)
        user_match_count=$((user_match_count + 1))
        user_entry=$directory_entry
        ;;
    esac
  done <<EOF
$passwd_listing
EOF
  case "$user_match_count" in
    0) return 1 ;;
    1) return 0 ;;
    *) return 2 ;;
  esac
}

lookup_group_entry() {
  group_listing=$(getent group 2>/dev/null) || return 2
  group_entry=
  group_match_count=0
  while IFS= read -r directory_entry || [ -n "$directory_entry" ]; do
    case "$directory_entry" in
      host-monitor:*)
        group_match_count=$((group_match_count + 1))
        group_entry=$directory_entry
        ;;
    esac
  done <<EOF
$group_listing
EOF
  case "$group_match_count" in
    0) return 1 ;;
    1) return 0 ;;
    *) return 2 ;;
  esac
}

managed_user_is_still_expected() {
  group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
  user_uid=$(printf '%s\n' "$user_entry" | cut -d: -f3)
  user_gid=$(printf '%s\n' "$user_entry" | cut -d: -f4)
  user_home=$(printf '%s\n' "$user_entry" | cut -d: -f6)
  user_shell=$(printf '%s\n' "$user_entry" | cut -d: -f7)

  [ "$user_uid" = "$recorded_user_uid" ] &&
    [ "$user_gid" = "$recorded_user_primary_gid" ] &&
    [ "$user_gid" = "$group_gid" ] &&
    [ "$group_gid" = "$recorded_group_gid" ] &&
    [ "$user_home" = /var/lib/host-monitor ] &&
    { [ "$user_shell" = /usr/sbin/nologin ] || [ "$user_shell" = /sbin/nologin ]; }
}

# Return 0 when the package-created group is referenced, 1 when it is unused,
# and 2 when usage cannot be established safely.
group_id_is_in_use() {
  sought_gid=$1
  current_group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
  group_members=$(printf '%s\n' "$group_entry" | cut -d: -f4-)
  [ "$current_group_gid" = "$sought_gid" ] || return 2
  case "$group_members" in
    *:*) return 2 ;;
    '') ;;
    *) return 0 ;;
  esac

  current_passwd_listing=$(getent passwd 2>/dev/null) || return 2
  while IFS=: read -r account_name _password _uid primary_gid _gecos _home _shell; do
    [ -n "$account_name" ] || continue
    if [ "$primary_gid" = "$sought_gid" ]; then
      return 0
    fi
  done <<EOF
$current_passwd_listing
EOF
  return 1
}

purge_local_data() {
  account_bookkeeping_trusted=1
  if ! account_state_is_trusted; then
    echo "host-monitor postremove: unsafe account bookkeeping directory; preserving package account records and accounts" >&2
    purge_incomplete=1
    account_bookkeeping_trusted=0
  else
    if { [ -e "$managed_user_marker" ] || [ -L "$managed_user_marker" ]; } &&
      ! marker_is_trusted "$managed_user_marker"; then
      echo "host-monitor postremove: unsafe managed-user marker; preserving package account records and accounts" >&2
      purge_incomplete=1
      account_bookkeeping_trusted=0
    fi
    if { [ -e "$managed_group_marker" ] || [ -L "$managed_group_marker" ]; } &&
      ! marker_is_trusted "$managed_group_marker"; then
      echo "host-monitor postremove: unsafe managed-group marker; preserving package account records and accounts" >&2
      purge_incomplete=1
      account_bookkeeping_trusted=0
    fi
    if [ "$account_bookkeeping_trusted" -eq 1 ] &&
      { [ -e "$managed_user_marker" ] || [ -L "$managed_user_marker" ]; } &&
      ! load_user_marker; then
      echo "host-monitor postremove: invalid managed-user marker; preserving package account records and accounts" >&2
      purge_incomplete=1
      account_bookkeeping_trusted=0
    fi
    if [ "$account_bookkeeping_trusted" -eq 1 ] &&
      { [ -e "$managed_group_marker" ] || [ -L "$managed_group_marker" ]; } &&
      ! load_group_marker; then
      echo "host-monitor postremove: invalid managed-group marker; preserving package account records and accounts" >&2
      purge_incomplete=1
      account_bookkeeping_trusted=0
    fi
    if [ "$account_bookkeeping_trusted" -eq 1 ] &&
      [ ! -e "$managed_user_marker" ] && [ ! -L "$managed_user_marker" ]; then
      user_lookup_status=0
      if lookup_user_entry; then
        echo "host-monitor postremove: managed-user marker is missing while the dedicated user still exists; preserving package account records and accounts" >&2
        purge_incomplete=1
        account_bookkeeping_trusted=0
      else
        user_lookup_status=$?
        if [ "$user_lookup_status" -ne 1 ]; then
          echo "host-monitor postremove: managed-user marker is missing and users cannot be enumerated safely; preserving package account records and accounts" >&2
          purge_incomplete=1
          account_bookkeeping_trusted=0
        fi
      fi
    fi
    if [ "$account_bookkeeping_trusted" -eq 1 ] &&
      [ ! -e "$managed_group_marker" ] && [ ! -L "$managed_group_marker" ]; then
      group_lookup_status=0
      if lookup_group_entry; then
        echo "host-monitor postremove: managed-group marker is missing while the dedicated group still exists; preserving package account records and accounts" >&2
        purge_incomplete=1
        account_bookkeeping_trusted=0
      else
        group_lookup_status=$?
        if [ "$group_lookup_status" -ne 1 ]; then
          echo "host-monitor postremove: managed-group marker is missing and groups cannot be enumerated safely; preserving package account records and accounts" >&2
          purge_incomplete=1
          account_bookkeeping_trusted=0
        fi
      fi
    fi
  fi

  # Fixed, non-configurable targets are intentional: a root maintainer script
  # must never expand an environment-controlled path into a recursive removal.
  rm -rf -- /var/lib/host-monitor
  rm -rf -- /etc/host-monitor
  rm -rf -- /etc/systemd/system/host-monitor.service.d

  if [ "$account_bookkeeping_trusted" -eq 1 ]; then
    if [ -e "$managed_user_marker" ] || [ -L "$managed_user_marker" ]; then
      if ! load_user_marker; then
        echo "host-monitor postremove: invalid managed-user marker; preserving the account" >&2
        purge_incomplete=1
      else
        user_lookup_status=0
        if lookup_user_entry; then
          group_lookup_status=0
          if lookup_group_entry; then
            if managed_user_is_still_expected; then
              if userdel host-monitor; then
                rm -f -- "$managed_user_marker"
              else
                echo "host-monitor postremove: could not remove the dedicated user" >&2
                purge_incomplete=1
              fi
            else
              echo "host-monitor postremove: dedicated user identity changed; leaving it for safety" >&2
              purge_incomplete=1
            fi
          else
            group_lookup_status=$?
            echo "host-monitor postremove: dedicated group is absent or could not be enumerated; preserving the user" >&2
            purge_incomplete=1
          fi
        else
          user_lookup_status=$?
          if [ "$user_lookup_status" -eq 1 ]; then
            rm -f -- "$managed_user_marker"
          else
            echo "host-monitor postremove: could not enumerate users; preserving the dedicated user" >&2
            purge_incomplete=1
          fi
        fi
      fi
    fi

    if [ -e "$managed_group_marker" ] || [ -L "$managed_group_marker" ]; then
      if ! load_group_marker; then
        echo "host-monitor postremove: invalid managed-group marker; preserving the group" >&2
        purge_incomplete=1
      else
        user_lookup_status=0
        if lookup_user_entry; then
          echo "host-monitor postremove: dedicated user remains; leaving its group" >&2
          purge_incomplete=1
        else
          user_lookup_status=$?
          if [ "$user_lookup_status" -eq 2 ]; then
            echo "host-monitor postremove: could not enumerate users; preserving the dedicated group" >&2
            purge_incomplete=1
          else
            group_lookup_status=0
            if lookup_group_entry; then
              current_group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
              if [ "$current_group_gid" != "$recorded_group_gid" ]; then
                echo "host-monitor postremove: dedicated group gid changed; leaving it for safety" >&2
                purge_incomplete=1
              else
                group_usage_status=0
                if group_id_is_in_use "$recorded_group_gid"; then
                  group_usage_status=0
                else
                  group_usage_status=$?
                fi
                case "$group_usage_status" in
                  0)
                    echo "host-monitor postremove: dedicated group is still in use; preserving it" >&2
                    purge_incomplete=1
                    ;;
                  1)
                    if groupdel host-monitor; then
                      rm -f -- "$managed_group_marker"
                    else
                      echo "host-monitor postremove: could not remove the dedicated group" >&2
                      purge_incomplete=1
                    fi
                    ;;
                  *)
                    echo "host-monitor postremove: could not verify group usage; preserving the group" >&2
                    purge_incomplete=1
                    ;;
                esac
              fi
            else
              group_lookup_status=$?
              if [ "$group_lookup_status" -eq 1 ]; then
                rm -f -- "$managed_group_marker"
              else
                echo "host-monitor postremove: could not enumerate groups; preserving the dedicated group" >&2
                purge_incomplete=1
              fi
            fi
          fi
        fi
      fi
    fi

    rmdir "$account_state_dir" >/dev/null 2>&1 || true
  fi
}

case "${1:-}" in
  purge)
    # Debian's explicit purge is the only package-manager transaction that
    # removes local identity. It never contacts the Host Monitoring Server.
    purge_local_data
    ;;
  *) : ;;
esac

systemd_daemon_reload

case "${1:-}" in
  purge)
    if [ "$purge_incomplete" -ne 0 ]; then
      echo "host-monitor postremove: local purge is incomplete; fix the account conflict and retry" >&2
      exit 1
    fi
    cat <<'EOF'
host-monitor 的本地配置、凭据、spool、GPU drop-in 和包管理的专用账户已清理。
此操作没有连接 Host Monitoring Server；请确认已在管理台永久删除对应实例。
EOF
    ;;
  *)
    cat <<EOF
host-monitor 程序和系统服务已移除；实例凭据、spool 与专用账户仍在本地。
配置文件只遵循包管理器的 config/noreplace 语义；产品不会备份或恢复它。完整备份、恢复和跨版本升级请使用独立升级仓库。
EOF
    ;;
esac
