#!/usr/bin/env python3
"""stdin -> stdout secret filter (C00 spike, THROWAWAY).

Every spike script pipes ALL of its output through this before it reaches a terminal, a log
or a file. It:
  1. replaces every exact SPIKE_* env value, and each component of it (the path after /live/,
     every query value, their URL-decoded forms), with <redacted>;
  2. rewrites any srt:// / rtmp:// / rtmps:// URL, anything after /live/, and every query
     string to a redacted form;
  3. drops any line that still mentions passphrase or streamid.
It never prints the values it knows.
"""
import os
import re
import sys
import urllib.parse

parts = set()
for k, v in os.environ.items():
    if not k.startswith("SPIKE_") or not v:
        continue
    for cand in (v, urllib.parse.unquote(v)):
        parts.add(cand)
        for m in re.finditer(r"[?&]([^=&\s]+)=([^&\s]+)", cand):
            parts.add(m.group(2))
            parts.add(urllib.parse.unquote(m.group(2)))
        m = re.search(r"/live2?/([^?\s]+)", cand)
        if m:
            parts.add(m.group(1))
            for seg in m.group(1).split("/"):
                parts.add(seg)
parts = sorted((p for p in parts if len(p) >= 8), key=len, reverse=True)

URL = re.compile(r"\b(srt|rtmps?)://[^\s\"'<>]+", re.I)
LIVE = re.compile(r"/live2?/[^\s\"'<>]+")
QUERY = re.compile(r"\?[^\s\"'<>]+")
DROP = re.compile(r"passphrase|streamid", re.I)

for line in sys.stdin:
    for p in parts:
        if p in line:
            line = line.replace(p, "<redacted>")
    line = URL.sub(lambda m: m.group(1) + "://<redacted>", line)
    line = LIVE.sub("/live/<redacted>", line)
    line = QUERY.sub("?<q>", line)
    if DROP.search(line):
        line = "[redact: dropped a line that named a credential field]\n"
    sys.stdout.write(line)
    sys.stdout.flush()
