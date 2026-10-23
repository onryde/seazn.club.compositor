#!/usr/bin/env python3
"""Drives `docker/supervise.sh` (the relay image's stop supervision, F-016) with a stub child, under
each given bash (check.yml: Ubuntu's, the image's). Stdlib only; run with `python3 -I`.

Each case is a wrapper script that sources the supervision file, optionally sleeps, then runs
`supervise <stub>` and exits with its status; the signals go to the wrapper alone, as tini sends
them. The stub writes every signal it catches to a file. The cases (the PR #36 review's I2 and m1):

  - no stop: the child's own 75 passes through;
  - INT reaches the child as USR1, TERM as USR2, and the exit is the child's own status after them;
  - INT then TERM, and INT three times;
  - the child dies of the forwarded signal itself: the exit is 128 + SIGUSR2, never the shell's
    own 143 (the c080c3f loop's bug: it stopped waiting once `kill -0` lost the child);
  - a stop before the child exists, and one before its handlers are installed, are held and then
    delivered (these need procfs: on a host without it they are skipped, and `--require-procfs`
    turns a skip into a failure);
  - TINI_KILL_PROCESS_GROUP set: exit 78, and the child never starts;
  - the child's stdout reaches the wrapper's stdout, never the pipe that carries its pid and status;
  - the child aborts (SIGABRT): the exit is 128 + SIGABRT;
  - in every case, a child killed by a signal (an exit above 128) is reported by exactly one line,
    `level=error msg=the compositor was killed by SIG<name> (status <exit>)`, the name from Python's
    own signal table; any other exit (75, 0, 70, 78) has no such line. The waiter's `wait` prints no
    job report of its own, as the trapping shell's did before F-019, so this line is the operator's
    only sign of the abort;
  - N races: the child exits 75 by itself 0.30 s after it starts and an INT lands within 15 ms of
    that; every exit
    is 75 (the round-1 loop, which `wait`ed in the trapping shell, lost 2 of 150 on bash 5.2.21).

Usage: supervise_test.py [--script docker/supervise.sh] [--bash PATH ...] [--races N] [--require-procfs]
"""

import argparse
import os
import random
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STUB = r"""#!/usr/bin/env bash
# The stand-in compositor. STUB_MODE: selfexit | chatty | onsig | late | disarm | abort.
echo started >> "$STUB_OUT"
got=
on() { got=$1; echo "$1" >> "$STUB_OUT"; }
arm() { trap 'on USR1' USR1; trap 'on USR2' USR2; }
case "$STUB_MODE" in
  selfexit) arm; sleep "$DELAY"; exit "$CODE" ;;
  # Dies of SIGABRT's default action, with no core file left on the host.
  abort) arm; sleep "$DELAY"; ulimit -c 0; kill -ABRT $$; sleep 5; exit 0 ;;
  # Writes number lines to its stdout, which must reach the wrapper's stdout, never the waiter's pipe.
  chatty) arm; echo 4242; echo 99; sleep "$DELAY"; exit "$CODE" ;;
  onsig) arm ;;
  late) sleep "$LATE"; arm ;;
  # Catches USR1, then lets USR2 take its default action: the next USR2 ends it inside the kernel,
  # with no handler of its own to run first.
  disarm) trap 'on USR1; trap - USR2' USR1; trap 'on USR2' USR2 ;;
esac
while [ -z "$got" ]; do sleep 0.01; done
sleep "$AFTER"
exit "$CODE"
"""

I, T = signal.SIGINT, signal.SIGTERM
USR2_DEATH = 128 + signal.SIGUSR2
ABRT_DEATH = 128 + signal.SIGABRT
KILLED = "the compositor was killed by"


def killed_line(code):
    """The one line a signal death owes, from Python's signal table (never from the script), or None."""
    if not isinstance(code, int) or code <= 128:
        return None
    return f"level=error msg={KILLED} {signal.Signals(code - 128).name} (status {code})"


def cases():
    """(name, needs procfs, stub env, wrapper pre-sleep, extra env, [(at s, signal)], exit, first seen)."""
    return [
        ("no stop: the child's own 75", False, dict(STUB_MODE="selfexit", DELAY="0.3", CODE="75"), 0, {}, [], 75, []),
        ("the child's stdout is not the status pipe", False, dict(STUB_MODE="chatty", DELAY="0.3", CODE="75"), 0, {}, [], 75, []),
        ("INT reaches the child as USR1", False, dict(STUB_MODE="onsig", AFTER="0.3", CODE="0"), 0, {}, [(0.3, I)], 0, ["USR1"]),
        ("TERM reaches the child as USR2", False, dict(STUB_MODE="onsig", AFTER="0.3", CODE="70"), 0, {}, [(0.3, T)], 70, ["USR2"]),
        ("INT then TERM", False, dict(STUB_MODE="onsig", AFTER="0.6", CODE="75"), 0, {}, [(0.3, I), (0.4, T)], 75, ["USR1"]),
        ("INT three times", False, dict(STUB_MODE="onsig", AFTER="0.6", CODE="0"), 0, {}, [(0.3, I), (0.35, I), (0.4, I)], 0, ["USR1"]),
        ("the child dies of the forwarded USR2", False, dict(STUB_MODE="disarm", AFTER="3", CODE="0"), 0, {}, [(0.3, I), (0.6, T)], USR2_DEATH, ["USR1"]),
        ("a stop before the child exists is held", True, dict(STUB_MODE="onsig", AFTER="0.1", CODE="0"), 0.5, {}, [(0.2, I)], 0, ["USR1"]),
        ("a stop before the child's handlers is held", True, dict(STUB_MODE="late", LATE="0.4", AFTER="0.1", CODE="0"), 0, {}, [(0.15, I)], 0, ["USR1"]),
        ("TINI_KILL_PROCESS_GROUP is refused", False, dict(STUB_MODE="selfexit", DELAY="0.1", CODE="0"), 0, {"TINI_KILL_PROCESS_GROUP": "1"}, [], 78, None),
        ("the child's abort is reported", False, dict(STUB_MODE="abort", DELAY="0.3"), 0, {}, [], ABRT_DEATH, []),
    ]


def wrapper(d, script, stub, pre):
    p = d / f"wrap-{pre}.sh"
    p.write_text(
        "set -euo pipefail\n"
        f". {script}\n"
        f"sleep {pre}\n"
        f"code=0\nsupervise {stub} || code=$?\nexit \"$code\"\n"
    )
    return p


def run(bash, wrap, env, sigs, limit=15, anchor=False):
    """Signal times count from the wrapper's start, or with `anchor` from the stub's own start (its
    "started" line), so a race lands at the child's real exit on a loaded host too."""
    out = Path(tempfile.mktemp(prefix="stub-out-", dir=wrap.parent))
    e = {k: v for k, v in os.environ.items() if k != "TINI_KILL_PROCESS_GROUP"}
    e.update(env, STUB_OUT=str(out))
    p = subprocess.Popen([bash, str(wrap)], env=e, start_new_session=True, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, text=True)
    t0 = time.monotonic()
    if anchor:
        while time.monotonic() - t0 < limit and not (out.exists() and "started" in out.read_text()):
            time.sleep(0.001)
        t0 = time.monotonic()
    for at, s in sigs:
        d = t0 + at - time.monotonic()
        if d > 0:
            time.sleep(d)
        try:
            os.kill(p.pid, s)
        except ProcessLookupError:
            pass
    try:
        code = p.wait(limit)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        p.wait()
        code = "TIMEOUT"
    err = p.stderr.read()
    stdout = p.stdout.read()
    lines = out.read_text().split() if out.exists() else []
    if out.exists():
        out.unlink()
    return code, lines, err, stdout


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--script", default=str(ROOT / "docker/supervise.sh"))
    ap.add_argument("--bash", action="append")
    ap.add_argument("--races", type=int, default=150)
    ap.add_argument("--require-procfs", action="store_true")
    a = ap.parse_args()
    bashes = a.bash or ["bash"]
    procfs = Path("/proc/self/status").exists()
    script = Path(a.script).resolve()
    failures, checked, skipped = [], 0, 0
    reports_present = reports_absent = 0
    with tempfile.TemporaryDirectory(prefix="supervise-test-") as tmp:
        d = Path(tmp)
        stub = d / "stub.sh"
        stub.write_text(STUB)
        stub.chmod(0o755)
        for bash in bashes:
            ver = subprocess.run([bash, "-c", "echo $BASH_VERSION"], capture_output=True, text=True).stdout.strip()
            for name, needs_procfs, env, pre, extra, sigs, want, first in cases():
                if needs_procfs and not procfs:
                    print(f"skip bash {ver}: {name} (no procfs)")
                    skipped += 1
                    continue
                # The death case repeats: the old loop lost it only when bash had reaped the child
                # before its `kill -0`, a race.
                reps = 20 if "dies of" in name else 1
                for rep in range(reps):
                    code, lines, err, stdout = run(bash, wrapper(d, script, stub, pre), {**env, **extra}, sigs)
                    seen = [l for l in lines if l != "started"]
                    ok = code == want
                    if env.get("STUB_MODE") == "chatty":
                        ok = ok and stdout.split() == ["4242", "99"]
                    if first is None:
                        ok = ok and lines == []
                    else:
                        ok = ok and "started" in lines and seen[: len(first)] == first
                    # The signal death's report: exactly one line for an exit above 128, none otherwise.
                    report = killed_line(want)
                    if report is None:
                        ok = ok and KILLED not in err
                        reports_absent += 1
                    else:
                        ok = ok and err.count(report) == 1 and err.count(KILLED) == 1
                        reports_present += 1
                    if not ok or rep == reps - 1:
                        print(f"{'ok  ' if ok else 'FAIL'} bash {ver}: {name} ({rep + 1} of {reps}): exit {code} (want {want}), child saw {seen}, "
                              + (f"report {report!r}" if report else "no killed-by report")
                              + ("" if ok or not err else f"\n     stderr: {err.strip()}"))
                    checked += 1
                    if not ok:
                        failures.append(f"bash {ver}: {name}")
                        break
            tally, odd = {}, []
            for _ in range(a.races):
                at = 0.30 + random.uniform(-0.015, 0.015)
                code, seen, err, _ = run(bash, wrapper(d, script, stub, 0), dict(STUB_MODE="selfexit", DELAY="0.3", CODE="75"), [(at, I)], anchor=True)
                tally[code] = tally.get(code, 0) + 1
                if code != 75 and len(odd) < 3:
                    odd.append(f"exit {code} with INT at {at:.3f} s, child saw {seen}, stderr {err.strip()!r}")
            for o in odd:
                print(f"     {o}")
            ok = a.races > 0 and tally == {75: a.races}
            print(f"{'ok  ' if ok else 'FAIL'} bash {ver}: {a.races} races (the child exits 75 0.30 s after its start, INT at 0.30 +- 0.015 s): {tally}")
            checked += 1
            if not ok:
                failures.append(f"bash {ver}: races {tally}")
    print(f"supervise cases checked: {checked}, skipped: {skipped}")
    print(f"killed-by reports checked: {reports_present} owed (exit above 128), {reports_absent} refused (any other exit)")
    if checked == 0:
        failures.append("nothing checked")
    if reports_present == 0 or reports_absent == 0:
        failures.append(f"killed-by reports: {reports_present} owed and {reports_absent} refused checked, want both above 0")
    if a.require_procfs and skipped:
        failures.append(f"{skipped} case(s) skipped with --require-procfs")
    if failures:
        print(f"{len(failures)} failure(s): {failures}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
