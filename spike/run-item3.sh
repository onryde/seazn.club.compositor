#!/usr/bin/env bash
# C00 item 3: SEI survival. Reader parses SEI per AU; raw TS of 20 s is cross-checked with ffmpeg.
. "$(dirname "$0")/env.sh"
TAG=${1:-sei}
redact_all "item3-$TAG.log"
rm -f "$OUT/$TAG.ts"
"${AS[@]}" python3 "$SPIKE_DIR/srt_reader.py" --tag "$TAG" --seconds 35 --dump-ts "$OUT/$TAG.ts" --dump-seconds 20
ls -l "$OUT/$TAG.ts" | awk '{print "ts bytes", $5}'
echo "ffprobe frames with SEI user-data side data:"
ffprobe -v error -select_streams v:0 -show_frames -show_entries frame=pict_type:frame_side_data=side_data_type "$OUT/$TAG.ts" 2>/dev/null \
  | grep -a -c -i "unregistered" | sed 's/^/  frames_with_unregistered_sei=/'
ffprobe -v error -select_streams v:0 -show_frames -show_entries frame=pict_type "$OUT/$TAG.ts" 2>/dev/null | grep -a -c "pict_type=" | sed 's/^/  frames_total=/'
echo "ffmpeg trace_headers (independent bitstream parser):"
ffmpeg -hide_banner -nostats -i "$OUT/$TAG.ts" -map 0:v:0 -c copy -bsf:v trace_headers -f null - 2>&1 \
  | grep -a -E "User Data Unregistered|uuid_iso_iec_11578" | sort | uniq -c | head -6
ffmpeg -hide_banner -nostats -i "$OUT/$TAG.ts" -map 0:v:0 -c copy -bsf:v trace_headers -f null - 2>&1 \
  | grep -a -o "SEAZN-C00-SEI-v1\|wall_ms=[0-9]*" | head -3
