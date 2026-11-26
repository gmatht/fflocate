#!/bin/sh
# Build the multi-call binary with each front end's code ordered after `.text`.
#
# The extra output sections (`.text.gui`, `.text.cli`) come from GNU ld's
# INSERT support. The flag is set only for the build target's rustflags, so an
# ordinary `cargo build` keeps the default layout and this script is the one
# place that opts into ordering.
#
# Mach-O has no linker scripts: on macOS this is a plain build, with the GUI
# code interleaved (present in the image, but only faulted in when the GUI
# runs — the ordering only bounds readahead around a text fault).
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
os=$(uname -s)

if [ "$os" = "Linux" ]; then
    script="$root/crates/ffbusy/sections.ld"
    triple=$(rustc -vV | sed -n 's/^host: //p' | tr 'a-z-' 'A-Z_')
    eval "export CARGO_TARGET_${triple}_RUSTFLAGS='-C link-arg=-Wl,-T,$script'"
    # Cargo hashes rustflags but not the file they point at, so editing the
    # script alone leaves a fingerprint-unchanged build: cargo reports nothing
    # to do and the old layout stays on disk. Dirty the crate that owns it.
    out="$root/target/release/ffbusy"
    if [ -f "$out" ] && [ "$script" -nt "$out" ]; then
        touch "$root/crates/ffbusy/src/main.rs"
    fi
else
    echo "build-ffbusy: $os has no linker scripts; building without section ordering" >&2
fi

exec cargo build -p ffbusy --release --bin ffbusy "$@"
