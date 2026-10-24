#!/bin/sh
# The X server's witness across in-place restarts (Owed 85, F-025). TEST ONLY: it is never in the
# relay image. `scripts/image_probe.py --restarts` bind-mounts this file at /usr/local/bin/Xvfb,
# which is ahead of /usr/bin on the image's PATH, so the entrypoint's `env … Xvfb` execs it; it
# appends one line per start to $log, then execs the real Xvfb with its stderr appended there too
# (the entrypoint sends both of Xvfb's streams to /dev/null, which is where Owed 85's reason went).
#
# The line holds this start's pid (the real Xvfb keeps it after `exec`), the pid in /tmp/.X99-lock
# (`none` when there is none: under `-displayfd` Xvfb writes no lock, so a lock here means a life
# started without it) and whether /tmp/.X11-unix/X99 exists (the previous life's socket, on a
# restart in place). It uses builtins alone before the `exec`: a forked helper would take a pid in
# the container's namespace, and the pids are what this witness is about. $log is under /tmp, on
# the container's writable layer, so a restart in place keeps it, as it keeps the socket.
log=/tmp/seazn-xvfb-witness.log
lock=none
if [ -e /tmp/.X99-lock ]; then
  IFS= read -r lock </tmp/.X99-lock 2>/dev/null || lock=unreadable
  lock=${lock##* }
fi
socket=no
[ -e /tmp/.X11-unix/X99 ] && socket=yes
echo "xvfb-witness pid=$$ lock=${lock:-empty} socket=$socket" >>"$log" 2>/dev/null || true
exec /usr/bin/Xvfb "$@" 2>>"$log"
