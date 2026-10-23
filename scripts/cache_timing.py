#!/usr/bin/env python3
"""The GHCR cache's timing table (decision log "CI: CEF layer from GHCR"). For each image.yml run:
the `image` job's "Build the image" and "Build the cef-test stage" seconds, the line its
"Build cache (GHCR, read)" step printed (warm, cold, or a timing run's path), and every blob of 1 GB
or more that BuildKit pulled: in which step, in how many seconds, at how many MB/s. Prints one
markdown table row per run. Stdlib and an authenticated `gh`; run with `python3 -I`.

A blob line is BuildKit's progress line `#17 sha256:<digest> 1.86GB / 1.86GB 543.6s done`. Its sizes
are decimal (1 GB = 10^9 bytes, docker/go-units' HumanSize), and the line belongs to the step whose
start and end hold its timestamp.

Usage: cache_timing.py [--repo onryde/seazn.club.compositor] RUN_ID [RUN_ID ...]
       cache_timing.py --self-test     (check.yml)
"""

import argparse
import json
import re
import subprocess
import sys
from datetime import datetime, timedelta, timezone

STEPS = ("Build the image", "Build the cef-test stage")
BLOB = re.compile(r"^(\S+Z) #\d+ sha256:([0-9a-f]{12,64}) ([\d.]+)(kB|MB|GB) / ([\d.]+)(kB|MB|GB) ([\d.]+)s done\s*$")
STATE = re.compile(r"^\S+Z build cache: (\S+) \(([^)]*)\)\s*$")
UNIT = {"kB": 1e3, "MB": 1e6, "GB": 1e9}


def when(ts):
    """A GitHub timestamp (`…Z`, any number of fraction digits) as an aware datetime."""
    m = re.fullmatch(r"(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d+))?Z", ts)
    if not m:
        raise ValueError(f"not a timestamp: {ts!r}")
    frac = (m.group(2) or "0")[:6].ljust(6, "0")
    return datetime.fromisoformat(f"{m.group(1)}.{frac}+00:00")


def parse(steps, log):
    """steps: the image job's [{name, startedAt, completedAt}]; log: its raw log. Returns a dict."""
    spans = {}
    for s in steps:
        if s.get("startedAt") and s.get("completedAt") and not s["completedAt"].startswith("0001"):
            spans[s["name"]] = (when(s["startedAt"]), when(s["completedAt"]))
    secs = {n: round((spans[n][1] - spans[n][0]).total_seconds()) if n in spans else None for n in STEPS}
    state, blobs = None, []
    for raw in log.splitlines():
        line = raw.rstrip("\r").lstrip("\ufeff")
        m = STATE.match(line)
        if m and state is None:
            state = m.group(2)
        m = BLOB.match(line)
        if not m or m.group(3) != m.group(5) or m.group(4) != m.group(6):
            continue
        size = float(m.group(3)) * UNIT[m.group(4)]
        if size < 1e9:
            continue
        t = when(m.group(1))
        step = next((n for n, (a, b) in spans.items() if a <= t <= b + timedelta(seconds=1)), "?")
        pulled = float(m.group(7))
        blobs.append({"step": step, "gb": size / 1e9, "s": pulled, "mbps": size / 1e6 / pulled if pulled else None})
    return {"secs": secs, "state": state, "blobs": blobs}


def row(run, parsed):
    s = parsed["secs"]
    blobs = "; ".join(f"{b['gb']:.2f} GB in {b['step']}: {b['s']:.1f} s, {b['mbps']:.1f} MB/s" for b in parsed["blobs"]) or "none"
    fmt = lambda v: "—" if v is None else f"{v} s"
    return (f"| {run['id']} | {run['event']} | `{run['sha'][:7]}` | {parsed['state'] or '—'} | "
            f"{fmt(s['Build the image'])} | {fmt(s['Build the cef-test stage'])} | {blobs} |")


def gh(args):
    return subprocess.run(["gh", *args], capture_output=True, text=True, check=True).stdout


def self_test():
    """Canned from run 39931987989 (the decision row's baseline), plus a small blob and a stray line."""
    steps = [{"name": "Build the image", "startedAt": "2026-10-23T09:29:26Z", "completedAt": "2026-10-23T09:32:03Z"},
             {"name": "Build the cef-test stage", "startedAt": "2026-10-23T09:32:03Z", "completedAt": "2026-10-23T09:42:34Z"},
             {"name": "Logs", "startedAt": "2026-10-23T09:50:42Z", "completedAt": "0001-01-01T00:00:00Z"}]
    log = "\n".join([
        "\ufeff2026-10-23T09:29:28.1000000Z build cache: ghcr.io/onryde/seazn-relay-cache:cef-0123456789abcdef (cold)",
        "2026-10-23T09:32:10.3006034Z #17 sha256:4d6d6f3c30340fe769934cdc6ea17d427a4390ff9d705b493c7aced48b397808 155.51MB / 155.51MB 5.4s done",
        "2026-10-23T09:35:00.0000000Z #17 sha256:540750fd3a7c5aa2675ffcda682098e08d7b2b38148a9afdf5978a66a78a7aff 651.17MB / 1.86GB 122.1s",
        "2026-10-23T09:41:08.5054219Z #17 sha256:540750fd3a7c5aa2675ffcda682098e08d7b2b38148a9afdf5978a66a78a7aff 1.86GB / 1.86GB 543.6s done\r",
        "2026-10-23T09:41:08.5055396Z #17 extracting sha256:540750fd3a7c5aa2675ffcda682098e08d7b2b38148a9afdf5978a66a78a7aff",
    ])
    p = parse(steps, log)
    failures = []

    def check(ok, what):
        print(f"{'ok  ' if ok else 'FAIL'} {what}")
        if not ok:
            failures.append(what)

    check(p["secs"] == {"Build the image": 157, "Build the cef-test stage": 631}, f"step seconds 157 and 631 ({p['secs']})")
    check(p["state"] == "cold", f"the cache line's state is 'cold' ({p['state']})")
    check(len(p["blobs"]) == 1, f"one blob of 1 GB or more, the done line only ({len(p['blobs'])})")
    if p["blobs"]:
        b = p["blobs"][0]
        check(b["step"] == "Build the cef-test stage" and b["s"] == 543.6 and abs(b["mbps"] - 3.42) < 0.01,
              f"in the cef-test step, 543.6 s, 3.42 MB/s ({b})")
    r = row({"id": 39931987989, "event": "pull_request", "sha": "f00dfacecafe"}, p)
    check(r.startswith("| 39931987989 | pull_request | `f00dfac` | cold | 157 s | 631 s | 1.86 GB in Build the cef-test stage: 543.6 s, 3.4 MB/s |"),
          f"the row ({r})")
    try:
        when("not a time")
        check(False, "a bad timestamp is refused")
    except ValueError:
        check(True, "a bad timestamp is refused")
    print(f"timing cases checked: 6, failures: {len(failures)}")
    return 1 if failures else 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default="onryde/seazn.club.compositor")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("runs", nargs="*")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    if not a.runs:
        ap.error("no run ids")
    print("| Run | Event | Head | Cache | Build the image | Build the cef-test stage | Blobs of 1 GB or more |")
    print("|---|---|---|---|---|---|---|")
    for rid in a.runs:
        v = json.loads(gh(["run", "view", rid, "--repo", a.repo, "--json", "databaseId,event,headSha,jobs"]))
        job = next((j for j in v["jobs"] if j["name"] == "image"), None)
        if job is None:
            print(f"| {rid} | {v['event']} | `{v['headSha'][:7]}` | no image job | — | — | — |")
            continue
        log = gh(["api", f"repos/{a.repo}/actions/jobs/{job['databaseId']}/logs"])
        print(row({"id": rid, "event": v["event"], "sha": v["headSha"]}, parse(job["steps"], log)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
