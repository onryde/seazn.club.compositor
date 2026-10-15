#!/usr/bin/env bash
# C00 owner session steps C+D: real-footage push to B, then stop/restart gaps of 5, 15, 30 s.
# Usage: run-cd.sh <T0 epoch s>  (C runs T0..T0+300; each stop is SIGKILL = a crashed compositor)
. /spike/env.sh
T0=$1
start_xvfb
seg() {  # seg <tag> <start epoch> <stop epoch>
  local tag=$1 s=$2 e=$3
  sleep $(( s - $(date +%s) ))
  echo "SEG $tag start_wall=$(date +%s.%3N) start_london=$(TZ=Europe/London date +%T.%3N)"
  "${AS[@]}" python3 /spike/chain.py --height 720 --seconds 900 --play-env SPIKE_FIX_SRT_PLAY_URL \
     --x264-bitrate 4000 --vbv-ms 1000 --x264-opts nal-hrd=cbr:filler=1 \
     $( [ "$tag" = C ] && echo "--snapshot $OUT/cd-C.ayuv --snapshot-at 150" ) \
     > >(python3 -u /spike/redact.py > "$OUT/cd-$tag.raw") 2>&1 &
  local cp=$!
  if [ "$tag" = C ]; then sleep 15; python3 /spike/proc_sample.py --root-pid "$cp" --seconds 270 --tag cd-C > "$OUT/cd-C.proc" 2>&1 & fi
  sleep $(( e - $(date +%s) ))
  pkill -KILL -f "[c]hain.py --height 720" ; local k=$(date +%s.%3N)
  echo "SEG $tag killed_wall=$k killed_london=$(TZ=Europe/London date +%T.%3N)"
  wait "$cp" 2>/dev/null
}
seg C   "$T0"            $(( T0 + 300 ))
seg D5  $(( T0 + 305 ))  $(( T0 + 365 ))
seg D15 $(( T0 + 380 ))  $(( T0 + 440 ))
seg D30 $(( T0 + 470 ))  $(( T0 + 530 ))
echo "CD done $(TZ=Europe/London date +%T)"
