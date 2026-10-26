#!/usr/bin/env python3
"""sim.py <probes results.jsonl>...: replay input-link policies against MEASURED Cloudflare serve maps.

Each probes trial built one fresh SRT reader a second (8 s life) through the outage. That is a map: "a
connect started in second s decodes after lat s, or never". A policy's connect at time t is looked up in
the probe built nearest to t (probes start on whole seconds), and it is on air at t + lat if that probe
decoded within the policy's connect timeout. Only the policy is modelled; Cloudflare's side is the trial's.

Times are seconds from the publisher kill. The stall is declared at +7.0 (measured 6.9 to 7.2); a stall
waits 1 s, and the k-th connect failure in a row waits wait(k).
  base         today: connect timeout 5 s, wait 2, 4, 8, 10, 10... (Thresholds::default)
  cap2         base with the back-off capped at 2 s
  live1        while the publisher reports live: connect timeout 4 s, wait 1 s, no growth. Run from the
               stall, so it is the gated policy only where the publisher is back before the stall (C17,
               and every 5.5 s trial)
  close        base, plus: the error Cloudflare's close puts on the link's build (the running one or the
               abandoned one still attached) rebuilds CLOSE_DELAY s later, the attempt count reset
  nudge        base, plus: the publisher reporting live (P2's connect + NUDGE_DELAY) rebuilds at once,
               the attempt count reset
  nudge_close  nudge and close together
  live1_close  live1 and close together
Phase: each policy also runs with its schedule shifted by 0.25, 0.5 and 0.75 s (the stall moves with the
last frame)."""
import json
import statistics
import sys

STALL = 7.0
CLOSE_DELAY = 1.0  # R0 saw the close up to 0.9 s before the other readers did (gaps run, trial 2)
NUDGE_DELAY = 0.5  # the phone's beat reaching web and web reaching the relay


def base_wait(k):
    return min(2 ** k, 10)


POLICIES = {
    "base": dict(timeout=5, wait=base_wait),
    "cap2": dict(timeout=5, wait=lambda k: min(2 ** k, 2)),
    "live1": dict(timeout=4, wait=lambda k: 1),
    "close": dict(timeout=5, wait=base_wait, close=True),
    "nudge": dict(timeout=5, wait=base_wait, nudge=True),
    "nudge_close": dict(timeout=5, wait=base_wait, close=True, nudge=True),
    "live1_close": dict(timeout=4, wait=lambda k: 1, close=True),
}


def load(path):
    rows = [json.loads(l) for l in open(path) if l.startswith("{")]
    out = []
    for t in (r for r in rows if r["kind"] == "summary"):
        by_s = {}
        for p in t["probes"]:
            lat = None if p["first_frame"] is None else (p["first_frame"] - p["built"]) / 1000
            by_s.setdefault(round(p["built"] / 1000), lat)
        k = t["kill_ms"]
        close = min((r["t_ms"] - k for r in rows if r["kind"] == "error" and r.get("reader") == "R0"
                     and k <= r["t_ms"] < k + 120_000), default=None)
        if close is None:
            raise SystemExit(f"{path} trial {t['trial']}: no close seen on R0")
        out.append({"name": f"{path.rsplit('/', 1)[-1].split('.')[0]} T{t['trial']}", "map": by_s,
                    "close": close / 1000, "p2": t["p2_connected_rel_kill_ms"] / 1000})
    return out


def served(tr, t, timeout):
    sec = round(t)
    if sec not in tr["map"]:
        raise SystemExit(f"{tr['name']}: no probe near +{t:.1f}")
    lat = tr["map"][sec]
    return lat is not None and lat <= timeout, lat


def run(tr, pol, shift):
    p = POLICIES[pol]
    t = STALL + shift + 1.0
    events = []
    if p.get("close"):
        events.append(("close", tr["close"], tr["close"] + CLOSE_DELAY))
    if p.get("nudge"):
        events.append(("nudge", tr["p2"] + NUDGE_DELAY, tr["p2"] + NUDGE_DELAY))
    nudge_first = [e for e in events if e[0] == "nudge" and e[2] < t]
    if nudge_first:  # the publisher is back before the stall: the nudge is the first rebuild
        t = nudge_first[0][2]
        events.remove(nudge_first[0])
    k, attempts = 0, 0
    while True:
        attempts += 1
        ok, lat = served(tr, t, p["timeout"])
        if ok:
            return t + lat, attempts
        k += 1
        nxt = t + p["timeout"] + p["wait"](k)
        due = [e for e in events if t <= e[1] < nxt]
        if due:
            e = min(due, key=lambda e: e[2])
            events.remove(e)
            nxt, k = max(e[2], t), 0
        t = nxt


trials = [tr for path in sys.argv[1:] for tr in load(path)]
checked = 0
print(f"{len(trials)} trials; P2 connected / Cloudflare's close (R0's error): "
      + ", ".join(f"{tr['name']} +{tr['p2']:.1f}/+{tr['close']:.1f}" for tr in trials))
for pol in POLICIES:
    res = []
    for tr in trials:
        row = [run(tr, pol, sh) for sh in (0, 0.25, 0.5, 0.75)]
        res.append((tr["name"], row))
        checked += len(row)
    vals = [v for _, row in res for v, _ in row]
    conn = statistics.median(a for _, row in res for _, a in row)
    print(f"{pol:>12}: median +{statistics.median(vals):.1f}, mean +{statistics.mean(vals):.1f}, "
          f"min +{min(vals):.1f}, max +{max(vals):.1f}; connects median {conn}")
    for name, row in res:
        print(f"{'':>14}{name}: " + ", ".join(f"+{v:.1f}({a})" for v, a in row))
if checked == 0:
    raise SystemExit("nothing simulated")
print(f"simulated {checked} schedules over {len(trials)} trials")
