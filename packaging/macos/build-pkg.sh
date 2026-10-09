#!/bin/sh
set -eu
umask 022

die() {
  echo "build-pkg: $*" >&2
  exit 1
}

: "${BINARY:?set BINARY to the xsoc binary}"
: "${VERSION:?set VERSION to the current numeric package version, for example 1.0.0}"
: "${SMART_PAYLOAD:?build smartctl with packaging/macos/build-smartmontools.sh and set SMART_PAYLOAD}"

script_dir="$(CDPATH= cd "$(dirname "$0")" && pwd)"
repository_root="$(CDPATH= cd "$script_dir/../.." && pwd)"
config_template="$repository_root/config/xsoc.json.example"
output="${OUTPUT:-xsoc-$VERSION.pkg}"
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
command -v lipo >/dev/null 2>&1 ||
  die "lipo is required to verify the macOS binary architecture"
binary_architectures="$(lipo -archs "$BINARY")" ||
  die "BINARY is not a readable Mach-O binary"
[ "$binary_architectures" = "arm64" ] ||
  die "BINARY must contain only arm64; found: $binary_architectures"
binary_version="$("$BINARY" --version)" || die "could not read the Client binary version"
[ "$binary_version" = "xsoc $VERSION" ] ||
  die "BINARY version '$binary_version' does not match frozen configuration format 1.0.0"
smart_binary="$SMART_PAYLOAD/bin/smartctl"
[ -f "$smart_binary" ] && [ ! -L "$smart_binary" ] && [ -x "$smart_binary" ] || die "missing regular bundled smartctl"
[ "$(lipo -archs "$smart_binary")" = arm64 ] || die "bundled smartctl must contain only arm64"
"$smart_binary" --version | head -1 | grep '^smartctl 7[.]5 ' >/dev/null || die "bundled smartctl must be version 7.5"
for verified_input in \
  'source/smartmontools-7.5.tar.gz:690b83ca331378da9ea0d9d61008c4b22dde391387b9bbad7f29387f2595f76e' \
  'source/COPYING:8177f97513213526df2cf6184d8ff986c675afb514d4e68a404010521b880643' \
  'share/drivedb.h:05057d5fa65eea87aa8ab9e051ba24bb71a4e032fdf1044f6c3326a42d9d5d14'
do
  input_path="$SMART_PAYLOAD/${verified_input%%:*}"
  [ -f "$input_path" ] && [ ! -L "$input_path" ] || die "missing regular smartmontools input: $input_path"
  input_digest="$(shasum -a 256 "$input_path")"
  [ "${input_digest%% *}" = "${verified_input#*:}" ] || die "smartmontools input digest mismatch: $input_path"
done
for source_info in build-smartmontools.sh BUILD-INFO.txt; do
  [ -f "$SMART_PAYLOAD/source/$source_info" ] && [ ! -L "$SMART_PAYLOAD/source/$source_info" ] || die "missing smartmontools build provenance: $source_info"
done
config_version="$(sed -n 's/^  "application_version": "\([^"]*\)",$/\1/p' "$config_template")"
[ "$config_version" = "1.0.0" ] ||
  die "xsoc.json.example application_version '$config_version' does not match frozen configuration format 1.0.0"
[ -n "$output" ] || die "OUTPUT must not be empty"
case "$output" in
  *.pkg) ;;
  *) die "OUTPUT must end in .pkg" ;;
esac
[ -d "$(dirname "$output")" ] || die "OUTPUT directory does not exist: $(dirname "$output")"
command -v pkgbuild >/dev/null 2>&1 || die "pkgbuild is required (run this script on macOS)"
command -v productbuild >/dev/null 2>&1 || die "productbuild is required (run this script on macOS)"
command -v plutil >/dev/null 2>&1 || die "plutil is required (run this script on macOS)"

plutil -lint "$script_dir/org.sarmg.xsoc.plist" >/dev/null
plutil -lint "$script_dir/org.sarmg.xsoc.logrotate.plist" >/dev/null
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
  codesign --verify --strict --verbose=2 "$smart_binary"
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
  sed "s/@XSOC_PACKAGE_VERSION@/$VERSION/g" \
    "$script_dir/scripts/$installer_script" >"$package_scripts/$installer_script"
  chmod 0755 "$package_scripts/$installer_script"
done
install -d "$root/usr/local/libexec" "$root/usr/local/bin" \
  "$root/usr/local/share/xsoc" "$root/Library/LaunchDaemons"
install -m 0755 "$BINARY" "$root/usr/local/libexec/xsoc"
install -d "$root/usr/local/libexec/xsoc-smartmontools/bin" \
  "$root/usr/local/libexec/xsoc-smartmontools/share" \
  "$root/usr/local/share/xsoc/smartmontools"
install -m 0755 "$smart_binary" "$root/usr/local/libexec/xsoc-smartmontools/bin/smartctl"
install -m 0644 "$SMART_PAYLOAD/share/drivedb.h" "$root/usr/local/libexec/xsoc-smartmontools/share/drivedb.h"
for source_info in COPYING smartmontools-7.5.tar.gz build-smartmontools.sh BUILD-INFO.txt; do
  install -m 0644 "$SMART_PAYLOAD/source/$source_info" "$root/usr/local/share/xsoc/smartmontools/$source_info"
done
ln -s ../libexec/xsoc "$root/usr/local/bin/xsoc"
install -m 0755 "$script_dir/xsoc-logrotate" \
  "$root/usr/local/libexec/xsoc-logrotate"
sed "s/@XSOC_PACKAGE_VERSION@/$VERSION/g" "$script_dir/uninstall.sh" \
  >"$root/usr/local/share/xsoc/uninstall.sh"
chmod 0755 "$root/usr/local/share/xsoc/uninstall.sh"
install -m 0644 "$script_dir/newsyslog.conf" \
  "$root/usr/local/share/xsoc/newsyslog.conf"
install -m 0644 "$script_dir/org.sarmg.xsoc.plist" \
  "$root/Library/LaunchDaemons/org.sarmg.xsoc.plist"
install -m 0644 "$script_dir/org.sarmg.xsoc.logrotate.plist" \
  "$root/Library/LaunchDaemons/org.sarmg.xsoc.logrotate.plist"
sed 's#"state_dir": "/var/lib/xsoc"#"state_dir": "/Library/Application Support/xsoc"#' \
  "$config_template" \
  >"$root/usr/local/share/xsoc/xsoc.json.example"
grep -F '"state_dir": "/Library/Application Support/xsoc"' \
  "$root/usr/local/share/xsoc/xsoc.json.example" >/dev/null ||
  die "could not bind the packaged configuration to the macOS Client state directory"
chmod 0644 "$root/usr/local/share/xsoc/xsoc.json.example"

component="$packages/xsoc-component.pkg"
pkgbuild --root "$root" --scripts "$package_scripts" --ownership recommended \
  --identifier org.sarmg.xsoc --version "$VERSION" --install-location / \
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
