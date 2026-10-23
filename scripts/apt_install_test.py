#!/usr/bin/env python3
"""Tests scripts/apt-install.sh, check.yml's GStreamer install (decision log "CI: apt bounded on a
trickling mirror", 2026-10-23). Stdlib only; run with `python3 -I` (check.yml).

Fakes on PATH stand in for sudo (runs its arguments as "root"), timeout (logs its bound, runs the
rest), apt-get (refuses unless "root"; logs its arguments and exits with the next code planned for
its phase), apt-config (logs), tee (refuses the mirror list unless "root") and sleep (logs, returns
at once). The mirror list is the runner image's three lines; the slow runs' logs size it
("Get:1 file:/etc/apt/apt-mirrors.txt Mirrorlist [144 B]"), and the fixture is checked against that.

  - the happy path: update, then a --download-only fetch, then a --no-download install, in that
    order; the fetch's arguments less --download-only equal the install's less --no-download, and
    carry main's `-y --no-install-recommends`; the two network phases each under a timeout with a
    kill grace, and the install under none; every apt-get call under sudo, with the same network
    settings; apt's effective settings logged; the mirror list untouched; no warning;
  - apt's own settings (retries R, timeout T) no looser than the runner image's zz-retries (1, 15 s),
    and leaving a silent mirror for the next one inside one update attempt, even if every try waits its full timeout: (R + 1) x T + (2^R - 1) s of retry
    delay (1, 2, 4 ... s, as the blackhole run's log spaces them) under the update bound;
  - a fetch that fails once (124, over its bound) then passes: one warning, azure dropped from the
    list (the other two lines kept, in order), the fetch run twice, the install once;
  - every fetch fails: one ::error:: naming the last exit (124, or apt's own 100), a non-zero exit,
    the install never run, the fetch run a bounded number of times, and the list after the last
    failure equal to the list after the first;
  - an update that fails once then passes: azure dropped before the fetch; every update fails: the
    fetch and the install never run;
  - no mirror list, a list holding only azure: the retry still runs, and the list is never emptied;
  - no packages named: refused, nothing run;
  - the bound: the worst case over the network phases (each attempt's bound plus its kill grace,
    plus each pause, read from the fakes' logs) fits inside the GStreamer step's own timeout-minutes
    in check.yml, less 90 s for the offline install (measured 20 to 34 s, five normal runs of
    2026-10-23); each fetch bound is at least 4x the slowest normal fetch (14 s, run 39977135072),
    and each update bound at least 4x the slowest normal update (10 s, run 39936996163);
  - check.yml's GStreamer step runs this script and carries a step-level timeout-minutes.

Each sweep prints how many cases it checked, counting only runs of the script that printed
anything, and a sweep that checked none is a failure; the number of checks made is printed too.

Usage: apt_install_test.py [--script scripts/apt-install.sh] [--workflow .github/workflows/check.yml]
"""

import argparse
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MIRRORS = (
    "http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n"
    "https://archive.ubuntu.com/ubuntu/\tpriority:2\n"
    "https://security.ubuntu.com/ubuntu/\tpriority:3\n"
)
WITHOUT_AZURE = MIRRORS.split("\n", 1)[1]
PACKAGES = ["libgstreamer1.0-dev", "gstreamer1.0-libav"]
OPTIONS = ("Acquire::Retries", "Acquire::http::Timeout", "Acquire::https::Timeout")
# check.yml's GStreamer step before PR #50: `apt-get install -y --no-install-recommends <packages>`.
MAIN_FLAGS = ("-y", "--no-install-recommends")
CASES = ("happy path", "fetch fails once", "every fetch times out", "every fetch errs",
         "update fails once", "every update fails", "no mirror list", "only azure listed", "no packages")
# A clean run makes 66 (PR #50 round 2); fewer means a case stopped early.
CHECKS_FLOOR = 60
# The runner image's own apt settings, /etc/apt/apt.conf.d/zz-retries (runner-images
# images/ubuntu/scripts/build/configure-apt.sh, tag ubuntu24/20261012.334, x64): the script may
# not be looser, or apt leaves a silent mirror later than it does on main.
IMAGE_RETRIES = 1
IMAGE_TIMEOUT_S = 15
# Measured on the normal runs of 2026-10-23 (docs/_INDEX.md, the decision row).
SLOWEST_FETCH_S = 14
SLOWEST_UPDATE_S = 10
INSTALL_ALLOWANCE_S = 90

FAKE_SUDO = '#!/bin/sh\nFAKE_ROOT=1 exec "$@"\n'
FAKE_CONFIG = '#!/bin/sh\nprintf "apt-config %s\\n" "$*" >> "$FAKE_LOG"\n'
FAKE_TEE = """#!/bin/sh
for a in "$@"; do
  if [ "$a" = "$APT_MIRRORS" ] && [ "${FAKE_ROOT:-}" != 1 ]; then
    echo "tee: $a: Permission denied" >&2
    exit 1
  fi
done
exec REAL_TEE "$@"
"""
FAKE_SLEEP = '#!/bin/sh\nprintf "sleep %s\\n" "$*" >> "$FAKE_LOG"\n'
FAKE_TIMEOUT = """#!/bin/sh
opts=""
while [ $# -gt 0 ]; do
  case "$1" in
    -k|-s) opts="$opts $1 $2"; shift 2 ;;
    -*) opts="$opts $1"; shift ;;
    *) break ;;
  esac
done
printf "timeout%s %s\\n" "$opts" "$1" >> "$FAKE_LOG"
shift
exec "$@"
"""
FAKE_APT = """#!/bin/sh
if [ "${FAKE_ROOT:-}" != 1 ]; then
  printf "unprivileged apt-get %s\\n" "$*" >> "$FAKE_LOG"
  exit 100
fi
printf "apt-get %s\\n" "$*" >> "$FAKE_LOG"
case " $* " in
  *" update "*) phase=update ;;
  *" --download-only "*) phase=fetch ;;
  *) phase=install ;;
esac
n=$(cat "$FAKE_DIR/$phase.n" 2>/dev/null || echo 0)
echo $((n + 1)) > "$FAKE_DIR/$phase.n"
codes=$(cat "$FAKE_DIR/$phase.codes" 2>/dev/null || echo 0)
set -- $codes
i=0
code=0
for c in "$@"; do
  code=$c
  [ "$i" -eq "$n" ] && break
  i=$((i + 1))
done
exit "$code"
"""

failures = []
checks = []
ran = []
every_call = []


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    checks.append(what)
    if not ok:
        failures.append(what)


def seconds(text):
    m = re.fullmatch(r"(\d+)(s|m)?", text)
    if not m:
        raise SystemExit(f"apt_install_test: a duration this file cannot read: {text!r}")
    return int(m.group(1)) * (60 if m.group(2) == "m" else 1)


def write_fakes(bindir):
    real_tee = shutil.which("tee")
    if real_tee is None:
        raise SystemExit("apt_install_test: no tee on PATH")
    for name, body in (("sudo", FAKE_SUDO), ("sleep", FAKE_SLEEP), ("timeout", FAKE_TIMEOUT),
                       ("apt-get", FAKE_APT), ("apt-config", FAKE_CONFIG),
                       ("tee", FAKE_TEE.replace("REAL_TEE", real_tee))):
        path = os.path.join(bindir, name)
        with open(path, "w") as f:
            f.write(body)
        os.chmod(path, stat.S_IRWXU)


def run(case, script, bindir, codes, mirrors=MIRRORS, packages=PACKAGES):
    """Runs the script under the fakes in `bindir`, with its own state and mirror list. Returns
    (exit code, output, calls, mirror list after). A run that printed nothing is not counted."""
    with tempfile.TemporaryDirectory() as d:
        for phase, plan in codes.items():
            with open(os.path.join(d, phase + ".codes"), "w") as f:
                f.write(" ".join(str(c) for c in plan))
        mirror_file = os.path.join(d, "apt-mirrors.txt")
        if mirrors is not None:
            with open(mirror_file, "w") as f:
                f.write(mirrors)
        log = os.path.join(d, "calls.log")
        open(log, "w").close()
        env = {"PATH": bindir + os.pathsep + os.environ.get("PATH", "/usr/bin:/bin"),
               "FAKE_LOG": log, "FAKE_DIR": d, "APT_MIRRORS": mirror_file, "LC_ALL": "C"}
        p = subprocess.run([script] + packages, env=env, capture_output=True, text=True, timeout=60)
        with open(log) as f:
            calls = f.read().splitlines()
        every_call.extend(calls)
        if (p.stdout + p.stderr).strip():
            ran.append(case)
        after = None
        if os.path.exists(mirror_file):
            with open(mirror_file) as f:
                after = f.read()
        return p.returncode, p.stdout + p.stderr, calls, after


def phases(calls):
    """Each apt-get call's phase, and the timeout line (or None) just before it."""
    out = []
    for i, c in enumerate(calls):
        if not c.startswith("apt-get "):
            continue
        args = c.split()[1:]
        phase = "update" if "update" in args else "fetch" if "--download-only" in args else "install"
        bound = calls[i - 1] if i > 0 and calls[i - 1].startswith("timeout") else None
        out.append((phase, args, bound))
    return out


def bound_of(line):
    """(duration, kill grace) of a fake timeout line: `timeout -k 5 45`."""
    words = line.split()[1:]
    kill = 0
    for i, w in enumerate(words):
        if w == "-k":
            kill = seconds(words[i + 1])
        elif w.startswith("--kill-after="):
            kill = seconds(w.split("=", 1)[1])
    return seconds(words[-1]), kill


def options_of(args):
    found = {}
    for i, a in enumerate(args):
        if a == "-o" and i + 1 < len(args) and "=" in args[i + 1]:
            k, v = args[i + 1].split("=", 1)
            found[k] = v
    return found


def worst_case(calls, phase):
    """Seconds one run can spend in `phase`: each of its attempts' bound plus kill grace, plus every
    pause. The all-fail runs pause only between the failing phase's attempts (checked here)."""
    total = 0
    for p, _, b in phases(calls):
        if p == phase:
            check(b is not None, f"every {phase} attempt in the all-fail {phase} run is under a timeout")
            if b is None:
                continue
            s, k = bound_of(b)
            total += s + k
    first = next((i for i, c in enumerate(calls) if c.startswith("apt-get ") and phases([c])[0][0] == phase), None)
    check(first is not None, f"the all-fail {phase} run made a {phase} attempt")
    if first is None:
        return 0
    for i, c in enumerate(calls):
        if c.startswith("sleep "):
            check(i > first, f"a pause in the all-fail {phase} run follows a {phase} attempt")
            total += seconds(c.split()[1])
    return total


def step_block(workflow, name):
    """The lines of the step `- name: <name>` in a workflow, up to the next step."""
    with open(workflow) as f:
        lines = f.read().splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"^(\s*)- name: (.*)$", line)
        if m and m.group(2).strip() == name:
            indent = m.group(1)
            block = [line]
            for nxt in lines[i + 1:]:
                if re.match(rf"^{indent}- ", nxt) or (nxt.strip() and len(nxt) - len(nxt.lstrip()) < len(indent)):
                    break
                block.append(nxt)
            return block
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--script", default=os.path.join(ROOT, "scripts", "apt-install.sh"))
    ap.add_argument("--workflow", default=os.path.join(ROOT, ".github", "workflows", "check.yml"))
    a = ap.parse_args()
    script = os.path.abspath(a.script)
    check(os.access(script, os.X_OK), f"{os.path.relpath(script, ROOT)} exists and is executable")
    if failures:
        return 1
    # One set of fakes for every case: macOS scans each new executable on its first run (about 2 s).
    with tempfile.TemporaryDirectory() as bindir:
        write_fakes(bindir)
        return sweep(a, script, bindir)


def sweep(a, script, bindir):
    check(len(MIRRORS.encode()) == 144, "the mirror-list fixture is 144 bytes, as the runner logs size it")

    # The happy path.
    rc, out, calls, after = run("happy path", script, bindir, {})
    ph = phases(calls)
    check(rc == 0, f"happy path: exit 0 (got {rc})")
    check([p for p, _, _ in ph] == ["update", "fetch", "install"],
          f"happy path: update, fetch, install, in that order (got {[p for p, _, _ in ph]})")
    check(all(b is not None for p, _, b in ph if p != "install"), "happy path: update and fetch each under a timeout")
    check(all(b is None for p, _, b in ph if p == "install"), "happy path: the install under no timeout")
    for p, args, _ in ph:
        opts = options_of(args)
        check(all(k in opts for k in OPTIONS), f"happy path: {p} carries {', '.join(OPTIONS)}")
        check(all(x in args for x in PACKAGES) == (p != "update"), f"happy path: {p} names the packages iff it installs")
    check(len({tuple(sorted(options_of(args).items())) for _, args, _ in ph}) == 1,
          "happy path: every apt-get call carries the same network settings")
    fetch_args = next((args for p, args, _ in ph if p == "fetch"), None)
    install_args = next((args for p, args, _ in ph if p == "install"), None)
    if fetch_args is not None and install_args is not None:
        check("--no-download" in install_args and "--download-only" not in install_args,
              "happy path: the install is --no-download (offline: a missing .deb fails at once)")
        check([x for x in fetch_args if x != "--download-only"] == [x for x in install_args if x != "--no-download"],
              "happy path: the fetch's arguments less --download-only equal the install's less --no-download")
        check(all(f in install_args for f in MAIN_FLAGS), f"happy path: the install carries main's {' '.join(MAIN_FLAGS)}")
    else:
        check(False, "happy path: both a fetch and an install ran")
    first_opts = options_of(ph[0][1]) if ph else {}
    dumps = [c for c in calls if c.startswith("apt-config ")]
    check(len(dumps) == 1 and "dump" in dumps[0].split()
          and options_of(dumps[0].split()[1:]) == first_opts,
          "happy path: apt's effective network settings logged once, with the same -o options")
    check(after == MIRRORS, "happy path: the mirror list untouched")
    check("::warning::" not in out and "::error::" not in out, "happy path: no warning, no error")
    happy = ph

    # apt's own settings leave a silent mirror inside one update attempt. The blackhole run spaced
    # the retries 1, 2, 4 ... s apart; count a full timeout for every try, to be safe.
    update_bound = None
    fetch_bound = None
    for p, args, b in happy:
        if b is None:
            continue
        if p == "update":
            update_bound = bound_of(b)[0]
            check(update_bound >= 4 * SLOWEST_UPDATE_S,
                  f"the update bound ({update_bound} s) is at least 4x the slowest normal update")
        if p == "fetch":
            fetch_bound = bound_of(b)[0]
    check(fetch_bound is not None and fetch_bound >= 4 * SLOWEST_FETCH_S,
          f"the fetch bound ({fetch_bound} s) is at least 4x the slowest normal fetch ({SLOWEST_FETCH_S} s)")
    r = first_opts.get("Acquire::Retries", "")
    ts = [first_opts.get(k, "") for k in ("Acquire::http::Timeout", "Acquire::https::Timeout")]
    if r.isdigit() and all(t.isdigit() and int(t) > 0 for t in ts) and update_bound:
        check(int(r) <= IMAGE_RETRIES and all(int(t) <= IMAGE_TIMEOUT_S for t in ts),
              f"Retries={r} and Timeouts={'/'.join(ts)} are no looser than the image's ({IMAGE_RETRIES}, {IMAGE_TIMEOUT_S} s)")
        leave = (int(r) + 1) * max(int(t) for t in ts) + (2 ** int(r) - 1)
        print(f"apt leaves a silent mirror after at most {leave} s (Retries={r}, Timeouts={'/'.join(ts)})")
        check(leave < update_bound, f"... which is under the {update_bound} s update bound")
    else:
        check(False, f"Retries and both Timeouts are whole seconds (got {r!r}, {ts!r})")

    # A fetch over its bound once, then through.
    rc, out, calls, after = run("fetch fails once", script, bindir, {"fetch": [124, 0]})
    ph = [p for p, _, _ in phases(calls)]
    check(rc == 0, f"fetch fails once: exit 0 (got {rc})")
    check(ph == ["update", "fetch", "fetch", "install"], f"fetch fails once: fetched twice, installed once (got {ph})")
    check(out.count("::warning::") == 1 and "::error::" not in out, "fetch fails once: one warning, no error")
    check(after == WITHOUT_AZURE, "fetch fails once: azure dropped, the other two lines kept in order")

    # Every fetch fails: over its bound, or with apt's own error.
    fetch_fail_calls = None
    for case, code in (("every fetch times out", 124), ("every fetch errs", 100)):
        rc, out, calls, after = run(case, script, bindir, {"fetch": [code] * 50})
        ph = [p for p, _, _ in phases(calls)]
        fetches = ph.count("fetch")
        errors = [line for line in out.splitlines() if line.startswith("::error::")]
        check(rc != 0, f"{case}: a non-zero exit (got {rc})")
        check("install" not in ph, f"{case}: the install never runs")
        check(2 <= fetches <= 5, f"{case}: fetched a bounded number of times, more than once (got {fetches})")
        check(len(errors) == 1 and f"exit {code}" in errors[0], f"{case}: one ::error::, naming exit {code}")
        check(out.count("::warning::") == fetches, f"{case}: one warning per failed attempt")
        check(after == WITHOUT_AZURE, f"{case}: the list after the last failure equals the list after the first")
        if code == 124:
            fetch_fail_calls = calls

    # An update that fails once, then passes: the fetch already reads the fallback list.
    rc, out, calls, after = run("update fails once", script, bindir, {"update": [100, 0]})
    ph = [p for p, _, _ in phases(calls)]
    check(rc == 0 and ph == ["update", "update", "fetch", "install"],
          f"update fails once: updated twice, then fetch and install (got {rc}, {ph})")
    check(after == WITHOUT_AZURE, "update fails once: azure dropped")

    # Every update fails.
    rc, out, calls, after = run("every update fails", script, bindir, {"update": [124] * 50})
    ph = [p for p, _, _ in phases(calls)]
    updates = ph.count("update")
    check(rc != 0, f"every update fails: a non-zero exit (got {rc})")
    check(ph == ["update"] * updates and 2 <= updates <= 5,
          f"every update fails: only updates, a bounded number, more than one (got {ph})")
    check(out.count("::error::") == 1, "every update fails: one ::error::")
    update_fail_calls = calls

    # No mirror list; a list holding only azure.
    rc, out, calls, after = run("no mirror list", script, bindir, {"fetch": [124, 0]}, mirrors=None)
    check(rc == 0 and [p for p, _, _ in phases(calls)].count("fetch") == 2,
          "no mirror list: the fetch is still retried, and passes")
    check(after is None, "no mirror list: none is written")
    only_azure = MIRRORS.split("\n", 1)[0] + "\n"
    rc, out, calls, after = run("only azure listed", script, bindir, {"fetch": [124, 0]}, mirrors=only_azure)
    check(rc == 0 and [p for p, _, _ in phases(calls)].count("fetch") == 2,
          "only azure listed: the fetch is still retried, and passes")
    check(after == only_azure, "only azure listed: the list is kept, never emptied")

    # No packages.
    rc, out, calls, after = run("no packages", script, bindir, {}, packages=[])
    check(rc != 0 and not any("apt-get" in c for c in calls), f"no packages: refused, nothing run (got {rc})")

    # Across every run: apt-get only ever under sudo, and every bounded attempt with a kill grace (a
    # hung apt-get that ignores the TERM is still killed inside the worst case below).
    check(not any(c.startswith("unprivileged apt-get") for c in every_call), "every apt-get call ran under sudo")
    bounded = [c for c in every_call if c.startswith("timeout")]
    graced = [c for c in bounded if bound_of(c)[1] > 0]
    print(f"bounded attempts checked: {len(bounded)}, with a kill grace: {len(graced)}")
    check(len(bounded) > 0 and len(graced) == len(bounded), "every bounded attempt carries a kill grace")

    cases = len(ran)
    print(f"script cases checked: {cases} ({', '.join(ran)})")
    check(cases > 0 and ran == list(CASES), "the script sweep ran every case, each printing something")

    # The bound. Worst case: every update but the last fails, the last runs to its bound, and every
    # fetch fails. The all-fail update run holds every update attempt's bound and pause; the all-fail
    # fetch run, past its one update, every fetch attempt's bound and pause.
    worst = worst_case(update_fail_calls, "update") + worst_case(fetch_fail_calls, "fetch")
    block = step_block(a.workflow, "GStreamer")
    check(block is not None, "check.yml has a GStreamer step")
    if block is None:
        return 1
    text = "\n".join(block)
    m = re.search(r"^\s*timeout-minutes:\s*(\d+)\s*$", text, re.M)
    check(m is not None, "the GStreamer step carries a step-level timeout-minutes")
    check(re.search(r"^\s*run:\s*scripts/apt-install\.sh\s+\S", text, re.M) is not None,
          "the GStreamer step runs scripts/apt-install.sh with its packages")
    if m:
        step_s = int(m.group(1)) * 60
        print(f"worst case over the network phases: {worst} s; the step's bound {step_s} s, "
              f"less {INSTALL_ALLOWANCE_S} s for the install")
        check(0 < worst <= step_s - INSTALL_ALLOWANCE_S,
              "the script fails on its own, with its own ::error::, before the step's bound")

    print(f"checks made: {len(checks)}")
    check(len(checks) >= CHECKS_FLOOR, f"at least {CHECKS_FLOOR} checks made")
    print(f"failures: {len(failures)}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
