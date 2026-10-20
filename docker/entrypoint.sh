#!/usr/bin/env bash
# The relay image's entrypoint (C12), run by tini. Starts Xvfb for cefsrc, gives the compositor a
# short private TMPDIR on a tmpfs, runs the compositor, and forwards the stop signals to it as
# SIGUSR1 (for SIGINT) and SIGUSR2 (for SIGTERM). Its exit code is the container's (0, 70, 75, 78;
# tini passes it through).
#
# Why not `exec`: CEF takes over SIGINT and SIGTERM inside the compositor's process once it starts
# (F-015). A SIGTERM the process receives then exits it at once with status 0: no drain, no final
# beat, no exit line. CEF leaves SIGUSR1 and SIGUSR2 alone, and the compositor reads them as SIGINT
# (the Final hold) and SIGTERM (the drain). tini sends a signal to this shell only (no -g), so the
# compositor never receives SIGINT or SIGTERM itself.
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

/usr/local/bin/compositor &
pid=$!
trap 'kill -USR1 "$pid" 2>/dev/null || true' INT
trap 'kill -TERM "$pid" 2>/dev/null || true' TERM # MUT
# `wait` returns early (above 128) when a trap runs; the child is then still alive, or a zombie that
# `kill -0` still finds, so the loop waits again until it has reaped the child and holds its status.
code=0
while kill -0 "$pid" 2>/dev/null; do
  set +e
  wait "$pid"
  code=$?
  set -e
done
exit "$code"
