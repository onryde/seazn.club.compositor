#!/usr/bin/env python3
"""Owed 81: a harness run's camera-frame cadence, stage by stage (_FINDINGS.md F-028).

Stages, all from the same run's out/<case>/:
  push   emit.jsonl: when fake-camera pushed each counter into its encoder (whole ms);
  sent   wire.jsonl: when each video PES left the camera's mux for srtsink (fake-camera --wire);
  input  tap.jsonl:  when input-tap, built as the compositor's input is, decoded each frame;
  shown  rec/:       which counter each programme frame shows (the recordings, read with ffmpeg).

For push, sent and input: inter-arrival gaps, frames that arrive less than BUNCH_MS after the one
before ("bunched"), and a 30 fps latest-frame sampler swept over 66 tick phases (the repeats a tick
grid would see from those arrivals alone). For shown: the observed repeats and skipped counters, and
the recording's PTS steps (a dropped or doubled programme frame shows there). Then each segment of
the programme is fitted against the input stage (one time offset per segment): the share of
programme frames that equal the latest input frame at a regular 30 fps tick.

Every count is printed, and a stage with nothing in it says NO DATA: zero frames is never a pass.
Stdlib and ffmpeg/ffprobe on PATH (for rec/ only); run with `python3 -I`, never inside the
artifact's directory.

Usage: cadence_stages.py OUT_DIR
       cadence_stages.py --self-test     (check.yml)
"""

import collections
import glob
import json
import os
import statistics
import subprocess
import sys

PERIOD_MS = 1000.0 / 30.0
PHASES = 66
BUNCH_MS = 10.0
# The camera strip (crates/harness/src/strip.rs), restated for this reader.
CELLS, CHECKSUM_BITS, ROWS_PER_CELL = 48, 16, 30
DARK_BELOW, LIGHT_ABOVE = 80, 176
# 90 kHz ticks per camera frame (camera::FPS = 30).
PTS_PER_FRAME = 3000


def checksum(c):
    return ((c * 2654435761) & 0xFFFFFFFF) >> 16


def read_strip(row_bytes, width, cell):
    """The counter in one frame's bottom `cell` rows (gray, `width` wide), or None (strip::read)."""
    lo, hi = cell // 4, cell - cell // 4
    bits = 0
    for k in range(CELLS):
        cx = k * cell
        s = 0
        for row in range(lo, hi):
            base = row * width + cx
            s += sum(row_bytes[base + lo:base + hi])
        mean = s // ((hi - lo) * (hi - lo))
        if mean < DARK_BELOW:
            bit = 0
        elif mean > LIGHT_ABOVE:
            bit = 1
        else:
            return None
        bits = (bits << 1) | bit
    c = bits >> CHECKSUM_BITS
    if c == 0 or (bits & 0xFFFF) != checksum(c):
        return None
    return c


def decode_segment(path):
    """[(pts_ms, counter)] for every frame of one recorded segment, in presentation order."""
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
         "stream=width,height:frame=pts_time", "-of", "json", path],
        check=True, capture_output=True, text=True).stdout
    j = json.loads(probe)
    w, h = j["streams"][0]["width"], j["streams"][0]["height"]
    pts = [float(f["pts_time"]) * 1000 if f.get("pts_time") else None for f in j.get("frames", [])]
    cell = h // ROWS_PER_CELL
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-map", "0:v:0", "-fps_mode", "passthrough",
         "-vf", f"crop={w}:{cell}:0:{h - cell},format=gray", "-f", "rawvideo", "-"],
        check=True, capture_output=True).stdout
    size = w * cell
    n = len(raw) // size
    if n != len(pts):
        raise SystemExit(f"{path}: {n} frames decoded, {len(pts)} PTS")
    return [(pts[i], read_strip(raw[i * size:(i + 1) * size], w, cell)) for i in range(n)]


def load_jsonl(path):
    if not os.path.exists(path):
        return []
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def sample(arrivals, phase_ms, t_from, t_to):
    """The latest counter arrived by each tick of a 30 fps grid (arrivals: sorted (t_ms, counter))."""
    shown, i, last, n = [], 0, None, 0
    while (t := t_from + phase_ms + n * PERIOD_MS) < t_to:
        while i < len(arrivals) and arrivals[i][0] <= t:
            last = arrivals[i][1] if last is None else max(last, arrivals[i][1])
            i += 1
        shown.append(last)
        n += 1
    return shown


def cadence(seq):
    """(pairs, repeated frames, skipped counters, skip events) over consecutive non-None frames."""
    pairs = rep = skipped = events = 0
    for a, b in zip(seq, seq[1:]):
        if a is None or b is None:
            continue
        pairs += 1
        if b == a:
            rep += 1
        elif b > a + 1:
            events += 1
            skipped += b - a - 1
    return pairs, rep, skipped, events


def arrivals_stats(arrivals):
    """The stage's facts: frames, missing counters, gaps, bunching, and the phase sweep."""
    arrivals = sorted(arrivals)
    times = [t for t, _ in arrivals]
    counters = [c for _, c in arrivals]
    gaps = [b - a for a, b in zip(times, times[1:])]
    t0, t1 = times[0] + 1000, times[-1] - 1000
    # Phases at the middle of each 1/66 step, so a perfectly periodic series never puts a tick
    # exactly on an arrival (a knife edge that float rounding decides).
    sweep = [cadence(sample(arrivals, (k + 0.5) * PERIOD_MS / PHASES, t0, t1)) for k in range(PHASES)]
    reps = sorted(s[1] for s in sweep)
    return {
        "frames": len(arrivals),
        "missing": counters[-1] - counters[0] + 1 - len(set(counters)),
        "out_of_order": sum(1 for a, b in zip(counters, counters[1:]) if b <= a),
        "gap_ms_p50": statistics.median(gaps) if gaps else None,
        "gap_ms_min": min(gaps) if gaps else None,
        "gap_ms_max": max(gaps) if gaps else None,
        "bunched": sum(1 for g in gaps if g < BUNCH_MS),
        "pairs": sweep[0][0],
        "repeats_min": reps[0],
        "repeats_median": reps[len(reps) // 2],
        "repeats_max": reps[-1],
        "phases_over_1pct": sum(1 for r in reps if r * 100 > sweep[0][0]),
    }


def sent_arrivals(wire):
    """(t_ms, counter) per video PES: PTS mapped to counters from each build's first counter."""
    firsts = {w["build"]: w["first_counter"] for w in wire if "first_counter" in w}
    by_build = collections.defaultdict(list)
    for w in wire:
        if w.get("stream") == "video" and w.get("pts90k") is not None:
            by_build[w["build"]].append(w)
    out = []
    for b, ws in by_build.items():
        if b not in firsts:
            raise SystemExit(f"wire.jsonl: build {b} has video PES but no first_counter line")
        p0 = ws[0]["pts90k"]
        out.extend((w["wall_us"] / 1000.0, firsts[b] + round((w["pts90k"] - p0) / PTS_PER_FRAME)) for w in ws)
    return out


def fit(arrivals, seq):
    """Best share of `seq` (one segment's counters, None for no strip) equal to the latest arrival
    at a regular tick, over offsets of ±100 ms in 0.25 ms around the first counter's arrival."""
    idx = [(j, c) for j, c in enumerate(seq) if c is not None]
    if not idx:
        return 0, 0
    j0, c0 = idx[0]
    a0 = next((t for t, c in arrivals if c >= c0), None)
    if a0 is None:
        return 0, len(idx)
    best = 0
    for step in range(-400, 401):
        t_first = a0 + step * 0.25
        i, last, m = 0, None, 0
        for j, c in idx:
            t = t_first + (j - j0) * PERIOD_MS
            while i < len(arrivals) and arrivals[i][0] <= t:
                last = arrivals[i][1] if last is None else max(last, arrivals[i][1])
                i += 1
            m += last == c
        best = max(best, m)
    return best, len(idx)


def report(out_dir):
    emit = load_jsonl(os.path.join(out_dir, "emit.jsonl"))
    wire = load_jsonl(os.path.join(out_dir, "wire.jsonl"))
    tap = load_jsonl(os.path.join(out_dir, "tap.jsonl"))
    stages = {
        # Whole ms, truncated: the push was in [ms, ms + 1), so its middle. A phase within half a
        # millisecond of the grid is still a knife edge here (phases_over_1pct of 1 or 2).
        "push": [(e["wall_ms"] + 0.5, e["counter"]) for e in emit],
        "sent": sent_arrivals(wire),
        "input": [(t["wall_us"] / 1000.0, t["counter"]) for t in tap if t.get("counter") is not None],
    }
    for name, arr in stages.items():
        if not arr:
            print(f"{name}: NO DATA")
            continue
        print(f"{name}: " + json.dumps(arrivals_stats(arr)))
    if wire:
        bad = sum(1 for w in wire if "bad_sync" in w)
        audio = sum(1 for w in wire if w.get("stream") == "audio")
        print(f"wire: {len(stages['sent'])} video PES, {audio} audio PES, {bad} bad buffers")
    if tap:
        print(f"tap: {len(tap)} frames decoded, {len(stages['input'])} with a strip")
    segs = sorted(glob.glob(os.path.join(out_dir, "rec", "**", "*.mp4"), recursive=True))
    total = [0, 0, 0, 0]
    steps = collections.Counter()
    fitted = matched = 0
    arr_in = sorted(stages["input"])
    for s in segs:
        frames = decode_segment(s)
        for (a, _), (b, _) in zip(frames, frames[1:]):
            if a is not None and b is not None:
                steps[round(b - a)] += 1
        seq = [c for _, c in frames]
        total = [x + y for x, y in zip(total, cadence(seq))]
        if arr_in and sum(c is not None for c in seq) >= 100:
            m, n = fit(arr_in, seq)
            matched, fitted = matched + m, fitted + n
    if not segs:
        print("shown: NO DATA")
        return 1
    print(f"shown: {len(segs)} segments, pairs {total[0]}, repeated frames {total[1]}, skipped counters "
          f"{total[2]}, skip events {total[3]}; recording PTS steps (ms) {dict(sorted(steps.items()))}")
    if arr_in:
        if fitted == 0:
            print("fit: NOTHING FITTED")
            return 1
        print(f"fit: {matched} of {fitted} programme strip frames ({100 * matched / fitted:.1f} %) are the "
              f"latest input-tap frame at a regular 30 fps tick")
    return 0


def self_test():
    checked = 0
    # A smooth 30 fps arrival series: no phase sees a repeat (ticks away from the arrival instants).
    smooth = [(i * PERIOD_MS + 0.5, i + 1) for i in range(900)]
    s = arrivals_stats(smooth)
    assert s["missing"] == 0 and s["bunched"] == 0 and s["repeats_max"] == 0, s
    checked += 1
    # Every other frame held one period and sent with the next: half the ticks repeat at every phase
    # off the bunch instant, and the bunch is counted.
    bunched = []
    for i in range(900):
        t = (i + (i % 2)) * PERIOD_MS + (0.5 if i % 2 == 0 else 1.0)
        bunched.append((t, i + 1))
    s = arrivals_stats(bunched)
    assert s["bunched"] == 449, s
    assert s["repeats_min"] * 100 >= s["pairs"] * 49, s
    checked += 1
    # The sampler takes the latest arrival by each tick, and cadence counts repeats and skips.
    assert sample([(0, 1), (10, 2), (20, 3), (50, 4)], 1, 0, 100) == [1, 3, 4]
    # One skipped counter (2 to 4) and two (4 to 7); a None breaks the pair, it is no skip.
    assert cadence([1, 1, 2, 4, 7, None, 8, 8]) == (5, 2, 3, 2)
    checked += 2
    # PTS to counters: per build, from that build's first counter (a rebuild restarts the PTS).
    wire = [
        {"build": 1, "first_counter": 7, "wall_us": 0},
        {"build": 1, "stream": "video", "pts90k": 126_000, "wall_us": 1_000},
        {"build": 1, "stream": "audio", "pts90k": 126_100, "wall_us": 1_100},
        {"build": 1, "stream": "video", "pts90k": 128_999, "wall_us": 34_000},
        {"build": 2, "first_counter": 100, "wall_us": 0},
        {"build": 2, "stream": "video", "pts90k": 126_000, "wall_us": 3_000_000},
        {"build": 2, "stream": "video", "pts90k": 132_001, "wall_us": 3_066_000},
    ]
    assert sent_arrivals(wire) == [(1.0, 7), (34.0, 8), (3000.0, 100), (3066.0, 102)], sent_arrivals(wire)
    checked += 1
    # The fit finds the offset at which a programme sampled from arrivals matches them exactly.
    arr = sorted(bunched)
    seq = sample(arr, 7.0, arr[0][0] + 1000, arr[-1][0] - 1000)
    m, n = fit(arr, seq)
    assert n > 0 and m == n, (m, n)
    checked += 1
    # The strip reader: a drawn strip reads its counter, a grey one reads None.
    w, cell = CELLS * 4, 4

    def strip_rows(counter):
        bits = (counter << CHECKSUM_BITS) | checksum(counter)
        row = bytearray()
        for k in range(CELLS):
            on = (bits >> (CELLS - 1 - k)) & 1
            row.extend([235 if on else 16] * cell)
        return bytes(row) * cell

    for c in (1, 2, 30, 123_456, 0xFFFF_FFFF):
        assert read_strip(strip_rows(c), w, cell) == c, c
        checked += 1
    assert read_strip(bytes([128]) * (w * cell), w, cell) is None
    checked += 1
    print(f"cadence_stages self-test: {checked} checks")
    assert checked == 12, checked
    return 0


def main():
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 64
    return report(sys.argv[1])


if __name__ == "__main__":
    sys.exit(main())
