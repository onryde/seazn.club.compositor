#!/usr/bin/env bash
# ffmpeg SRT playback probe: URL via a concat script on stdin (not argv), stderr scrubbed to disk.
mkdir -p /tmp/c00; cd /tmp
printf "ffconcat version 1.0\nfile '%s'\n" "$SPIKE_A_SRT_PLAY_URL" | timeout -s KILL "${1:-25}" \
  ffmpeg -hide_banner -loglevel verbose -analyzeduration 1 -fflags -nobuffer -probesize 32 \
  -f concat -safe 0 -protocol_whitelist pipe,srt,udp,file -i pipe:0 -map 0 -c copy -f null - 2>&1 \
  | tr '\r' '\n' | python3 -u /spike/redact.py > /tmp/c00/ffsrt3.log
echo "rc=${PIPESTATUS[1]} lines=$(wc -l < /tmp/c00/ffsrt3.log)"
grep -a -v -E "^\s*$" /tmp/c00/ffsrt3.log | tail -12 | cut -c1-180
