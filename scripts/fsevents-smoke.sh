#!/bin/sh
# Verify the macOS watcher end to end.
#
# The unit tests in fsevents.rs are macOS-gated, so a Linux `cargo test` never
# executes them. This script proves the parts that only a real Mac can: that the
# daemon starts with the FSEvents backend, indexes a fresh root, and then
# follows create / rename / delete without falling back to a full rescan.
set -eu

cd "$(dirname "$0")/.."

work=$(mktemp -d)
root="$work/root"
state="$work/state"
mkdir -p "$root" "$state"
printf 'seed\n' > "$root/seed.txt"

cargo build --release -p fflocated -p ff

./target/release/fflocated \
    --socket "$state/s.sock" \
    --state-dir "$state/store" \
    --root "$root" \
    --backend fsevents >"$state/log" 2>&1 &
daemon=$!
trap 'kill "$daemon" 2>/dev/null || true' EXIT

count() {
    # `ff --count` exits 1 when nothing matches (locate semantics), so a
    # non-zero status is not a failure by itself: the number on stdout is the
    # answer. Only an empty or non-numeric answer means the socket could not be
    # asked, and that is reported as `error` so it can never match a count.
    out=$(./target/release/ff --socket "$state/s.sock" --count "$1" 2>/dev/null) || true
    case $out in
        '' | *[!0-9]*) echo error ;;
        *) echo "$out" ;;
    esac
}

# wait_for_count <needle> <expected> <seconds>
wait_for_count() {
    needle=$1 expected=$2 limit=$3 n=0
    while [ "$n" -lt "$limit" ]; do
        if [ "$(count "$needle")" = "$expected" ]; then
            return 0
        fi
        n=$((n + 1))
        sleep 1
    done
    echo "FAIL: '$needle' never reached count $expected (still $(count "$needle"))" >&2
    echo "--- daemon log ---" >&2
    cat "$state/log" >&2 || true
    echo "--- stats ---" >&2
    ./target/release/ff --socket "$state/s.sock" stats >&2 || true
    return 1
}

# 1. The initial crawl must find the seed file.
wait_for_count seed.txt 1 60

# 2. A file created after startup must arrive through FSEvents (no rescan).
printf 'live\n' > "$root/live.txt"
wait_for_count live.txt 1 25

# 3. A rename must move the entry, not duplicate it.
mv "$root/live.txt" "$root/moved.txt"
wait_for_count moved.txt 1 25
wait_for_count live.txt 0 25

# 4. A delete must tombstone the entry.
rm "$root/moved.txt"
wait_for_count moved.txt 0 25

# 5. The backend must really be FSEvents, and no rescan must have papered over
#    a dropped event (a drop is allowed, an unexplained silence is not).
stats=$(./target/release/ff --socket "$state/s.sock" stats)
echo "$stats"
echo "$stats" | grep -q '"fsevents"' || {
    echo "FAIL: daemon is not running the fsevents backend" >&2
    exit 1
}

echo "ok: FSEvents indexed, created, renamed and deleted live"
