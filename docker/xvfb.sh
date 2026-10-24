# shellcheck shell=bash
# The relay image's X server for cefsrc (C12; Owed 85, F-025). Sourced by the entrypoint after
# supervise.sh, whose `log` it uses. `start_xvfb <display> <dir>` starts Xvfb on :<display> without
# the job token and returns 0 once Xvfb itself says it is ready (logging how long that took, to the
# wait's 0.1 s poll), or 70 with the reason logged. <dir> holds the ready file: /tmp, which the
# entrypoint passes; `scripts/xvfb_start_test.py` passes its own and drives this file with a stub
# Xvfb (check.yml); image.yml's in-place restart probe runs it with the real one.
#
# Why `-displayfd` (Owed 85, F-025). A restart in place (Docker's `--restart on-failure`, ADR 0007
# item 5; compose's restart in C14) keeps the container's writable layer, /tmp included, and its new
# pid namespace gives the new Xvfb its predecessor's pid. Started the old way, the new Xvfb found the
# old `/tmp/.X<n>-lock` naming a live pid, its own (os/utils.c's LockServer: `kill(pid, 0)`
# succeeds), and exited "Server is already active for display", while the old socket file satisfied
# the old wait for it: CEF found no X server. `-displayfd` takes away both halves:
# - it sets Xvfb's `nolock` (os/utils.c's ProcessCommandLine), so Xvfb neither reads nor writes a
#   lock, and no life leaves one for the next. The socket an earlier life left needs no removing:
#   Xvfb's transport unlinks the path before it binds (xtrans's SocketUNIXCreateListener).
# - ready is Xvfb's own word: the display number it writes to that fd (os/connection.c's
#   NotifyParentProcess, once its sockets listen and its screen and input are up), into a file made
#   new at each start, never a socket file that may be an earlier life's. An Xvfb that exits before
#   it is ready ends the wait at once.
# So nothing is removed first: beside `-displayfd`, no leftover changes the start (the PR #54
# review's I1; decision log, 2026-10-24).

start_xvfb() {
  local display=$1 dir=$2 ready xpid i line= t0 now
  if ! ready=$(mktemp "$dir/.seazn-xvfb-ready.XXXXXX"); then
    log error "Xvfb did not start: no file for its ready line"
    return 70
  fi
  # Bash 5's clock in microseconds, whatever the locale's decimal point (empty before bash 5).
  t0=${EPOCHREALTIME:-}
  t0=${t0//[!0-9]/}
  # MUTANT (Owed 85 review I1): the -displayfd half reverted, i.e. the pre-fix start: no -displayfd,
  # the socket-file wait. Never to merge.
  env -u JOB_TOKEN Xvfb ":$display" -screen 0 1920x1080x24 -nolisten tcp >/dev/null 2>&1 &
  for _ in $(seq 1 50); do [ -e "$dir/.X11-unix/X$display" ] && break; sleep 0.1; done
  rm -f "$ready"
  if [ -e "$dir/.X11-unix/X$display" ]; then
    log info "Xvfb ready"
    return 0
  fi
  if [ -n "$line" ]; then
    log error "Xvfb did not start: its ready line named a display other than :$display"
  elif kill -0 "$xpid" 2>/dev/null; then
    log error "Xvfb did not start: not ready after 5 s"
  else
    log error "Xvfb did not start: it exited before it was ready"
  fi
  return 70
}
