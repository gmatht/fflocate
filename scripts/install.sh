#!/bin/sh
# Install a release build of fflocate and (re)start its daemon under systemd.
#
#   scripts/install.sh [options]
#
# scripts/package-linux.sh builds artifacts for distribution; this installs on
# the machine you are on. It builds the release binaries, copies them to a
# prefix, writes the unit whose ExecStart matches where they actually landed,
# reloads systemd and restarts the daemon — the steps that otherwise get done
# by hand and drift apart (a stale /usr/local/bin/fflocated under a unit that
# was never re-pointed is how a machine ends up serving an older protocol than
# its clients speak).
#
# Two instances, because they are two different services:
#
#   (default)  per-user:  ~/.local/bin + a `systemd --user` unit. Indexes $HOME
#                         and keeps its socket and store in the user's own
#                         runtime/state directories.
#   --system   machine:   /usr/local/bin + a system unit. Runs as root, indexes
#                         privileged paths, and answers each query as the
#                         caller. Needs root, and the socket group (`fflocate`
#                         by default) that clients must be in to connect.
#
# Nothing is deleted and no store is touched: restarting a daemon reloads its
# snapshot and catches up on what changed while it was down.
#
# Options:
#   --system            install the machine-wide instance (must run as root)
#   --prefix DIR        where the binaries go (default: ~/.local/bin, or
#                       /usr/local/bin with --system)
#   --socket PATH       daemon socket (default: the daemon's own: the user's
#                       runtime dir, or /run/fflocated.sock with --system)
#   --state-dir DIR     index/store directory (default: the daemon's own)
#   --root DIR          index this directory instead of the daemon's default
#                       roots. Repeatable. Without it a per-user daemon
#                       indexes every native mount, including `/`, which a
#                       normal user cannot watch (the root is then degraded and
#                       every change is found by rescanning the whole tree).
#   --group NAME        system socket group (default: fflocate)
#   --no-build          use the binaries already in target/release
#   --no-start          install binaries + unit, do not enable or start
#   --force             stop a running daemon that systemd does not manage
#   --dry-run           print what would be installed, change nothing
#   -h, --help          this text
set -eu

usage() {
    sed -n '2,/^set -eu$/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
}

die() {
    printf '%s: %s\n' "$0" "$*" >&2
    exit 1
}

log() {
    printf '%s\n' "$*"
}

mode=user
prefix=
socket=
state_dir=
# Newline-separated, because a path may contain a space (or a tab) and the
# unit's ExecStart needs each root as its own quoted argument.
roots=
group=fflocate
build=1
start=1
force=0
dry_run=0

while [ "$#" -gt 0 ]; do
    case $1 in
        --system) mode=system ;;
        --prefix) shift; [ "$#" -gt 0 ] || die "--prefix needs a directory"; prefix=$1 ;;
        --socket) shift; [ "$#" -gt 0 ] || die "--socket needs a path"; socket=$1 ;;
        --state-dir) shift; [ "$#" -gt 0 ] || die "--state-dir needs a directory"; state_dir=$1 ;;
        --root)
            shift
            [ "$#" -gt 0 ] || die "--root needs a directory"
            roots="$roots$1
"
            ;;
        --group) shift; [ "$#" -gt 0 ] || die "--group needs a name"; group=$1 ;;
        --no-build) build=0 ;;
        --no-start) start=0 ;;
        --force) force=1 ;;
        --dry-run) dry_run=1 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
    shift
done

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

case $mode in
    system)
        [ -n "$prefix" ] || prefix=/usr/local/bin
        unit_dir=/etc/systemd/system
        unit=$unit_dir/fflocated.service
        wanted_by=multi-user.target
        default_socket=/run/fflocated.sock
        pidfile=/run/fflocated.pid
        if [ "$(id -u)" -ne 0 ] && [ "$dry_run" -eq 0 ]; then
            die "--system installs a root daemon and a unit under /etc; re-run as root"
        fi
        ;;
    *)
        [ -n "$prefix" ] || prefix=$HOME/.local/bin
        unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
        unit=$unit_dir/fflocated.service
        wanted_by=default.target
        default_socket=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/fflocate.sock
        pidfile=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/fflocate.pid
        ;;
esac

[ -n "$socket" ] || socket=$default_socket

# A command run for real, or printed when --dry-run asked for the plan.
run() {
    if [ "$dry_run" -eq 1 ]; then
        printf '  + %s\n' "$*"
        return 0
    fi
    "$@"
}

# ---- build -----------------------------------------------------------------
bindir=$repo_root/target/release
if [ "$build" -eq 1 ]; then
    # `--system` runs under sudo, and root's PATH does not contain the
    # invoking user's toolchain (a rustup install lives in ~/.cargo/bin, and
    # its CARGO_HOME/RUSTUP_HOME are in that user's home too). A bare `cargo`
    # therefore fails with "cargo: not found" *before* anything is installed,
    # which reads like a broken script rather than a missing tool. Find the
    # binary explicitly, including under SUDO_USER's home, and say what to do
    # when it really is missing.
    cargo_bin=$(command -v cargo 2>/dev/null || true)
    if [ -z "$cargo_bin" ] && [ -n "${SUDO_USER:-}" ]; then
        sudo_home=$(getent passwd "$SUDO_USER" 2>/dev/null | cut -d: -f6)
        if [ -n "$sudo_home" ] && [ -x "$sudo_home/.cargo/bin/cargo" ]; then
            cargo_bin=$sudo_home/.cargo/bin/cargo
            # rustup resolves the toolchain through these; without them the
            # binary runs but cannot find a toolchain of its own.
            CARGO_HOME=${CARGO_HOME:-$sudo_home/.cargo}
            RUSTUP_HOME=${RUSTUP_HOME:-$sudo_home/.rustup}
            export CARGO_HOME RUSTUP_HOME
        fi
    fi
    if [ -z "$cargo_bin" ]; then
        die "cargo not found in PATH (root's PATH usually lacks a rustup install). \
Install the binaries as the user who owns the toolchain instead: \
'cargo build --release -p ff -p fflocated -p ff-gui' then re-run with --no-build"
    fi
    log "building release binaries (ff, fflocated, ff-gui) with $cargo_bin"
    run "$cargo_bin" build --release --manifest-path "$repo_root/Cargo.toml" \
        -p ff -p fflocated -p ff-gui
fi

[ -f "$bindir/fflocated" ] || die "no $bindir/fflocated (build it: cargo build --release -p ff -p fflocated -p ff-gui)"

# ---- install ---------------------------------------------------------------
log "installing into $prefix"
run install -d -m 0755 "$prefix"
for b in fflocated ff ff-gui; do
    if [ -f "$bindir/$b" ]; then
        run install -m 0755 "$bindir/$b" "$prefix/$b"
    else
        log "  (no $b in $bindir; skipping)"
    fi
done

# A binary earlier on PATH silently wins over the one just installed, which is
# exactly how a stale copy keeps being used after an "install".
for b in fflocated ff ff-gui; do
    found=$(command -v "$b" 2>/dev/null || true)
    if [ -n "$found" ] && [ "$found" != "$prefix/$b" ]; then
        log "  warning: $found shadows $prefix/$b on PATH"
    fi
done

# ---- unit ------------------------------------------------------------------
exec_start=$prefix/fflocated
if [ "$mode" = system ]; then
    # `--system` already implies /run/fflocated.sock and
    # /var/lib/fflocate/<format>; the flags below only override them when the
    # operator asked for somewhere else.
    exec_start="$exec_start --system --system-group $group"
fi
if [ -n "$socket" ]; then
    exec_start="$exec_start --socket \"$socket\""
fi
if [ -n "$state_dir" ]; then
    exec_start="$exec_start --state-dir \"$state_dir\""
fi
if [ -n "$roots" ]; then
    old_ifs=$IFS
    IFS='
'
    for root in $roots; do
        [ -n "$root" ] || continue
        exec_start="$exec_start --root \"$root\""
    done
    IFS=$old_ifs
fi

unit_text() {
    printf '%s\n' '[Unit]'
    if [ "$mode" = system ]; then
        printf '%s\n' 'Description=fflocate filename index daemon (system instance)'
        printf '%s\n' 'Documentation=https://github.com/fflocate'
        printf '%s\n' 'After=local-fs.target'
    else
        printf '%s\n' 'Description=fflocate filename index daemon'
        printf '%s\n' 'Documentation=https://github.com/fflocate'
    fi
    printf '%s\n' '' '[Service]' 'Type=simple'
    printf '%s\n' "ExecStart=$exec_start"
    printf '%s\n' 'Restart=on-failure' 'RestartSec=2'
    printf '%s\n' '' '[Install]'
    printf '%s\n' "WantedBy=$wanted_by"
}

if [ "$dry_run" -eq 1 ]; then
    log "would write $unit:"
    unit_text | sed 's/^/  | /'
else
    if [ "$mode" = system ]; then
        group_line=$(getent group "$group" 2>/dev/null || true)
        if [ -z "$group_line" ]; then
            log "creating the '$group' group (clients must be in it to use the system socket)"
            run groupadd --system "$group"
        fi
    fi
    mkdir -p "$unit_dir"
    tmp=$(mktemp)
    trap 'rm -f "$tmp"' EXIT
    unit_text >"$tmp"
    # A hand-written unit is never thrown away silently: keep it beside the new
    # one so "why did my daemon lose its --state-dir" has an answer on disk.
    if [ -f "$unit" ] && ! cmp -s "$tmp" "$unit"; then
        backup=$unit.bak.$(date +%Y%m%d-%H%M%S)
        log "keeping the previous unit as $backup"
        cp -p "$unit" "$backup"
    fi
    install -m 0644 "$tmp" "$unit"
    log "wrote $unit"
fi

# ---- restart ---------------------------------------------------------------
is_active() {
    if [ "$mode" = system ]; then
        systemctl is-active --quiet fflocated.service 2>/dev/null
    else
        systemctl --user is-active --quiet fflocated.service 2>/dev/null
    fi
}

# `systemctl` or `systemctl --user`, kept as words so --dry-run prints the
# command the operator would actually run rather than a helper's name.
if [ "$mode" = system ]; then
    sc=systemctl
else
    sc="systemctl --user"
fi

if [ "$start" -eq 1 ]; then
    # A daemon started by hand holds the pidfile and the socket, and the unit
    # would fail to bind under it. Its pid is in the pidfile, so it is stopped
    # by pid — never by name — and only when --force said so.
    if ! is_active && [ -f "$pidfile" ]; then
        pid=$(cat "$pidfile" 2>/dev/null || true)
        case $pid in ''|*[!0-9]*) pid= ;; esac
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            if [ "$force" -eq 0 ]; then
                if [ "$dry_run" -eq 1 ]; then
                    log "  note: pid $pid is a daemon systemd does not manage; without --force this install would refuse"
                else
                    die "a daemon systemd does not manage is running (pid $pid, $pidfile); stop it with 'kill $pid' or re-run with --force"
                fi
            else
                log "stopping the daemon systemd does not manage (pid $pid)"
                run kill -TERM "$pid" || true
                if [ "$dry_run" -eq 0 ]; then
                    i=0
                    while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 50 ]; do
                        sleep 0.1
                        i=$((i + 1))
                    done
                    if kill -0 "$pid" 2>/dev/null; then
                        log "  pid $pid ignored SIGTERM; sending SIGKILL"
                        run kill -KILL "$pid" || true
                    fi
                fi
            fi
        fi
    fi

    run $sc daemon-reload
    run $sc enable fflocated.service
    run $sc restart fflocated.service

    if [ "$dry_run" -eq 0 ]; then
        i=0
        while [ "$i" -lt 20 ]; do
            is_active && break
            sleep 0.5
            i=$((i + 1))
        done
        if is_active; then
            log "fflocated is active"
            if [ -S "$socket" ]; then
                log "socket: $socket"
            else
                log "  warning: no socket at $socket yet (a cold start indexes before it binds)"
            fi
            log "check it with: ff stats"
        else
            log "fflocated did not become active; the last journal lines:"
            $sc status fflocated.service --no-pager 2>&1 | tail -n 15 || true
            exit 1
        fi
    fi
fi

if [ "$dry_run" -eq 0 ]; then
    log ""
    log "installed: $prefix/{fflocated,ff,ff-gui}"
    log "unit:      $unit"
    if [ "$start" -eq 1 ]; then
        log "status:    $sc status fflocated"
    else
        log "not started; enable it with: $sc enable --now fflocated"
    fi
fi
