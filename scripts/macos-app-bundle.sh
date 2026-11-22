#!/bin/sh
# Assemble fflocate.app around the universal ff-gui binary.
#
#   scripts/macos-app-bundle.sh <version> <bindir> <appdir>
#
# <bindir> holds the universal `ff-gui`; <appdir> is the bundle to create
# (…/fflocate.app). The .pkg puts it in /Applications, which is what makes the
# GUI double-clickable in Finder and gives it a Finder icon and a version — a
# bare binary in /usr/local/bin has neither.
#
# The icon is built from the 1024 px art already in the tree with `sips` and
# `iconutil`. On a host without them (Linux, where
# scripts/verify-release-workflow.py runs the packaging under stubs) the icon is
# skipped and the bundle is otherwise identical: the binary still sets its own
# Dock icon at runtime from the embedded PNG.
#
# Info.plist is written by hand rather than through `plutil`, so the file is the
# same on every host and its contents can be checked where it is packaged.
set -eu

if [ "$#" -ne 3 ]; then
    echo "usage: $0 <version> <bindir> <appdir>" >&2
    exit 2
fi

version=$1
bindir=$2
app=$3

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
icon_src=$repo_root/crates/ff-gui/assets/fflocate.png

if [ ! -f "$bindir/ff-gui" ]; then
    echo "$0: no binary at $bindir/ff-gui" >&2
    exit 1
fi

# CFBundleVersion is three period-separated integers; the marketing string
# ("0.1.1-beta") belongs in CFBundleShortVersionString, which allows it.
build_version=$(printf '%s' "$version" | sed 's/-.*//')
if [ -z "$build_version" ]; then
    echo "$0: cannot derive a CFBundleVersion from '$version'" >&2
    exit 1
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 0755 "$bindir/ff-gui" "$app/Contents/MacOS/ff-gui"

# ---- icon ------------------------------------------------------------------
# AppIcon.icns is the only form Finder and the Dock read before the app runs.
icon_key=
if [ -f "$icon_src" ] && command -v sips >/dev/null 2>&1 && command -v iconutil >/dev/null 2>&1; then
    iconset=$(mktemp -d)/AppIcon.iconset
    mkdir -p "$iconset"
    for spec in \
        "16 16x16" "32 16x16@2x" \
        "32 32x32" "64 32x32@2x" \
        "128 128x128" "256 128x128@2x" \
        "256 256x256" "512 256x256@2x" \
        "512 512x512" "1024 512x512@2x"
    do
        set -- $spec
        sips -z "$1" "$1" "$icon_src" --out "$iconset/icon_$2.png" >/dev/null
    done
    if iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns" >/dev/null 2>&1; then
        icon_key='  <key>CFBundleIconFile</key><string>AppIcon</string>'
    else
        echo "$0: iconutil could not build AppIcon.icns; packaging without it" >&2
    fi
    rm -rf "$(dirname -- "$iconset")"
fi

# ---- Info.plist ------------------------------------------------------------
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>fflocate</string>
  <key>CFBundleExecutable</key><string>ff-gui</string>
  <key>CFBundleIdentifier</key><string>com.fflocate.ffgui</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>fflocate</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$build_version</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
$icon_key
</dict>
</plist>
EOF

# An unparseable plist is an app macOS refuses to launch, so lint it where the
# tool is Apple's. Linux ships a swift-corelibs `plutil` that rejects a valid
# plist outright, so the check is Darwin-only; the packaging script greps this
# file's contents on every host either way.
if [ "$(uname -s)" = Darwin ] && command -v plutil >/dev/null 2>&1; then
    plutil -lint "$app/Contents/Info.plist" >/dev/null
fi

echo "$app"
