#!/usr/bin/env python3
"""The compositor Machine's log, from the platform's log stream (C15, soak.yml).

soak.yml streams `flyctl logs --machine <id> --json` into a file for the whole run, restarting the
stream if it drops. That file is a run of JSON objects (one per log entry, each possibly pretty
printed over several lines, a restarted stream repeating the buffer it replays, and a killed stream
leaving a cut-off last object). This turns it into what the checker's logfacts reads, one line per
entry in arrival order:

    <RFC 3339 UTC stamp, microseconds, Z> <message>

the shape of `docker logs --timestamps` (crates/harness/src/logfacts.rs), so the soak's log is
parsed by the same code as C14's. It also writes logscan.json, check (b)'s raw count: every line
whose message carries `msg=overlay hidden`, with or without a reason, which verdict.jq compares
with the hides logfacts parsed (a hide logfacts cannot parse is a bare line, and a failure).

    python3 -I harness/soak/flylog.py --in <stream> --machine <id> --log <out> --scan <out>

An entry of another Machine is dropped (the stream is filtered by Machine already; this is the
guard). An entry with no usable stamp is written bare (logfacts then reads its own ts=). Duplicate
entries (same stamp, Machine and message: a replayed buffer) are written once. Exit 0, or 1 when
the stream held no entry at all for the Machine (an empty log reads clean, so it is refused here).
"""

import argparse
import datetime
import json
import re
import sys

HIDDEN = re.compile(r"(^|\s)msg=overlay hidden(\s|$)")


def entries(text):
    """Every JSON object in `text`, skipping what does not parse (a cut-off object, a stray line)."""
    dec = json.JSONDecoder()
    i = 0
    n = len(text)
    while i < n:
        j = text.find("{", i)
        if j < 0:
            return
        try:
            obj, end = dec.raw_decode(text, j)
        except ValueError:
            i = j + 1
            continue
        if isinstance(obj, dict):
            yield obj
        i = end


def stamp(ts):
    """`ts` as `YYYY-MM-DDTHH:MM:SS.ffffffZ` (UTC), or None when it is not an RFC 3339 time."""
    if not isinstance(ts, str) or not ts:
        return None
    t = ts.strip()
    if t.endswith("Z"):
        t = t[:-1] + "+00:00"
    # fromisoformat takes at most 6 fraction digits before 3.11; the platform may send 9.
    m = re.match(r"^(.*T\d\d:\d\d:\d\d)(\.\d+)?([+-]\d\d:\d\d)$", t)
    if not m:
        return None
    frac = (m.group(2) or ".0")[1:7].ljust(6, "0")
    try:
        d = datetime.datetime.fromisoformat(f"{m.group(1)}.{frac}{m.group(3)}")
    except ValueError:
        return None
    if d.tzinfo is None:
        return None
    return d.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def convert(text, machine):
    lines = []
    seen = set()
    other = 0
    for e in entries(text):
        msg = e.get("message")
        if not isinstance(msg, str):
            continue
        inst = e.get("instance")
        if isinstance(inst, str) and inst and inst != machine:
            other += 1
            continue
        key = (e.get("timestamp"), inst, msg)
        if key in seen:
            continue
        seen.add(key)
        msg = msg.rstrip("\r\n").replace("\n", " ")
        s = stamp(e.get("timestamp"))
        lines.append(f"{s} {msg}" if s else msg)
    return lines, other


def main(argv):
    p = argparse.ArgumentParser()
    p.add_argument("--in", dest="src", required=True)
    p.add_argument("--machine", required=True)
    p.add_argument("--log", required=True)
    p.add_argument("--scan", required=True)
    a = p.parse_args(argv)
    if not re.fullmatch(r"[0-9a-z]+", a.machine):
        print("flylog: --machine must be a Machine id", file=sys.stderr)
        return 64
    with open(a.src, encoding="utf-8", errors="replace") as f:
        text = f.read()
    lines, other = convert(text, a.machine)
    with open(a.log, "w", encoding="utf-8") as f:
        for line in lines:
            f.write(line + "\n")
    hidden = sum(1 for line in lines if HIDDEN.search(line))
    with open(a.scan, "w", encoding="utf-8") as f:
        json.dump({"lines": len(lines), "overlay_hidden_lines": hidden, "other_machine_entries": other}, f)
        f.write("\n")
    print(f"flylog: {len(lines)} lines, {hidden} overlay hidden, {other} of another Machine dropped")
    if not lines:
        print("flylog: FAIL the stream held no entry for the Machine", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
