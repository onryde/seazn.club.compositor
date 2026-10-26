#!/usr/bin/env python3
"""sweep.py <results.jsonl>...: F-032's hold, per trial, for the round-2 runs (timeoutSeconds sweep, the
clean-close arm and the RTMPS control). Times are seconds from the drop (SIGKILL, or SIGTERM for a clean
close).

  close     R0's first error after the drop (R0 is the reader warm at the drop), and the spread of the
            errors on the probes attached at it (started after P2's connect and before R0's error, not yet
            decoded, in their 8 s life), within 1.5 s before to 2 s after it: census.py's rule.
  window    probes started from P2's connect to the close: decoded / started.
  after     the first probe started at or after the close: start -> first frame.
  hls       the HLS head (media sequence + segments) 4 s after the drop, once the segment in flight at
            the drop is out; when it first moved past that (a new publisher's media in HLS); the first
            ENDLIST; the first master status other than 200.
Errors count only inside the trial (the drop to the trial's end): R0's tag repeats in every trial.
Zero trials counted is a failure."""
import json
import sys

LIFE = 8.0
n = 0
for path in sys.argv[1:]:
    rows = [json.loads(line) for line in open(path) if line.startswith("{")]
    if not rows:
        raise SystemExit(f"{path}: no lines")
    name = path.rsplit("/", 1)[-1].split(".")[0]
    start = next(r for r in rows if r["kind"] == "run_start")
    afters = {r["trial"]: r["after_s"] for r in rows if r["kind"] == "trial_start" and "after_s" in r}
    for t in (r for r in rows if r["kind"] == "summary"):
        n += 1
        k = t["kill_ms"]
        trial = t["trial"]
        p2 = t["p2_connected_rel_kill_ms"]
        p2 = None if p2 is None else p2 / 1000
        after_s = afters.get(trial, start["after_s"] if not isinstance(start["after_s"], list) else start["after_s"][-1])
        errs = {}
        for r in rows:
            if r["kind"] == "error" and k <= r["t_ms"] <= k + after_s * 1000:
                errs.setdefault(r["reader"], (r["t_ms"] - k) / 1000)
        close = errs.get("R0")
        probes = [{"tag": p["tag"], "b": p["built"] / 1000,
                   "f": None if p["first_frame"] is None else p["first_frame"] / 1000, "ended": p["ended"]}
                  for p in t["probes"]]
        line = f"{name} T{trial} ({start.get('note', '')}; drop {start.get('drop', 'kill')}): P2 " + \
            ("none" if p2 is None else f"+{p2:.1f}")
        if close is None:
            line += "; R0: no error"
        else:
            lo = p2 if p2 is not None else 0.0
            att = [p for p in probes if lo <= p["b"] < close and p["b"] + LIFE > close
                   and (p["f"] is None or p["f"] > close)]
            spread = [errs[p["tag"]] for p in att if p["tag"] in errs and close - 1.5 <= errs[p["tag"]] <= close + 2]
            es = [close] + spread
            win = [p for p in probes if p2 is not None and p2 <= p["b"] < close]
            after = next((p for p in probes if p["b"] >= close), None)
            line += (f"; close +{close:.2f} (attached {len(att)}, closed {len(spread)}; errors +{min(es):.2f} to "
                     f"+{max(es):.2f}); window {len([p for p in win if p['f'] is not None and p['f'] <= close])}"
                     f"/{len(win)} decoded")
            if after is not None:
                line += f"; after: +{after['b']:.1f} -> " + ("none" if after["f"] is None else f"+{after['f']:.1f}")
        hls = [r for r in rows if r["kind"] == "hls" and r["trial"] == trial]
        if hls:
            before = [r for r in hls if r["t_ms"] <= k + 4000 and isinstance(r.get("head"), int)]
            head0 = before[-1]["head"] if before else None
            post = [r for r in hls if r["t_ms"] > k]
            moved = next(((r["t_ms"] - k) / 1000 for r in post if r["t_ms"] > k + 4000 and head0 is not None
                          and isinstance(r.get("head"), int) and r["head"] > head0), None)
            endl = next(((r["t_ms"] - k) / 1000 for r in post if r.get("endlist")), None)
            not200 = next((r for r in post if r["master"] != 200), None)
            last = post[-1]
            line += (f"; hls: head at +4 {head0}, moved +{moved:.1f}" if moved is not None else
                     f"; hls: head at +4 {head0}, never moved") + \
                (f", ENDLIST +{endl:.1f}" if endl is not None else ", no ENDLIST") + \
                (f", master {not200['master']} at +{(not200['t_ms'] - k) / 1000:.1f}" if not200 else ", master 200 throughout") + \
                f" (last poll +{(last['t_ms'] - k) / 1000:.1f}: {last['master']})"
        print(line)
if n == 0:
    raise SystemExit("nothing counted")
print(f"{n} trials")
