#!/bin/sh
# The .pkg's postinstall: register the daemon for the user logged in at the
# console.
#
# The LaunchAgent is per user (~/Library/LaunchAgents), because the daemon
# indexes that user's $HOME and keeps its socket and index there. A shared agent
# under /Library/LaunchAgents would need one fixed home baked into its plist —
# and `launchd_plist()` would expand $HOME for whichever user generated it. So
# the plist is generated *as* the console user by `ff service install` (the same
# code path the README documents), then bootstrapped into that user's gui/<uid>
# domain. Accounts created later run `ff service install` themselves.
#
# This runs as root. Nothing here fails the install: the binaries are already in
# place, and `installer` reports a postinstall failure as a failed install, so a
# machine with no graphical login still gets a usable ff.
set -u

LOG=/var/log/fflocate-install.log
FF=/usr/local/bin/ff
LABEL=com.fflocate.fflocated

log() {
    printf '%s %s\n' "$(date '+%Y-%m-%dT%H:%M:%S')" "$*" >>"$LOG" 2>/dev/null || true
}

log "postinstall: $0 $*"

user=$(stat -f%Su /dev/console 2>/dev/null || true)
case "$user" in
    "" | root | loginwindow)
        log "no console user (got '$user'); skipped the LaunchAgent"
        log "run '$FF service install' as each user who wants the daemon"
        exit 0
        ;;
esac

uid=$(id -u "$user" 2>/dev/null || true)
home=$(dscl . -read "/Users/$user" NFSHomeDirectory 2>/dev/null | awk '{print $2}')
if [ -z "$uid" ] || [ -z "$home" ]; then
    log "cannot resolve $user (uid '$uid', home '$home'); skipped the LaunchAgent"
    exit 0
fi
if [ ! -x "$FF" ]; then
    log "no $FF in the payload; skipped the LaunchAgent"
    exit 0
fi

# `sudo -H`: HOME must be the user's, not root's, or the plist lands in
# /var/root/Library/LaunchAgents and points at a home nothing listens on.
log "writing the LaunchAgent as $user (uid $uid, home $home)"
installed=""
if ! installed=$(launchctl asuser "$uid" sudo -u "$user" -H "$FF" service install 2>&1); then
    printf '%s\n' "$installed" >>"$LOG" 2>/dev/null || true
    log "'ff service install' failed for $user; run it by hand: $FF service install"
    exit 0
fi
printf '%s\n' "$installed" >>"$LOG" 2>/dev/null || true

# `ff service install` prints `wrote <absolute path>` on stdout (observed: exit
# 0, empty stderr, then the commands to load it). Take the path it reports
# rather than re-deriving it: the CLI resolves the plist from $HOME, and this
# script resolves the home through dscl, which can disagree for a relocated
# account.
plist=$(printf '%s\n' "$installed" | sed -n 's/^[[:space:]]*wrote //p' | head -1)
if [ ! -f "${plist:-}" ]; then
    # A build that does not print it, or prints a home this script did not
    # expect: a per-user agent is always written here.
    plist="$home/Library/LaunchAgents/$LABEL.plist"
fi
if [ ! -f "$plist" ]; then
    log "no plist found after 'ff service install'; the daemon may need starting by hand"
    exit 0
fi

# An upgrade replaces a running agent; bootout first so bootstrap is not a
# no-op for an already-registered label.
launchctl asuser "$uid" launchctl bootout "gui/$uid/$LABEL" >>"$LOG" 2>&1 || true
if ! launchctl asuser "$uid" launchctl bootstrap "gui/$uid" "$plist" >>"$LOG" 2>&1; then
    log "bootstrap failed; load it by hand:"
    log "  launchctl bootstrap gui/$uid $plist"
    exit 0
fi
launchctl asuser "$uid" launchctl enable "gui/$uid/$LABEL" >>"$LOG" 2>&1 || true
launchctl asuser "$uid" launchctl kickstart -k "gui/$uid/$LABEL" >>"$LOG" 2>&1 || true
log "daemon registered for $user"

# The plist and the log directory were created as the user; an upgrade may have
# run while they existed, so make sure nothing root-owned is left behind.
chown "$user" "$home/Library/LaunchAgents" 2>/dev/null || true
chown "$user" "$plist" 2>/dev/null || true
mkdir -p "$home/Library/Logs" 2>/dev/null || true
chown "$user" "$home/Library/Logs" 2>/dev/null || true
chown "$user" "$home/Library/Logs/fflocated.log" 2>/dev/null || true

exit 0
