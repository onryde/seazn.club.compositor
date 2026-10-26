#!/usr/bin/env python3
"""census.py <results.jsonl>...: Cloudflare's close, reader by reader (F-032's counts).

For each trial: the close (R0's first error; if R0 got none, the first error at least 10 s after the
kill on a probe started after P2's connect and before that error, and not decoded by it, and the line
says R0 missed it; in a relay run the first error on a build started after the kill), and every reader
still attached when it came: started after P2's connect and before the close, not yet decoded, and still
running (a probe within its 8 s life; a relay build until its link's next build retires it). Each one
ends one of three ways: closed (an error from 1.5 s before the close to 2 s after it), re-attached
(decoded after the close with no error) or silent (neither). With no close at all, a reader attached
through +29.9..+31.2 is closed only by an error in that span. Then the readers started after the close
(after P2's connect if that came later), and why each miss missed: `trial_end` cut it short, it started
inside the spread of the close (before the last attached reader's error), it got an error later in its
life (`error`), or it was silent for its whole 8 s life. Errors count from the kill to the next trial's
start (R0's tag repeats in every trial). Each reader's close error is counted once (in a relay run the
build that times the close is also an attached one). Times are seconds from the kill. Zero
readers counted is a failure."""
import json
import sys

LIFE = 8.0
tot = {"trials": 0, "closes": [], "closed": 0, "reattached": 0, "silent": 0, "after": 0, "after_decoded": 0,
       "miss_trial_end": 0, "miss_spread": 0, "miss_error": 0, "miss_silent": 0, "r0_missed": 0, "probe_trials": 0}
for path in sys.argv[1:]:
    rows = [json.loads(l) for l in open(path) if l.startswith("{")]
    name = path.rsplit("/", 1)[-1].split(".")[0]
    starts = sorted(r["t_ms"] for r in rows if r["kind"] == "trial_start")
    for t in (r for r in rows if r["kind"] == "summary"):
        k = t["kill_ms"]
        stop = min((x for x in starts if x > k), default=k + 120_000)
        no_pub = t["p2_connected_rel_kill_ms"] is None  # a trial with no P2 (round 2's --gaps -1)
        p2 = 0.0 if no_pub else t["p2_connected_rel_kill_ms"] / 1000
        errs = {}
        for r in rows:
            if r["kind"] == "error" and k <= r["t_ms"] < stop:
                errs.setdefault(r["reader"], (r["t_ms"] - k) / 1000)
        probes = "probes" in t
        readers = []
        if probes:
            for p in t["probes"]:
                b = p["built"] / 1000
                f = None if p["first_frame"] is None else p["first_frame"] / 1000
                readers.append({"tag": p["tag"], "built": b, "frame": f, "until": b + LIFE, "ended": p["ended"],
                                "end": None if p["end"] is None else p["end"] / 1000})
            close = errs.get("R0")
            r0_missed = close is None
            if r0_missed:
                cand = [errs[r["tag"]] for r in readers if r["tag"] in errs and errs[r["tag"]] >= 10
                        and p2 <= r["built"] < errs[r["tag"]] and (r["frame"] is None or r["frame"] > errs[r["tag"]])]
                close = min(cand, default=None)
        else:
            r0_missed = False
            gens = sorted(t["generations"], key=lambda g: g["built"])
            for g in gens:
                pol = g["tag"].rsplit(".", 1)[0] if "." in g["tag"] else "base"
                nxt = min((h["built"] for h in gens if h["built"] > g["built"]
                           and (h["tag"].rsplit(".", 1)[0] if "." in h["tag"] else "base") == pol), default=1e12)
                f = None if g["first_frame"] is None else g["first_frame"] / 1000
                readers.append({"tag": g["tag"], "built": g["built"] / 1000, "frame": f, "until": nxt / 1000,
                                "ended": g["ended"], "end": None if g["end"] is None else g["end"] / 1000})
            close = min((errs[r["tag"]] for r in readers if r["built"] > 0 and r["tag"] in errs), default=None)
        close_tag = ("R0" if not r0_missed else next(r["tag"] for r in readers if errs.get(r["tag"]) == close)) if probes and close is not None \
            else next((r["tag"] for r in readers if r["built"] > 0 and errs.get(r["tag"]) == close), None)
        if close is None:
            # No error to time it by: list the builds that were attached through the hold's measured end
            # (+29.9 to +31.2 in the trials that did show it) and what became of them.
            late = [r for r in readers if p2 <= r["built"] < 29.9 and r["until"] > 31.2
                    and (r["frame"] is None or r["frame"] > 29.9)
                    and not (r["ended"] == "trial_end" and r["end"] is not None and r["end"] < 31.2)]
            def fate(r):
                e = errs.get(r["tag"])
                if e is not None and 29.9 <= e <= 31.2:
                    return "closed", f"closed +{e:.2f}"
                if r["frame"] is not None:
                    return "reattached", f"re-attached, frame +{r['frame']:.1f}"
                return "silent", "silent"
            for r in late:
                tot[fate(r)[0]] += 1
            print(f"{name} T{t['trial']}: no close error; attached through +29.9..+31.2: "
                  + (", ".join(f"{r['tag']} {fate(r)[1]}" for r in late) or "none"))
            continue
        tot["trials"] += 1
        tot["r0_missed"] += r0_missed
        tot["probe_trials"] += probes
        att = [r for r in readers if p2 <= r["built"] < close and r["until"] > close
               and (r["frame"] is None or r["frame"] > close)]
        out = {"closed": [], "reattached": [], "silent": []}
        for r in att:
            e = errs.get(r["tag"])
            if e is not None and close - 1.5 <= e <= close + 2:
                out["closed"].append((r["tag"], round(e, 2)))
            elif r["frame"] is not None:
                out["reattached"].append(f"{r['tag']} frame +{r['frame']:.1f}")
            else:
                out["silent"].append(r["tag"])
        by_reader = {close_tag: close}
        by_reader.update(dict(out["closed"]))
        closes = list(by_reader.values())
        tot["closes"] += closes
        spread_end = max(closes)
        for key in ("closed", "reattached", "silent"):
            tot[key] += len(out[key])
        line = (f"{name} T{t['trial']}: P2 " + ("none" if no_pub else f"+{p2:.1f}") + f", close +{close:.2f}"
                + (f" (R0 missed it; the first attached error, {close_tag})" if r0_missed else "") + " (attached errors "
                f"+{min(closes):.2f} to +{spread_end:.2f}); attached {len(att)}: closed {len(out['closed'])}, "
                f"re-attached {out['reattached']}, silent {out['silent']}")
        if probes and not no_pub:
            start = max(close, p2)
            after = [r for r in readers if r["built"] >= start]
            dec = [r for r in after if r["frame"] is not None]
            miss = [r for r in after if r["frame"] is None]
            cut = [r for r in miss if r["ended"] == "trial_end"]
            spread = [r for r in miss if r["ended"] != "trial_end" and r["built"] < spread_end]
            errd = [r for r in miss if r["ended"] != "trial_end" and r["built"] >= spread_end and r["tag"] in errs]
            silent = [r for r in miss if r["ended"] != "trial_end" and r["built"] >= spread_end and r["tag"] not in errs]
            tot["after"] += len(after)
            tot["after_decoded"] += len(dec)
            tot["miss_trial_end"] += len(cut)
            tot["miss_spread"] += len(spread)
            tot["miss_error"] += len(errd)
            tot["miss_silent"] += len(silent)
            line += (f"; after +{start:.1f}: {len(dec)}/{len(after)} decoded, misses: trial_end {len(cut)}, "
                     f"spread {len(spread)}, error {len(errd)} {[(r['tag'], round(errs[r['tag']], 2)) for r in errd]}, "
                     f"silent {len(silent)} {[r['tag'] for r in silent]}")
        print(line)
cl = tot["closes"]
if tot["trials"] == 0 or not cl or tot["closed"] + tot["reattached"] + tot["silent"] == 0:
    raise SystemExit("nothing counted")
print(f"trials {tot['trials']}; close errors seen +{min(cl):.2f} to +{max(cl):.2f} (n={len(cl)}); attached readers: "
      f"closed {tot['closed']}, re-attached {tot['reattached']}, silent {tot['silent']}; after the close "
      f"{tot['after_decoded']}/{tot['after']} decoded, misses: trial_end {tot['miss_trial_end']}, spread "
      f"{tot['miss_spread']}, error {tot['miss_error']}, silent {tot['miss_silent']}; R0 missed the close in "
      f"{tot['r0_missed']} of {tot['probe_trials']} probe trials")
