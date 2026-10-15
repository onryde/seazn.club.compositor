#!/usr/bin/env bash
# C00 YouTube-health follow-up: one full-chain push to B with explicit encoder settings.
# Usage: run-yt.sh <tag> <seconds> <bg camera|snow> <kbps> <vbv_ms|-1> <x264 option-string>
. /spike/env.sh
TAG=$1; SECS=$2; BG=$3; KB=$4; VBV=$5; OPTS=$6
redact_all "yt-$TAG.log"
start_xvfb
echo "YT start_utc=$(date -u +%FT%TZ) start_london=$(TZ=Europe/London date +%FT%T%z) tag=$TAG"
"${AS[@]}" python3 /spike/chain.py --height 720 --seconds "$SECS" --bg "$BG" --x264-bitrate "$KB" --vbv-ms "$VBV" --x264-opts "$OPTS" \
  > >(python3 -u /spike/redact.py > "$OUT/yt-$TAG.raw") 2>&1 &
CP=$!
sleep 15
python3 /spike/proc_sample.py --root-pid "$CP" --seconds $(( SECS - 20 )) --tag "yt-$TAG" > "$OUT/yt-$TAG.proc" 2>&1
wait "$CP"
grep -a CHAINSTART "$OUT/yt-$TAG.raw"
grep -a CHAINSTAT "$OUT/yt-$TAG.raw" | tail -2
grep -a RESULT "$OUT/yt-$TAG.raw"
grep -a PROCSUMMARY "$OUT/yt-$TAG.proc" | cut -c1-330
echo "YT end_utc=$(date -u +%FT%TZ) end_london=$(TZ=Europe/London date +%FT%T%z)"
