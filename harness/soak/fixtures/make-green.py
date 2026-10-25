#!/usr/bin/env python3
"""Generates harness/soak/fixtures/green/: a 30-minute miniature soak run (C15), synthetic.

Only the report fields verdict.jq reads, under the checker's own names (harness::checker::Report).
With `dropouts`, the same run on the memory experiment's schedule (schedule.jq's dropout_plan):
camera dropouts every 125 s from minute 1, no disconnect, so no output outage. With `none`, its
control: the same drive with nothing in it.

The compositor log (compositor.log, flylog.py's shape) carries a `memory split` line a minute,
from before the drive to after it, with a planted step at every dropout: the compositor's RSS
STEP_RSS_KB and its anon STEP_ANON_KB, CEF's renderer STEP_CEF_KB, the tree STEP_TREE_MB, and the
input generation one higher, after each dropout has ended (file, shmem and threads stay put). Every
kind names how many children were listed and read. The beats' memMb carries the same tree step,
over an alternating 0/2 MB noise whose median over any even run of beats is exactly 1, so the
diagnosis's windowed step reads STEP_TREE_MB exactly.
Run: python3 -I harness/soak/fixtures/make-green.py harness/soak/fixtures/green
     python3 -I harness/soak/fixtures/make-green.py harness/soak/fixtures/green-dropouts dropouts
     python3 -I harness/soak/fixtures/make-green.py harness/soak/fixtures/green-none none
"""
import datetime
import json
import os
import sys

OUT = sys.argv[1]
SCHEDULE = sys.argv[2] if len(sys.argv) > 2 else "soak"
assert SCHEDULE in ("soak", "dropouts", "none"), SCHEDULE
os.makedirs(OUT, exist_ok=True)

S = 1792800000000  # the drive's start (epoch ms)
D = 30 * 60  # the drive, s
HB = 5000  # heartbeat_interval_ms
ALIVE = 2000  # cef_alive_every_ms
PHASE, EVERY, PAUSE, HOLD = 150, 300, 10, 30
DISCONNECTS = [300, 900, 1500]  # schedule.jq's plan for a 30-minute drive
if SCHEDULE in ("dropouts", "none"):
    PHASE, EVERY = 60, 125  # soak.json's dropout_only
    DISCONNECTS = []
STEP_RSS_KB, STEP_ANON_KB, STEP_CEF_KB, STEP_TREE_MB = 9216, 8192, 1024, 10
TREE_MB = 1400  # the beats' memMb before any dropout, less the noise
LINE_FROM_MS, LINE_EVERY_MS = S - 55000, 60000  # the memory split's lines: drive start + 5 s, + k min
CRASH_S = 720  # one CEF crash at minute 12: its hide is printed, never failed

# The dropouts' times, first: the beats' memMb steps when one ends.
DROPOUTS = []
k = 0
# soak_plan's count, or dropout_plan's (each dropout owns a whole period); none has no dropout.
while SCHEDULE != "none" and ((PHASE + k * EVERY + PAUSE <= D) if SCHEDULE == "soak" else (PHASE + (k + 1) * EVERY <= D)):
    at = S + (PHASE + k * EVERY) * 1000
    DROPOUTS.append((k, at, at + 3, at + PAUSE * 1000 + 5))
    k += 1
ENDS = [d[3] for d in DROPOUTS]


def ended(t):
    return sum(1 for e in ENDS if e <= t)


def mem_mb(i, t):
    return TREE_MB + (i % 2) * 2 + ended(t) * STEP_TREE_MB


beats = []
t = S - 60000
i = 0
dropped = 0
while t <= S + D * 1000 + 10000:
    rel = (t - S) / 1000
    if i % 120 == 119:
        dropped += 1
    graphics = "restarting" if CRASH_S <= rel < CRASH_S + 5 else "ready"
    beats.append({
        "received_ms": t,
        "body": {
            "state": "playing", "videoState": "running", "fps": 30.0, "bitrateKbps": 4010,
            "droppedFrames": dropped, "encoderSpeed": 1.0, "cpuPct": 52.0,
            "memMb": mem_mb(i, t), "programmePhase": "live", "input": "connected",
            "output": "connected", "graphics": graphics, "pipelineRebuilds": 0, "sinkSwaps": 0,
            "rung": "full", "scoreStale": False,
        },
    })
    t += HB
    i += 1
beats.append({"received_ms": t + 12000, "body": {"state": "stopped", "exitReason": "ended"}})

outages = [{
    "from_wall_ms": S + d * 1000 + 350, "to_wall_ms": S + (d + HOLD) * 1000 + 2600, "kind": "between",
    "counter_before": None, "counter_after": None, "counter_gap_ms": None, "lost_ms": 1200,
} for d in DISCONNECTS]

alive = [{"at_ms": S - 30000 + k * ALIVE, "gen": 1} for k in range((D * 1000 + 40000) // ALIVE)]
crash_ms = S + CRASH_S * 1000
alive = [a for a in alive if not (crash_ms <= a["at_ms"] < crash_ms + 4000)]
for a in alive:
    if a["at_ms"] >= crash_ms + 4000:
        a["gen"] = 2

report = {
    "frames": D * 30, "duration_ms": D * 1000, "fps_mean": 29.99,
    "pts_gaps": [], "output_outages": outages, "checker_failures": [],
    "heartbeats": {"count": len(beats), "rungs": ["full"], "series": beats},
    "log": {
        "lines": 2400, "boots": [S - 90000], "programme_started": [S - 70000],
        "cef_ready": [{"at_ms": S - 65000, "gen": 1}, {"at_ms": crash_ms + 3500, "gen": 2}],
        "cef_painted": [{"at_ms": S - 65000, "gen": 1}, {"at_ms": crash_ms + 3500, "gen": 2}],
        "blank_reloads": [],
        "cef_crashed": [{"at_ms": crash_ms, "gen": 1}],
        "cef_alive": alive,
        "graphics_restart": [{"at_ms": crash_ms + 1000, "gen": 2}],
        "overlay_hidden": [{"at_ms": crash_ms + 200, "reason": "crash", "gen": 1}],
    },
}
with open(os.path.join(OUT, "report.json"), "w") as f:
    json.dump(report, f, separators=(",", ":"))
    f.write("\n")

actions = [{"kind": "start", "at_ms": S, "drive_s": D, "schedule": SCHEDULE}]
for k, planned, at, end in DROPOUTS:
    actions.append({"kind": "dropout", "slot": k, "planned_ms": planned, "at_ms": at, "end_ms": end,
                    "ok": True, "pause": 200, "resume": 200})
for n, d in enumerate(DISCONNECTS):
    at = S + d * 1000
    actions.append({"kind": "disconnect", "slot": n, "planned_ms": at, "at_ms": at + 4, "end_ms": at + HOLD * 1000 + 6,
                    "ok": True, "dropped_packets": 412, "families": ["ipv4", "ipv6"]})
actions.sort(key=lambda a: a["at_ms"])
actions.append({"kind": "end", "at_ms": S + D * 1000})
with open(os.path.join(OUT, "actions.jsonl"), "w") as f:
    for a in actions:
        f.write(json.dumps(a, separators=(",", ":")) + "\n")

control = {"desiredState": "ending", "beats": len(beats), "beats_refused": 0, "session_fetches": 1,
           "score_down_until_ms": 0, "other_session_requests": 0, "unauthorized_requests": 0,
           "score_refused": 0, "page_hang_armed": False, "page_hang_served_ms": None}
with open(os.path.join(OUT, "control-state.json"), "w") as f:
    json.dump(control, f, separators=(",", ":"))
    f.write("\n")

with open(os.path.join(OUT, "deliveries.jsonl"), "w") as f:
    for n in range(D // 20):
        f.write(json.dumps({"at_ms": S + 5000 + n * 20000, "ok": True, "status": 200, "seq": n + 1},
                           separators=(",", ":")) + "\n")

with open(os.path.join(OUT, "overlay-samples.jsonl"), "w") as f:
    for m in range(D // 60):
        f.write(json.dumps({"minute": m, "at_ms": S + m * 60000 + 30000, "region_diff": 41.5, "differs": True},
                           separators=(",", ":")) + "\n")

with open(os.path.join(OUT, "logscan.json"), "w") as f:
    json.dump({"lines": 2400, "overlay_hidden_lines": 1}, f, separators=(",", ":"))
    f.write("\n")


def stamp(ms, digits):
    t = datetime.datetime.fromtimestamp(ms / 1000, tz=datetime.timezone.utc)
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond:06d}"[:digits] + "Z"


lines = []
t = LINE_FROM_MS
while t <= S + D * 1000 + 60000:
    n = ended(t)
    msg = (f"memory split input_gen={1 + n} tree_mb={mem_mb((t - (S - 60000)) // HB, t)} rss_kb={612000 + n * STEP_RSS_KB} "
           f"anon_kb={500000 + n * STEP_ANON_KB} file_kb=100000 shmem_kb=12000 threads=87 cef_poll={(t - S) // 1000 + 600} "
           f"cef_renderer_kb={200000 + n * STEP_CEF_KB} cef_renderer_n=1 cef_renderer_listed=1 "
           f"cef_gpu_process_kb=90000 cef_gpu_process_n=1 cef_gpu_process_listed=1 "
           f"cef_utility_kb=40000 cef_utility_n=2 cef_utility_listed=2 cef_zygote_kb=15000 cef_zygote_n=2 cef_zygote_listed=2 "
           f"cef_other_kb=- cef_other_n=0 cef_other_listed=0")
    lines.append(f"{stamp(t + 2, 6)} ts={stamp(t, 3)} level=info msg={msg}")
    lines.append(f"{stamp(t + 3002, 6)} ts={stamp(t + 3000, 3)} level=info msg=output fps beat=29.97 floor=27.00")
    t += LINE_EVERY_MS
with open(os.path.join(OUT, "compositor.log"), "w") as f:
    f.write("\n".join(lines) + "\n")
print(f"{SCHEDULE}: {len(beats)} beats, {len(actions)} actions, {len(alive)} cef alive, {len(lines)} log lines")
