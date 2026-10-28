#!/usr/bin/env python3
"""A cost leg's files, made up, for the cost verdict's tests (test-cost.sh, test-workflow.sh).

    python3 -I harness/soak/cost/fixture.py <dir> [mutation...]

One green leg on performance-4x (the size rule's max: 3.0 cores), a 600 s drive from S: a sample a
second (585 in the window from +15 s), 565 of them at 1.0 core and 20 at 1.9 (the p95, by nearest
rank, is the 556th value: 1.0); beats every 5 s, all full and graphics ready (a beat every 5 s seldom
sees the reload, so no beat says it happened); no drop; one kill 2 s after the midpoint, and the
compositor's own log of it (flylog.py's shape, the compositor's `ts=`): `cef crashed gen=1` 0.4 s
after the kill, `graphics restart gen=2`, `cef ready gen=2` 4 s after it; a boundary every 6 s,
each one moment (100 in 10 minutes); eight stills of eight asked. cpu6-at-max and cpu6-over are the
same leg on 6 CPUs (performance-6x), its 40 hottest samples at 4.5 and 4.6 cores.

Each mutation changes one thing:
  cpu-over, cpu-at-max, cpu6-over, cpu6-at-max, few-samples, ncpu   the samples
  stepped, drops, no-rung-field, no-dropped-field         the beats
  no-start, two-starts, no-end, short-drive               the actions
  no-kill, no-renderer, two-kills, kill-late, kill-early-80ms, kill-rc, kill-no-rc   leg.sh's kill lines
  no-log, no-crash, two-crashes, crash-before-kill, no-ready, ready-same-gen   compositor.log
  few-moments, unread                                     the deliveries
  no-stills                                               the stills
"""
import datetime, io, json, os, sys, tarfile

d, muts = sys.argv[1], set(sys.argv[2:])
os.makedirs(d, exist_ok=True)
S, DRIVE = 1_000_000_000, 600
E = S + DRIVE * 1000
K = S + DRIVE * 500 + (110_000 if "kill-late" in muts else -80 if "kill-early-80ms" in muts else 2_000)


def w(name, rows):
    with open(os.path.join(d, name), "w") as f:
        for r in rows:
            f.write((r if isinstance(r, str) else json.dumps(r)) + "\n")


six = "cpu6-over" in muts or "cpu6-at-max" in muts
hot = 40 if "cpu-over" in muts or "cpu-at-max" in muts or six else 20
hot_at = {"cpu-over": 3.1, "cpu-at-max": 3.0, "cpu6-over": 4.6, "cpu6-at-max": 4.5}
hot_v = next((v for m, v in hot_at.items() if m in muts), 1.9)
ncpu = 8 if "ncpu" in muts else 6 if six else 4
samples = []
for i, t in enumerate(range(S + 16_000, E + 1, 1000)):
    if "few-samples" in muts and i % 5 == 0:
        continue
    v = hot_v if i >= 585 - hot else 1.0
    samples.append({"t_ms": t, "dt_ms": 1000, "ncpu": ncpu, "machine": v, "tree": v - 0.1,
                    "cats": {"compositor": 0.7, "renderer": 0.05, "gpu": 0.04, "utility": 0.0, "zygote": 0.0,
                             "cef_other": 0.0, "xvfb": 0.01, "self": 0.01, "other": 0.02},
                    "renderers": [{"pid": 41 if t < K else 43, "client": 7 if t < K else 8, "cores": 0.05}],
                    "top_thread": {"name": "x264enc:src", "cores": 0.4}, "mem_used_mb": 700, "procs": 30})
# Samples before the window, a duplicate and flyctl's own line are never counted.
rows = [{"t_ms": S + 5000, "ncpu": 4, "machine": 3.9, "cats": {}}] + samples + [samples[0], "Connecting to fdaa::3... complete", '{"t_ms": 1, "machi']
w("cost-samples.jsonl", rows)

beats = []
for t in range(S - 30_000, E + 5_001, 5000):
    b = {"state": "playing", "rung": "full", "droppedFrames": 0, "encoderSpeed": 1.0, "fps": 30.0, "graphics": "ready", "cpuPct": 50.0}
    if "stepped" in muts and K + 15_000 < t <= K + 20_000:
        b["rung"] = "overlay_half_rate"
    if "drops" in muts and t >= K + 5000:
        b["droppedFrames"] = 3
    if "no-rung-field" in muts:
        del b["rung"]
    if "no-dropped-field" in muts:
        del b["droppedFrames"]
    beats.append({"received_ms": t, "body": b})
final = {"state": "stopped", "exitReason": "ended", "droppedFrames": 3 if "drops" in muts else 0}
if "no-dropped-field" in muts:
    del final["droppedFrames"]
beats.append({"received_ms": E + 8000, "body": final})
w("heartbeats.jsonl", beats)

actions = [{"kind": "start", "at_ms": S, "drive_s": 590 if "short-drive" in muts else DRIVE}, {"kind": "end", "at_ms": E}]
if "two-starts" in muts:
    actions.insert(1, {"kind": "start", "at_ms": S + 1, "drive_s": DRIVE})
if "no-start" in muts:
    actions = actions[1:]
if "no-end" in muts:
    actions = actions[:1]
w("actions.jsonl", actions)

P = S + DRIVE * 500
kills = [] if "no-kill" in muts else [
    {"at_ms": K, "killed": [] if "no-renderer" in muts else [41], "clients": [7], "renderers": [{"pid": 41, "client": 7}], "planned_ms": P, "try": 1}]
if "no-kill" not in muts and "kill-no-rc" not in muts:
    kills.append({"rc_at_ms": K + 5, "rc": 1 if "kill-rc" in muts else 0, "planned_ms": P, "try": 1})
if "two-kills" in muts:
    kills += [{"at_ms": K + 30_000, "killed": [42], "clients": [7], "planned_ms": P, "try": 1}]
w("cost-kill.jsonl", kills)


def logline(ms, msg):
    t = datetime.datetime.fromtimestamp(ms / 1000, tz=datetime.timezone.utc)
    ts = t.strftime("%Y-%m-%dT%H:%M:%S.") + "%03dZ" % (ms % 1000)
    # The platform's stamp is its receive time, a little after the compositor's own ts=.
    plat = datetime.datetime.fromtimestamp((ms + 37) / 1000, tz=datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f") + "Z"
    return f"{plat} ts={ts} level=info msg={msg}"


if "no-log" not in muts:
    log = [(S - 25_000, "graphics restart gen=1"), (S - 21_000, "cef ready gen=1"), (S - 10_000, "cef alive gen=1"),
           (K - 1000, "overlay hidden reason=hang gen=1 (not a crash line)")]
    crash_at = K - 60_000 if "crash-before-kill" in muts else K + 400
    if "no-crash" not in muts:
        log.append((crash_at, "cef crashed gen=1"))
    log.append((crash_at + 1000, "graphics restart gen=2"))
    if "no-ready" not in muts:
        log.append((crash_at + 3600, "cef ready gen=1" if "ready-same-gen" in muts else "cef ready gen=2"))
    log.append((crash_at + 9000, "cef alive gen=2"))
    if "two-crashes" in muts:
        log += [(K + 30_000, "cef crashed gen=2"), (K + 31_000, "graphics restart gen=3"), (K + 34_000, "cef ready gen=3")]
    log.sort()
    w("compositor.log", [logline(ms, m) for ms, m in log] + ["a bare line with no stamp"])

every = 70_000 if "few-moments" in muts else 6000
dels = []
for t in range(S + 1000, E, every):
    m = None if "unread" in muts else ["four"]
    dels.append({"at_ms": t, "ok": True, "status": 201, "seq": len(dels) + 2, "moments": m})
dels.append({"at_ms": S + 2000, "ok": False, "status": 409, "code": "SEQ_CONFLICT"})
w("deliveries.jsonl", dels)

instants = [(f"moment{i}", S + DRIVE * 250 + i * 1000) for i in range(6)] + [("reload", P + 45_000), ("end", E - 30_000)]
written = [] if "no-stills" in muts else instants
w("stills.log", [f"stills: {lb} at {ms}: {lb}-{ms}.jpg" if (lb, ms) in written else f"stills: {lb} at {ms}: missed (no segment)"
                 for lb, ms in instants] + [f"stills: {len(written)} written"])
with tarfile.open(os.path.join(d, "stills.tar"), "w") as tar:
    info = tarfile.TarInfo("stills")
    info.type = tarfile.DIRTYPE
    tar.addfile(info)
    for lb, ms in written:
        body = b"\xff\xd8\xff\xe0 not a real jpeg"
        info = tarfile.TarInfo(f"stills/{lb}-{ms}.jpg")
        info.size = len(body)
        tar.addfile(info, io.BytesIO(body))
w("done", [{"ok": True, "checker_exit": 0, "sampler_exit": 0, "went_live": True, "ended_early": None, "leaks": 0,
            "leg": "A", "stills_exit": 1 if "no-stills" in muts else 0}])
