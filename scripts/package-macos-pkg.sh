#!/bin/sh
# Package a release build for macOS as a .pkg installer.
#
#   scripts/package-macos-pkg.sh <version> <bindir> <outdir>
#
# <bindir> holds universal (arm64 + x86_64) `ff`, `fflocated` and `ff-gui`; the
# release job lipos them from the two per-target builds first. The payload
# installs the CLI and daemon into /usr/local/bin, `fflocate.app` into
# /Applications (with `ff-gui` on PATH as a link into it), and the postinstall
# (scripts/macos-pkg-postinstall.sh) writes and loads the daemon's LaunchAgent
# for the user logged in at the console — so "install and configure" is one
# command, unlike the hand install in the README.
#
# The .app is what makes the GUI a normal Mac application: double-clickable in
# Finder, with a bundle identifier, a Finder icon and a version. Without it the
# user has a bare binary in /usr/local/bin that Finder cannot open and whose
# only icon is the one it paints on its Dock tile at runtime.
#
# The package is unsigned. Once a "Developer ID Installer" certificate exists,
# sign between productbuild and the upload with
#   productsign --sign "Developer ID Installer: ..." in.pkg out.pkg
# and notarise the result (`notarytool submit --wait`, then `stapler staple`).
set -eu

if [ "$#" -ne 3 ]; then
    echo "usage: $0 <version> <bindir> <outdir>" >&2
    exit 2
fi

version=$1
bindir=$2
outdir=$3

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

for b in ff fflocated ff-gui; do
    # Existence, not the mode: this script installs each binary with an explicit
    # 0755 into the payload anyway, and `lipo -create` is not obliged to carry
    # the input's executable bit on its output.
    if [ ! -f "$bindir/$b" ]; then
        echo "$0: no binary at $bindir/$b" >&2
        exit 1
    fi
    # A .pkg that quietly carried one architecture would be a broken universal
    # installer, and `lipo -create` of two missing slices still exits 0.
    archs=$(lipo -archs "$bindir/$b")
    case "$archs" in
        *arm64*x86_64* | *x86_64*arm64*) ;;
        *)
            echo "$0: $bindir/$b is not universal (lipo -archs: $archs)" >&2
            echo "  merge the two builds with: lipo -create <arm64> <x86_64> -output $bindir/$b" >&2
            exit 1
            ;;
    esac
done

mkdir -p "$outdir"
outdir=$(CDPATH='' cd -- "$outdir" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# ---- payload ---------------------------------------------------------------
root="$work/root"
mkdir -p "$root/usr/local/bin"
for b in ff fflocated; do
    install -m 0755 "$bindir/$b" "$root/usr/local/bin/$b"
done

# The GUI ships as an .app bundle in /Applications; `ff-gui` stays on PATH as a
# link into it, so `ff --gui` and a terminal launch work and there is one copy
# of the binary on disk.
"$here/macos-app-bundle.sh" "$version" "$bindir" "$root/Applications/fflocate.app"
ln -s /Applications/fflocate.app/Contents/MacOS/ff-gui "$root/usr/local/bin/ff-gui"

# The bundle's own claims, checked where it is built: a plist that names the
# wrong executable is an app that will not launch, and macOS will not tell the
# user why.
plist="$root/Applications/fflocate.app/Contents/Info.plist"
for want in '<string>ff-gui</string>' "<string>$version</string>" 'com.fflocate.ffgui'; do
    if ! grep -qF "$want" "$plist"; then
        echo "$0: $plist does not contain $want" >&2
        exit 1
    fi
done

# ---- scripts ---------------------------------------------------------------
scripts="$work/scripts"
mkdir -p "$scripts"
install -m 0755 "$here/macos-pkg-postinstall.sh" "$scripts/postinstall"

component="$work/fflocate-component.pkg"
pkgbuild --root "$root" \
    --identifier com.fflocate.pkg \
    --version "$version" \
    --root-owner-group \
    --scripts "$scripts" \
    --install-location / \
    "$component"

# pkgbuild can exit 0 with an empty payload; the installed files are the point.
# `pkgutil --payload-files` prints a leading `.` and then `./`-prefixed paths
# (an older form used a leading `/`), so both are stripped before comparing.
payload=$(pkgutil --payload-files "$component" | sed -e 's|^\./||' -e 's|^/||')
for f in usr/local/bin/ff usr/local/bin/fflocated usr/local/bin/ff-gui \
         Applications/fflocate.app/Contents/Info.plist \
         Applications/fflocate.app/Contents/MacOS/ff-gui; do
    if ! printf '%s\n' "$payload" | grep -qx "$f"; then
        echo "$0: payload is missing $f" >&2
        printf '%s\n' "$payload" >&2
        exit 1
    fi
done

pkg="$outdir/fflocate-$version-macos-universal.pkg"
productbuild --package "$component" \
    --identifier com.fflocate.pkg \
    --version "$version" \
    "$pkg"

echo "$pkg"
