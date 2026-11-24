#!/bin/sh
# Packs the GPUI app into an AppImage with linuxdeploy (the release pinned for
# the Tauri app's AppImage). Build it first: cargo build --release in app-gpui.
# Usage: scripts/appimage-gpui.sh OUTPUT.AppImage
#
# Bundled: the libraries the binary links (xcb, xkbcommon). Left to the system,
# as on any desktop: Vulkan and its drivers, Wayland, fonts and glibc; GPUI
# loads Vulkan and Wayland at run time, and its fonts come from the system's
# font folders.
set -eu
output=$(realpath -m "$1")
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

linuxdeploy=${LINUXDEPLOY:-$HOME/.cache/tauri/linuxdeploy-x86_64.AppImage}
if [ ! -x "$linuxdeploy" ]; then
  mkdir -p "$(dirname "$linuxdeploy")"
  curl -fsSL -o "$linuxdeploy" \
    https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage
  echo "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d  $linuxdeploy" | sha256sum -c
  chmod +x "$linuxdeploy"
fi

cp "$root/app/src-tauri/icons/128x128.png" "$work/parallax-gpui.png"
# GPUI's license (Apache-2.0) asks for a copy to go with the program.
gpui=$(cargo metadata --format-version 1 --locked --manifest-path "$root/app-gpui/Cargo.toml" |
  python3 -c 'import json,sys,os; m=json.load(sys.stdin); print(next(os.path.dirname(p["manifest_path"]) for p in m["packages"] if p["name"]=="gpui"))')
mkdir -p "$work/AppDir/usr/share/doc/parallax-gpui"
cp "$root/LICENSE" "$root/THIRD-PARTY-NOTICES.md" "$work/AppDir/usr/share/doc/parallax-gpui/"
cp "$gpui/LICENSE-APACHE" "$work/AppDir/usr/share/doc/parallax-gpui/LICENSE-gpui-APACHE"
cd "$work"
# linuxdeploy is itself an AppImage; runners have no FUSE. LDAI_OUTPUT names
# the AppImage its built-in plugin writes.
APPIMAGE_EXTRACT_AND_RUN=1 LDAI_OUTPUT=app.AppImage LINUXDEPLOY_EXCLUDED_LIBRARIES='libwayland-*' \
  "$linuxdeploy" --appdir AppDir \
  --executable "$root/app-gpui/target/release/parallax-gpui" \
  --desktop-file "$root/app-gpui/packaging/parallax-gpui.desktop" \
  --icon-file "$work/parallax-gpui.png" \
  --output appimage
mkdir -p "$(dirname "$output")"
mv app.AppImage "$output"
echo "$output"
