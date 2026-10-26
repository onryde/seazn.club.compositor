#!/usr/bin/env python3
"""sim.py <probes results.jsonl>...: replay input-link policies against MEASURED Cloudflare serve maps.

Each probes trial built one fresh SRT reader a second (8 s life) through the outage. That is a map: "a
connect started in second s decodes after lat s, gets an error at e, or neither". A policy's connect at
time t is looked up in the probe built nearest to t (probes start on whole seconds): it is on air at
t + lat if that probe decoded within the policy's connect timeout, and it sees that probe's error, if
any, while it is still running (until the link's next build retires it, as the adapter's abandoned builds
run). Only the policy is modelled; Cloudflare's side is the trial's.

Times are seconds from the publisher kill. The stall is declared at +7.0 (measured 6.9 to 7.2), so the
last frame is at +2.0; a stall waits 1 s, and the k-th connect failure in a row waits wait(k).
  base         today: connect timeout 5 s, wait 2, 4, 8, 10, 10... (Thresholds::default)
  cap2         base with the back-off capped at 2 s
  live1        while the publisher reports live: connect timeout 4 s, wait 1 s, no growth. Run from the
               stall, so it is the gated policy only where the publisher is back before the stall (every
               5.5 s trial, and C17)
  close        base, plus the bounded close rule: the first error on a running build (current or abandoned)
               that completed its handshake and never carried media, at least CLOSE_AFTER_LAST_FRAME s after
               the last frame, rebuilds CLOSE_DELAY s later with the attempt count reset. Once per outage
               (re-armed only by a Running generation, which ends the replay). Every probe handshook, so the
               handshake condition always holds here; production must check it (F-032)
  nudge        base, plus the phone-live nudge as the relay can receive it: on web's reply to the first
               heartbeat after the phone's live edge, PHONE_TO_WEB + 0..HEARTBEAT s after P2's connect
               (the trial's own: the replica's, so +8.5 on the RTMPS maps, where Cloudflare saw +6.5).
               Once per edge. Never tears down an input still Running with a fresh frame (it waits for the
               stall); otherwise it rebuilds at once, a connect in progress included (once per edge, so at
               most one is discarded); the attempt count is reset
  nudge_close  nudge and close together
  live1_close  live1 and close together
Phases: the schedule is shifted by 0, 0.25, 0.5 and 0.75 s (the stall moves with the last frame); the
nudge policies also sweep the heartbeat phase over 0, 1.25, 2.5, 3.75 and 5 s."""
import json
import statistics
import sys

STALL = 7.0
STALL_AFTER = 5.0  # input_stall_after_ms
CLOSE_DELAY = 1.0  # readers saw one close up to 0.9 s apart (gaps run, trial 2)
CLOSE_AFTER_LAST_FRAME = 20.0  # the close came 27.9 to 29.2 s after the last frame; errors at a new
#                                publisher's arrival come about 4 s after it, and none fell between
PHONE_TO_WEB = 0.5  # the phone's live report reaching web: assumed, not measured
HEARTBEAT = 5.0  # heartbeat_interval_ms: web answers the relay only in a beat's reply
SHIFTS = (0, 0.25, 0.5, 0.75)
BEATS = (0, 1.25, 2.5, 3.75, 5.0)


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
        k = t["kill_ms"]
        first_err = {}
        for r in rows:
            if r["kind"] == "error" and k <= r["t_ms"] < k + 120_000:
                first_err.setdefault(r["reader"], (r["t_ms"] - k) / 1000)
        by_s = {}
        for p in t["probes"]:
            b = p["built"] / 1000
            lat = None if p["first_frame"] is None else p["first_frame"] / 1000 - b
            err = first_err.get(p["tag"])
            no_media = p["first_byte"] is None or (err is not None and p["first_byte"] / 1000 > err)
            hs = p["handshake"] is not None
            by_s.setdefault(round(b), {"lat": lat, "err": err if (hs and no_media) else None, "built": b})
        out.append({"name": f"{path.rsplit('/', 1)[-1].split('.')[0]} T{t['trial']}", "map": by_s,
                    "p2": t["p2_connected_rel_kill_ms"] / 1000})
    return out


def probe(tr, t):
    sec = round(t)
    if sec not in tr["map"]:
        raise SystemExit(f"{tr['name']}: no probe near +{t:.1f}")
    return tr["map"][sec]


def run(tr, pol, shift, beat):
    p = POLICIES[pol]
    stall = STALL + shift
    last_frame = stall - STALL_AFTER
    t = stall + 1.0
    nudge_at = tr["p2"] + PHONE_TO_WEB + beat if p.get("nudge") else None
    if nudge_at is not None and nudge_at < t:
        # Still Running and fresh until the stall: the latched nudge acts at the stall, not 1 s after it.
        t = max(nudge_at, stall)
        nudge_at = None
    close_armed = bool(p.get("close"))
    k, attempts = 0, 0
    while True:
        attempts += 1
        pr = probe(tr, t)
        if pr["lat"] is not None and pr["lat"] <= p["timeout"]:
            return t + pr["lat"], attempts
        k += 1
        end = t + p["timeout"]
        nxt = end + p["wait"](k)
        cands = []
        e = pr["err"]
        if close_armed and e is not None and t <= e < nxt and e - last_frame >= CLOSE_AFTER_LAST_FRAME:
            cands.append(("close", e + CLOSE_DELAY))
        if nudge_at is not None and nudge_at < nxt:
            cands.append(("nudge", nudge_at))
        if cands:
            what, at = min(cands, key=lambda c: c[1])
            if what == "close":
                close_armed = False
            else:
                nudge_at = None
            nxt, k = max(at, t), 0
        t = nxt


trials = [tr for path in sys.argv[1:] for tr in load(path)]
checked = 0
print(f"{len(trials)} trials; P2 connected: " + ", ".join(f"{tr['name']} +{tr['p2']:.1f}" for tr in trials))
for pol in POLICIES:
    beats = BEATS if POLICIES[pol].get("nudge") else (0,)
    res = []
    for tr in trials:
        row = [run(tr, pol, sh, bt) for bt in beats for sh in SHIFTS]
        res.append((tr["name"], row))
        checked += len(row)
    vals = [v for _, row in res for v, _ in row]
    conn = statistics.median(a for _, row in res for _, a in row)
    print(f"{pol:>12}: n={len(vals)} median +{statistics.median(vals):.1f}, mean +{statistics.mean(vals):.1f}, "
          f"min +{min(vals):.1f}, max +{max(vals):.1f}; connects median {conn}")
    for name, row in res:
        vs = [v for v, _ in row]
        print(f"{'':>14}{name}: median +{statistics.median(vs):.1f}, min +{min(vs):.1f}, max +{max(vs):.1f}")
if checked == 0:
    raise SystemExit("nothing simulated")
print(f"simulated {checked} schedules over {len(trials)} trials")
