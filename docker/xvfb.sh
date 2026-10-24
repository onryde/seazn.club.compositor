# shellcheck shell=bash
# The relay image's X server for cefsrc (C12; Owed 85, F-025). Sourced by the entrypoint after
# supervise.sh, whose `log` it uses. `start_xvfb <display> <dir>` starts Xvfb on :<display> without
# the job token and returns 0 once Xvfb itself says it accepts connections, or 70 with the reason
# logged. <dir> is where that Xvfb keeps its lock and its socket directory: /tmp (Xvfb's LOCK_DIR and
# UNIX_DIR), which the entrypoint passes; `scripts/xvfb_start_test.py` passes its own and drives this
# file with a stub Xvfb (check.yml); image.yml's in-place restart probe runs it with the real one.
#
# - An earlier life's lock and socket are removed first. A restart in place (Docker's
#   `--restart on-failure`, ADR 0007 item 5) keeps the container's writable layer, so the previous
#   Xvfb's `.X<n>-lock` and `.X11-unix/X<n>` are still there, and the restarted container's new pid
#   namespace gives the new Xvfb the very pid the old one wrote into that lock (the entrypoint's
#   forks before it never vary). Xvfb's lock check (`kill(pid, 0)` in os/utils.c's LockServer) then
#   finds a live process, itself, and Xvfb exits ("Server is already active for display"), while the
#   old socket file satisfied the old wait for it: CEF connected to no server and never initialised
#   (F-025). A container start is a new pid namespace, so no process of an earlier life is left to
#   own either file: here they are always leftovers.
# - Ready is Xvfb's own word: the display number it writes to `-displayfd` once its sockets listen
#   (os/connection.c's NotifyParentProcess, after CreateWellKnownSockets), never a socket file that
#   exists. An Xvfb that exits before it is ready ends the wait at once.

start_xvfb() {
  local display=$1 dir=$2 ready xpid i line=
  rm -f "$dir/.X$display-lock" "$dir/.X11-unix/X$display" "$dir"/.seazn-xvfb-ready.* 2>/dev/null || true
  if ! ready=$(mktemp "$dir/.seazn-xvfb-ready.XXXXXX"); then
    log error "Xvfb did not start: no file for its ready line"
    return 70
  fi
  # Xvfb needs no token (the PR #36 review's m2).
  env -u JOB_TOKEN Xvfb ":$display" -screen 0 1920x1080x24 -nolisten tcp -displayfd 3 \
    3>"$ready" >/dev/null 2>&1 &
  xpid=$!
  # Up to 5 s, as before Owed 85. `read` succeeds on a whole line only: Xvfb writes the number, then
  # the newline.
  for ((i = 0; i < 50; i++)); do
    if IFS= read -r line <"$ready"; then break; fi
    kill -0 "$xpid" 2>/dev/null || break
    sleep 0.1
  done
  IFS= read -r line <"$ready" || line=
  rm -f "$ready"
  if [ "$line" = "$display" ]; then
    return 0
  fi
  if kill -0 "$xpid" 2>/dev/null; then
    log error "Xvfb did not start: not ready after 5 s"
  else
    log error "Xvfb did not start: it exited before it was ready"
  fi
  return 70
}
