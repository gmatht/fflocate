#!/bin/sh
# Package a release build for Linux: a .deb for a package manager, and a plain
# tarball for everywhere else.
#
#   scripts/package-linux.sh <version> <bindir> <outdir>
#
# <bindir> is a release build (`ff`, `fflocated`, `ff-gui`); <outdir> is created
# if it does not exist. Both artifacts are versioned, so a release job only has
# to upload `dist/*`.
#
# The .deb installs under /usr/bin and carries a systemd *user* unit at
# /usr/lib/systemd/user/fflocated.service, so `systemctl --user enable --now
# fflocated` is the whole setup. Nothing is enabled by the package itself: the
# daemon indexes one user's $HOME and keeps its socket and index there, so
# switching it on for every account is the admin's decision. (`ff service
# install` writes the same unit under ~/.config instead, for a tarball user.)
#
# ff-gui only *recommends* GTK3: the binary dlopens it, so a headless install
# (daemon + CLI only) is still a valid one.
# A packaging run must not inherit the caller's umask: dpkg records the modes
# it finds, and a file written 0664 by a umask 002 shell is a lintian warning in
# every install.
umask 022
set -eu

if [ "$#" -ne 3 ]; then
    echo "usage: $0 <version> <bindir> <outdir>" >&2
    exit 2
fi

version=$1
bindir=$2
outdir=$3

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

for b in ff fflocated ff-gui; do
    # Existence, not the mode: the staging below installs each binary 0755, so a
    # copied or cached build without the bit is still packageable.
    if [ ! -f "$bindir/$b" ]; then
        echo "$0: no binary at $bindir/$b" >&2
        echo "  build it with: cargo build --release -p ff -p fflocated -p ff-gui" >&2
        exit 1
    fi
done

mkdir -p "$outdir"
outdir=$(CDPATH='' cd -- "$outdir" && pwd)
# One scratch tree for both layouts; dpkg records the mode of `$stage` as `./`,
# and `install -D` creates it 0755 despite mktemp's 0700 parent.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage=$work/deb
archive=$work/tar/fflocate-$version

arch=$(dpkg --print-architecture)
unit=fflocated.service

# A packaged install lives in /usr/bin, so the unit is written here rather than
# taken from `ff service unit`, whose ExecStart points at ~/.cargo/bin.
write_unit() {
    cat <<'EOF'
[Unit]
Description=fflocate filename index daemon
Documentation=https://github.com/gmatht/fflocate

[Service]
Type=simple
ExecStart=/usr/bin/fflocated
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
EOF
}

# ---- .deb ------------------------------------------------------------------
install -D -m 0755 "$bindir/ff"        "$stage/usr/bin/ff"
install -D -m 0755 "$bindir/fflocated" "$stage/usr/bin/fflocated"
install -D -m 0755 "$bindir/ff-gui"    "$stage/usr/bin/ff-gui"
install -D -m 0644 "$repo_root/README.md" "$stage/usr/share/doc/fflocate/README.md"
mkdir -p "$stage/usr/lib/systemd/user"
write_unit > "$stage/usr/lib/systemd/user/$unit"

mkdir -p "$stage/DEBIAN"
installed_size=$(du -sk "$stage/usr" | cut -f1)
cat > "$stage/DEBIAN/control" <<EOF
Package: fflocate
Version: $version
Architecture: $arch
Installed-Size: $installed_size
Maintainer: gmatht <gmatht@users.noreply.github.com>
Section: utils
Priority: optional
Homepage: https://github.com/gmatht/fflocate
Recommends: libgtk-3-0 | libgtk-3-0t64
Description: realtime filename index and search
 fflocated watches the filesystem and keeps a filename index; ff queries it
 over a unix socket, and ff-gui is a graphical browser over the same protocol.
 .
 The daemon is per user: enable it with "systemctl --user enable --now
 fflocated". It indexes \$HOME and answers only what the caller may read.
EOF

deb="$outdir/fflocate_${version}_${arch}.deb"
dpkg-deb --root-owner-group --build "$stage" "$deb"

# ---- tarball ---------------------------------------------------------------
# The layout the README documents by hand, minus the paths the reader chooses.
tree=$archive
install -D -m 0755 "$bindir/ff"        "$tree/bin/ff"
install -D -m 0755 "$bindir/fflocated" "$tree/bin/fflocated"
install -D -m 0755 "$bindir/ff-gui"    "$tree/bin/ff-gui"
install -D -m 0644 "$repo_root/README.md" "$tree/README.md"
mkdir -p "$tree/systemd"
write_unit > "$tree/systemd/$unit"

tgz="$outdir/fflocate-$version-linux-$(uname -m).tar.gz"
tar -C "$work/tar" -czf "$tgz" "fflocate-$version"

echo "$deb"
echo "$tgz"
