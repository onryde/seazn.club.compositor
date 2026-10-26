#!/usr/bin/env python3
"""analyze.py <results.jsonl>...: per-trial tables from owed108.py's summary lines (times in s, from the kill).

probes: P2's connect; R0's end (Cloudflare's close of the old publisher's playback sessions); the probes
built between P2's connect and that close, and how many of them decoded; the first probe built after the
close and its first-frame latency; the latency of every probe built after the close.
relay: each generation (built, handshake, first byte, first frame, how it ended, errors), and when the camera
was back on air."""
import json
import statistics
import sys


def s(v):
    return "-" if v is None else f"{v / 1000:.1f}"


for path in sys.argv[1:]:
    rows = [json.loads(l) for l in open(path) if l.startswith("{")]
    start = next(r for r in rows if r["kind"] == "run_start")
    print(f"== {path.rsplit('/', 1)[-1]}: part {start['part']}, {start['trials']} trial(s), gst {start['gst']}")
    kills = [r["t_ms"] for r in rows if r["kind"] == "kill"]
    for t in (r for r in rows if r["kind"] == "summary"):
        k = t["kill_ms"]
        nxt = min((x for x in kills if x > k + 1000), default=float("inf"))
        first_err = {}
        for r in rows:
            if r["kind"] == "error" and k <= r["t_ms"] < nxt:
                first_err.setdefault(r["reader"], r["t_ms"] - k)
        p2c = t["p2_connected_rel_kill_ms"]
        print(f"-- trial {t['trial']}: gap {t['gap_s']} s, P2 spawned +{s(t['p2_spawn_rel_kill_ms'])}, "
              f"P2 connected +{s(p2c)} ({t['p2_spawns']} spawn(s))")
        if start["part"] == "probes":
            r0 = t["r0"]
            close = first_err.get("R0")
            errs = [p for p in t["probes"] if p["tag"] in first_err]
            close_times = sorted(first_err[p["tag"]] for p in errs)
            print(f"   R0: frames {r0['frames']}, ended {r0['ended']} at +{s(r0['end'])}, first error "
                  f"+{s(first_err.get('R0'))} {r0['errors'][:1]}")
            print(f"   bursts (before the kill): {t.get('bursts_decoded', '-')}/{t.get('bursts_checked', '-')} decoded")
            print(f"   probes checked {t['probes_checked']}; erroring probes at: "
                  + ", ".join(f"{p['tag']}(built +{s(p['built'])})@+{s(first_err[p['tag']])}" for p in errs))
            cut = close
            win = [p for p in t["probes"] if p["built"] >= p2c and (cut is None or p["built"] < cut)]
            served = [p for p in win if p["first_frame"] is not None]
            print(f"   built after P2's connect and before the close (+{s(cut)}): {len(win)}, decoded {len(served)}: "
                  + ", ".join(f"{p['tag']} built +{s(p['built'])} frame +{s(p['first_frame'])}" for p in served))
            after = [p for p in t["probes"] if cut is not None and p["built"] >= cut]
            ok = [p for p in after if p["first_frame"] is not None]
            if after:
                lat = [p["first_frame"] - p["built"] for p in ok]
                first = min(after, key=lambda p: p["built"])
                print(f"   built after the close: {len(after)}, decoded {len(ok)}; first built +{s(first['built'])} "
                      f"frame +{s(first['first_frame'])}; latency median {s(statistics.median(lat)) if lat else '-'} "
                      f"(min {s(min(lat)) if lat else '-'}, max {s(max(lat)) if lat else '-'})")
            print(f"   first decoded after P2: +{s(t['first_decoded_after_p2_rel_kill_ms'])}")
        else:
            for g in t["generations"]:
                print(f"   {g['tag']:>5}: built +{s(g['built'])} handshake +{s(g['handshake'])} byte +{s(g['first_byte'])} "
                      f"frame +{s(g['first_frame'])} ended {g['ended']} +{s(g['end'])} first error +{s(first_err.get(g['tag']))} "
                      f"{g['errors'][:1]}")
            on_air = t["on_air"] if isinstance(t["on_air"], dict) else {"base": t["on_air"]}
            back = t["back_on_air_rel_kill_ms"]
            back = back if isinstance(back, dict) else {"base": back}
            rb = t["rebuilds_after_kill"]
            rb = rb if isinstance(rb, dict) else {"base": rb}
            for pol in on_air:
                b = back[pol]
                print(f"   [{pol}] rebuilds after the kill {rb[pol]}; on air: "
                      + ", ".join(f"gen{x['gen']}@+{s(x['t'])}" for x in on_air[pol])
                      + f"; back on air +{s(b)}, {s(b - p2c if b and p2c else None)} s after P2's connect")
