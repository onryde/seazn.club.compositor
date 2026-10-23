# shellcheck shell=bash
# The relay image's stop supervision (C12; F-016, and the PR #36 review's I2, m1 and m6). Sourced by
# the entrypoint as its first command, so its traps exist before anything else runs, then
# `supervise <command…>` runs the compositor under a waiter subshell and returns the compositor's own
# exit status, which the waiter writes to a private file under /tmp. `scripts/supervise_test.py` drives this file with a stub child (check.yml).
#
# - CEF takes over SIGINT and SIGTERM inside the compositor's process once it starts (F-016): a raw
#   one exits it at once with status 0, with no drain and no final beat. So the stops are forwarded
#   as SIGUSR1 (for SIGINT) and SIGUSR2 (for SIGTERM), which the compositor reads as the same two
#   (`wiring::STOP_SIGNALS`) and CEF leaves alone.
# - A stop that arrives before the child exists, or before it has installed its handlers, is held,
#   then sent once both USR handlers show in the child's `/proc/<pid>/status` (SigCgt): SIGUSR1's and
#   SIGUSR2's default action would end the child with no beat at all (the review's m1).
# - tini signals this shell only, unless TINI_KILL_PROCESS_GROUP is set: tini then signals the whole
#   group, the compositor gets the raw signal, and F-016's instant exit follows. That is refused at
#   start (78, EX_CONFIG), before anything runs (the review's m6).

log() { echo "ts=$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ) level=$1 msg=$2" >&2; }

if [ -n "${TINI_KILL_PROCESS_GROUP+set}" ]; then
  log error "TINI_KILL_PROCESS_GROUP is set: tini would signal the compositor directly, which skips its drain and final beat"
  exit 78
fi

_sv_pid=
_sv_armed=
_sv_pending=
_sv_forwarded=

# $1: USR1 or USR2. Sends it now, or holds it until the child can hear it.
_sv_forward() {
  _sv_forwarded=1
  if [ -n "$_sv_armed" ]; then
    kill -"$1" "$_sv_pid" 2>/dev/null || true
  else
    _sv_pending="$_sv_pending $1"
  fi
}
trap '_sv_forward USR1' INT
trap '_sv_forward USR2' TERM

# True once the child catches SIGUSR1 (10) and SIGUSR2 (12): bits 9 and 11 of SigCgt, 0xa00. With no
# procfs (a host test on macOS), there is nothing to wait on.
_sv_armed_now() {
  [ -r "/proc/$$/status" ] || return 0
  local cgt
  cgt=$(sed -n 's/^SigCgt:[[:space:]]*//p' "/proc/$_sv_pid/status" 2>/dev/null) || return 1
  [ -n "$cgt" ] || return 1
  (( (16#$cgt & 0xa00) == 0xa00 ))
}

# MUT37: round 1's supervise (the trapping shell `wait`s on the child, re-waiting after each trap).
supervise() {
  "$@" &
  _sv_pid=$!
  local i
  # Up to 10 s for the handlers; a child that exits first ends the wait.
  for ((i = 0; i < 200; i++)); do
    _sv_armed_now && break
    kill -0 "$_sv_pid" 2>/dev/null || break
    sleep 0.05
  done
  _sv_armed=1
  local s
  for s in $_sv_pending; do
    kill -"$s" "$_sv_pid" 2>/dev/null || true
  done
  _sv_pending=
  local code= rc
  while :; do
    _sv_forwarded=
    set +e
    wait "$_sv_pid"
    rc=$?
    set -e
    if [ "$rc" -eq 127 ] && [ -n "$code" ]; then break; fi
    code=$rc
    [ -n "$_sv_forwarded" ] || break
  done
  # The killed-by report, kept as at 341ae7a so only the wait design differs.
  local sig
  if ((code > 128)) && sig=$(kill -l "$code" 2>/dev/null); then
    log error "the compositor was killed by SIG${sig#SIG} (status $code)"
  fi
  return "$code"
}
