#!/usr/bin/env python3
"""Drives `docker/xvfb.sh` (the relay image's X server start; Owed 85, F-025) with a stub Xvfb, under
each given bash (check.yml: Ubuntu's, the image's). Stdlib only; run with `python3 -I`.

Each case is a wrapper that sources `docker/supervise.sh` (its `log` and its stop traps, as the
entrypoint does) and `docker/xvfb.sh`, then runs `start_xvfb <display> <dir>` and exits with its
status. The stub stands in for Xvfb as xorg-server 21.1.12 behaves (the PR #54 review's I1: the
stub first written refused a lock even under `-displayfd`, which the real binary never does):

  - under `-displayfd` it neither reads nor writes the lock (os/utils.c's ProcessCommandLine sets
    `nolock`, and LockServer returns at once); without it, it refuses a lock that is there ("Server
    is already active for display N", exit 1, before any socket), because on a restart in place
    the pid in that lock is always live: the new Xvfb's own (F-025, image run 40057826178), and
    otherwise takes the lock;
  - it replaces its socket file, an earlier life's included (xtrans's SocketUNIXCreateListener
    unlinks the path before it binds), then after STUB_DELAY records that it is ready and writes
    the display number and a newline to its `-displayfd`, as two writes (os/connection.c's
    NotifyParentProcess), then stays up until the case ends;
  - `die`: binds, then exits 1 without a word on `-displayfd`; `refuse`: exits 1 before it binds,
    as Xvfb's lock refusal does (os/osinit.c's LockServer comes before CreateWellKnownSockets);
  - `hang`: binds and never says it is ready; `nonl`: writes the number and never the newline;
    `wrong`: writes another display's number.

The cases (each answers one of F-025's questions, AGENTS.md section 5's four among them):

  - fresh, with no leftovers: ready (0), not before the stub's ready line, the stub still up;
  - a restart in place, the previous life's socket left (the fixed start leaves no lock): ready;
  - a second restart in the same <dir> (what the first restart's Xvfb left): ready again;
  - F-025's leftovers, a lock naming a live pid and its socket: ready (`-displayfd`'s nolock);
  - Xvfb exits before it is ready, after binding: 70 at once (well inside the 5 s wait), with the
    line `Xvfb did not start: it exited before it was ready` (the old socket-file wait passed);
  - Xvfb refuses before it binds, an earlier life's socket left (F-025's failure): 70 at once;
  - Xvfb never ready: 70 after the 5 s wait, `Xvfb did not start: not ready after 5 s`;
  - the number without its newline: not ready, 70 after the wait (an empty answer is no answer);
  - another display's number: 70 at once, `... named a display other than :99` (the review's m6);
  - the ready line's newline comes 0.5 s after its number: still ready, not before the newline;
  - a SIGTERM during the wait (supervise's trap holds it): the start carries on, ready (0);
  - another display (42), with its own leftovers.

In every case the stub runs with SESSION_ID set (the control) and without JOB_TOKEN (the PR #36
review's m2), with `-nolisten tcp` and `-displayfd`; no start takes a lock; the ready file is gone
afterwards; each leftover a case plants is one its Xvfb met (so the case is not vacuous); and a good
start logs `Xvfb ready after N ms` once (bash 5's clock; plain `Xvfb ready` before bash 5), N
between the stub's own delay and the case's wall time (the review's m3).

Usage: xvfb_start_test.py [--bash PATH ...]
"""

import argparse
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SUPERVISE = ROOT / "docker/supervise.sh"
XVFB = ROOT / "docker/xvfb.sh"
# The entrypoint's wait: 50 polls of 0.1 s (xvfb.sh; unchanged by Owed 85).
WAIT_S = 5.0
# How long after a start returns its Xvfb must still be up (an Xvfb refusing exits in milliseconds).
SETTLE_S = 0.5
STUB = r"""#!/usr/bin/env bash
# The stand-in Xvfb. STUB_X_MODE: up | split | nonl | wrong | die | refuse | hang.
disp=${1#:}
fd=
prev=
for a in "$@"; do
  [ "$prev" = -displayfd ] && fd=$a
  prev=$a
done
lock="$STUB_ROOT/.X$disp-lock"
sock="$STUB_ROOT/.X11-unix/X$disp"
had_lock=no; [ -e "$lock" ] && had_lock=yes
had_sock=no; [ -e "$sock" ] && had_sock=yes
echo "start pid=$$ disp=$disp fd=${fd:-none} lock=$had_lock socket=$had_sock job_token=${JOB_TOKEN+set} session_id=${SESSION_ID+set} argv=$*" >> "$STUB_OUT"
trap 'echo "gone pid=$$" >> "$STUB_OUT"' EXIT
# os/utils.c: `-displayfd` sets nolock. Without it, LockServer refuses a lock naming a live pid.
if [ -z "$fd" ]; then
  if [ "$had_lock" = yes ]; then
    echo "Fatal server error: Server is already active for display $disp" >&2
    exit 1
  fi
  printf '%10d\n' $$ > "$lock"
  echo "locked pid=$$" >> "$STUB_OUT"
fi
if [ "$STUB_X_MODE" = refuse ]; then
  echo "Fatal server error: refused before binding" >&2
  exit 1
fi
mkdir -p "$STUB_ROOT/.X11-unix"
rm -f "$sock"
: > "$sock"
case "$STUB_X_MODE" in
  die) sleep 0.2; exit 1 ;;
  hang) sleep 30; exit 0 ;;
esac
sleep "$STUB_DELAY"
echo "ready pid=$$" >> "$STUB_OUT"
if [ -n "$fd" ]; then
  case "$STUB_X_MODE" in
    wrong) printf '7\n' >&"$fd" ;;
    nonl) printf '%s' "$disp" >&"$fd" ;;
    *)
      printf '%s' "$disp" >&"$fd"
      [ "$STUB_X_MODE" = split ] && sleep 0.5
      printf '\n' >&"$fd"
      ;;
  esac
  eval "exec $fd>&-"
fi
sleep 30
"""


def wrapper(d, display, xdir):
    p = d / f"wrap-{display}.sh"
    p.write_text(
        "set -euo pipefail\n"
        f". {SUPERVISE}\n"
        f". {XVFB}\n"
        f"code=0\nstart_xvfb {display} {xdir} || code=$?\n"
        "echo \"returned $code\" >> \"$STUB_OUT\"\n"
        "exit \"$code\"\n"
    )
    return p


def run(bash, wrap, xdir, stubdir, out, mode, delay, sigs=(), limit=20):
    env = {k: v for k, v in os.environ.items() if k not in ("TINI_KILL_PROCESS_GROUP", "JOB_TOKEN")}
    env.update(PATH=f"{stubdir}:{env.get('PATH', '/usr/bin:/bin')}", STUB_OUT=str(out), STUB_ROOT=str(xdir),
               STUB_X_MODE=mode, STUB_DELAY=str(delay), JOB_TOKEN="test-job-token-not-a-secret",
               SESSION_ID="test-session")
    p = subprocess.Popen([bash, str(wrap)], env=env, start_new_session=True, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, text=True)
    t0 = time.monotonic()
    for at, s in sigs:
        dt = t0 + at - time.monotonic()
        if dt > 0:
            time.sleep(dt)
        try:
            os.kill(p.pid, s)
        except ProcessLookupError:
            pass
    try:
        code = p.wait(limit)
    except subprocess.TimeoutExpired:
        code = "TIMEOUT"
    took = time.monotonic() - t0
    # An Xvfb that is up stays up for 30 s; one that was refusing as the start returned has exited
    # (and said so, "gone") well within this.
    time.sleep(SETTLE_S)
    lines = out.read_text().splitlines() if out.exists() else []
    # The stub stays up as Xvfb does: end it with the wrapper's group (no job control, one group).
    try:
        os.killpg(p.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    p.wait()
    err = p.stderr.read()
    if out.exists():
        out.unlink()
    return code, took, err, lines


def leave_previous_life(xdir, display, lock=False):
    """What a SIGKILLed Xvfb leaves on a restart in place: its socket file, and, when it was started
    without `-displayfd` (before Owed 85's fix, F-025), its lock: its pid, as Xvfb writes it
    (`%10lu\\n`), which on a restart in place is live."""
    (xdir / ".X11-unix").mkdir(exist_ok=True)
    if lock:
        (xdir / f".X{display}-lock").write_text("%10d\n" % 10)
    (xdir / ".X11-unix" / f"X{display}").write_text("")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bash", action="append")
    a = ap.parse_args()
    bashes = a.bash or ["bash"]
    failures, checked = [], 0
    with tempfile.TemporaryDirectory(prefix="xvfb-test-") as tmp:
        d = Path(tmp)
        stubdir = d / "bin"
        stubdir.mkdir()
        stub = stubdir / "Xvfb"
        stub.write_text(STUB)
        stub.chmod(0o755)
        for bash in bashes:
            ver = subprocess.run([bash, "-c", "echo $BASH_VERSION"], capture_output=True, text=True).stdout.strip()
            # EPOCHREALTIME is bash 5's (bash(1)); before it the start logs no duration.
            timed = int(ver.split(".")[0]) >= 5
            n = 0

            def case(name, mode, display=99, delay=0.3, before=None, sigs=(), want=0, at_least=0.0,
                     at_most=None, said=None, xdir=None, met=("no", "no"), took_ms=0.0):
                nonlocal n, checked
                n += 1
                if xdir is None:
                    xdir = d / f"x{n}"
                    xdir.mkdir()
                if before:
                    before(xdir)
                out = d / f"out-{n}"
                code, took, err, lines = run(bash, wrapper(d, display, xdir), xdir, stubdir, out, mode, delay, sigs)
                started = [l for l in lines if l.startswith("start ")]
                why = []
                if code != want:
                    why.append(f"exit {code}, want {want}")
                if took < at_least:
                    why.append(f"returned after {took:.2f} s, before the stub's ready line ({at_least} s)")
                if at_most is not None and took > at_most:
                    why.append(f"returned after {took:.2f} s, want under {at_most} s")
                if len(started) != 1:
                    why.append(f"{len(started)} Xvfb start(s), want 1")
                for s in started:
                    f = dict(kv.split("=", 1) for kv in s.split()[1:8])
                    if f.get("disp") != str(display):
                        why.append(f"display {f.get('disp')}, want {display}")
                    if (f.get("lock"), f.get("socket")) != met:
                        why.append(f"Xvfb met lock={f.get('lock')} socket={f.get('socket')}, the case planted "
                                   f"lock={met[0]} socket={met[1]}")
                    if f.get("job_token") != "" or f.get("session_id") != "set":
                        why.append(f"JOB_TOKEN {f.get('job_token') or 'unset'}, the control SESSION_ID "
                                   f"{f.get('session_id') or 'unset'}: want unset and set")
                    if f.get("fd") in (None, "none") or "-nolisten tcp" not in s:
                        why.append(f"argv lacks -displayfd or -nolisten tcp: {s}")
                locked = [l for l in lines if l.startswith("locked ")]
                if locked:
                    why.append(f"an Xvfb took the lock ({locked}): every start owes -displayfd's nolock")
                if want == 0 and started:
                    pid = started[-1].split()[1].split("=", 1)[1]
                    order = [l for l in lines if l in (f"ready pid={pid}", "returned 0")]
                    if order[:2] != [f"ready pid={pid}", "returned 0"]:
                        why.append(f"the start returned 0 before its Xvfb was ready ({order})")
                    if f"gone pid={pid}" in lines:
                        why.append(f"the start returned 0 but its Xvfb exited within {SETTLE_S} s of it")
                    m = re.findall(r"level=info msg=Xvfb ready(?: after (\d+) ms)?$", err, re.M)
                    if len(m) != 1:
                        why.append(f"want the line 'Xvfb ready' once, stderr {err.strip()!r}")
                    elif timed and not (m[0] and took_ms <= int(m[0]) <= took * 1000):
                        why.append(f"'Xvfb ready after {m[0] or '?'} ms', want between the stub's {took_ms:.0f} "
                                   f"ms and the case's {took * 1000:.0f} ms")
                    elif not timed and m[0]:
                        why.append(f"a duration before bash 5: {m[0]} ms")
                if said is None and "Xvfb did not start" in err:
                    why.append(f"an error line on a good start: {err.strip()!r}")
                if said is not None and err.count(said) != 1:
                    why.append(f"want the line {said!r} once, stderr {err.strip()!r}")
                left = sorted(p_.name for p_ in xdir.glob(".seazn-xvfb-ready.*"))
                if left:
                    why.append(f"the ready file is left: {left}")
                ok = not why
                print(f"{'ok  ' if ok else 'FAIL'} bash {ver}: {name}: exit {code} after {took:.2f} s"
                      + ("" if ok else f"\n     {'; '.join(why)}\n     stub: {lines}"))
                checked += 1
                if not ok:
                    failures.append(f"bash {ver}: {name}")
                return xdir

            case("fresh, no leftovers: ready once Xvfb says so", "up", at_least=0.3, took_ms=300)
            again = case("a restart in place: the previous life's socket left (no lock: -displayfd)", "up",
                         before=lambda x: leave_previous_life(x, 99), met=("no", "yes"), at_least=0.3,
                         took_ms=300)
            # The first restart's Xvfb was ended by the case (SIGKILL, as at a container's end), so its
            # socket is what the second restart meets.
            case("a second restart in the same dir: what the first restart's Xvfb left", "up", xdir=again,
                 met=("no", "yes"), at_least=0.3, took_ms=300)
            case("F-025's leftovers, a lock naming a live pid and its socket: -displayfd's nolock", "up",
                 before=lambda x: leave_previous_life(x, 99, lock=True), met=("yes", "yes"), at_least=0.3,
                 took_ms=300)
            case("Xvfb exits before it is ready, after binding: refused at once", "die", want=70,
                 at_most=WAIT_S / 2, said="Xvfb did not start: it exited before it was ready")
            case("Xvfb refuses before binding, an earlier life's socket left (F-025): refused at once", "refuse",
                 before=lambda x: leave_previous_life(x, 99), met=("no", "yes"), want=70, at_most=WAIT_S / 2,
                 said="Xvfb did not start: it exited before it was ready")
            case("Xvfb never ready: refused after the wait", "hang", want=70, at_least=WAIT_S - 0.5,
                 at_most=WAIT_S * 3, said="Xvfb did not start: not ready after 5 s")
            case("the number without its newline: no answer, refused after the wait", "nonl", want=70,
                 at_least=WAIT_S - 0.5, at_most=WAIT_S * 3, said="Xvfb did not start: not ready after 5 s")
            case("another display's number: refused at once", "wrong", want=70, at_most=WAIT_S / 2,
                 said="Xvfb did not start: its ready line named a display other than :99")
            case("the ready line's newline comes 0.5 s after its number", "split", at_least=0.8, took_ms=800)
            case("a SIGTERM during the wait: held, and the start carries on", "up", delay=1.0,
                 sigs=[(0.3, signal.SIGTERM)], at_least=1.0, took_ms=1000)
            case("another display (42), with its own leftovers", "up", display=42,
                 before=lambda x: leave_previous_life(x, 42, lock=True), met=("yes", "yes"), at_least=0.3,
                 took_ms=300)
    print(f"xvfb start cases checked: {checked}")
    if checked == 0:
        failures.append("nothing checked")
    if failures:
        print(f"{len(failures)} failure(s): {failures}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
