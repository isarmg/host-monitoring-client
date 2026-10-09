#!/bin/sh
# Build only the read-only smartctl executable; never install/start smartd.
set -eu
umask 022
: "${OUTPUT_DIRECTORY:?set OUTPUT_DIRECTORY to a new payload directory}"
source_sha256=690b83ca331378da9ea0d9d61008c4b22dde391387b9bbad7f29387f2595f76e
prefix=/usr/local/libexec/xsoc-smartmontools
[ "$(uname -s):$(uname -m)" = Darwin:arm64 ] || {
  echo 'build-smartmontools requires native macOS arm64' >&2; exit 1;
}
[ ! -e "$OUTPUT_DIRECTORY" ] && [ ! -L "$OUTPUT_DIRECTORY" ] || {
  echo 'OUTPUT_DIRECTORY must not already exist' >&2; exit 1;
}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
archive="$work/smartmontools-7.5.tar.gz"
if [ -n "${SOURCE_ARCHIVE:-}" ]; then
  cp "$SOURCE_ARCHIVE" "$archive"
else
  curl --fail --location --retry 2 --proto '=https' --proto-redir '=https' \
    https://github.com/smartmontools/smartmontools/releases/download/RELEASE_7_5/smartmontools-7.5.tar.gz \
    --output "$archive"
fi
actual=$(shasum -a 256 "$archive")
[ "${actual%% *}" = "$source_sha256" ] || {
  echo 'smartmontools source SHA-256 mismatch' >&2; exit 1;
}
tar -xzf "$archive" -C "$work"
(
  cd "$work/smartmontools-7.5"
  ./configure --prefix="$prefix" --sbindir="$prefix/bin" \
    --with-drivedbdir="$prefix/share" --with-nvme-devicescan
  make -j "${BUILD_JOBS:-4}" smartctl
)
binary="$work/smartmontools-7.5/smartctl"
[ "$(lipo -archs "$binary")" = arm64 ] || { echo 'smartctl must be arm64' >&2; exit 1; }
"$binary" --version | head -1 | grep '^smartctl 7[.]5 ' >/dev/null
install -d "$OUTPUT_DIRECTORY/bin" "$OUTPUT_DIRECTORY/share" "$OUTPUT_DIRECTORY/source"
install -m 0755 "$binary" "$OUTPUT_DIRECTORY/bin/smartctl"
install -m 0644 "$work/smartmontools-7.5/drivedb.h" "$OUTPUT_DIRECTORY/share/drivedb.h"
install -m 0644 "$work/smartmontools-7.5/COPYING" "$OUTPUT_DIRECTORY/source/COPYING"
install -m 0644 "$archive" "$OUTPUT_DIRECTORY/source/smartmontools-7.5.tar.gz"
install -m 0644 "$0" "$OUTPUT_DIRECTORY/source/build-smartmontools.sh"
printf 'version=7.5\nsource_sha256=%s\nprefix=%s\nconfigure=--sbindir=PREFIX/bin --with-drivedbdir=PREFIX/share --with-nvme-devicescan\n' \
  "$source_sha256" "$prefix" > "$OUTPUT_DIRECTORY/source/BUILD-INFO.txt"
echo "Built verified smartctl payload: $OUTPUT_DIRECTORY"
