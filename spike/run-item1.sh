#!/usr/bin/env bash
# C00 item 1: cefsrc on this Machine. Usage: run-item1.sh [stg-overlay-url]
. "$(dirname "$0")/env.sh"
redact_all item1.log
STG=${1:-}
start_xvfb
echo "== item1 start $(date -u +%FT%TZ) arch=$(uname -m) nproc=$(nproc) cpu=$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs) mem_mb=$(( $(grep MemTotal /proc/meminfo | awk '{print $2}') / 1024 ))"
cat /opt/gstcefsrc/BUILDINFO 2>/dev/null
gst-launch-1.0 --version | head -1
PAGE=file://$SPIKE_DIR/overlay-test.html

echo "== 1a the brief's literal command (caps framerate=30/1)"
timeout 40 "${AS[@]}" gst-launch-1.0 -e cefsrc url="$PAGE" num-buffers=900 ! video/x-raw,width=1920,height=1080,framerate=30/1 \
  ! fpsdisplaysink video-sink=fakesink text-overlay=false sync=true -v > "$OUT/1a.raw" 2>&1
echo "1a exit=$?"; grep -a -E "ERROR|not-negotiated|negotiat" "$OUT/1a.raw" | head -5

echo "== 1b corrected: max-video-framerate=30/1, caps without framerate; 900 buffers; pidstat + tree sampler"
"${AS[@]}" gst-launch-1.0 -e cefsrc url="$PAGE" max-video-framerate=30/1 num-buffers=900 ! video/x-raw,width=1920,height=1080 \
  ! fpsdisplaysink video-sink=fakesink text-overlay=false sync=true -v > "$OUT/1b.raw" 2>&1 &
GP=$!
sleep 4
pidstat -u -r -h -C "gstcefsub|gst-launch" 1 25 > "$OUT/1b.pidstat" 2>&1 &
python3 "$SPIKE_DIR/proc_sample.py" --root-pid "$GP" --seconds 25 --tag 1b > "$OUT/1b.proc" 2>&1
wait "$GP"; echo "1b exit=$?"
grep -a "last-message" "$OUT/1b.raw" | tail -2 | sed -E 's/.*last-message = //'
grep -a PROCSUMMARY "$OUT/1b.proc"
python3 - "$OUT/1b.pidstat" <<'PY'
import sys, collections
rows = [l.split() for l in open(sys.argv[1]) if l.strip() and not l.startswith(('#', 'Linux'))]
# -h format: Time UID PID %usr %system %guest %wait %CPU CPU minflt/s majflt/s VSZ RSS %MEM Command
per_t = collections.defaultdict(lambda: [0.0, 0])
for r in rows:
    try:
        t, cpu, rss = r[0], float(r[7]), int(r[12])
    except (ValueError, IndexError):
        continue
    per_t[t][0] += cpu; per_t[t][1] += rss
vals = list(per_t.values())
if vals:
    print("PIDSTAT samples=%d avg_cpu_pct=%.1f max_cpu_pct=%.1f avg_rss_mb=%.0f max_rss_mb=%.0f procs=%s" % (
        len(vals), sum(v[0] for v in vals)/len(vals), max(v[0] for v in vals),
        sum(v[1] for v in vals)/len(vals)/1024, max(v[1] for v in vals)/1024,
        sorted({r[-1] for r in rows if len(r) > 13})))
else:
    print("PIDSTAT samples=0 (FAIL: nothing sampled)")
PY

echo "== 1c readiness positive + alpha + composite (listen-for-js-signals=true, page signals at 500 ms)"
GST_DEBUG="cefsrc:4,cefconsole:4" "${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "$PAGE" --js-ready --composite --seconds 30 \
  --dump-dir "$OUT" --tag 1c > "$OUT/1c.raw" 2>&1
grep -a RESULT "$OUT/1c.raw"
echo "1c ready log lines: $(grep -a -c 'js signal processed successfully: ready' "$OUT/1c.raw")"
grep -a -E "js signal processed|seazn:" "$OUT/1c.raw" | sed -E 's/^([0-9:.]+) .*(js signal.*|seazn:.*)$/\1 \2/' | head -5
raw_to_png "$OUT/1c-raw.bgra" bgra 1920 1080 "$OUT/1c-raw.png"
raw_to_png "$OUT/1c-mixed.bgra" bgra 1920 1080 "$OUT/1c-mixed.png"

echo "== 1d readiness NEGATIVE (page never signals, listen-for-js-signals=true)"
GST_DEBUG="cefsrc:4,cefconsole:4" "${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "$PAGE?noready=1" --js-ready --seconds 5 \
  --wait-first 15 --tag 1d > "$OUT/1d.raw" 2>&1
grep -a RESULT "$OUT/1d.raw"
echo "1d ready log lines: $(grep -a -c 'js signal processed successfully: ready' "$OUT/1d.raw")  (expect 0)"
echo "1d page ready-attempt lines: $(grep -a -c 'seazn:ready-attempt' "$OUT/1d.raw")  (expect 0)"
pgrep -f cef_probe.py >/dev/null && { echo "1d probe process still alive after exit"; pkill -9 -f cef_probe.py; } || echo "1d probe process gone"
pkill -9 -f gstcefsubprocess 2>/dev/null; sleep 1

echo "== 1e console channel without the JS router (listen-for-js-signals=false)"
GST_DEBUG="cefsrc:4,cefconsole:4" "${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "$PAGE" --seconds 6 --tag 1e > "$OUT/1e.raw" 2>&1
grep -a RESULT "$OUT/1e.raw" | python3 -c 'import sys,json; r=json.loads(sys.stdin.read()[7:]); print("1e first_buffer_s", r["first_buffer_s"], "buffers", r["buffers"])'
echo "1e cefconsole lines: attempt=$(grep -a -c 'seazn:ready-attempt' "$OUT/1e.raw") ack=$(grep -a -c 'seazn:ready-ack' "$OUT/1e.raw") nofn=$(grep -a -c 'seazn:no-gstSendMsg' "$OUT/1e.raw")"
grep -a 'seazn:ready-attempt' "$OUT/1e.raw" | head -1 | cut -c1-160

echo "== 1g nice + oom via GST_CEF_SUBPROCESS_PATH wrapper"
GST_CEF_SUBPROCESS_PATH="$SPIKE_DIR/cef-nice.sh" "${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "$PAGE" --seconds 8 --tag 1g > "$OUT/1g.raw" 2>&1 &
PP=$!
sleep 6
for p in $(pgrep -f gstcefsubprocess); do
  printf "1g pid=%s ni=%s type=%s oom_adj=%s\n" "$p" "$(ps -o ni= -p "$p" | xargs)" "$(tr '\0' '\n' < /proc/$p/cmdline | grep -a -m1 -- '--type=' | cut -d= -f2)" "$(cat /proc/$p/oom_score_adj)"
done
ps -o ni= -p "$PP" | xargs -I{} echo "1g host process ni={}"
wait "$PP"; grep -a RESULT "$OUT/1g.raw" | python3 -c 'import sys,json; r=json.loads(sys.stdin.read()[7:]); print("1g buffers", r["buffers"], "fps", r.get("fps_arrival_avg"))'

if [ -n "$STG" ]; then
  echo "== 1f real stg overlay page (no readiness signal today), 30 s, tree sampler"
  "${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "$STG" --seconds 30 --wait-first 30 --dump-dir "$OUT" --dump-at 20 --tag 1f > "$OUT/1f.raw" 2>&1 &
  PP=$!
  sleep 8
  python3 "$SPIKE_DIR/proc_sample.py" --root-pid "$PP" --seconds 25 --tag 1f > "$OUT/1f.proc" 2>&1
  wait "$PP"
  grep -a RESULT "$OUT/1f.raw"; grep -a PROCSUMMARY "$OUT/1f.proc"
  raw_to_png "$OUT/1f-raw.bgra" bgra 1920 1080 "$OUT/1f-raw.png"
fi
echo "== item1 end $(date -u +%FT%TZ)"
