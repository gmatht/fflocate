#!/usr/bin/env bash
# Run the non-hermetic end-to-end tests: the ones that spawn real `fflocated`
# and `ff` binaries and, in some cases, need a real kernel (inotify, and the
# crash/mount/watch-limit fault injection from DESIGN.md §14.3).
#
# These are kept out of a plain `cargo test` so that `cargo test` stays hermetic
# and fast. The tests skip themselves when FFLOCATE_BIN_DIR is unset.
#
# Usage:
#   scripts/e2e.sh                 # build release, run the e2e suite
#   FFLOCATE_E2E_SOAK=1 scripts/e2e.sh   # additionally run the soak (§14.5)
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

echo "== building release =="
cargo build --release

export FFLOCATE_BIN_DIR="$ROOT/target/release"
echo "FFLOCATE_BIN_DIR=$FFLOCATE_BIN_DIR"

echo
echo "== crash / fault-injection: real fflocated, kill -9, torn journal (§14.3c) =="
cargo test -p fflocated --test fault_crash -- --test-threads=1 --nocapture

echo
echo "== mount/umount during crawl, watch-limit exhaustion (§14.3d,e) =="
cargo test -p fflocated --test fault_fsops -- --test-threads=1 --nocapture

if [ "${FFLOCATE_E2E_SOAK:-0}" = "1" ]; then
  echo
  echo "== soak: thousands of ops with periodic full-tree audits (§14.5) =="
  cargo test -p fflocate --release --test soak -- --nocapture
fi

echo
echo "== daemon end-to-end (real crawl, live events, journal restart, socket) =="
cargo test -p fflocated --test daemon_e2e -- --nocapture

echo
echo "== locate compatibility via symlinked argv[0] (§12) =="
cargo test -p ff --test locate_e2e -- --test-threads=1 --nocapture

echo
echo "== manual update heals offline edits (§7/§15) =="
cargo test -p ff --test update_e2e -- --test-threads=1 --nocapture

echo
echo "all e2e checks passed"
