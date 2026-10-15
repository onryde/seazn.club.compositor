#!/usr/bin/env bash
# C00 item 5: full chain cost. Usage: run-item5.sh <720|1080> [seconds=290] [overlay-url]
. "$(dirname "$0")/env.sh"
H=$1; SECS=${2:-290}; OV=${3:-file://$SPIKE_DIR/overlay-test.html}
redact_all "item5-$H.log"
start_xvfb
echo "ITEM5 start $(date -u +%FT%TZ) h=$H secs=$SECS"
EXTRA=()
case "$OV" in file://*) ;; *) EXTRA=(--no-js-ready);; esac
"${AS[@]}" python3 "$SPIKE_DIR/chain.py" --height "$H" --seconds "$SECS" --overlay "$OV" "${EXTRA[@]}" \
  --snapshot "$OUT/item5-$H.ayuv" --snapshot-at 60 > >(python3 -u "$SPIKE_DIR/redact.py" > "$OUT/item5-$H.raw") 2>&1 &
CP=$!   # the runuser/python pid itself (process substitution keeps $! on the chain, not the redactor)
sleep 15
python3 "$SPIKE_DIR/proc_sample.py" --root-pid "$CP" --seconds $(( SECS - 20 )) --tag "item5-$H" > "$OUT/item5-$H.proc" 2>&1
wait "$CP"
grep -a -E "CHAINSTAT" "$OUT/item5-$H.raw" | tail -3
grep -a RESULT "$OUT/item5-$H.raw"
grep -a PROCSUMMARY "$OUT/item5-$H.proc"
if [ "$H" = 720 ]; then W=1280; else W=1920; fi
raw_to_png "$OUT/item5-$H.ayuv" ayuv "$W" "$H" "$OUT/item5-$H.png"
echo "ITEM5 end $(date -u +%FT%TZ)"
