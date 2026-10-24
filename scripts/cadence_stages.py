#!/usr/bin/env python3
"""Owed 81: a harness run's camera-frame cadence, stage by stage (_FINDINGS.md F-029).

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

Then the camera mux's holds (whole periods from push to sent; F-029's audio-wait rule, frame by
frame), the chain from sent to input (input(c) minus sent(c + k)), and the fit's contrasts (the
programme against push, an ideal 30 fps series, sent and sent(c + 2), which it should NOT match).

Every count is printed, and a stage with nothing in it says NO DATA. Zero checked is never a pass:
a stage whose file is there but empty, a programme with no pairs, a fit of nothing, or a frame sent
before it was pushed prints FAIL and exits 1.
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
# The camera's audio: 48 kHz, 1 600 samples a frame, into AAC-LC frames of 1 024; lcm(1 600, 1 024)
# is 25 600 samples, so the audio-wait pattern repeats every 16 frames.
SAMPLES_PER_FRAME, AAC_FRAME, AUDIO_PERIOD = 1600, 1024, 16


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


def quantile(values, q):
    """The value at index floor(q n) of `values` sorted (None for none)."""
    v = sorted(values)
    return v[min(len(v) - 1, int(q * len(v)))] if v else None


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
        # F-002's percentiles, for a like-for-like read (index floor(q n) of the sorted gaps).
        "gap_ms_p90": quantile(gaps, 0.9),
        "gap_ms_p99": quantile(gaps, 0.99),
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


def holds(pushed, sent):
    """Whole periods each frame waited between its push and its first video PES at `srtsink`:
    {counter: periods}, and the counters sent before they were pushed (the PTS-to-counter map is
    off: a lost first PES, or two fake-camera processes appending one build). `pushed`: {counter:
    wall_ms, truncated}; `sent`: [(t_ms, counter)]."""
    first = {}
    for t, c in sent:
        first.setdefault(c, t)
    out, early = {}, []
    for c in sorted(first):
        if c not in pushed:
            continue
        if first[c] < pushed[c]:
            early.append(c)
        else:
            out[c] = round((first[c] - (pushed[c] + 0.5)) / PERIOD_MS)
    return out, early


def audio_held(n, lead=0):
    """F-029's audio-wait rule: the camera's mux lets video frame n (from the build's first) go only
    once an AAC frame stamped at or after it (1024 k >= 1600 n) is encoded, which needs 1024 (k + 1)
    samples, and by then the camera has pushed 1600 (n + 1) + `lead` samples."""
    k = -(-SAMPLES_PER_FRAME * n // AAC_FRAME)
    return AAC_FRAME * (k + 1) > SAMPLES_PER_FRAME * (n + 1) + lead


def held_residues(lead=0):
    """The frames (mod AUDIO_PERIOD) the audio-wait rule holds at a given audio lead (samples)."""
    return [n for n in range(AUDIO_PERIOD) if audio_held(n, lead)]


def chain(sent_first, input_first, k):
    """input(c) minus sent(c + k), ms, for every counter in both ({counter: first t_ms})."""
    return [input_first[c] - sent_first[c + k] for c in sorted(input_first) if c + k in sent_first]


def vacuous(frames, with_file, shown_pairs, fitted):
    """Why a report checked nothing (empty: it checked something). `frames`: {stage: frames read};
    `with_file`: the stages whose file is in the run; `fitted`: None when there is no input."""
    out = [f"{name}: its file has no frames" for name in ("push", "sent", "input")
           if name in with_file and frames.get(name, 0) == 0]
    if shown_pairs == 0:
        out.append("shown: 0 pairs")
    if fitted == 0:
        out.append("fit: nothing fitted")
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


def first_by_counter(arrivals):
    first = {}
    for t, c in sorted(arrivals):
        first.setdefault(c, t)
    return first


def report_holds(emit, wire, sent):
    """The mux's holds (push to sent) and the audio-wait rule's frame-by-frame prediction. Returns
    the number of frames sent before they were pushed (a failure)."""
    pushed = {e["counter"]: e["wall_ms"] for e in emit}
    h, early = holds(pushed, sent)
    if early:
        print(f"holds: FAIL {len(early)} frames sent before they were pushed (first {early[:5]}): "
              f"the PTS-to-counter map is off")
    if not h:
        return len(early)
    firsts = sorted(w["first_counter"] for w in wire if "first_counter" in w)
    print(f"holds: {len(h)} frames, periods from push to sent {dict(sorted(collections.Counter(h.values()).items()))}")
    by_res = collections.defaultdict(lambda: [0, 0])
    agree = 0
    for c, periods in h.items():
        n = c - max(f for f in firsts if f <= c)
        by_res[n % AUDIO_PERIOD][0] += periods > 0
        by_res[n % AUDIO_PERIOD][1] += 1
        agree += (periods > 0) == audio_held(n)
    print("  held share by (counter - build's first) mod 16: "
          + " ".join(f"{r}:{100 * a // b}%" for r, (a, b) in sorted(by_res.items())))
    print(f"  the audio-wait rule (held at lead 0: residues {held_residues(0)}) agrees on {agree} of "
          f"{len(h)} ({100 * agree / len(h):.1f} %); it holds no frame from a lead of "
          f"{next(lead for lead in range(AAC_FRAME) if not held_residues(lead))} samples")
    return len(early)


def report(out_dir):
    paths = {name: os.path.join(out_dir, f) for name, f in
             (("push", "emit.jsonl"), ("sent", "wire.jsonl"), ("input", "tap.jsonl"))}
    with_file = {name for name, path in paths.items() if os.path.exists(path)}
    emit, wire, tap = (load_jsonl(paths[name]) for name in ("push", "sent", "input"))
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
    early = report_holds(emit, wire, stages["sent"]) if emit and stages["sent"] else 0
    sent_first, input_first = first_by_counter(stages["sent"]), first_by_counter(stages["input"])
    if sent_first and input_first:
        for k in range(4):
            d = chain(sent_first, input_first, k)
            if d:
                med = statistics.median(d)
                near = sum(1 for x in d if abs(x - med) <= 20)
                print(f"chain: input(c) - sent(c + {k}) median {med:.0f} ms, within 20 ms of it "
                      f"{near} of {len(d)} ({100 * near / len(d):.1f} %)")
    # The fit's contrasts: the programme against series it should NOT match (the PR #59 review's
    # I-3), so the input's share has something to be told from.
    series = {"input": sorted(stages["input"])}
    if stages["push"]:
        push = sorted(stages["push"])
        c0, t0 = min(c for _, c in push), min(t for t, _ in push)
        series["push"] = push
        series["ideal 30 fps"] = sorted((t0 + (c - c0) * PERIOD_MS, c) for _, c in push)
    if sent_first:
        series["sent"] = sorted((t, c) for c, t in sent_first.items())
        series["sent(c + 2)"] = sorted((sent_first[c + 2], c) for c in sent_first if c + 2 in sent_first)
    segs = sorted(glob.glob(os.path.join(out_dir, "rec", "**", "*.mp4"), recursive=True))
    total = [0, 0, 0, 0]
    steps = collections.Counter()
    fits = {name: [0, 0] for name, arr in series.items() if arr}
    excluded = 0
    for seg in segs:
        frames = decode_segment(seg)
        for (a, _), (b, _) in zip(frames, frames[1:]):
            if a is not None and b is not None:
                steps[round(b - a)] += 1
        seq = [c for _, c in frames]
        total = [x + y for x, y in zip(total, cadence(seq))]
        if sum(c is not None for c in seq) < 100:
            excluded += 1
            continue
        for name in fits:
            m, n = fit(series[name], seq)
            fits[name][0] += m
            fits[name][1] += n
    if not segs:
        print("shown: NO DATA")
        return 1
    print(f"shown: {len(segs)} segments ({excluded} with under 100 strip frames, not fitted), pairs "
          f"{total[0]}, repeated frames {total[1]}, skipped counters {total[2]}, skip events {total[3]}; "
          f"recording PTS steps (ms) {dict(sorted(steps.items()))}")
    for name, (m, n) in fits.items():
        if n:
            print(f"fit against {name}: {m} of {n} programme strip frames ({100 * m / n:.1f} %) are the "
                  f"latest {name} frame at a regular 30 fps tick")
    fitted = fits["input"][1] if "input" in fits else None
    failures = vacuous({name: len(arr) for name, arr in stages.items()}, with_file, total[0], fitted)
    if early:
        failures.append("holds: frames sent before they were pushed")
    for f in failures:
        print(f"FAIL {f}")
    return 1 if failures else 0


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
    # The fit finds the offset at which a programme sampled from arrivals matches them exactly...
    arr = sorted(bunched)
    seq = sample(arr, 7.0, arr[0][0] + 1000, arr[-1][0] - 1000)
    m, n = fit(arr, seq)
    assert n > 0 and m == n, (m, n)
    checked += 1
    # ...and no offset makes a smooth series explain that programme (about 70 % match), so 100 %
    # is a finding, not the fit's floor (the PR #59 review's I-3: `m += 1` passed the line above).
    m, n = fit(sorted(smooth), seq)
    assert n > 0 and m * 100 < n * 90, (m, n)
    checked += 1
    # A tick exactly on an arrival shows it, and an arrival out of counter order never takes the
    # sampler back (the latest counter wins).
    assert sample([(0.0, 1), (PERIOD_MS, 2)], 0.0, 0.0, 2 * PERIOD_MS) == [1, 2]
    assert sample([(0, 1), (5, 3), (10, 2)], 15, 0, 20) == [3]
    checked += 2
    # Gap percentiles: 100 gaps of 1 to 100 ms read 91 at p90 and 100 at p99 (index floor(q n)).
    assert (quantile(range(100, 0, -1), 0.9), quantile(range(1, 101), 0.99), quantile([], 0.5)) == (91, 100, None)
    checked += 1
    # Missing and out-of-order counters are counted (arrival order: 1, 3, 2, 5; 4 never came).
    s = arrivals_stats([(i * PERIOD_MS, c) for i, c in enumerate([1, 3, 2, 5] + list(range(6, 100)))])
    assert (s["missing"], s["out_of_order"]) == (1, 1), s
    checked += 1
    # The mux's holds: periods from push to sent, per counter; a frame sent before it was pushed
    # means the PTS-to-counter map is off (the review's m-5), and is reported, not binned.
    # Counter 5 waited 29.5 ms, 0.89 of a period: one period, to the nearest.
    h, early = holds({1: 0, 2: 33, 3: 66, 4: 100, 5: 133},
                     [(0.4, 1), (66.9, 2), (67.0, 3), (99.0, 4), (163.0, 5)])
    assert h == {1: 0, 2: 1, 3: 0, 5: 1} and early == [4], (h, early)
    checked += 1
    # The audio-wait rule (F-029), from its own arithmetic: AAC frame k = ceil(1600 n / 1024) is
    # encoded once 1024 (k + 1) samples are in, and the camera has pushed 1600 (n + 1) + lead.
    # 1024 ceil(25 n / 16) - 1600 n - 576 > lead holds exactly for 9 n mod 16 in 1..6 at lead 0
    # (n = 2, 4, 6, 9, 11, 13), for 9 n mod 16 = 1 alone up to lead 383 (n = 9), never from 384.
    assert held_residues(0) == [2, 4, 6, 9, 11, 13], held_residues(0)
    assert held_residues(383) == [9] and held_residues(384) == [], (held_residues(383), held_residues(384))
    checked += 2
    # The chain from one stage to the next: input(c) minus sent(c + k).
    assert chain({1: 0.0, 2: 33.0, 3: 66.0, 4: 99.0}, {1: 1266.0, 2: 1299.0}, 2) == [1200.0, 1200.0]
    checked += 1
    # Zero checked is a failure: a stage whose file is there but empty, a programme with no pairs,
    # or a fit of nothing (the review's I-4: the alignment-1 run read "repeated frames 0", exit 0).
    assert vacuous({"push": 10, "sent": 10, "input": 0}, {"input"}, 100, 50) == ["input: its file has no frames"]
    assert vacuous({"push": 10, "sent": 10, "input": 10}, {"push", "sent", "input"}, 0, 0) == [
        "shown: 0 pairs", "fit: nothing fitted"]
    assert vacuous({"push": 10, "sent": 0, "input": 0}, {"push"}, 100, None) == []
    checked += 3
    # The strip reader: a drawn strip reads its counter, a grey one reads None.
    w, cell = CELLS * 4, 4

    def strip_rows(counter, flip=0):
        bits = (counter << CHECKSUM_BITS) | (checksum(counter) ^ flip)
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
    # A strip whose checksum is wrong, or whose counter is 0 (an all-dark strip), reads nothing.
    assert read_strip(strip_rows(30, flip=1), w, cell) is None
    assert read_strip(strip_rows(0), w, cell) is None
    checked += 2
    print(f"cadence_stages self-test: {checked} checks")
    assert checked == 26, checked
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
