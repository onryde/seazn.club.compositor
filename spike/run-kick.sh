#!/usr/bin/env bash
# C00 item 2: does a second simultaneous reader kick the first? Same Machine (same source IP).
# Usage: run-kick.sh [r1_seconds=150] [r2_delay=40] [r2_seconds=60]
. "$(dirname "$0")/env.sh"
R1=${1:-150}; D=${2:-40}; R2=${3:-60}
redact_all kick.log
echo "KICK r1 start $(date +%s.%N)"
"${AS[@]}" python3 "$SPIKE_DIR/srt_reader.py" --tag kick-r1 --seconds "$R1" 2>&1 | python3 -u "$SPIKE_DIR/redact.py" > "$OUT/kick-r1.raw" &
P1=$!
sleep "$D"
echo "KICK r2 start $(date +%s.%N)"
"${AS[@]}" python3 "$SPIKE_DIR/srt_reader.py" --tag kick-r2 --seconds "$R2" 2>&1 | python3 -u "$SPIKE_DIR/redact.py" > "$OUT/kick-r2.raw" &
P2=$!
wait "$P2"; echo "KICK r2 done $(date +%s.%N)"
wait "$P1"; echo "KICK r1 done $(date +%s.%N)"
grep -a RESULT "$OUT/kick-r1.raw"
grep -a RESULT "$OUT/kick-r2.raw"
