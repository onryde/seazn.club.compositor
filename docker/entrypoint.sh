#!/usr/bin/env bash
# The relay image's entrypoint (C12), run by tini. Sources the stop supervision first (its traps must
# exist before anything else runs), takes cores off for this shell and tini, starts Xvfb for cefsrc
# (xvfb.sh: no X lock, and Xvfb's own ready word), gives the compositor a short private TMPDIR on a
# tmpfs, then runs the compositor under `supervise`. Its exit code is the compositor's own (0, 70,
# 75, 78; tini passes it through).
#
# Why not `exec`: CEF takes over SIGINT and SIGTERM inside the compositor's process once it starts
# (F-016). A SIGTERM the process receives then exits it at once with status 0: no drain, no final
# beat, no exit line. `supervise.sh` forwards the stops as SIGUSR1 and SIGUSR2 instead; see there.
set -euo pipefail

# shellcheck source=docker/supervise.sh
. /usr/local/lib/seazn/supervise.sh
# shellcheck source=docker/xvfb.sh
. /usr/local/lib/seazn/xvfb.sh

# Owed 68 (the PR #36 review's I1 and m2): this shell, tini and Xvfb hold the job token in their
# environment and are dumpable. A core limit of 1 byte is refused by the kernel for a piped
# core_pattern and is below the ELF minimum for a file one; 0 would stop only the file case. tini is
# this shell's parent (PID 1 under Docker, not elsewhere). The compositor sets its own.
prlimit --pid "$$" --core=1:1 2>/dev/null || log warn "core limit 1 not set on the entrypoint"
prlimit --pid "$PPID" --core=1:1 2>/dev/null || log warn "core limit 1 not set on tini (pid $PPID)"

# Owed 85 (F-025): a restart in place keeps /tmp. Started the old way, the new Xvfb refused the
# previous one's lock while the old wait for the socket file passed on the previous one's socket.
# xvfb.sh starts it with `-displayfd`: no lock, and ready on Xvfb's own word.
start_xvfb 99 /tmp || exit 70

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

code=0
supervise /usr/local/bin/compositor || code=$?
exit "$code"
