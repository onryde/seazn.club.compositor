#!/usr/bin/env bash
# C00 owner session steps C+D: real-footage push to B, then stop/restart gaps of 5, 15, 30 s.
# Usage: run-cd.sh <T0 epoch s>  (C runs T0..T0+300; each stop is SIGKILL = a crashed compositor)
# A push that exits early (e.g. rtmp2sink "Socket I/O timed out") is retried up to 3 times, 2 s apart;
# every attempt is logged. This measures YouTube's gap tolerance, not the spike's lack of retry.
. /spike/env.sh
T0=$1
start_xvfb
now() { echo "wall=$(date +%s.%3N) london=$(TZ=Europe/London date +%T.%3N)"; }
seg() {  # seg <tag> <start epoch> <stop epoch>
  local tag=$1 s=$2 e=$3 att=0 cp raw
  sleep $(( s - $(date +%s) ))
  echo "SEG $tag start $(now)"
  while :; do
    att=$((att + 1)); raw="$OUT/cd-$tag-a$att.raw"
    echo "SEG $tag attempt=$att $(now)"
    "${AS[@]}" python3 /spike/chain.py --height 720 --seconds 900 --play-env SPIKE_FIX_SRT_PLAY_URL \
       --x264-bitrate 4000 --vbv-ms 1000 --x264-opts nal-hrd=cbr:filler=1 \
       $( [ "$tag" = C ] && echo "--snapshot $OUT/cd-C.ayuv --snapshot-at 150" ) \
       > >(python3 -u /spike/redact.py > "$raw") 2>&1 &
    cp=$!
    if [ "$tag" = C ] && [ $att = 1 ]; then
      ( sleep 15; python3 /spike/proc_sample.py --root-pid "$cp" --seconds 270 --tag cd-C > "$OUT/cd-C.proc" 2>&1 ) &
    fi
    while kill -0 "$cp" 2>/dev/null && [ "$(date +%s)" -lt "$e" ]; do sleep 0.5; done
    kill -0 "$cp" 2>/dev/null && break            # still pushing at the stop time
    wait "$cp" 2>/dev/null
    echo "SEG $tag attempt=$att EXITED_EARLY $(now) why=$(grep -a -o 'Connection error[^"\\]*' "$raw" | head -1)"
    [ $att -ge 4 ] && { echo "SEG $tag GAVE_UP after $att attempts"; break; }
    [ "$(date +%s)" -ge "$e" ] && break
    sleep 2
  done
  pkill -KILL -f "[c]hain.py --height 720"
  echo "SEG $tag killed $(now)"
  wait "$cp" 2>/dev/null
}
seg C   "$T0"            $(( T0 + 300 ))
seg D5  $(( T0 + 305 ))  $(( T0 + 365 ))
seg D15 $(( T0 + 380 ))  $(( T0 + 440 ))
seg D30 $(( T0 + 470 ))  $(( T0 + 530 ))
echo "CD done $(now)"
