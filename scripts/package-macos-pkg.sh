#!/bin/sh
# Package a release build for macOS as a .pkg installer.
#
#   scripts/package-macos-pkg.sh <version> <bindir> <outdir>
#
# <bindir> holds universal (arm64 + x86_64) `ff`, `fflocated` and `ff-gui`; the
# release job lipos them from the two per-target builds first. The payload
# installs all three into /usr/local/bin, and the postinstall
# (scripts/macos-pkg-postinstall.sh) writes and loads the daemon's LaunchAgent
# for the user logged in at the console — so "install and configure" is one
# command, unlike the hand install in the README.
#
# No .app bundle: ff-gui is a bare binary (its Dock icon is embedded, see
# STATUS.md), so there is nothing to put an Info.plist in.
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
for b in ff fflocated ff-gui; do
    install -m 0755 "$bindir/$b" "$root/usr/local/bin/$b"
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
for b in ff fflocated ff-gui; do
    if ! pkgutil --payload-files "$component" | sed 's|^/||' | grep -qx "usr/local/bin/$b"; then
        echo "$0: payload is missing usr/local/bin/$b" >&2
        pkgutil --payload-files "$component" >&2
        exit 1
    fi
done

pkg="$outdir/fflocate-$version-macos-universal.pkg"
productbuild --package "$component" \
    --identifier com.fflocate.pkg \
    --version "$version" \
    "$pkg"

echo "$pkg"
