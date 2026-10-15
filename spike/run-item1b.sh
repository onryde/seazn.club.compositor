#!/usr/bin/env bash
# C00 item 1, second pass: clean 30 s fps + CPU with readiness, alpha (raw + blended), the
# js-signals-beside-another-source repro, oom_score_adj as non-root, and the stg page's frames.
. "$(dirname "$0")/env.sh"
redact_all item1b.log
STG=${1:-}
start_xvfb
PAGE=file://$SPIKE_DIR/overlay-test.html
echo "== item1b start $(date -u +%FT%TZ)"

echo "== 1c2 readiness positive, cefsrc->appsink only, 30 s, tree sampler"
GST_DEBUG="cefsrc:4,cefconsole:4" "${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "$PAGE" --js-ready --seconds 30 \
  --dump-dir "$OUT" --tag 1c2 > "$OUT/1c2.raw" 2>&1 &
PP=$!
sleep 5
python3 "$SPIKE_DIR/proc_sample.py" --root-pid "$PP" --seconds 25 --tag 1c2 > "$OUT/1c2.proc" 2>&1
wait "$PP"
grep -a RESULT "$OUT/1c2.raw"; grep -a PROCSUMMARY "$OUT/1c2.proc"
echo "1c2 ready log lines: $(grep -a -c 'js signal processed successfully: ready' "$OUT/1c2.raw")"
grep -a -E "js signal processed|seazn:ready" "$OUT/1c2.raw" | sed -E 's/^([0-9:.]+) .*(js signal.*|seazn:.*)$/\1 \2/' | head -4
raw_to_png "$OUT/1c2-raw.bgra" bgra 1920 1080 "$OUT/1c2-raw.png"

echo "== 1h alpha through compositor over opaque black (no js signals)"
"${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "$PAGE" --composite --seconds 10 --dump-dir "$OUT" --tag 1h > "$OUT/1h.raw" 2>&1
grep -a RESULT "$OUT/1h.raw"
raw_to_png "$OUT/1h-mixed.bgra" bgra 1920 1080 "$OUT/1h-mixed.png"

echo "== 1i repro: listen-for-js-signals=true beside another live source (gst-launch)"
GST_DEBUG="*:2" timeout 25 "${AS[@]}" gst-launch-1.0 -e cefsrc url=$PAGE max-video-framerate=30/1 listen-for-js-signals=true num-buffers=90 \
  ! video/x-raw,format=BGRA,width=1920,height=1080 ! queue ! mix.sink_1 \
  videotestsrc is-live=true pattern=black num-buffers=90 ! video/x-raw,format=BGRA,width=1920,height=1080,framerate=30/1 ! mix.sink_0 \
  compositor name=mix background=black sink_1::zorder=1 ! video/x-raw,format=BGRA,width=1920,height=1080,framerate=30/1 ! fakesink > "$OUT/1i.raw" 2>&1
echo "1i exit=$?"
grep -a -v dbus "$OUT/1i.raw" | grep -a -E "ERROR|WARN.*(negotiat|flow|not-linked)|Execution ended|Got EOS|debug info" | head -6 | cut -c1-260

echo "== 1j raise a CEF child's oom_score_adj to 1000 as the non-root user"
"${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "$PAGE" --seconds 6 --tag 1j > "$OUT/1j.raw" 2>&1 &
PP=$!
sleep 4
for p in $(pgrep -u spike gstcefsubproces); do
  t=$(grep -a -o -- '--type=[a-z-]*' /proc/$p/cmdline | head -1)
  before=$(cat /proc/$p/oom_score_adj)
  "${AS[@]}" sh -c "echo 1000 > /proc/$p/oom_score_adj" 2>/dev/null && r=ok || r=refused
  echo "1j pid=$p $t before=$before write=$r after=$(cat /proc/$p/oom_score_adj)"
done
wait "$PP"

if [ -n "$STG" ]; then
  echo "== 1f2 stg overlay page, 40 s, last-frame dump"
  "${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "$STG" --seconds 40 --wait-first 30 --dump-dir "$OUT" --dump-at 1000 --tag 1f2 > "$OUT/1f2.raw" 2>&1
  grep -a RESULT "$OUT/1f2.raw"
  raw_to_png "$OUT/1f2-raw.bgra" bgra 1920 1080 "$OUT/1f2-raw.png"
fi
echo "== item1b end $(date -u +%FT%TZ)"
