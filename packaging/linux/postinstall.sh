#!/bin/sh
set -eu

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

service_name=host-monitor.service
package_version=0.9.3
client_binary=/usr/bin/host-monitor
account_state_dir=/var/lib/host-monitor-package
state_dir=/var/lib/host-monitor
config_dir=/etc/host-monitor
config_path="$config_dir/config.json"
managed_user_marker="$account_state_dir/managed-user"
managed_group_marker="$account_state_dir/managed-group"
group_marker_state=absent
user_marker_state=absent
recorded_group_gid=
recorded_user_uid=
recorded_user_primary_gid=
group_created_now=0
group_creation_committed=0
rollback_group_gid=
user_created_now=0
user_creation_committed=0
rollback_user_uid=
rollback_user_primary_gid=

die() {
  echo "host-monitor postinstall: $*" >&2
  exit 1
}

for command_name in getent groupadd groupdel useradd userdel install cut chown chmod cp rm mv stat awk; do
  command -v "$command_name" >/dev/null 2>&1 ||
    die "required command is unavailable: $command_name"
done
[ -x "$client_binary" ] || die "installed Client binary is missing or not executable"

[ "$("$client_binary" --version)" = "host-monitor $package_version" ] ||
  die "installed binary version does not match package lifecycle version $package_version"

read_path_metadata() {
  metadata_path=$1
  path_metadata=$(stat -c '%u:%g:%a' -- "$metadata_path") ||
    die "cannot read ownership and permissions for $metadata_path"
  path_uid=${path_metadata%%:*}
  path_metadata_remainder=${path_metadata#*:}
  path_gid=${path_metadata_remainder%%:*}
  path_mode=${path_metadata_remainder#*:}
  case "$path_uid:$path_gid:$path_mode" in
    *[!0-9:]*) die "$metadata_path has invalid ownership or permission metadata" ;;
    :*|*::*|*:) die "$metadata_path has incomplete ownership or permission metadata" ;;
  esac
}

require_current_config() {
  current_config_path=$1
  config_version_marker=$(
    awk -v expected="$package_version" '
      {
        remaining = $0
        while (match(remaining, /"application_version"[[:space:]]*:/)) {
          seen += 1
          remaining = substr(remaining, RSTART + RLENGTH)
          if (match(remaining, /^[[:space:]]*"[^"]*"/)) {
            value = substr(remaining, RSTART, RLENGTH)
            sub(/^[[:space:]]*"/, "", value)
            sub(/"$/, "", value)
            if (value == expected) valid += 1
          }
        }
      }
      END { printf "%d:%d", seen, valid }
    ' "$current_config_path"
  ) || die "cannot inspect $current_config_path"
  [ "$config_version_marker" = 1:1 ] ||
    die "$current_config_path must contain exactly one current application_version $package_version marker"
}

load_group_marker() {
  marker_format_seen=0
  marker_gid_seen=0
  recorded_group_gid=
  while IFS= read -r marker_line || [ -n "$marker_line" ]; do
    case "$marker_line" in
      format="$package_version")
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

load_user_marker() {
  marker_format_seen=0
  marker_uid_seen=0
  marker_primary_gid_seen=0
  recorded_user_uid=
  recorded_user_primary_gid=
  while IFS= read -r marker_line || [ -n "$marker_line" ]; do
    case "$marker_line" in
      format="$package_version")
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

inspect_existing_markers() {
  if [ -e "$managed_group_marker" ] || [ -L "$managed_group_marker" ]; then
    [ -f "$managed_group_marker" ] && [ ! -L "$managed_group_marker" ] ||
      die "managed group marker is not a safe regular file"
    read_path_metadata "$managed_group_marker"
    [ "$path_uid:$path_gid:$path_mode" = 0:0:600 ] ||
      die "managed group marker must be owned by root:root with permissions 0600"
    if load_group_marker; then
      group_marker_state=valid
    else
      die "managed group marker is invalid"
    fi
  fi

  if [ -e "$managed_user_marker" ] || [ -L "$managed_user_marker" ]; then
    [ -f "$managed_user_marker" ] && [ ! -L "$managed_user_marker" ] ||
      die "managed user marker is not a safe regular file"
    read_path_metadata "$managed_user_marker"
    [ "$path_uid:$path_gid:$path_mode" = 0:0:600 ] ||
      die "managed user marker must be owned by root:root with permissions 0600"
    if load_user_marker; then
      user_marker_state=valid
    else
      die "managed user marker is invalid"
    fi
  fi

  if [ "$user_marker_state" = valid ] && [ "$group_marker_state" != valid ]; then
    die "managed user marker exists without its managed group marker"
  fi
}

write_group_marker() {
  marker_gid=$1
  marker_temporary="$account_state_dir/.managed-group.$$"
  umask 077
  {
    printf 'format=%s\n' "$package_version"
    printf 'gid=%s\n' "$marker_gid"
  } >"$marker_temporary"
  chown root:root "$marker_temporary"
  chmod 0600 "$marker_temporary"
  mv -f -- "$marker_temporary" "$managed_group_marker"
  group_marker_state=valid
  recorded_group_gid=$marker_gid
}

write_user_marker() {
  marker_uid=$1
  marker_primary_gid=$2
  marker_temporary="$account_state_dir/.managed-user.$$"
  umask 077
  {
    printf 'format=%s\n' "$package_version"
    printf 'uid=%s\n' "$marker_uid"
    printf 'primary_gid=%s\n' "$marker_primary_gid"
  } >"$marker_temporary"
  chown root:root "$marker_temporary"
  chmod 0600 "$marker_temporary"
  mv -f -- "$marker_temporary" "$managed_user_marker"
  user_marker_state=valid
  recorded_user_uid=$marker_uid
  recorded_user_primary_gid=$marker_primary_gid
}

# Enumerate the account databases during rollback instead of interpreting every
# failed keyed lookup as "absent". A temporary NSS failure must never authorize
# deletion of an account whose identity cannot be proved.
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

rollback_user_is_exact() {
  current_uid=$(printf '%s\n' "$user_entry" | cut -d: -f3)
  current_gid=$(printf '%s\n' "$user_entry" | cut -d: -f4)
  current_home=$(printf '%s\n' "$user_entry" | cut -d: -f6)
  current_shell=$(printf '%s\n' "$user_entry" | cut -d: -f7)
  [ -n "$rollback_user_uid" ] && [ -n "$rollback_user_primary_gid" ] &&
    [ "$current_uid" = "$rollback_user_uid" ] &&
    [ "$current_gid" = "$rollback_user_primary_gid" ] &&
    [ "$current_home" = "$state_dir" ] &&
    { [ "$current_shell" = /usr/sbin/nologin ] || [ "$current_shell" = /sbin/nologin ]; }
}

# Return 0 when the just-created group is referenced, 1 when it is unused, and
# 2 when usage cannot be established safely.
rollback_group_is_in_use() {
  current_group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
  group_members=$(printf '%s\n' "$group_entry" | cut -d: -f4-)
  [ -n "$rollback_group_gid" ] && [ "$current_group_gid" = "$rollback_group_gid" ] ||
    return 2
  case "$group_members" in
    *:*) return 2 ;;
    '') ;;
    *) return 0 ;;
  esac

  current_passwd_listing=$(getent passwd 2>/dev/null) || return 2
  while IFS=: read -r account_name _password _uid primary_gid _gecos _home _shell; do
    [ -n "$account_name" ] || continue
    if [ "$primary_gid" = "$rollback_group_gid" ]; then
      return 0
    fi
  done <<EOF
$current_passwd_listing
EOF
  return 1
}

rollback_account_creation() {
  rollback_status=$?
  trap - EXIT
  set +e
  if [ "$rollback_status" -eq 0 ]; then
    exit 0
  fi

  # Marker publication is the ownership commit point. Only records created by
  # this invocation and not yet committed are candidates for rollback. Every
  # deletion re-enumerates NSS and requires the exact numeric identity plus the
  # immutable service attributes established by useradd/groupadd.
  if [ "$user_created_now" -eq 1 ] && [ "$user_creation_committed" -eq 0 ] &&
    [ "$user_marker_state" != valid ]; then
    user_lookup_status=0
    if lookup_user_entry; then
      if rollback_user_is_exact; then
        userdel host-monitor ||
          echo "host-monitor postinstall: could not roll back the incomplete dedicated user" >&2
      else
        echo "host-monitor postinstall: refusing to roll back a dedicated user whose identity changed" >&2
      fi
    else
      user_lookup_status=$?
      if [ "$user_lookup_status" -eq 2 ]; then
        echo "host-monitor postinstall: could not enumerate users while rolling back account creation" >&2
      fi
    fi
  fi

  if [ "$group_created_now" -eq 1 ] && [ "$group_creation_committed" -eq 0 ] &&
    [ "$group_marker_state" != valid ]; then
    group_lookup_status=0
    if lookup_group_entry; then
      current_group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
      if [ -z "$rollback_group_gid" ] || [ "$current_group_gid" != "$rollback_group_gid" ]; then
        echo "host-monitor postinstall: refusing to roll back a dedicated group whose gid changed" >&2
      else
        group_usage_status=0
        if rollback_group_is_in_use; then
          group_usage_status=0
        else
          group_usage_status=$?
        fi
        case "$group_usage_status" in
          1)
            groupdel host-monitor ||
              echo "host-monitor postinstall: could not roll back the incomplete dedicated group" >&2
            ;;
          0)
            echo "host-monitor postinstall: refusing to roll back an incomplete group that is in use" >&2
            ;;
          *)
            echo "host-monitor postinstall: could not verify incomplete group usage; preserving it" >&2
            ;;
        esac
      fi
    else
      group_lookup_status=$?
      if [ "$group_lookup_status" -eq 2 ]; then
        echo "host-monitor postinstall: could not enumerate groups while rolling back account creation" >&2
      fi
    fi
  fi
  rm -f -- "$account_state_dir/.managed-user.$$" "$account_state_dir/.managed-group.$$"
  exit "$rollback_status"
}

# nFPM lays down the config before invoking this hook. Refuse a missing,
# retained-from-another-version, or redirected payload before creating any
# account or package-owned state.
[ -d "$config_dir" ] && [ ! -L "$config_dir" ] ||
  die "$config_dir is not a safe current-package config directory"
[ -f "$config_path" ] && [ ! -L "$config_path" ] ||
  die "$config_path is not a safe current-package config file"
require_current_config "$config_path"
read_path_metadata "$config_dir"
initial_config_dir_metadata="$path_uid:$path_gid:$path_mode"
read_path_metadata "$config_path"
initial_config_metadata="$path_uid:$path_gid:$path_mode"

# This bookkeeping directory is deliberately separate from the Client-writable
# state directory and records only that the current package owns the dedicated
# account. Never normalize a pre-existing foreign directory: doing so would
# adopt attacker-controlled marker paths while running as root.
if [ -e "$account_state_dir" ] || [ -L "$account_state_dir" ]; then
  [ -d "$account_state_dir" ] && [ ! -L "$account_state_dir" ] ||
    die "package account state path is not a safe directory"
  read_path_metadata "$account_state_dir"
  [ "$path_uid:$path_gid:$path_mode" = 0:0:700 ] ||
    die "package account state directory must be owned by root:root with permissions 0700"
fi
install -d -m 0700 -o root -g root "$account_state_dir"
[ -d "$account_state_dir" ] && [ ! -L "$account_state_dir" ] ||
  die "package account state path did not become a safe directory"
read_path_metadata "$account_state_dir"
[ "$path_uid:$path_gid:$path_mode" = 0:0:700 ] ||
  die "package account state directory was not created as root:root with permissions 0700"
inspect_existing_markers

# A pre-existing Client-writable state tree is accepted only when the protected
# current-version markers already bind it to the exact service identity. Its
# numeric owner is checked after the account database has been read below.
state_dir_preexisting=0
if [ -e "$state_dir" ] || [ -L "$state_dir" ]; then
  [ -d "$state_dir" ] && [ ! -L "$state_dir" ] ||
    die "$state_dir is not a safe directory"
  state_dir_preexisting=1
  [ "$group_marker_state" = valid ] && [ "$user_marker_state" = valid ] ||
    die "refusing to adopt pre-existing $state_dir without current package ownership markers"
fi

# Only the two states produced by this package are valid here: the freshly
# extracted root-owned config, or the same-version config normalized by a
# previous successful postinstall.
case "$initial_config_dir_metadata" in
  0:0:755) ;;
  "0:$recorded_group_gid:750")
    [ "$group_marker_state" = valid ] ||
      die "$config_dir has stale package-managed ownership"
    ;;
  *) die "$config_dir has foreign ownership or permissions" ;;
esac
case "$initial_config_metadata" in
  0:0:600) ;;
  "0:$recorded_group_gid:640")
    [ "$group_marker_state" = valid ] ||
      die "$config_path has stale package-managed ownership"
    ;;
  *) die "$config_path has foreign ownership or permissions" ;;
esac

# Account creation mutates global NSS state outside the package payload. If a
# later validation or atomic marker publication fails, roll back only the exact
# uncommitted numeric identity created by this invocation.
trap rollback_account_creation EXIT

group_lookup_status=0
if lookup_group_entry; then
  group_lookup_status=0
else
  group_lookup_status=$?
  [ "$group_lookup_status" -eq 1 ] ||
    die "dedicated group database is unavailable or ambiguous"
  [ "$group_marker_state" = absent ] ||
    die "package-managed host-monitor group is missing"
  groupadd --system host-monitor
  group_created_now=1
  lookup_group_entry || die "new dedicated group could not be enumerated"
fi
group_gid=$(printf '%s\n' "$group_entry" | cut -d: -f3)
rollback_group_gid=$group_gid
case "$group_gid" in
  ''|*[!0-9]*) die "dedicated group has an invalid gid" ;;
esac
if [ "$group_created_now" -eq 1 ]; then
  # Record group ownership before creating the user. If useradd later fails,
  # an explicit purge can still identify this installer-created group safely.
  write_group_marker "$group_gid"
  group_creation_committed=1
elif [ "$group_marker_state" != valid ]; then
  die "existing host-monitor group has no current $package_version ownership marker"
elif [ "$recorded_group_gid" != "$group_gid" ]; then
  die "package-managed host-monitor group was replaced with a different gid"
fi

user_lookup_status=0
if lookup_user_entry; then
  user_lookup_status=0
else
  user_lookup_status=$?
  [ "$user_lookup_status" -eq 1 ] ||
    die "dedicated user database is unavailable or ambiguous"
  [ "$user_marker_state" = absent ] ||
    die "package-managed host-monitor user is missing"
  nologin_shell=/usr/sbin/nologin
  if [ ! -x "$nologin_shell" ]; then
    nologin_shell=/sbin/nologin
  fi
  [ -x "$nologin_shell" ] || die "neither /usr/sbin/nologin nor /sbin/nologin exists"
  useradd --system --gid host-monitor --home-dir "$state_dir" \
    --shell "$nologin_shell" host-monitor
  user_created_now=1
  lookup_user_entry || die "new dedicated user could not be enumerated"
fi

# Refuse to run the service under an unexpected pre-existing identity. An
# existing account is accepted only when the current 0.9.3 marker binds its
# exact numeric identity.
user_uid=$(printf '%s\n' "$user_entry" | cut -d: -f3)
user_gid=$(printf '%s\n' "$user_entry" | cut -d: -f4)
rollback_user_uid=$user_uid
rollback_user_primary_gid=$user_gid
user_home=$(printf '%s\n' "$user_entry" | cut -d: -f6)
user_shell=$(printf '%s\n' "$user_entry" | cut -d: -f7)

case "$user_uid:$user_gid" in
  *[!0-9:]*) die "dedicated account has an invalid numeric identity" ;;
  :*|*:) die "dedicated account has a missing numeric identity" ;;
esac
[ "$user_gid" = "$group_gid" ] || die "host-monitor user does not use the dedicated group"
[ "$user_home" = "$state_dir" ] || die "host-monitor user has an unexpected home"
case "$user_shell" in
  /usr/sbin/nologin|/sbin/nologin) ;;
  *) die "host-monitor user has an interactive or unexpected shell" ;;
esac

if [ "$user_created_now" -eq 1 ]; then
  write_user_marker "$user_uid" "$user_gid"
  user_creation_committed=1
elif [ "$user_marker_state" != valid ]; then
  die "existing host-monitor user has no current $package_version ownership marker"
elif
  { [ "$recorded_user_uid" != "$user_uid" ] ||
    [ "$recorded_user_primary_gid" != "$user_gid" ]; }; then
  die "package-managed host-monitor user was replaced with a different numeric identity"
fi

if [ "$state_dir_preexisting" -eq 1 ]; then
  read_path_metadata "$state_dir"
  [ "$path_uid:$path_gid:$path_mode" = "$user_uid:$user_gid:700" ] ||
    die "$state_dir is not owned by the recorded host-monitor identity with permissions 0700"
else
  install -d -m 0700 -o host-monitor -g host-monitor "$state_dir"
  [ -d "$state_dir" ] && [ ! -L "$state_dir" ] ||
    die "$state_dir did not become a safe directory"
  read_path_metadata "$state_dir"
  [ "$path_uid:$path_gid:$path_mode" = "$user_uid:$user_gid:700" ] ||
    die "$state_dir was not created for the recorded host-monitor identity with permissions 0700"
fi

[ -d "$config_dir" ] && [ ! -L "$config_dir" ] ||
  die "$config_dir was redirected during postinstall"
[ -f "$config_path" ] && [ ! -L "$config_path" ] ||
  die "$config_path was redirected during postinstall"
require_current_config "$config_path"
chown root:host-monitor "$config_dir"
chmod 0750 "$config_dir"
chown root:host-monitor "$config_path"
chmod 0640 "$config_path"
read_path_metadata "$config_dir"
[ "$path_uid:$path_gid:$path_mode" = "0:$user_gid:750" ] ||
  die "$config_dir could not be secured for the host-monitor group"
read_path_metadata "$config_path"
[ "$path_uid:$path_gid:$path_mode" = "0:$user_gid:640" ] ||
  die "$config_path could not be secured for the host-monitor group"

service_started=0
if [ -d /run/systemd/system ]; then
  command -v systemctl >/dev/null 2>&1 || die "systemd is running but systemctl is unavailable"
  systemctl daemon-reload
  systemctl enable "$service_name"
  systemctl restart "$service_name"
  systemctl is-active --quiet "$service_name" || die "$service_name did not remain active"
  service_started=1
fi

if [ "$service_started" -eq 1 ]; then
  cat <<'EOF'

host-monitor 服务已启动，但新安装尚未配对，当前不会发送经过授权的遥测。
请在本机发起浏览器配对：

  sudo host-monitor pair --config /etc/host-monitor/config.json \
    --server https://host-monitoring.example.com

管理台只生成一次性激活码，不分发软件，也不会接触 Client 的长期通信 secret。

配对后验证状态（只读，不发送或清理队列）：

  sudo -u host-monitor host-monitor status --output human \
    --config /etc/host-monitor/config.json
  sudo -u host-monitor host-monitor doctor --output human \
    --config /etc/host-monitor/config.json

查看服务和日志：

  systemctl status host-monitor.service
  journalctl -u host-monitor.service -n 100 --no-pager

EOF
else
  cat <<'EOF'

host-monitor 文件已安装；当前环境没有运行 systemd，因此没有启用或启动后台服务。
进入正常启动的系统后执行：

  sudo systemctl enable --now host-monitor.service
  sudo host-monitor pair --config /etc/host-monitor/config.json \
    --server https://host-monitoring.example.com

配对后使用 `host-monitor status --output human` 验证授权状态。

EOF
fi

# 默认 unit 设置 PrivateDevices=yes，会屏蔽 /dev/nvidia* 与 /dev/dri。
# 需要 GPU 采集时安装随包分发的 drop-in。
if [ -e /dev/nvidiactl ] || [ -d /dev/dri ]; then
  gpu_groups=""
  for gpu_group in render video; do
    if getent group "$gpu_group" >/dev/null 2>&1; then
      if [ -n "$gpu_groups" ]; then
        gpu_groups="$gpu_groups,$gpu_group"
      else
        gpu_groups="$gpu_group"
      fi
    fi
  done
  cat <<'EOF'

检测到本机存在 GPU 设备节点。默认 unit 出于安全考虑设置了 PrivateDevices=yes，
因此裸 shell 中的 probe 结果不能代表 systemd 服务实际可见的 GPU。
EOF
  if [ -n "$gpu_groups" ]; then
    cat <<EOF

本机存在设备访问组：$gpu_groups。如确需启用 GPU 指标，请显式执行：

  usermod -aG $gpu_groups host-monitor
  mkdir -p /etc/systemd/system/host-monitor.service.d
  cp /usr/share/host-monitor/host-monitor-gpu.conf \
     /etc/systemd/system/host-monitor.service.d/gpu.conf
  systemctl daemon-reload && systemctl restart host-monitor

EOF
  else
    cat <<'EOF'

未找到 render/video 设备访问组，因此没有给出会导致服务启动失败的盲目配置命令。
请先确认设备节点的实际属组，再授予 host-monitor 最小必要组权限并安装 GPU drop-in。

EOF
  fi
fi
