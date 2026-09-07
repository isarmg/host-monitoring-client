#!/bin/sh
set -eu
umask 022

die() {
  echo "build-pkg: $*" >&2
  exit 1
}

: "${BINARY:?set BINARY to the host-monitor binary}"
: "${VERSION:?set VERSION to the current numeric package version, for example 0.9.3}"

script_dir="$(CDPATH= cd "$(dirname "$0")" && pwd)"
repository_root="$(CDPATH= cd "$script_dir/../.." && pwd)"
config_template="$repository_root/config/host-monitor.json.example"
output="${OUTPUT:-host-monitor-$VERSION.pkg}"
installer_identity="${INSTALLER_IDENTITY:-}"

case "$VERSION" in
  ''|*[!0-9.]*|.*|*..*|*.)
    die "VERSION must contain one to four dot-separated numeric components"
    ;;
esac
component_count="$(printf '%s\n' "$VERSION" | awk -F. '{ print NF }')"
[ "$component_count" -le 4 ] || die "VERSION must contain at most four numeric components"

[ -f "$BINARY" ] || die "BINARY is not a regular file: $BINARY"
[ -x "$BINARY" ] || die "BINARY is not executable: $BINARY"
binary_version="$("$BINARY" --version)" || die "could not read the Client binary version"
[ "$binary_version" = "host-monitor $VERSION" ] ||
  die "BINARY version '$binary_version' does not match package VERSION $VERSION"
config_version="$(sed -n 's/^  "application_version": "\([^"]*\)",$/\1/p' "$config_template")"
[ "$config_version" = "$VERSION" ] ||
  die "host-monitor.json.example application_version '$config_version' does not match package VERSION $VERSION"
[ -n "$output" ] || die "OUTPUT must not be empty"
case "$output" in
  *.pkg) ;;
  *) die "OUTPUT must end in .pkg" ;;
esac
[ -d "$(dirname "$output")" ] || die "OUTPUT directory does not exist: $(dirname "$output")"
command -v pkgbuild >/dev/null 2>&1 || die "pkgbuild is required (run this script on macOS)"
command -v productbuild >/dev/null 2>&1 || die "productbuild is required (run this script on macOS)"
command -v plutil >/dev/null 2>&1 || die "plutil is required (run this script on macOS)"

plutil -lint "$script_dir/org.sarmg.hostmonitor.plist" >/dev/null
plutil -lint "$script_dir/org.sarmg.hostmonitor.logrotate.plist" >/dev/null
for installer_script in preinstall postinstall; do
  [ -x "$script_dir/scripts/$installer_script" ] ||
    die "$script_dir/scripts/$installer_script must be executable"
done

if [ -n "$installer_identity" ]; then
  case "$installer_identity" in
    *[[:cntrl:]]*) die "INSTALLER_IDENTITY must be a single line" ;;
    'Developer ID Installer: '*) ;;
    *)
      die "INSTALLER_IDENTITY must be the full 'Developer ID Installer: …' identity name"
      ;;
  esac
  command -v security >/dev/null 2>&1 || die "security is required for a signed build"
  command -v codesign >/dev/null 2>&1 || die "codesign is required for a signed build"
  # Do not use the `codesigning` policy here: Apple Installer identities are intentionally
  # distinct from application code-signing identities and that filter hides them.
  if ! security find-identity -v | grep -F "\"$installer_identity\"" >/dev/null; then
    die "installer signing identity was not found in the current keychain: $installer_identity"
  fi
  # A signed container does not make an unsigned payload trusted. Distribution builds must
  # sign the Mach-O independently with a Developer ID Application identity first.
  codesign --verify --strict --verbose=2 "$BINARY"
  binary_signature="$(codesign -d --verbose=4 "$BINARY" 2>&1)"
  if ! printf '%s\n' "$binary_signature" | grep -F 'Authority=Developer ID Application:' >/dev/null; then
    die "signed pkg builds require BINARY to use a Developer ID Application identity"
  fi
fi

work="$(mktemp -d)"
root="$work/root"
packages="$work/packages"
package_scripts="$work/scripts"
install -d "$root" "$packages" "$package_scripts"
trap 'rm -rf "$work"' EXIT
for installer_script in preinstall postinstall; do
  sed "s/@HOST_MONITOR_PACKAGE_VERSION@/$VERSION/g" \
    "$script_dir/scripts/$installer_script" >"$package_scripts/$installer_script"
  chmod 0755 "$package_scripts/$installer_script"
done
install -d "$root/usr/local/libexec" "$root/usr/local/bin" \
  "$root/usr/local/share/host-monitor" "$root/Library/LaunchDaemons"
install -m 0755 "$BINARY" "$root/usr/local/libexec/host-monitor"
ln -s ../libexec/host-monitor "$root/usr/local/bin/host-monitor"
install -m 0755 "$script_dir/host-monitor-logrotate" \
  "$root/usr/local/libexec/host-monitor-logrotate"
sed "s/@HOST_MONITOR_PACKAGE_VERSION@/$VERSION/g" "$script_dir/uninstall.sh" \
  >"$root/usr/local/share/host-monitor/uninstall.sh"
chmod 0755 "$root/usr/local/share/host-monitor/uninstall.sh"
install -m 0644 "$script_dir/newsyslog.conf" \
  "$root/usr/local/share/host-monitor/newsyslog.conf"
install -m 0644 "$script_dir/org.sarmg.hostmonitor.plist" \
  "$root/Library/LaunchDaemons/org.sarmg.hostmonitor.plist"
install -m 0644 "$script_dir/org.sarmg.hostmonitor.logrotate.plist" \
  "$root/Library/LaunchDaemons/org.sarmg.hostmonitor.logrotate.plist"
sed 's#"state_dir": "/var/lib/host-monitor"#"state_dir": "/Library/Application Support/host-monitor"#' \
  "$config_template" \
  >"$root/usr/local/share/host-monitor/host-monitor.json.example"
grep -F '"state_dir": "/Library/Application Support/host-monitor"' \
  "$root/usr/local/share/host-monitor/host-monitor.json.example" >/dev/null ||
  die "could not bind the packaged configuration to the macOS Client state directory"
chmod 0644 "$root/usr/local/share/host-monitor/host-monitor.json.example"

component="$packages/host-monitor-component.pkg"
pkgbuild --root "$root" --scripts "$package_scripts" --ownership recommended \
  --identifier org.sarmg.hostmonitor --version "$VERSION" --install-location / \
  "$component"

if [ -n "$installer_identity" ]; then
  productbuild --distribution "$script_dir/Distribution.xml" \
    --resources "$script_dir/Resources" --package-path "$packages" \
    --sign "$installer_identity" "$output"
  pkgutil --check-signature "$output"
  echo "Built signed installer package: $output"
  echo "Notarization and stapling are intentionally not performed by this script."
else
  productbuild --distribution "$script_dir/Distribution.xml" \
    --resources "$script_dir/Resources" --package-path "$packages" "$output"
  echo "Built unsigned package: $output"
  echo "Use only as an explicitly marked prerelease; it is not signed, notarized, or stapled."
fi
