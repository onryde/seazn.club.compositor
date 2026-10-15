#!/usr/bin/env bash
# C00 A/B: the same C09 encode of the phone feed, pushed DIRECT to YouTube (leg 1), then via CF Input B (leg 2).
# Usage: run-ab.sh <T1 epoch s> [leg_s=600] [gap_s=180]
# Leg 1 runs T1..T1+leg; leg 2 runs T1+leg+gap..T1+2*leg+gap. A push that exits early is retried up to
# 3 times, 2 s apart, and every attempt is logged. The only difference between the legs is the destination env.
. /spike/env.sh
T1=$1; LEG=${2:-600}; GAP=${3:-180}
start_xvfb
now() { echo "wall=$(date +%s.%3N) london=$(TZ=Europe/London date +%T.%3N)"; }
leg() {  # leg <tag> <out env> <start epoch> <stop epoch>
  local tag=$1 envname=$2 s=$3 e=$4 att=0 cp raw
  sleep $(( s - $(date +%s) ))
  echo "LEG $tag start $(now)"
  while :; do
    att=$((att + 1)); raw="$OUT/ab-$tag-a$att.raw"
    echo "LEG $tag attempt=$att $(now)"
    "${AS[@]}" python3 /spike/chain.py --height 720 --seconds 1800 --play-env SPIKE_FIX_SRT_PLAY_URL --out-env "$envname" \
       --x264-bitrate 4000 --vbv-ms 1000 --x264-opts nal-hrd=cbr:filler=1 \
       > >(python3 -u /spike/redact.py > "$raw") 2>&1 &
    cp=$!
    if [ $att = 1 ]; then
      ( sleep 15; python3 /spike/proc_sample.py --root-pid "$cp" --seconds $(( e - s - 30 )) --tag "ab-$tag" > "$OUT/ab-$tag.proc" 2>&1 ) &
    fi
    while kill -0 "$cp" 2>/dev/null && [ "$(date +%s)" -lt "$e" ]; do sleep 0.5; done
    kill -0 "$cp" 2>/dev/null && break
    wait "$cp" 2>/dev/null
    echo "LEG $tag attempt=$att EXITED_EARLY $(now) why=$(grep -a -o 'Connection error[^"\\]*' "$raw" | head -1)"
    [ $att -ge 4 ] && { echo "LEG $tag GAVE_UP after $att attempts"; break; }
    [ "$(date +%s)" -ge "$e" ] && break
    sleep 2
  done
  pkill -KILL -P "$cp" 2>/dev/null; kill -KILL "$cp" 2>/dev/null   # this leg's own runuser and its chain child only
  echo "LEG $tag stopped $(now)"
  wait "$cp" 2>/dev/null
}
leg L1direct SPIKE_YT_DIRECT_URL "$T1"                      $(( T1 + LEG ))
leg L2viaCF  SPIKE_B_RTMPS_URL   $(( T1 + LEG + GAP ))      $(( T1 + 2 * LEG + GAP ))
echo "AB done $(now)"
