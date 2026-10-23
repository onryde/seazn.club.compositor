#!/usr/bin/env python3
"""Tests scripts/apt-install.sh, check.yml's GStreamer install (decision log "CI: apt bounded on a
trickling mirror", 2026-10-23). Stdlib only; run with `python3 -I` (check.yml).

Fakes on PATH stand in for sudo (runs its arguments), timeout (logs its bound, runs the rest),
apt-get (logs its arguments and exits with the next code planned for its phase) and sleep (logs,
returns at once). The mirror list is the runner image's three lines; the slow runs' logs size it
("Get:1 file:/etc/apt/apt-mirrors.txt Mirrorlist [144 B]"), and the fixture is checked against that.

  - the happy path: update, then a --download-only fetch, then the install, in that order; the two
    network phases each under a timeout and the install under none; every apt-get call carries the
    short network options; the mirror list untouched; no warning;
  - a fetch that fails once (124, over its bound) then passes: one warning, azure dropped from the
    list (the other two lines kept, in order), the fetch run twice, the install once;
  - every fetch fails: one ::error::, a non-zero exit, the install never run, the fetch run a bounded
    number of times, and the list after the last failure equal to the list after the first;
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

Each sweep prints how many cases it checked, and a sweep that checked none is a failure.

Usage: apt_install_test.py [--script scripts/apt-install.sh] [--workflow .github/workflows/check.yml]
"""

import argparse
import os
import re
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
# Measured on the normal runs of 2026-10-23 (docs/_INDEX.md, the decision row).
SLOWEST_FETCH_S = 14
SLOWEST_UPDATE_S = 10
INSTALL_ALLOWANCE_S = 90

FAKE_SUDO = '#!/bin/sh\nexec "$@"\n'
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


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        failures.append(what)


def seconds(text):
    m = re.fullmatch(r"(\d+)(s|m)?", text)
    if not m:
        raise SystemExit(f"apt_install_test: a duration this file cannot read: {text!r}")
    return int(m.group(1)) * (60 if m.group(2) == "m" else 1)


def write_fakes(bindir):
    for name, body in (("sudo", FAKE_SUDO), ("sleep", FAKE_SLEEP), ("timeout", FAKE_TIMEOUT),
                       ("apt-get", FAKE_APT)):
        path = os.path.join(bindir, name)
        with open(path, "w") as f:
            f.write(body)
        os.chmod(path, stat.S_IRWXU)


def run(script, bindir, codes, mirrors=MIRRORS, packages=PACKAGES):
    """Runs the script under the fakes in `bindir`, with its own state and mirror list. Returns
    (exit code, output, calls, mirror list after)."""
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
    first = next(i for i, c in enumerate(calls) if c.startswith("apt-get ") and phases([c])[0][0] == phase)
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
    cases = 0

    # The happy path.
    rc, out, calls, after = run(script, bindir, {})
    cases += 1
    ph = phases(calls)
    check(rc == 0, f"happy path: exit 0 (got {rc})")
    check([p for p, _, _ in ph] == ["update", "fetch", "install"],
          f"happy path: update, fetch, install, in that order (got {[p for p, _, _ in ph]})")
    check(all(b is not None for p, _, b in ph if p != "install"), "happy path: update and fetch each under a timeout")
    check(all(b is None for p, _, b in ph if p == "install"), "happy path: the offline install under no timeout")
    for p, args, _ in ph:
        opts = options_of(args)
        check(all(k in opts for k in OPTIONS), f"happy path: {p} carries {', '.join(OPTIONS)}")
        check(all(x in args for x in PACKAGES) == (p != "update"), f"happy path: {p} names the packages iff it installs")
    check(after == MIRRORS, "happy path: the mirror list untouched")
    check("::warning::" not in out and "::error::" not in out, "happy path: no warning, no error")
    happy = ph

    # The short network options: a silent connection is cut and retried well inside one attempt.
    fetch_bound = None
    for p, args, b in happy:
        if b is None:
            continue
        if p == "fetch":
            fetch_bound = bound_of(b)[0]
            opts = options_of(args)
            for k in ("Acquire::http::Timeout", "Acquire::https::Timeout"):
                v = opts.get(k, "")
                check(v.isdigit() and 0 < int(v) * 4 <= fetch_bound,
                      f"{k}={v} is at most a quarter of the fetch's {fetch_bound} s bound")
            r = opts.get("Acquire::Retries", "")
            check(r.isdigit() and int(r) >= 3, f"Acquire::Retries={r} retries a failed file at least 3 times")
        if p == "update":
            ub = bound_of(b)[0]
            check(ub >= 4 * SLOWEST_UPDATE_S, f"the update bound ({ub} s) is at least 4x the slowest normal update")
    check(fetch_bound is not None and fetch_bound >= 4 * SLOWEST_FETCH_S,
          f"the fetch bound ({fetch_bound} s) is at least 4x the slowest normal fetch ({SLOWEST_FETCH_S} s)")

    # A fetch over its bound once, then through.
    rc, out, calls, after = run(script, bindir, {"fetch": [124, 0]})
    cases += 1
    ph = [p for p, _, _ in phases(calls)]
    check(rc == 0, f"fetch fails once: exit 0 (got {rc})")
    check(ph == ["update", "fetch", "fetch", "install"], f"fetch fails once: fetched twice, installed once (got {ph})")
    check(out.count("::warning::") == 1 and "::error::" not in out, "fetch fails once: one warning, no error")
    check(after == WITHOUT_AZURE, "fetch fails once: azure dropped, the other two lines kept in order")

    # Every fetch fails.
    rc, out, calls, after = run(script, bindir, {"fetch": [124] * 50})
    cases += 1
    ph = [p for p, _, _ in phases(calls)]
    fetches = ph.count("fetch")
    check(rc != 0, f"every fetch fails: a non-zero exit (got {rc})")
    check("install" not in ph, "every fetch fails: the install never runs")
    check(2 <= fetches <= 5, f"every fetch fails: fetched a bounded number of times, more than once (got {fetches})")
    check(out.count("::error::") == 1, "every fetch fails: one ::error::")
    check(out.count("::warning::") == fetches, "every fetch fails: one warning per failed attempt")
    check(after == WITHOUT_AZURE, "every fetch fails: the list after the last failure equals the list after the first")
    fetch_fail_calls = calls

    # An update that fails once, then passes: the fetch already reads the fallback list.
    rc, out, calls, after = run(script, bindir, {"update": [100, 0]})
    cases += 1
    ph = [p for p, _, _ in phases(calls)]
    check(rc == 0 and ph == ["update", "update", "fetch", "install"],
          f"update fails once: updated twice, then fetch and install (got {rc}, {ph})")
    check(after == WITHOUT_AZURE, "update fails once: azure dropped")

    # Every update fails.
    rc, out, calls, after = run(script, bindir, {"update": [124] * 50})
    cases += 1
    ph = [p for p, _, _ in phases(calls)]
    updates = ph.count("update")
    check(rc != 0, f"every update fails: a non-zero exit (got {rc})")
    check(ph == ["update"] * updates and 2 <= updates <= 5,
          f"every update fails: only updates, a bounded number, more than one (got {ph})")
    check(out.count("::error::") == 1, "every update fails: one ::error::")
    update_fail_calls = calls

    # No mirror list; a list holding only azure.
    rc, out, calls, after = run(script, bindir, {"fetch": [124, 0]}, mirrors=None)
    cases += 1
    check(rc == 0 and [p for p, _, _ in phases(calls)].count("fetch") == 2,
          "no mirror list: the fetch is still retried, and passes")
    check(after is None, "no mirror list: none is written")
    only_azure = MIRRORS.split("\n", 1)[0] + "\n"
    rc, out, calls, after = run(script, bindir, {"fetch": [124, 0]}, mirrors=only_azure)
    cases += 1
    check(rc == 0 and [p for p, _, _ in phases(calls)].count("fetch") == 2,
          "only azure listed: the fetch is still retried, and passes")
    check(after == only_azure, "only azure listed: the list is kept, never emptied")

    # No packages.
    rc, out, calls, after = run(script, bindir, {}, packages=[])
    cases += 1
    check(rc != 0 and not any(c.startswith("apt-get") for c in calls), f"no packages: refused, nothing run (got {rc})")

    print(f"script cases checked: {cases}")
    check(cases >= 8, "the script sweep checked every case")

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

    print(f"failures: {len(failures)}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
