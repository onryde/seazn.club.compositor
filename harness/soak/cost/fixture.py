#!/usr/bin/env python3
"""A cost leg's files, made up, for the cost verdict's tests (test-cost.sh, test-workflow.sh).

    python3 -I harness/soak/cost/fixture.py <dir> [mutation...]

One green leg on performance-4x (the size rule's max: 2.0 cores), a 600 s drive from S: a sample a
second (585 in the window from +15 s), 565 of them at 1.0 core and 20 at 1.9 (the p95, by nearest
rank, is the 556th value: 1.0); beats every 5 s, all full, no drop, graphics restarting for 10 s after
the kill and ready again; one kill 2 s after the midpoint; a boundary every 6 s, each one moment (100
in 10 minutes). Each mutation changes one thing: cpu-over, cpu-at-max, few-samples, ncpu, stepped,
drops, never-ready, no-kill, no-renderer, two-kills, kill-late, few-moments, unread.
"""
import json, os, sys
d, muts = sys.argv[1], set(sys.argv[2:])
os.makedirs(d, exist_ok=True)
S, DRIVE = 1_000_000_000, 600
E = S + DRIVE * 1000
K = S + DRIVE * 500 + (70_000 if "kill-late" in muts else 2_000)
def w(name, rows):
    with open(os.path.join(d, name), "w") as f:
        for r in rows:
            f.write((r if isinstance(r, str) else json.dumps(r)) + "\n")
hot = 40 if "cpu-over" in muts or "cpu-at-max" in muts else 20
hot_v = 2.1 if "cpu-over" in muts else (2.0 if "cpu-at-max" in muts else 1.9)
samples = []
for i, t in enumerate(range(S + 16_000, E + 1, 1000)):
    if "few-samples" in muts and i % 5 == 0:
        continue
    v = hot_v if i >= 585 - hot else 1.0
    samples.append({"t_ms": t, "dt_ms": 1000, "ncpu": 8 if "ncpu" in muts else 4, "machine": v, "tree": v - 0.1,
                    "cats": {"compositor": 0.7, "renderer": 0.05, "gpu": 0.04, "utility": 0.0, "zygote": 0.0,
                             "cef_other": 0.0, "xvfb": 0.01, "self": 0.01, "other": 0.02},
                    "renderers": [{"pid": 41, "client": 7, "cores": 0.05}],
                    "top_thread": {"name": "x264enc:src", "cores": 0.4}, "mem_used_mb": 700, "procs": 30})
# Samples before the window, a duplicate and flyctl's own line are never counted.
rows = [{"t_ms": S + 5000, "ncpu": 4, "machine": 3.9, "cats": {}}] + samples + [samples[0], "Connecting to fdaa::3... complete", '{"t_ms": 1, "machi']
w("cost-samples.jsonl", rows)
beats = []
for t in range(S - 30_000, E + 5_001, 5000):
    g = "restarting" if K < t <= K + 10_000 else "ready"
    if "never-ready" in muts and t > K:
        g = "restarting"
    b = {"state": "playing", "rung": "full", "droppedFrames": 0, "encoderSpeed": 1.0, "fps": 30.0, "graphics": g, "cpuPct": 50.0}
    if "stepped" in muts and K + 15_000 < t <= K + 20_000:
        b["rung"] = "overlay_half_rate"
    if "drops" in muts and t >= K + 5000:
        b["droppedFrames"] = 3
    beats.append({"received_ms": t, "body": b})
beats.append({"received_ms": E + 8000, "body": {"state": "stopped", "exitReason": "ended", "droppedFrames": 3 if "drops" in muts else 0}})
w("heartbeats.jsonl", beats)
w("actions.jsonl", [{"kind": "start", "at_ms": S, "drive_s": DRIVE}, {"kind": "end", "at_ms": E}])
kills = [] if "no-kill" in muts else [
    {"at_ms": K, "killed": [] if "no-renderer" in muts else [41], "clients": [7], "planned_ms": S + DRIVE * 500, "try": 1},
    {"rc_at_ms": K + 5, "rc": 0, "planned_ms": S + DRIVE * 500, "try": 1}]
if "two-kills" in muts:
    kills += [{"at_ms": K + 30_000, "killed": [42], "clients": [7], "planned_ms": S + DRIVE * 500, "try": 1}]
w("cost-kill.jsonl", kills)
every = 70_000 if "few-moments" in muts else 6000
dels = []
for t in range(S + 1000, E, every):
    m = None if "unread" in muts else ["four"]
    dels.append({"at_ms": t, "ok": True, "status": 201, "seq": len(dels) + 2, "moments": m})
dels.append({"at_ms": S + 2000, "ok": False, "status": 409, "code": "SEQ_CONFLICT"})
w("deliveries.jsonl", dels)
w("done", [{"ok": True, "checker_exit": 0, "sampler_exit": 0, "went_live": True, "ended_early": None, "leaks": 0,
            "leg": "A", "stills_exit": 0}])
