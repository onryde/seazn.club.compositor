#!/usr/bin/env python3
"""sweep.py <results.jsonl>...: F-032's hold, per trial, for the round-2 runs (timeoutSeconds sweep, the
clean-close arm and the RTMPS control). Give the files in the order they ran: a trial's start is timed
from the publisher that ended before it, which may be in the previous file. Times are seconds from the
drop (SIGKILL, or SIGTERM for a clean close) unless named otherwise.

  close     R0's first error after the drop (R0 is the reader warm at the drop). If R0 got none, the
            first error at least 10 s after the drop on a probe attached at that moment, and the line
            says R0 missed it. Then the spread of the errors on the probes attached at it (started after
            P2's connect and before the close, not yet decoded, in their 8 s life), within 1.5 s before
            to 2 s after it: census.py's rule.
  window    probes started from P2's connect to the close: decoded / started.
  after     the first probe started at or after the close: start -> first frame.
  start     how the trial began: P1's connect after the previous publisher's end (and how it ended),
            and what the HLS master served at the trial's first poll. Polling starts with the trial, so
            that poll can come before P1's connect (2.8 s before it in the RTMPS control). `fresh`: no
            master (the previous broadcast was over). `held`: the previous broadcast's master, still
            200; then when it stopped (seconds from P1's connect) and when a new variant appeared.
  hls       the HLS head (media sequence + segments) 4 s after the drop, once the segment in flight at
            the drop is out; when it first moved past that (a new publisher's media in HLS); whether
            every variant served after the drop was the one served before it; the first ENDLIST; the
            first master status other than 200.
A trial whose R0 was not warm at the drop is invalid: printed, not counted. Errors count only inside the
trial (the drop to the trial's end): R0's tag repeats in every trial. Zero trials counted is a failure."""
import json
import sys

LIFE = 8.0
n = 0
groups = {"fresh": [], "held": [], "no hls": []}
held_new = []
p2_same = []
last_end = None
for path in sys.argv[1:]:
    rows = [json.loads(line) for line in open(path) if line.startswith("{")]
    if not rows:
        raise SystemExit(f"{path}: no lines")
    name = path.rsplit("/", 1)[-1].split(".")[0]
    start = next(r for r in rows if r["kind"] == "run_start")
    afters = {r["trial"]: r["after_s"] for r in rows if r["kind"] == "trial_start" and "after_s" in r}
    tstarts = {r["trial"]: r["t_ms"] for r in rows if r["kind"] == "trial_start"}
    kills = {r["trial"]: r for r in rows if r["kind"] == "kill"}
    ends = [r for r in rows if r["kind"] == "pub_killed"]
    conns = [r for r in rows if r["kind"] == "pub_connected" and r["pub"] == "P1"]
    for t in (r for r in rows if r["kind"] == "summary"):
        k = t["kill_ms"]
        trial = t["trial"]
        label = f"{name} T{trial}"
        p1c = next((r for r in conns if tstarts.get(trial, 0) <= r["t_ms"] < k), None)
        if p1c is None:
            raise SystemExit(f"{label}: no P1 connect before the drop")
        prev = last_end if last_end is not None and last_end["wall"] < p1c["wall"] else None
        last_end = max((r for r in ends if r["t_ms"] < (tstarts.get(trial + 1) or float("inf"))),
                       key=lambda r: r["wall"], default=last_end)
        if not kills[trial].get("warm_ok"):
            print(f"{label}: invalid, R0 not warm at the drop; not counted")
            continue
        n += 1
        p2 = t["p2_connected_rel_kill_ms"]
        p2 = None if p2 is None else p2 / 1000
        after_s = afters.get(trial, start["after_s"] if not isinstance(start["after_s"], list) else start["after_s"][-1])
        errs = {}
        for r in rows:
            if r["kind"] == "error" and k <= r["t_ms"] <= k + after_s * 1000:
                errs.setdefault(r["reader"], (r["t_ms"] - k) / 1000)
        probes = [{"tag": p["tag"], "b": p["built"] / 1000,
                   "f": None if p["first_frame"] is None else p["first_frame"] / 1000, "ended": p["ended"]}
                  for p in t["probes"]]
        lo = p2 if p2 is not None else 0.0
        close = errs.get("R0")
        r0_missed = close is None
        if r0_missed:
            close = min((errs[p["tag"]] for p in probes if p["tag"] in errs and errs[p["tag"]] >= 10
                         and lo <= p["b"] < errs[p["tag"]] < p["b"] + LIFE
                         and (p["f"] is None or p["f"] > errs[p["tag"]])), default=None)
        line = f"{label} ({start.get('note', '')}; drop {start.get('drop', 'kill')}): P2 " + \
            ("none" if p2 is None else f"+{p2:.1f}")
        if close is None:
            line += "; no close error on R0 or an attached probe"
        else:
            att = [p for p in probes if lo <= p["b"] < close and p["b"] + LIFE > close
                   and (p["f"] is None or p["f"] > close)]
            spread = [errs[p["tag"]] for p in att if p["tag"] in errs and close - 1.5 <= errs[p["tag"]] <= close + 2]
            es = ([] if r0_missed else [close]) + spread
            win = [p for p in probes if p2 is not None and p2 <= p["b"] < close]
            after = next((p for p in probes if p["b"] >= close), None)
            line += (f"; close +{close:.2f}" + (" (R0 missed it)" if r0_missed else "")
                     + f" (attached {len(att)}, closed {len(spread)}; errors +{min(es):.2f} to "
                     f"+{max(es):.2f}); window {len([p for p in win if p['f'] is not None and p['f'] <= close])}"
                     f"/{len(win)} decoded")
            if after is not None:
                line += f"; after: +{after['b']:.1f} -> " + ("none" if after["f"] is None else f"+{after['f']:.1f}")
        rel = lambda r: (r["t_ms"] - p1c["t_ms"]) / 1000
        line += "; start: P1 " + (f"{p1c['wall'] - prev['wall']:.1f} s after the previous publisher's end "
                                  f"({prev.get('how')})" if prev else "first known publisher")
        hls = [r for r in rows if r["kind"] == "hls" and r["trial"] == trial]
        pre = [r for r in hls if r["t_ms"] < k]
        if not pre:
            kind = "no hls"
        elif pre[0]["master"] != 200:
            kind = "fresh"
            first = next((r for r in pre if r["master"] == 200), None)
            line += (f", fresh (master {pre[0]['master']} at {rel(pre[0]):+.1f}"
                     + (f", first 200 at {rel(first):+.1f})" if first else ", no 200 before the drop)"))
        else:
            kind = "held"
            old = pre[0]["variant_id"]
            last_old = [r for r in pre if r["master"] == 200 and r["variant_id"] == old][-1]
            stop = next((r for r in pre if r["t_ms"] > last_old["t_ms"] and r["master"] != 200), None)
            new = next((r for r in pre if r["master"] == 200 and r["variant_id"] != old), None)
            line += (f", held (previous variant 200 to {rel(last_old):+.1f}"
                     + (f", master {stop['master']} from {rel(stop):+.1f}" if stop else ", never stopped")
                     + (f", new variant at {rel(new):+.1f})" if new else ", no new variant before the drop)"))
            held_new.append((label, stop is not None and new is not None, rel(last_old), rel(stop) if stop else None))
        groups[kind].append((label, close, r0_missed, start.get("ingest"), start.get("note", "")))
        if hls:
            before = [r for r in hls if r["t_ms"] <= k + 4000 and isinstance(r.get("head"), int)]
            head0 = before[-1]["head"] if before else None
            post = [r for r in hls if k < r["t_ms"] <= k + after_s * 1000]
            moved = next(((r["t_ms"] - k) / 1000 for r in post if r["t_ms"] > k + 4000 and head0 is not None
                          and isinstance(r.get("head"), int) and r["head"] > head0), None)
            ok = [r for r in pre if r["master"] == 200]
            vid = ok[-1]["variant_id"] if ok else None
            ids = sorted({r["variant_id"] for r in post if r["master"] == 200})
            same = ids == [vid] if vid is not None else None
            if p2 is not None and moved is not None:
                p2_same.append((label, same))
            endl = next(((r["t_ms"] - k) / 1000 for r in post if r.get("endlist")), None)
            not200 = next((r for r in post if r["master"] != 200), None)
            last = post[-1]
            line += (f"; hls: head at +4 {head0}, moved +{moved:.1f}" if moved is not None else
                     f"; hls: head at +4 {head0}, never moved") + \
                (", same variant after the drop" if same else f", variants after the drop {len(ids)} (before: {vid})") + \
                (f", ENDLIST +{endl:.1f}" if endl is not None else ", no ENDLIST") + \
                (f", master {not200['master']} at +{(not200['t_ms'] - k) / 1000:.1f}" if not200 else ", master 200 throughout") + \
                f" (last poll +{(last['t_ms'] - k) / 1000:.1f}: {last['master']})"
        print(line)
if n == 0:
    raise SystemExit("nothing counted")
print(f"{n} trials")
for kind, ts in groups.items():
    if ts:
        print(f"{kind} start ({len(ts)}): " + "; ".join(
            f"{lab} {ing} " + ("no close" if c is None else f"+{c:.2f}" + (" (R0 missed)" if m else ""))
            for lab, c, m, ing, _ in ts))
if held_new:
    print(f"held starts that got a new HLS stream before the drop: {sum(ok for _, ok, _, _ in held_new)} of "
          f"{len(held_new)}; previous master last 200 at +{min(a for _, _, a, _ in held_new):.1f} to "
          f"+{max(a for _, _, a, _ in held_new):.1f}, stopped at +"
          f"{min(b for _, _, _, b in held_new if b is not None):.1f} to +{max(b for _, _, _, b in held_new if b is not None):.1f} "
          f"after P1's connect")
if p2_same:
    print(f"trials with P2 in HLS on the variant served before the drop: {sum(bool(s) for _, s in p2_same)} of {len(p2_same)}")
