#!/usr/bin/env python3
"""The overlay sampler, check (d) of the soak (C15): is the overlay on air, once a minute?

After the run, on the harness Machine. MediaMTX records the programme (`live/dest`) and, separately,
the camera as published (`cam`), both as fMP4 segments named by their wall-clock start
(`%Y-%m-%d_%H-%M-%S-%f`, UTC). For each minute k of the drive it takes one instant T in that minute,
clear of every scheduled dropout and disconnect (a paused camera or a dropped destination has no
honest frame), and decodes three frames: the programme at T, the camera at T, and the camera
`control_gap_s` later. The frame is cut into bands of `sample_band_rows` rows.

- A band is STABLE when the two camera frames agree in it (mean absolute difference at most
  `stable_band_max`): the camera's picture does not move there, so the camera at T is that band's
  reference whatever the programme's latency. On `ball`, every band but the ball's and the strip's
  is stable; on noise, none is.
- `region_diff` is the largest mean absolute difference between the programme and the camera over
  the stable bands: the overlay's pixels, where it is drawn over a still part of the picture.
- `median_diff` is the median of the same differences: the floor, which an overlay drawn over a
  part of the picture leaves near zero. A floor at or over `overlay_diff_min` is a uniform level
  shift or a misalignment between the two recordings, not an overlay.
- A minute `differs` (the overlay is on air) when some band is stable, region_diff >=
  `overlay_diff_min` and median_diff is under it. With no stable band there is no reference, and
  with a raised floor the reference does not line up: either way the sample carries an error and
  differs false, so check (d) fails rather than passes on a picture it cannot read.

    python3 -I harness/soak/sampler.py --programme <dir> --camera <dir> --actions <actions.jsonl>
        --soak <soak.json> --out <overlay-samples.jsonl> [--ffmpeg <bin>]

Writes one JSON line per minute: {minute, at_ms, differs, region_diff, median_diff, stable_bands,
moving_bands, programme, camera} or {minute, at_ms, differs: false, error}. Exit 0 (the verdict judges), 64 on a usage error,
1 when the run has no start action (nothing to sample).

The cost legs' stills (C15: "screenshots of each leg's programme are looked at, and never committed"):

    python3 -I harness/soak/sampler.py --programme <dir> --soak <soak.json> --stills <dir>
        --still-at <label>:<epoch ms>[,<label>:<epoch ms>...]

writes the programme at each instant as <dir>/<label>-<ms>.jpg (its own size, in colour) and prints
one line per still; an instant with no segment or no frame is printed as missed. Exit 0 when at
least one still was written, 1 when none was.
"""

import argparse
import datetime
import json
import os
import re
import subprocess
import sys

NAME = re.compile(r"^(\d{4})-(\d\d)-(\d\d)_(\d\d)-(\d\d)-(\d\d)-(\d{1,9})\.mp4$")
W, H = 1280, 720


def seg_start_ms(name):
    m = NAME.match(name)
    if not m:
        return None
    y, mo, d, h, mi, s, frac = m.groups()
    us = int(frac.ljust(6, "0")[:6])
    t = datetime.datetime(int(y), int(mo), int(d), int(h), int(mi), int(s), us, tzinfo=datetime.timezone.utc)
    return int(t.timestamp() * 1000)


def segments(root):
    """[(start_ms, path)] of every segment under root, by start."""
    out = []
    for dirpath, _, files in os.walk(root):
        for f in files:
            ms = seg_start_ms(f)
            if ms is not None:
                out.append((ms, os.path.join(dirpath, f)))
    out.sort()
    return out


_DURATIONS = {}


def duration_ms(ffprobe, path):
    """The segment's own duration (ffprobe), or None."""
    if path not in _DURATIONS:
        d = None
        try:
            p = subprocess.run([ffprobe, "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path],
                               capture_output=True, text=True, timeout=30, check=False)
            if p.returncode == 0:
                d = int(float(p.stdout.strip()) * 1000)
        except (OSError, subprocess.TimeoutExpired, ValueError):
            d = None
        _DURATIONS[path] = d
    return _DURATIONS[path]


def locate(segs, t_ms, max_len_ms, ffprobe="ffprobe"):
    """The segment holding t_ms and the offset into it (s), or None (a gap). A segment ends at its
    own duration, at the next one's start, and at most max_len_ms after its start."""
    for i, (start, path) in enumerate(segs):
        if t_ms < start:
            break
        end = start + max_len_ms
        if i + 1 < len(segs):
            end = min(end, segs[i + 1][0])
        if start <= t_ms < end:
            d = duration_ms(ffprobe, path)
            if d is not None and t_ms >= start + d:
                return None
            return path, (t_ms - start) / 1000.0
    return None


def frame(ffmpeg, path, offset_s):
    """One GRAY8 W x H frame at offset_s into path, or None."""
    try:
        p = subprocess.run(
            [ffmpeg, "-v", "error", "-nostdin", "-ss", f"{offset_s:.3f}", "-i", path, "-frames:v", "1",
             "-vf", f"scale={W}:{H},format=gray", "-f", "rawvideo", "pipe:1"],
            capture_output=True, timeout=60, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if p.returncode != 0 or len(p.stdout) != W * H:
        return None
    return p.stdout


def still(ffmpeg, path, offset_s, out):
    """The frame at offset_s into path as a JPEG at out; True when written."""
    try:
        p = subprocess.run(
            [ffmpeg, "-v", "error", "-nostdin", "-y", "-ss", f"{offset_s:.3f}", "-i", path, "-frames:v", "1",
             "-q:v", "3", out],
            capture_output=True, timeout=60, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return p.returncode == 0 and os.path.exists(out) and os.path.getsize(out) > 0


def stills(a, soak):
    os.makedirs(a.stills, exist_ok=True)
    prog = segments(a.programme)
    max_len = soak["sample_segment_max_s"] * 1000
    wrote = 0
    for item in a.still_at.split(","):
        label, _, ms = item.partition(":")
        if not re.fullmatch(r"[a-z0-9-]+", label) or not ms.isdigit():
            print(f"stills: FAIL bad instant {item!r} (<label>:<epoch ms>)", file=sys.stderr)
            return 64
        loc = locate(prog, int(ms), max_len, a.ffprobe)
        out = os.path.join(a.stills, f"{label}-{ms}.jpg")
        if loc is not None and still(a.ffmpeg, loc[0], loc[1], out):
            wrote += 1
            print(f"stills: {label} at {ms}: {os.path.basename(out)}")
        else:
            print(f"stills: {label} at {ms}: missed ({'no segment' if loc is None else 'no frame'})")
    print(f"stills: {wrote} written")
    return 0 if wrote > 0 else 1


def bands(a, b, rows):
    """The mean absolute difference of each band of `rows` rows."""
    out = []
    step = rows * W
    for i in range(0, W * H, step):
        x = a[i:i + step]
        y = b[i:i + step]
        out.append(sum(abs(p - q) for p, q in zip(x, y)) / len(x))
    return out


def judge(prog, cam, cam_later, soak):
    """The sample's fields from its three frames."""
    rows = soak["sample_band_rows"]
    control = bands(cam, cam_later, rows)
    stable = [i for i, c in enumerate(control) if c <= soak["stable_band_max"]]
    if not stable:
        return {"differs": False, "error": "no stable band: the camera moves everywhere (no reference)",
                "moving_bands": len(control)}
    diff = bands(prog, cam, rows)
    over = sorted(diff[i] for i in stable)
    rd = round(over[-1], 2)
    n = len(over)
    md = round(over[n // 2] if n % 2 else (over[n // 2 - 1] + over[n // 2]) / 2, 2)
    out = {"differs": rd >= soak["overlay_diff_min"] and md < soak["overlay_diff_min"], "region_diff": rd,
           "median_diff": md, "stable_bands": len(stable), "moving_bands": len(control) - len(stable)}
    if md >= soak["overlay_diff_min"]:
        out["error"] = "the programme differs from the camera everywhere (a level shift or misalignment, not an overlay)"
    return out


def plan(actions, soak):
    """(start_ms, drive_s, [(from_ms, to_ms)] windows to avoid)."""
    start = next((a for a in actions if a.get("kind") == "start"), None)
    if start is None:
        return None
    margin = soak["sample_margin_s"] * 1000
    windows = [(a["at_ms"] - margin, a.get("end_ms", a["at_ms"]) + margin)
               for a in actions if a.get("kind") in ("dropout", "disconnect") and isinstance(a.get("at_ms"), int)]
    return start["at_ms"], start["drive_s"], windows


def main(argv):
    p = argparse.ArgumentParser()
    for k in ("programme", "soak"):
        p.add_argument("--" + k, required=True)
    for k in ("camera", "actions", "out", "stills", "still-at"):
        p.add_argument("--" + k)
    p.add_argument("--ffmpeg", default="ffmpeg")
    p.add_argument("--ffprobe", default="ffprobe")
    a = p.parse_args(argv)
    with open(a.soak, encoding="utf-8") as f:
        soak = json.load(f)
    if a.stills is not None or a.still_at is not None:
        if not (a.stills and a.still_at):
            p.error("--stills and --still-at go together")
        return stills(a, soak)
    if not (a.camera and a.actions and a.out):
        p.error("--camera, --actions and --out are required (or --stills and --still-at)")
    actions = []
    if os.path.exists(a.actions):
        with open(a.actions, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line:
                    try:
                        actions.append(json.loads(line))
                    except ValueError:
                        pass
    pl = plan(actions, soak)
    if pl is None:
        print("sampler: FAIL no start action: nothing to sample", file=sys.stderr)
        return 1
    start_ms, drive_s, windows = pl
    every = soak["sample_every_s"] * 1000
    minutes = drive_s * 1000 // every
    max_len = soak["sample_segment_max_s"] * 1000
    prog = segments(a.programme)
    cam = segments(a.camera)
    rows = []
    for k in range(minutes):
        base = start_ms + k * every
        t = next((base + o * 1000 for o in soak["sample_offsets_s"]
                  if not any(lo <= base + o * 1000 <= hi for lo, hi in windows)), None)
        row = {"minute": k, "at_ms": t if t is not None else base}
        if t is None:
            row.update(differs=False, error="every sample instant falls in a scheduled action")
        else:
            lp, lc = locate(prog, t, max_len, a.ffprobe), locate(cam, t, max_len, a.ffprobe)
            lc2 = locate(cam, t + soak["control_gap_s"] * 1000, max_len, a.ffprobe)
            if lp is None or lc is None or lc2 is None:
                row.update(differs=False, error="no " + ("programme" if lp is None else "camera") + " segment at the instant")
            else:
                fp, fc, fc2 = frame(a.ffmpeg, *lp), frame(a.ffmpeg, *lc), frame(a.ffmpeg, *lc2)
                if fp is None or fc is None or fc2 is None:
                    row.update(differs=False, error="a frame did not decode")
                else:
                    row.update(judge(fp, fc, fc2, soak))
                    row.update(programme=os.path.basename(lp[0]), camera=os.path.basename(lc[0]))
        rows.append(row)
    with open(a.out, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, separators=(",", ":")) + "\n")
    on = sum(1 for r in rows if r["differs"])
    errs = sum(1 for r in rows if "error" in r)
    print(f"sampler: {len(rows)} minutes, {on} on air, {errs} without a sample")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
