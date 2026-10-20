#!/usr/bin/env bash
# The relay image's entrypoint (C12), run by tini. Starts Xvfb for cefsrc, gives the compositor a
# short private TMPDIR on a tmpfs, then execs the compositor, so it receives the stop signal directly
# from tini and its exit code is the container's (0, 70, 75, 78; tini passes it through).
set -euo pipefail

log() { echo "ts=$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ) level=$1 msg=$2" >&2; }

Xvfb :99 -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 &
for _ in $(seq 1 50); do [ -e /tmp/.X11-unix/X99 ] && break; sleep 0.1; done
[ -e /tmp/.X11-unix/X99 ] || { log error "Xvfb did not start"; exit 70; }

# Owed 58: the CEF profile lives under TMPDIR (it holds the page URL and token in clear, F-014), so
# TMPDIR is a tmpfs that nothing else writes to, and its path is short (an 85-character TMPDIR killed
# CEF at its start in a test; 59 ran: a Unix socket path's limit). /dev/shm is the tmpfs a non-root
# process can use. Failing that, a private directory under /tmp, with a warning.
tmp=/dev/shm/seazn
if mkdir -p "$tmp" 2>/dev/null && chmod 0700 "$tmp" 2>/dev/null; then
  fs=$(stat -f -c %T /dev/shm 2>/dev/null || echo unknown)
  [ "$fs" = "tmpfs" ] || log warn "TMPDIR $tmp is on $fs, not a tmpfs"
else
  tmp=/tmp/seazn
  mkdir -p "$tmp" && chmod 0700 "$tmp"
  log warn "no /dev/shm for TMPDIR; using $tmp, which is not a tmpfs"
fi
export TMPDIR="$tmp"

exec /usr/local/bin/compositor
