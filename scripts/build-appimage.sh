#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n1)"
arch="$(uname -m)"
app_name="USB Capture"
app_dir_name="USB_Capture.AppDir"
dist_dir="$root/dist"
app_dir="$dist_dir/$app_dir_name"
cache_dir="$root/.cache/appimage"
output="$dist_dir/USB_Capture-${version}-${arch}.AppImage"
tool="$cache_dir/appimagetool-${arch}.AppImage"
tool_url="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${arch}.AppImage"

echo "Building USB Capture $version ($arch)"
cargo build --release

rm -rf "$app_dir"
mkdir -p "$cache_dir" "$app_dir/usr/bin" "$app_dir/usr/lib" \
  "$app_dir/usr/share/applications" \
  "$app_dir/usr/share/metainfo" \
  "$app_dir/usr/share/icons/hicolor/256x256/apps" \
  "$app_dir/usr/share/icons/hicolor/512x512/apps" \
  "$app_dir/usr/share/icons/hicolor/scalable/apps"

install -m 755 "$root/target/release/shadowcast-app" "$app_dir/usr/bin/shadowcast-app"
if command -v strip >/dev/null; then
  strip "$app_dir/usr/bin/shadowcast-app" || true
fi

# Bundle portable X11/xkb helper libs. Leave PipeWire, ALSA, Vulkan, Wayland,
# and libc on the host so the app talks to the running session.
bundle_lib() {
  local name="$1"
  local found=""
  local cache
  cache="$(ldconfig -p 2>/dev/null || true)"
  found="$(awk -v n="$name" '$1 == n { print $NF; exit }' <<<"$cache")"
  if [[ -z "$found" || ! -e "$found" ]]; then
    shopt -s nullglob
    local candidates=(/usr/lib/"$name" /usr/lib64/"$name" /lib/"$name" /lib64/"$name" /usr/lib/*/"$name")
    shopt -u nullglob
    if [[ ${#candidates[@]} -gt 0 && -e "${candidates[0]}" ]]; then
      found="${candidates[0]}"
    fi
  fi
  if [[ -n "$found" && -e "$found" ]]; then
    cp -L "$found" "$app_dir/usr/lib/"
    echo "Bundled $name"
  else
    echo "Warning: $name not found; AppImage will use the host copy" >&2
  fi
}

bundle_lib libxkbcommon.so.0
bundle_lib libxkbcommon-x11.so.0
bundle_lib libxcb.so.1
bundle_lib libxcb-xkb.so.1
bundle_lib libXau.so.6
bundle_lib libXdmcp.so.6

install -m 644 "$root/packaging/linux/com.crwgrant.usb-capture.desktop" \
  "$app_dir/usr/share/applications/com.crwgrant.usb-capture.desktop"
install -m 644 "$root/packaging/linux/com.crwgrant.usb-capture.metainfo.xml" \
  "$app_dir/usr/share/metainfo/com.crwgrant.usb-capture.metainfo.xml"
install -m 644 "$root/packaging/linux/com.crwgrant.usb-capture.metainfo.xml" \
  "$app_dir/usr/share/metainfo/com.crwgrant.usb-capture.appdata.xml"
install -m 644 "$root/packaging/linux/usb-capture.svg" \
  "$app_dir/usr/share/icons/hicolor/scalable/apps/usb-capture.svg"

rsvg-convert -w 256 -h 256 "$root/packaging/linux/usb-capture.svg" \
  -o "$app_dir/usr/share/icons/hicolor/256x256/apps/usb-capture.png"
rsvg-convert -w 512 -h 512 "$root/packaging/linux/usb-capture.svg" \
  -o "$app_dir/usr/share/icons/hicolor/512x512/apps/usb-capture.png"

ln -sfn usr/share/applications/com.crwgrant.usb-capture.desktop \
  "$app_dir/com.crwgrant.usb-capture.desktop"
ln -sfn usr/share/icons/hicolor/256x256/apps/usb-capture.png "$app_dir/usb-capture.png"

cat > "$app_dir/AppRun" << 'EOF'
#!/bin/bash
set -euo pipefail
HERE="$(dirname "$(readlink -f "$0")")"
export PATH="$HERE/usr/bin:${PATH:-}"
export LD_LIBRARY_PATH="$HERE/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/usr/bin/shadowcast-app" "$@"
EOF
chmod +x "$app_dir/AppRun"

if [[ ! -x "$tool" ]]; then
  echo "Downloading appimagetool"
  curl -fsSL "$tool_url" -o "$tool"
  chmod +x "$tool"
fi

export ARCH="$arch"
export APPIMAGE_EXTRACT_AND_RUN=1
"$tool" "$app_dir" "$output"

chmod +x "$output"
echo "Wrote $output"
ls -lh "$output"
