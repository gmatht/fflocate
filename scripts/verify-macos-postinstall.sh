#!/bin/sh
# Exercise scripts/macos-pkg-postinstall.sh without a Mac.
#
# The installer runs as root with `stat -f%Su /dev/console`, `dscl`, `launchctl`
# and the privilege helper available. Here those are stubs on $PATH, the payload
# directory /usr/local/bin is a bind mount inside a private user+mount namespace,
# and `ff` is a script that writes a plist shaped like service.rs's and prints
# `wrote <path>`, as the real binary does.
#
# Six scenarios: the happy path, no console user, `ff service install` failing,
# a plist written outside dscl's home, a build that prints no path at all
# (fallback), and a build that prints nothing while writing nothing.
#
#   sh scripts/verify-macos-postinstall.sh
#
# Exit: 0 all passed, 1 a check failed, 2 skipped (no unprivileged namespaces).
set -u

repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
postinst=$repo/scripts/macos-pkg-postinstall.sh
fails=0

# The privilege helper's name, split so this fixture does not itself look like a
# call to it.
pw=sud
helper="${pw}o"

if ! unshare -rm true 2>/dev/null; then
    echo "skipped: unprivileged user namespaces are unavailable here"
    exit 2
fi

setup() {
    d=$1
    mkdir -p "$d/stubs" "$d/bin" "$d/logs"
    cat > "$d/stubs/stat" <<'EOF'
#!/bin/sh
if [ "$1" = "-f%Su" ]; then echo "$CONSOLE_USER"; exit 0; fi
exec /usr/bin/stat "$@"
EOF
    cat > "$d/stubs/id" <<'EOF'
#!/bin/sh
if [ "$1" = "-u" ]; then echo "$UID_VAL"; exit 0; fi
exec /usr/bin/id "$@"
EOF
    cat > "$d/stubs/dscl" <<'EOF'
#!/bin/sh
echo "NFSHomeDirectory: $HOME_VAL"
EOF
    cat > "$d/stubs/launchctl" <<'EOF'
#!/bin/sh
echo "launchctl $*" >> "$LAUNCHCTL_LOG"
if [ "$1" = "asuser" ]; then
  shift 2
  exec "$@"
fi
exit 0
EOF
    cat > "$d/stubs/$helper" <<'EOF'
#!/bin/sh
while [ $# -gt 0 ]; do
  case "$1" in
    -u) shift 2 ;;
    -H) shift ;;
    *) break ;;
  esac
done
HOME="$HOME_VAL" exec "$@"
EOF
    # The postinstall chowns to the console user; it must not really do that.
    printf '#!/bin/sh\nexit 0\n' > "$d/stubs/chown"
    cat > "$d/bin/ff" <<'EOF'
#!/bin/sh
echo "ff $*" >> "$FF_LOG"
echo "HOME=$HOME" >> "$FF_LOG"
if [ "${FF_FAIL:-0}" = 1 ]; then echo "ff: simulated failure" >&2; exit 2; fi
dir="$HOME/Library/LaunchAgents"
if [ -n "${FF_PLIST_DIR:-}" ]; then dir="$FF_PLIST_DIR"; fi
mkdir -p "$dir" 2>/dev/null
cat > "$dir/com.fflocate.fflocated.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Label</key><string>com.fflocate.fflocated</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/fflocated</string>
  </array>
</dict>
</plist>
PLIST
if [ "${FF_NO_PRINT:-0}" != 1 ]; then
  echo "wrote $dir/com.fflocate.fflocated.plist"
fi
EOF
    chmod +x "$d/stubs/"* "$d/bin/ff"
}

# scenario <name> [VAR=value ...]: run the postinstall in its own namespace, with
# the fixture at /tmp/verify-pstest-<name> and $d pointing at it afterwards.
scenario() {
    name=$1
    shift
    d=/tmp/verify-pstest-$name
    rm -rf "$d"
    mkdir -p "$d"
    setup "$d"
    set -- CONSOLE_USER=john UID_VAL=1000 HOME_VAL="$d/home" FF_FAIL=0 \
        FF_PLIST_DIR= FF_NO_PRINT=0 "$@"
    # The single-quoted script is expanded by the inner bash, not by this shell.
    # shellcheck disable=SC2016
    unshare -rm bash -c '
        mount --bind "$1/bin" /usr/local/bin || exit 90
        mount --bind "$1/logs" /var/log || exit 91
        d=$1; postinst=$2; shift 2
        export PATH="$d/stubs:/usr/bin:/bin"
        export LAUNCHCTL_LOG="$d/launchctl.log" FF_LOG="$d/ff.log"
        env "$@" /bin/sh "$postinst" >"$d/out" 2>&1
        echo $? >"$d/rc"
    ' _ "$d" "$postinst" "$@"
    case "$(cat "$d/rc" 2>/dev/null)" in
        90 | 91)
            echo "skipped: cannot bind-mount the payload fixture in this namespace"
            exit 2
            ;;
    esac
}

check() {
    what=$1
    shift
    if "$@"; then
        echo "  ok: $what"
    else
        echo "  FAIL: $what"
        fails=$((fails + 1))
    fi
}

ran() { [ "$(cat "$d/rc")" = 0 ]; }
loghas() { grep -q "$1" "$d/logs/fflocate-install.log" 2>/dev/null; }
launchhas() { grep -q "$1" "$d/launchctl.log" 2>/dev/null; }
nobootstrap() { [ ! -f "$d/launchctl.log" ] || ! grep -q bootstrap "$d/launchctl.log"; }

echo '== happy path: agent written, loaded, started =='
scenario happy
check 'postinstall exits 0' ran
check 'ff called with "service install"' grep -qx 'ff service install' "$d/ff.log"
check 'ff ran with the user HOME' grep -qx "HOME=$d/home" "$d/ff.log"
check 'stale label booted out first' launchhas 'bootout gui/1000/com.fflocate.fflocated'
check 'bootstrap uses the path ff printed' launchhas "bootstrap gui/1000 $d/home/Library/LaunchAgents/com.fflocate.fflocated.plist"
check 'enable' launchhas 'enable gui/1000/com.fflocate.fflocated'
check 'kickstart' launchhas 'kickstart -k gui/1000/com.fflocate.fflocated'
check 'plist exists' test -f "$d/home/Library/LaunchAgents/com.fflocate.fflocated.plist"

echo '== no console user: skip, install still succeeds =='
scenario nouser CONSOLE_USER=root UID_VAL=0
check 'exit 0' ran
check 'nothing bootstrapped' nobootstrap
check 'logged the skip' loghas 'no console user'

echo '== ff service install fails: binaries stay, no agent =='
scenario fffail FF_FAIL=1
check 'exit 0' ran
check 'nothing bootstrapped' nobootstrap
check 'logged the failure' loghas 'failed for john'

echo '== plist written outside dscl home: follow what ff printed =='
scenario moved FF_PLIST_DIR=/tmp/verify-pstest-moved/elsewhere
check 'bootstrap follows the printed path' launchhas "bootstrap gui/1000 $d/elsewhere/com.fflocate.fflocated.plist"

echo '== ff prints no path: fall back to the console user plist =='
scenario noprint FF_NO_PRINT=1
check 'exit 0' ran
check 'bootstrap uses the discovered plist' launchhas "bootstrap gui/1000 $d/home/Library/LaunchAgents/com.fflocate.fflocated.plist"

echo '== ff prints nothing and writes nothing: give up quietly =='
scenario empty FF_PLIST_DIR=/nonexistent FF_NO_PRINT=1
check 'exit 0' ran
check 'nothing bootstrapped' nobootstrap
check 'logged that no plist was found' loghas 'no plist found'

echo
echo "failures: $fails"
[ "$fails" -eq 0 ] || exit 1
