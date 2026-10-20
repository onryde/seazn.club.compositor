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
  - N races: the child exits 75 by itself at 0.30 s and an INT lands within 15 ms of it; every exit
    is 75.

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
# The stand-in compositor. STUB_MODE: selfexit | onsig | late | disarm.
echo started >> "$STUB_OUT"
got=
on() { got=$1; echo "$1" >> "$STUB_OUT"; }
arm() { trap 'on USR1' USR1; trap 'on USR2' USR2; }
case "$STUB_MODE" in
  selfexit) arm; sleep "$DELAY"; exit "$CODE" ;;
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


def cases():
    """(name, needs procfs, stub env, wrapper pre-sleep, extra env, [(at s, signal)], exit, first seen)."""
    return [
        ("no stop: the child's own 75", False, dict(STUB_MODE="selfexit", DELAY="0.3", CODE="75"), 0, {}, [], 75, []),
        ("INT reaches the child as USR1", False, dict(STUB_MODE="onsig", AFTER="0.3", CODE="0"), 0, {}, [(0.3, I)], 0, ["USR1"]),
        ("TERM reaches the child as USR2", False, dict(STUB_MODE="onsig", AFTER="0.3", CODE="70"), 0, {}, [(0.3, T)], 70, ["USR2"]),
        ("INT then TERM", False, dict(STUB_MODE="onsig", AFTER="0.6", CODE="75"), 0, {}, [(0.3, I), (0.4, T)], 75, ["USR1"]),
        ("INT three times", False, dict(STUB_MODE="onsig", AFTER="0.6", CODE="0"), 0, {}, [(0.3, I), (0.35, I), (0.4, I)], 0, ["USR1"]),
        ("the child dies of the forwarded USR2", False, dict(STUB_MODE="disarm", AFTER="3", CODE="0"), 0, {}, [(0.3, I), (0.6, T)], USR2_DEATH, ["USR1"]),
        ("a stop before the child exists is held", True, dict(STUB_MODE="onsig", AFTER="0.1", CODE="0"), 0.5, {}, [(0.2, I)], 0, ["USR1"]),
        ("a stop before the child's handlers is held", True, dict(STUB_MODE="late", LATE="0.4", AFTER="0.1", CODE="0"), 0, {}, [(0.15, I)], 0, ["USR1"]),
        ("TINI_KILL_PROCESS_GROUP is refused", False, dict(STUB_MODE="selfexit", DELAY="0.1", CODE="0"), 0, {"TINI_KILL_PROCESS_GROUP": "1"}, [], 78, None),
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


def run(bash, wrap, env, sigs, limit=15):
    out = Path(tempfile.mktemp(prefix="stub-out-", dir=wrap.parent))
    e = {k: v for k, v in os.environ.items() if k != "TINI_KILL_PROCESS_GROUP"}
    e.update(env, STUB_OUT=str(out))
    p = subprocess.Popen([bash, str(wrap)], env=e, start_new_session=True, stderr=subprocess.PIPE, text=True)
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
    lines = out.read_text().split() if out.exists() else []
    if out.exists():
        out.unlink()
    return code, lines, err


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
                    code, lines, err = run(bash, wrapper(d, script, stub, pre), {**env, **extra}, sigs)
                    seen = [l for l in lines if l != "started"]
                    ok = code == want
                    if first is None:
                        ok = ok and lines == []
                    else:
                        ok = ok and "started" in lines and seen[: len(first)] == first
                    if not ok or rep == reps - 1:
                        print(f"{'ok  ' if ok else 'FAIL'} bash {ver}: {name} ({rep + 1} of {reps}): exit {code} (want {want}), child saw {seen}"
                              + ("" if ok or not err else f"\n     stderr: {err.strip()}"))
                    checked += 1
                    if not ok:
                        failures.append(f"bash {ver}: {name}")
                        break
            tally = {}
            for _ in range(a.races):
                at = 0.30 + random.uniform(-0.015, 0.015)
                code, _, _ = run(bash, wrapper(d, script, stub, 0), dict(STUB_MODE="selfexit", DELAY="0.3", CODE="75"), [(at, I)])
                tally[code] = tally.get(code, 0) + 1
            ok = a.races > 0 and tally == {75: a.races}
            print(f"{'ok  ' if ok else 'FAIL'} bash {ver}: {a.races} races (the child exits 75 at 0.30 s, INT at 0.30 +- 0.015 s): {tally}")
            checked += 1
            if not ok:
                failures.append(f"bash {ver}: races {tally}")
    print(f"supervise cases checked: {checked}, skipped: {skipped}")
    if checked == 0:
        failures.append("nothing checked")
    if a.require_procfs and skipped:
        failures.append(f"{skipped} case(s) skipped with --require-procfs")
    if failures:
        print(f"{len(failures)} failure(s): {failures}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
