#!/bin/sh
set -eu

PATH=/bin:/usr/bin:/sbin:/usr/sbin
export PATH

identifier="org.sarmg.hostmonitor"
user="_hostmonitor"
group="_hostmonitor"
state="/Library/Application Support/host-monitor"
log="/var/log/host-monitor.log"
share="/usr/local/share/host-monitor"
ownership_dir="/var/db/host-monitor"
ownership_marker="$ownership_dir/account-ownership"
package_version="@HOST_MONITOR_PACKAGE_VERSION@"
purge=0
assume_yes=0
user_created=0
group_created=0
created_user_uid="-"
created_user_primary_gid="-"
created_group_gid="-"
ownership_marker_valid=0
ownership_dir_state="absent"
purge_incomplete=0
owned_user_blocked=0

usage() {
  cat <<'EOF'
Usage: sudo uninstall.sh [--purge [--yes]]

Without --purge, removes the executable and LaunchDaemons but preserves local identity,
credentials, configuration, spool, logs, the dedicated account, package receipt, and this
maintenance helper so a reinstall can resume the same instance.

--purge  Permanently delete all preserved local Client data, logs, account/group, helper,
         and package receipt. Revoke the instance in the Host Monitoring Web console first.
--yes    Skip the interactive PURGE confirmation (for managed, non-interactive removal).
EOF
}

die() {
  echo "host-monitor uninstall: $*" >&2
  exit 1
}

inspect_package_receipt() {
  receipt_listing=""
  if receipt_listing="$(LC_ALL=C pkgutil --pkgs='^org[.]sarmg[.]hostmonitor$' 2>&1)"; then
    case "$receipt_listing" in
      "$identifier") return 0 ;;
      '') return 1 ;;
      *)
        echo "Could not inspect package receipt $identifier: pkgutil returned unexpected output: $receipt_listing" >&2
        return 2
        ;;
    esac
  else
    receipt_status="$?"
    echo "Could not inspect package receipt $identifier (pkgutil status $receipt_status): $receipt_listing" >&2
    return 2
  fi
}

read_path_metadata() {
  metadata_path="$1"
  path_metadata="$(stat -f '%u:%g:%Mp:%Lp' "$metadata_path" 2>/dev/null)" || return 1
  path_uid="${path_metadata%%:*}"
  path_metadata_remainder="${path_metadata#*:}"
  path_gid="${path_metadata_remainder%%:*}"
  path_metadata_remainder="${path_metadata_remainder#*:}"
  path_special_mode="${path_metadata_remainder%%:*}"
  path_mode="${path_metadata_remainder#*:}"
  case "$path_uid:$path_gid:$path_special_mode:$path_mode" in
    *[!0-9:]*|:*|*::*|*:) return 1 ;;
  esac
}

path_has_no_extended_acl() {
  acl_path="$1"
  acl_listing="$(LC_ALL=C ls -lde "$acl_path" 2>/dev/null)" || return 1
  acl_first_line=""
  acl_has_additional_lines=0
  while IFS= read -r acl_line; do
    if [ -z "$acl_first_line" ]; then
      acl_first_line="$acl_line"
    else
      acl_has_additional_lines=1
    fi
  done <<EOF
$acl_listing
EOF
  [ -n "$acl_first_line" ] && [ "$acl_has_additional_lines" -eq 0 ] || return 1
  acl_permissions="${acl_first_line%% *}"
  case "$acl_permissions" in
    ''|*+) return 1 ;;
  esac
}

validate_ownership_directory() {
  [ -d "$ownership_dir" ] && [ ! -L "$ownership_dir" ] || return 1
  read_path_metadata "$ownership_dir" || return 1
  [ "$path_uid:$path_gid:$path_special_mode:$path_mode" = 0:0:0:700 ] || return 1
  path_has_no_extended_acl "$ownership_dir"
}

validate_ownership_marker_path() {
  marker_path="$1"
  [ -f "$marker_path" ] && [ ! -L "$marker_path" ] || return 1
  read_path_metadata "$marker_path" || return 1
  [ "$path_uid:$path_gid:$path_special_mode:$path_mode" = 0:0:0:600 ] || return 1
  path_has_no_extended_acl "$marker_path"
}

ownership_directory_contains_only_marker() {
  ownership_entry_count=0
  for ownership_entry in \
    "$ownership_dir"/* \
    "$ownership_dir"/.[!.]* \
    "$ownership_dir"/..?*
  do
    if [ ! -e "$ownership_entry" ] && [ ! -L "$ownership_entry" ]; then
      continue
    fi
    [ "$ownership_entry" = "$ownership_marker" ] || return 1
    ownership_entry_count=$((ownership_entry_count + 1))
  done
  [ "$ownership_entry_count" -eq 1 ]
}

for argument in "$@"; do
  case "$argument" in
    --purge) purge=1 ;;
    --yes) assume_yes=1 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      die "unknown option: $argument"
      ;;
  esac
done

[ "$(id -u)" -eq 0 ] || die "run this helper with sudo"
if [ "$assume_yes" -eq 1 ] && [ "$purge" -ne 1 ]; then
  die "--yes is only valid together with --purge"
fi

if [ "$purge" -eq 1 ]; then
  cat >&2 <<'EOF'
WARNING: --purge does not revoke the server-side Client credential.
First revoke/decommission this instance in the Host Monitoring Web console. Continuing permanently
deletes this Mac's host-id, client-token, pairing state, queued reports, configuration, and logs.
EOF
  if [ "$assume_yes" -ne 1 ]; then
    [ -t 0 ] || die "non-interactive purge requires both --purge and --yes"
    printf "Type PURGE to continue: " >&2
    IFS= read -r confirmation
    [ "$confirmation" = "PURGE" ] || die "purge cancelled"
  fi
fi

load_ownership_marker() {
  if [ ! -e "$ownership_dir" ] && [ ! -L "$ownership_dir" ]; then
    return 0
  fi
  if ! validate_ownership_directory; then
    ownership_dir_state="invalid"
    echo "Ignoring unsafe Client account ownership directory; account and group will be preserved" >&2
    purge_incomplete=1
    return 0
  fi
  ownership_dir_state="valid"

  if [ ! -e "$ownership_marker" ] && [ ! -L "$ownership_marker" ]; then
    return 0
  fi
  if ! validate_ownership_marker_path "$ownership_marker"; then
    echo "Ignoring unsafe Client account ownership marker; account and group will be preserved" >&2
    purge_incomplete=1
    return 0
  fi

  marker_user=0
  marker_group=0
  marker_user_uid="-"
  marker_user_primary_gid="-"
  marker_group_gid="-"
  seen_user=0
  seen_group=0
  seen_format=0
  seen_user_uid=0
  seen_user_primary_gid=0
  seen_group_gid=0
  marker_invalid=0
  while IFS= read -r marker_line; do
    case "$marker_line" in
      format="$package_version")
        [ "$seen_format" -eq 0 ] || marker_invalid=1
        seen_format=1
        ;;
      user_created=0|user_created=1)
        [ "$seen_user" -eq 0 ] || marker_invalid=1
        marker_user="${marker_line#user_created=}"
        seen_user=1
        ;;
      group_created=0|group_created=1)
        [ "$seen_group" -eq 0 ] || marker_invalid=1
        marker_group="${marker_line#group_created=}"
        seen_group=1
        ;;
      user_uid=*)
        [ "$seen_user_uid" -eq 0 ] || marker_invalid=1
        marker_user_uid="${marker_line#user_uid=}"
        seen_user_uid=1
        ;;
      user_primary_gid=*)
        [ "$seen_user_primary_gid" -eq 0 ] || marker_invalid=1
        marker_user_primary_gid="${marker_line#user_primary_gid=}"
        seen_user_primary_gid=1
        ;;
      group_gid=*)
        [ "$seen_group_gid" -eq 0 ] || marker_invalid=1
        marker_group_gid="${marker_line#group_gid=}"
        seen_group_gid=1
        ;;
      *) marker_invalid=1 ;;
    esac
  done < "$ownership_marker"
  if [ "$marker_invalid" -eq 1 ] || [ "$seen_format" -ne 1 ] ||
    [ "$seen_user" -ne 1 ] || [ "$seen_group" -ne 1 ] ||
    [ "$seen_user_uid" -ne 1 ] || [ "$seen_user_primary_gid" -ne 1 ] ||
    [ "$seen_group_gid" -ne 1 ]; then
    echo "Ignoring invalid Client account ownership marker; account and group will be preserved" >&2
    purge_incomplete=1
    return 0
  fi
  if [ "$marker_user" -eq 1 ]; then
    case "$marker_user_uid" in ''|*[!0-9]*) marker_invalid=1 ;; esac
    case "$marker_user_primary_gid" in ''|*[!0-9]*) marker_invalid=1 ;; esac
  elif [ "$marker_user_uid" != "-" ] || [ "$marker_user_primary_gid" != "-" ]; then
    marker_invalid=1
  fi
  if [ "$marker_group" -eq 1 ]; then
    case "$marker_group_gid" in ''|*[!0-9]*) marker_invalid=1 ;; esac
  elif [ "$marker_group_gid" != "-" ]; then
    marker_invalid=1
  fi
  if [ "$marker_invalid" -eq 1 ]; then
    echo "Ignoring invalid Client account ownership IDs; account and group will be preserved" >&2
    purge_incomplete=1
    return 0
  fi
  if [ "$marker_user" -eq 1 ] && [ "$marker_group" -eq 1 ] &&
    [ "$marker_user_primary_gid" != "$marker_group_gid" ]; then
    echo "Ignoring inconsistent Client account ownership binding; account and group will be preserved" >&2
    purge_incomplete=1
    return 0
  fi
  user_created="$marker_user"
  group_created="$marker_group"
  created_user_uid="$marker_user_uid"
  created_user_primary_gid="$marker_user_primary_gid"
  created_group_gid="$marker_group_gid"
  ownership_marker_valid=1
}

if [ "$purge" -eq 1 ]; then
  load_ownership_marker
fi

inspect_launchd_job() {
  inspected_job_target="$1"
  if launchd_inspection_output="$(LC_ALL=C launchctl print "$inspected_job_target" 2>&1)"; then
    launchd_inspection_status=0
  else
    launchd_inspection_status=$?
  fi
  case "$launchd_inspection_status" in
    0) return 0 ;;
    3|113)
      case "$launchd_inspection_output" in
        *'Could not find service "'*' in domain for '*|*'Could not find specified service'*)
          return 1
          ;;
      esac
      ;;
  esac
  echo "Could not inspect $inspected_job_target (launchctl status $launchd_inspection_status)" >&2
  return 2
}

stop_job() {
  service_target="$1"
  if inspect_launchd_job "$service_target"; then
    service_state=0
  else
    service_state=$?
  fi
  case "$service_state" in
    0) launchctl bootout "$service_target" ;;
    1) ;;
    *) die "refusing to remove files while the launchd job state is unknown" ;;
  esac
}

# Stop the rotation helper first: if it is between bootout and bootstrap of the Client, its
# signal trap restores the Client, and the second call below then stops that restored job.
stop_job system/org.sarmg.hostmonitor.logrotate
stop_job system/org.sarmg.hostmonitor

rm -f /Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist
rm -f /Library/LaunchDaemons/org.sarmg.hostmonitor.plist
rm -f /usr/local/libexec/host-monitor-logrotate
rm -f /usr/local/libexec/host-monitor
rm -f "$share/newsyslog.conf"
rm -f "$share/host-monitor.json.example"

command_link="/usr/local/bin/host-monitor"
if [ -L "$command_link" ]; then
  link_target="$(readlink "$command_link")"
  case "$link_target" in
    ../libexec/host-monitor|/usr/local/libexec/host-monitor)
      rm -f "$command_link"
      ;;
    *)
      echo "Preserving unexpected symlink $command_link -> $link_target" >&2
      ;;
  esac
elif [ -e "$command_link" ]; then
  echo "Preserving non-package path at $command_link" >&2
fi

if [ "$purge" -ne 1 ]; then
  rmdir /usr/local/libexec >/dev/null 2>&1 || true
  cat <<EOF
host-monitor program and LaunchDaemons were removed.

Preserved for identity-safe reinstall:
  $state
  $log and rotated logs
  local account $user:$group
  package receipt $identifier
  $share/uninstall.sh

To permanently decommission later, first revoke the instance in the Web console, then run:
  sudo $share/uninstall.sh --purge
EOF
  exit 0
fi

# This assertion keeps the destructive target fixed even if this script is edited or sourced.
[ "$state" = "/Library/Application Support/host-monitor" ] ||
  die "refusing to purge an unexpected state path"
rm -rf "$state"
rm -f "$log"
for archived_log in /var/log/host-monitor.log.*; do
  if [ -e "$archived_log" ] || [ -L "$archived_log" ]; then
    rm -f "$archived_log"
  fi
done

dscl_value() {
  record="$1"
  field="$2"
  dscl_record="$(dscl . -read "$record" "$field" 2>/dev/null)" || return 1
  printf '%s\n' "$dscl_record" | sed -n "s/^$field: //p"
}

listing_contains_id() {
  account_listing="$1"
  sought_id="$2"
  while read -r _record_name listed_id _remaining; do
    if [ "$listed_id" = "$sought_id" ]; then
      return 0
    fi
  done <<EOF
$account_listing
EOF
  return 1
}

listing_contains_name() {
  account_listing="$1"
  sought_name="$2"
  while read -r listed_name _remaining; do
    if [ "$listed_name" = "$sought_name" ]; then
      return 0
    fi
  done <<EOF
$account_listing
EOF
  return 1
}

record_attribute_has_values() {
  record_dump="$1"
  attribute="$2"
  inside_attribute=0
  while IFS= read -r record_line; do
    case "$record_line" in
      "$attribute:"*)
        inside_attribute=1
        attribute_value="${record_line#*:}"
        case "$attribute_value" in
          *[![:space:]]*) return 0 ;;
        esac
        ;;
      [[:space:]]*)
        if [ "$inside_attribute" -eq 1 ]; then
          case "$record_line" in
            *[![:space:]]*) return 0 ;;
          esac
        fi
        ;;
      *)
        if [ "$inside_attribute" -eq 1 ]; then
          return 1
        fi
        ;;
    esac
  done <<EOF
$record_dump
EOF
  return 1
}

text_contains_token() (
  token_text="$1"
  sought_token="$2"
  # Directory Service tokens never contain whitespace. Disable pathname expansion before the
  # intentional IFS split so an administrator-created `*` value cannot expand against cwd.
  set -f
  set -- $token_text
  for candidate_token in "$@"; do
    if [ "$candidate_token" = "$sought_token" ]; then
      return 0
    fi
  done
  return 1
)

record_attribute_contains_token() {
  record_dump="$1"
  attribute="$2"
  sought_token="$3"
  inside_attribute=0
  while IFS= read -r record_line; do
    case "$record_line" in
      "$attribute:"*)
        inside_attribute=1
        attribute_value="${record_line#*:}"
        if text_contains_token "$attribute_value" "$sought_token"; then
          return 0
        fi
        ;;
      [[:space:]]*)
        if [ "$inside_attribute" -eq 1 ] &&
          text_contains_token "$record_line" "$sought_token"; then
          return 0
        fi
        ;;
      *)
        if [ "$inside_attribute" -eq 1 ]; then
          return 1
        fi
        ;;
    esac
  done <<EOF
$record_dump
EOF
  return 1
}

group_is_in_use() {
  usage_group_name="$1"
  usage_group_id="$2"
  if ! usage_primary_group_listing="$(dscl . -list /Users PrimaryGroupID)"; then
    return 2
  fi
  if listing_contains_id "$usage_primary_group_listing" "$usage_group_id"; then
    return 0
  fi

  if ! usage_group_record="$(dscl . -read "/Groups/$usage_group_name")"; then
    return 2
  fi
  for usage_membership_attribute in GroupMembership GroupMembers NestedGroups; do
    if record_attribute_has_values "$usage_group_record" "$usage_membership_attribute"; then
      return 0
    fi
  done

  usage_group_guid="$(dscl_value "/Groups/$usage_group_name" GeneratedUID || true)"
  case "$usage_group_guid" in
    ''|*[!0-9A-Fa-f-]*) return 2 ;;
  esac
  [ "${#usage_group_guid}" -eq 36 ] || return 2

  # Do not infer anything from `dscl -search` exit codes: macOS versions do not provide a
  # sufficiently useful contract for distinguishing "no match" from a query failure. Enumerate
  # every local group and read each record instead; any failed enumeration/read is unknown.
  if ! usage_all_group_names="$(dscl . -list /Groups)"; then
    return 2
  fi
  while IFS= read -r usage_referencing_group; do
    [ -n "$usage_referencing_group" ] || continue
    [ "$usage_referencing_group" = "$usage_group_name" ] && continue
    if ! usage_referencing_record="$(dscl . -read "/Groups/$usage_referencing_group")"; then
      return 2
    fi
    if record_attribute_contains_token "$usage_referencing_record" NestedGroups "$usage_group_guid" ||
      record_attribute_contains_token "$usage_referencing_record" GroupMembers "$usage_group_guid" ||
      record_attribute_contains_token "$usage_referencing_record" GroupMembership "$usage_group_name"; then
      return 0
    fi
  done <<EOF
$usage_all_group_names
EOF
  return 1
}

user_record_state="unknown"
group_record_state="unknown"
if local_user_names="$(dscl . -list /Users)"; then
  if listing_contains_name "$local_user_names" "$user"; then
    user_record_state="present"
  else
    user_record_state="absent"
  fi
else
  echo "Could not enumerate local users; account cleanup will fail closed" >&2
  purge_incomplete=1
fi
if local_group_names="$(dscl . -list /Groups)"; then
  if listing_contains_name "$local_group_names" "$group"; then
    group_record_state="present"
  else
    group_record_state="absent"
  fi
else
  echo "Could not enumerate local groups; account cleanup will fail closed" >&2
  purge_incomplete=1
fi

current_group_gid=""
if [ "$group_record_state" = "present" ]; then
  current_group_gid="$(dscl_value "/Groups/$group" PrimaryGroupID || true)"
  case "$current_group_gid" in
    ''|*[!0-9]*) current_group_gid="" ;;
  esac
fi

if [ "$user_record_state" = "present" ]; then
  if [ "$user_created" -eq 1 ]; then
    user_uid="$(dscl_value "/Users/$user" UniqueID || true)"
    user_gid="$(dscl_value "/Users/$user" PrimaryGroupID || true)"
    user_shell="$(dscl_value "/Users/$user" UserShell || true)"
    user_home="$(dscl_value "/Users/$user" NFSHomeDirectory || true)"
    if [ "$user_shell" = "/usr/bin/false" ] &&
      [ "$user_home" = "/var/empty" ] &&
      [ -n "$current_group_gid" ] &&
      [ "$user_uid" = "$created_user_uid" ] &&
      [ "$user_gid" = "$created_user_primary_gid" ] &&
      [ "$user_gid" = "$current_group_gid" ]; then
      if dscl . -delete "/Users/$user"; then
        user_created=0
        created_user_uid="-"
        created_user_primary_gid="-"
      else
        echo "Could not delete marker-owned $user; preserving its ownership proof" >&2
        purge_incomplete=1
        owned_user_blocked=1
      fi
    else
      echo "Preserving $user because its attributes no longer match the installer account" >&2
      purge_incomplete=1
      owned_user_blocked=1
    fi
  else
    if [ "$ownership_marker_valid" -eq 1 ]; then
      echo "Preserving pre-existing $user account (not created by this package)" >&2
    else
      echo "Preserving $user because no root-only marker proves this package created it" >&2
      purge_incomplete=1
      owned_user_blocked=1
    fi
  fi
elif [ "$user_record_state" = "absent" ]; then
  user_created=0
  created_user_uid="-"
  created_user_primary_gid="-"
elif [ "$user_created" -eq 1 ]; then
  echo "Preserving marker-owned $user because local user enumeration failed" >&2
  purge_incomplete=1
  owned_user_blocked=1
fi

if [ "$group_record_state" = "present" ]; then
  if [ "$group_created" -eq 1 ]; then
    group_gid="$(dscl_value "/Groups/$group" PrimaryGroupID || true)"
    group_in_use=0
    group_usage_status=0
    if [ "$owned_user_blocked" -eq 1 ]; then
      # Keep the installer group available so an administrator can restore the user's original
      # PrimaryGroupID and retry the purge without having to reconstruct the group record.
      group_in_use=1
    fi
    case "$group_gid" in
      ''|*[!0-9]*) group_in_use=1 ;;
      *)
        if group_is_in_use "$group" "$group_gid"; then
          group_in_use=1
        else
          group_usage_status="$?"
        fi
        if [ "$group_usage_status" -eq 2 ]; then
          echo "Could not safely determine whether $group is still in use; preserving it (fail closed)" >&2
          group_in_use=1
          purge_incomplete=1
        fi
        ;;
    esac
    if [ "$group_gid" = "$created_group_gid" ] &&
      [ "$group_in_use" -eq 0 ]; then
      if dscl . -delete "/Groups/$group"; then
        group_created=0
        created_group_gid="-"
      else
        echo "Could not delete marker-owned $group; preserving its ownership proof" >&2
        purge_incomplete=1
      fi
    else
      echo "Preserving $group because its attributes changed or it is still in use" >&2
      purge_incomplete=1
    fi
  else
    if [ "$ownership_marker_valid" -eq 1 ]; then
      echo "Preserving pre-existing $group group (not created by this package)" >&2
    else
      echo "Preserving $group because no root-only marker proves this package created it" >&2
      purge_incomplete=1
    fi
  fi
elif [ "$group_record_state" = "absent" ]; then
  group_created=0
  created_group_gid="-"
elif [ "$group_created" -eq 1 ]; then
  echo "Preserving marker-owned $group because local group enumeration failed" >&2
  purge_incomplete=1
fi

write_ownership_marker() {
  marker_temporary="$ownership_dir/.account-ownership.$$"
  if [ -e "$marker_temporary" ] || [ -L "$marker_temporary" ]; then
    echo "Refusing to replace an unsafe Client ownership marker temporary path" >&2
    return 1
  fi
  if ! (
    umask 077
    set -C
    printf 'format=%s\nuser_created=%s\nuser_uid=%s\nuser_primary_gid=%s\ngroup_created=%s\ngroup_gid=%s\n' \
      "$package_version" "$user_created" "$created_user_uid" \
      "$created_user_primary_gid" "$group_created" "$created_group_gid" \
      > "$marker_temporary"
  ); then
    rm -f "$marker_temporary"
    return 1
  fi
  chown root:wheel "$marker_temporary" || { rm -f "$marker_temporary"; return 1; }
  chmod -N "$marker_temporary" || { rm -f "$marker_temporary"; return 1; }
  chmod 0600 "$marker_temporary" || { rm -f "$marker_temporary"; return 1; }
  validate_ownership_marker_path "$marker_temporary" || {
    rm -f "$marker_temporary"
    return 1
  }
  mv -f "$marker_temporary" "$ownership_marker" || {
    rm -f "$marker_temporary"
    return 1
  }
  # The temporary file and destination share the same validated directory, so rename(2)
  # preserves the already-verified inode metadata. Keep the rename as the final commit point:
  # a fallible post-commit check could no longer roll the account deletion back safely.
}

if [ "$ownership_marker_valid" -eq 1 ]; then
  if ! validate_ownership_directory ||
    ! validate_ownership_marker_path "$ownership_marker"; then
    echo "Client ownership proof changed during purge; preserving it for inspection" >&2
    ownership_marker_valid=0
    purge_incomplete=1
  elif [ "$user_created" -eq 0 ] && [ "$group_created" -eq 0 ]; then
    if [ "$purge_incomplete" -eq 0 ]; then
      if ! ownership_directory_contains_only_marker; then
        echo "Preserving the completed Client ownership marker because bookkeeping contains unexpected entries" >&2
        purge_incomplete=1
      elif ! rm -f "$ownership_marker" ||
        [ -e "$ownership_marker" ] || [ -L "$ownership_marker" ]; then
        echo "Could not remove the completed Client ownership marker" >&2
        purge_incomplete=1
        if [ ! -e "$ownership_marker" ] && [ ! -L "$ownership_marker" ] &&
          ! write_ownership_marker; then
          echo "Could not restore the completed Client ownership marker" >&2
        fi
      elif rmdir "$ownership_dir" >/dev/null 2>&1; then
        ownership_dir_state="absent"
      else
        echo "Could not remove the Client ownership directory; restoring its completed marker" >&2
        purge_incomplete=1
        if ! write_ownership_marker; then
          echo "Could not restore the completed Client ownership marker" >&2
        fi
      fi
    fi
  elif ! write_ownership_marker; then
    echo "Could not safely update the remaining Client ownership proof" >&2
    purge_incomplete=1
  fi
fi

if [ "$ownership_dir_state" = "valid" ] && [ "$purge_incomplete" -eq 0 ]; then
  if ! rmdir "$ownership_dir" >/dev/null 2>&1; then
    echo "Could not remove the non-empty Client ownership directory" >&2
    purge_incomplete=1
  fi
fi

if [ "$purge_incomplete" -eq 1 ]; then
  cat >&2 <<EOF
host-monitor purge is incomplete: local state, logs, and program files were removed, but an
installer-owned account/group or its ownership marker could not be safely removed. The package
receipt and maintenance helper were retained for repair and retry:
  $share/uninstall.sh
EOF
  exit 2
fi

if inspect_package_receipt; then
  receipt_state=0
else
  receipt_state="$?"
fi
case "$receipt_state" in
  0) pkgutil --forget "$identifier" >/dev/null ;;
  1) ;;
  *)
    echo "Package receipt state is unknown; the receipt and maintenance helper were retained for repair and retry." >&2
    exit 2
    ;;
esac

rm -f "$share/uninstall.sh"
rmdir "$share" >/dev/null 2>&1 || true
rmdir /usr/local/libexec >/dev/null 2>&1 || true

echo "host-monitor program and installer-owned local data were purged; server revoke is separate."
exit 0
