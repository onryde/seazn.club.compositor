#!/usr/bin/env python3
"""cfjoin.py <results dir>: pairs each Cloudflare status change on the spike's Input A
(results/cloudflare-status.jsonl) with the run's own publisher events: a `client_disconnect` with the
SIGKILL before it, a `connected` with the publisher start before it. Prints each gap, and the change's
time from the trial's kill. Zero pairs is a failure."""
import datetime
import glob
import json
import sys

R = sys.argv[1]


def ts(s):
    return datetime.datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


events = []  # (wall, kind, pub, file, kill wall)
for path in sorted(glob.glob(f"{R}/*.jsonl")):
    if path.endswith("cloudflare-status.jsonl"):
        continue
    kill = None
    for line in open(path):
        if not line.startswith("{"):
            continue
        r = json.loads(line)
        if r["kind"] == "kill":
            kill = r["wall"]
        if r["kind"] in ("pub_killed", "pub_start"):
            events.append((r["wall"], r["kind"], r["pub"], path.rsplit("/", 1)[-1], kill))
events.sort()
pairs = 0
for line in open(f"{R}/cloudflare-status.jsonl"):
    s = json.loads(line)
    if not s["input"].startswith("spike") or not s["entered"].startswith("2026-10-26"):
        continue
    at = ts(s["entered"])
    want = "pub_killed" if s["state"] == "disconnected" else "pub_start"
    before = [e for e in events if e[1] == want and e[0] <= at and at - e[0] < 60]
    if not before:
        print(f"{s['entered']} {s['state']} ({s['ingestProtocol']}): no {want} within 60 s before it")
        continue
    e = before[-1]
    rel = f", kill +{at - e[4]:.2f}" if e[4] is not None and at - e[4] < 120 else ""
    print(f"{s['entered']} {s['state']} ({s['ingestProtocol']}): {e[3]} {e[2]} {want} +{at - e[0]:.2f} s{rel}")
    pairs += 1
if pairs == 0:
    raise SystemExit("nothing paired")
print(f"{pairs} pairs")
