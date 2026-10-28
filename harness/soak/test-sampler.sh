#!/usr/bin/env bash
# sampler.py (check (d), C15) on synthetic recordings, made here with ffmpeg in the shape MediaMTX
# writes (fMP4 segments of 10 s named by their UTC start):
#
# - the camera: black, a white 72 px square at row 240 moving 8 px a frame (fake-camera's `ball`),
#   and a 40-row strip of noise at the bottom, new every frame (the burned-in strip). So of the 18
#   bands of 40 rows, three move (rows 240-311 span bands 6 and 7, the strip is band 17) and 15 are
#   stable: the geometry, not the sampler, says so.
# - the programme: the same picture half a second later (latency), with a grey 400x40 bar at row
#   560 (band 14, stable) except from 12 s to 18 s, and with its last segment (30-40 s) missing.
# - a compressed soak.json: a "minute" of 6 s, a 30 s drive, sample offsets 2 and 4 s, a margin of
#   1 s; a 36 s drive (six "minutes"); a disconnect from 19.5 to 20.5 s.
#
# Expected, minute by minute: 0 and 1 on air; 2 off air (the bar is gone, and there is no error);
# 3 on air from its second offset (the first falls in the disconnect); 4 on air; 5 an error (no
# programme segment); the floor (median band difference) near zero where on air. Then: an all-noise
# camera has no stable band (every sample an error, never on air), a programme 8 luma brighter than
# the camera with no bar is a level shift (never on air), and a run with no start action is refused.
# Last, the cost legs' stills: a JPEG at each asked instant, taken at that instant, a missed one where
# there is no segment, and the refusals.
#
#   harness/soak/test-sampler.sh        (needs ffmpeg and python3; prints `checked: N`)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
command -v ffmpeg >/dev/null || { echo "FAIL test-sampler.sh needs ffmpeg"; exit 1; }
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT

checked=0
failures=0
pass() { checked=$((checked + 1)); echo "PASS $1"; }
fail() { checked=$((checked + 1)); failures=$((failures + 1)); echo "FAIL $1${2:+: $2}"; }

# Segments of 10 s from one 40 s render, renamed to their UTC starts from 12:00:00.
render() { # <dir> <filtergraph producing [v]>
  mkdir -p "$1"
  ffmpeg -v error -nostdin -f lavfi -i "color=black:s=1280x720:r=30:d=40" -f lavfi -i "color=white:s=72x72:r=30:d=40" \
    -f lavfi -i "nullsrc=s=1280x40:r=30:d=40" -filter_complex "$2" -map "[v]" \
    -c:v libx264 -preset ultrafast -g 30 -keyint_min 30 -sc_threshold 0 -pix_fmt yuv420p \
    -f segment -segment_time 10 -reset_timestamps 1 -segment_format mp4 "$1/out%d.mp4"
  local i=0
  for s in 00 10 20 30; do mv "$1/out$i.mp4" "$1/2026-10-24_12-00-$s-000000.mp4"; i=$((i + 1)); done
}
strip='[2:v]geq=lum=random(1)*255:cb=128:cr=128[s]'
render "$W/cam" "$strip;[0:v][1:v]overlay=x='mod(n*8,1208)':y=240[b];[b][s]overlay=x=0:y=680[v]"
render "$W/prog" "$strip;[0:v][1:v]overlay=x='mod((n+15)*8,1208)':y=240[b];[b][s]overlay=x=0:y=680[c];color=gray:s=400x40:r=30:d=40[bar];[c][bar]overlay=x=440:y=560:enable='not(between(t,12,18))'[v]"
rm "$W/prog/2026-10-24_12-00-30-000000.mp4"
render "$W/noise" "[0:v]geq=lum=random(1)*255:cb=128:cr=128[v]"
# The programme as the camera half a second later, 8 luma brighter everywhere, and no bar: a level
# shift, which the largest band difference alone would read as on air.
render "$W/shift" "$strip;[0:v][1:v]overlay=x='mod((n+15)*8,1208)':y=240[b];[b][s]overlay=x=0:y=680[c];[c]lutyuv=y='clip(val+8,0,255)'[v]"

t0=$(python3 -I -c 'import datetime; print(int(datetime.datetime(2026,10,24,12,0,0,tzinfo=datetime.timezone.utc).timestamp()*1000))')
jq '. + {sample_every_s: 6, sample_offsets_s: [2, 4], sample_margin_s: 1}' "$HERE/soak.json" >"$W/soak.json"
printf '%s\n' "{\"kind\":\"start\",\"at_ms\":$t0,\"drive_s\":36}" \
  "{\"kind\":\"disconnect\",\"slot\":0,\"at_ms\":$((t0 + 19500)),\"end_ms\":$((t0 + 20500))}" \
  "{\"kind\":\"end\",\"at_ms\":$((t0 + 36000))}" >"$W/actions.jsonl"

rc=0
python3 -I "$HERE/sampler.py" --programme "$W/prog" --camera "$W/cam" --actions "$W/actions.jsonl" --soak "$W/soak.json" \
  --out "$W/samples.jsonl" >"$W/sampler.out" 2>&1 || rc=$?
[ "$rc" -eq 0 ] && pass "the sampler ran (exit 0)" || fail "the sampler" "exit $rc: $(cat "$W/sampler.out")"
s() { jq -e -s "$1" "$W/samples.jsonl" >/dev/null 2>&1; }
s 'length == 6 and (map(.minute) == [0,1,2,3,4,5])' && pass "one sample a minute, 6 minutes" || fail "the minutes" "$(jq -c -s 'map(.minute)' "$W/samples.jsonl")"
s 'map(.differs) == [true, true, false, true, true, false]' && pass "on air 0, 1, 3, 4; off air 2; 5 unsampled" \
  || fail "the verdicts" "$(jq -c -s 'map({minute, differs, region_diff, error})' "$W/samples.jsonl")"
s '[.[0,1,2,3,4] | .stable_bands == 15 and .moving_bands == 3] | all' && pass "15 stable bands, 3 moving (the ball's two, the strip)" \
  || fail "the bands" "$(jq -c -s 'map({stable_bands, moving_bands})' "$W/samples.jsonl")"
s '.[2] | (has("error") | not) and .region_diff < 4' && pass "minute 2: no error, the bar's band reads clean" || fail "minute 2" "$(jq -c -s '.[2]' "$W/samples.jsonl")"
s ".[3].at_ms == $((t0 + 22000)) and .[0].at_ms == $((t0 + 2000))" && pass "minute 3 sampled at its second offset (the first is in the disconnect)" \
  || fail "minute 3's instant" "$(jq -c -s 'map(.at_ms)' "$W/samples.jsonl")"
s '.[5].error | test("no programme segment")' && pass "minute 5: no programme segment, an error" || fail "minute 5" "$(jq -c -s '.[5]' "$W/samples.jsonl")"

rc=0
python3 -I "$HERE/sampler.py" --programme "$W/prog" --camera "$W/noise" --actions "$W/actions.jsonl" --soak "$W/soak.json" \
  --out "$W/noise.jsonl" >/dev/null 2>&1 || rc=$?
jq -e -s '(map(.differs) | any | not) and ([.[0,1,2,3,4] | .error | test("no stable band")] | all)' "$W/noise.jsonl" >/dev/null 2>&1 \
  && pass "a noise camera: no stable band, never on air" || fail "the noise camera" "exit $rc; $(jq -c -s 'map({differs, error})' "$W/noise.jsonl" 2>/dev/null)"

s '[.[0,1,3,4] | .median_diff < 2] | all' && pass "on air: the floor (median_diff) stays under 2 where only the bar differs" \
  || fail "the floor" "$(jq -c -s 'map({minute, region_diff, median_diff})' "$W/samples.jsonl")"

rc=0
python3 -I "$HERE/sampler.py" --programme "$W/shift" --camera "$W/cam" --actions "$W/actions.jsonl" --soak "$W/soak.json" \
  --out "$W/shift.jsonl" >/dev/null 2>&1 || rc=$?
jq -e -s '(map(.differs) | any | not) and ([.[0,1,2,3,4] | .median_diff >= 4 and (.error | test("level shift"))] | all)' "$W/shift.jsonl" >/dev/null 2>&1 \
  && pass "a level-shifted programme with no overlay: never on air, a level-shift error" \
  || fail "the level shift" "exit $rc; $(jq -c -s 'map({differs, region_diff, median_diff, error})' "$W/shift.jsonl" 2>/dev/null)"

grep -v '"start"' "$W/actions.jsonl" >"$W/nostart.jsonl"
rc=0
python3 -I "$HERE/sampler.py" --programme "$W/prog" --camera "$W/cam" --actions "$W/nostart.jsonl" --soak "$W/soak.json" \
  --out "$W/x.jsonl" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] && pass "no start action: refused" || fail "no start action" "exit $rc"

# An instant past its segment's last frame (soak 40702117071, minutes 0 and 2: the instant 2-3 ms
# before the next segment's start, after the earlier one's last frame): the programme's first
# segment cut to 8 s and named 10 ms late, so minute 1's first instant (8 s) is 7.99 s into a segment
# whose last frame is at 7.967 s and which ends at 8 s. ffmpeg decodes nothing there. The minute is
# sampled at its second offset (10 s, in the next segment), as an instant in a scheduled action is.
mkdir -p "$W/edge"
cp "$W/prog/"*.mp4 "$W/edge/"
ffmpeg -v error -nostdin -i "$W/prog/2026-10-24_12-00-00-000000.mp4" -t 8 -c:v libx264 -preset ultrafast -g 30 \
  -pix_fmt yuv420p "$W/edge/2026-10-24_12-00-00-010000.mp4"
rm "$W/edge/2026-10-24_12-00-00-000000.mp4"
rc=0
python3 -I "$HERE/sampler.py" --programme "$W/edge" --camera "$W/cam" --actions "$W/actions.jsonl" --soak "$W/soak.json" \
  --out "$W/edge.jsonl" >/dev/null 2>&1 || rc=$?
jq -e -s ".[1] | .differs and (has(\"error\") | not) and .at_ms == $((t0 + 10000))" "$W/edge.jsonl" >/dev/null 2>&1 \
  && pass "an instant past its segment's last frame: the minute sampled at its next offset, on air" \
  || fail "the segment's tail" "exit $rc; $(jq -c -s '.[1]' "$W/edge.jsonl" 2>/dev/null)"
jq -e -s '.[5].error | test("no programme segment")' "$W/edge.jsonl" >/dev/null 2>&1 \
  && pass "every instant of a minute without a frame: still an error" || fail "minute 5 with the fallback" "$(jq -c -s '.[5]' "$W/edge.jsonl" 2>/dev/null)"

# The cost legs' stills (--stills/--still-at): a JPEG at each instant, at the programme's own size,
# taken at that instant (the bar shows at 5 s and is gone at 15 s: its region's mean luma says so),
# and a missed one where there is no segment (30-40 s).
luma() { # <jpeg>: the mean luma of the bar's region, 0-255
  ffmpeg -v error -nostdin -i "$1" -vf "crop=400:40:440:560,scale=1:1:flags=area" -f rawvideo -pix_fmt gray - | od -An -tu1 | tr -d ' \n'
}
rc=0
python3 -I "$HERE/sampler.py" --programme "$W/prog" --soak "$W/soak.json" --stills "$W/stills" \
  --still-at "bar:$((t0 + 5000)),nobar:$((t0 + 15000)),gone:$((t0 + 35000))" >"$W/stills.out" 2>&1 || rc=$?
n_jpg=$(find "$W/stills" -name '*.jpg' | wc -l | tr -d ' ')
if [ "$rc" -eq 0 ] && [ "$n_jpg" -eq 2 ] && grep -qx 'stills: 2 written' "$W/stills.out" \
  && grep -qF "stills: gone at $((t0 + 35000)): missed (no segment)" "$W/stills.out"; then
  pass "stills: 2 of 3 written (exit 0), the one with no segment named missed"
else fail "stills: the run" "exit $rc, $n_jpg jpg: $(cat "$W/stills.out")"; fi
dims="$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height -of csv=p=0 "$W/stills/bar-$((t0 + 5000)).jpg" 2>/dev/null || true)"
[ "$dims" = "mjpeg,1280,720" ] && pass "stills: a JPEG at the programme's size (1280x720)" || fail "stills: the format" "$dims"
on="$(luma "$W/stills/bar-$((t0 + 5000)).jpg" 2>/dev/null || echo x)" off="$(luma "$W/stills/nobar-$((t0 + 15000)).jpg" 2>/dev/null || echo x)"
[[ "$on" =~ ^[0-9]+$ && "$off" =~ ^[0-9]+$ ]] && [ "$on" -gt 100 ] && [ "$off" -lt 40 ] \
  && pass "stills: each at its instant (the bar's region reads $on with the bar at 5 s, $off without it at 15 s)" \
  || fail "stills: the instants" "bar $on, no bar $off"
rc=0
python3 -I "$HERE/sampler.py" --programme "$W/prog" --soak "$W/soak.json" --stills "$W/stills2" --still-at "gone:$((t0 + 35000))" \
  >"$W/stills2.out" 2>&1 || rc=$?
[ "$rc" -eq 1 ] && grep -qx 'stills: 0 written' "$W/stills2.out" && pass "stills: none written is exit 1" || fail "stills: none written" "exit $rc"
rc=0
python3 -I "$HERE/sampler.py" --programme "$W/prog" --soak "$W/soak.json" --stills "$W/stills3" --still-at "Bad:$t0" >"$W/stills3.out" 2>&1 || rc=$?
[ "$rc" -eq 64 ] && grep -qF 'stills: FAIL bad instant' "$W/stills3.out" && pass "stills: a bad instant is refused (64)" || fail "stills: a bad instant" "exit $rc"
rc=0
python3 -I "$HERE/sampler.py" --programme "$W/prog" --soak "$W/soak.json" --stills "$W/stills4" >"$W/stills4.out" 2>&1 || rc=$?
[ "$rc" -eq 2 ] && grep -qF -- '--stills and --still-at go together' "$W/stills4.out" && pass "stills: --stills without --still-at is a usage error" \
  || fail "stills: the pair" "exit $rc"

echo "checked: $checked, $failures failures"
[ "$checked" -gt 0 ] && [ "$failures" -eq 0 ]
