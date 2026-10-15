#!/usr/bin/env bash
# C00 cold start: used as a Machine's boot command. Logs wall-clock marks; the first cefsrc buffer's
# wall time minus the local `fly machine start/run` time is the cold start.
. "$(dirname "$0")/env.sh"
redact_all coldstart.log
echo "COLD script_start_wall=$(date +%s.%N) uptime_s=$(cut -d' ' -f1 /proc/uptime)"
start_xvfb
"${AS[@]}" python3 "$SPIKE_DIR/cef_probe.py" --url "file://$SPIKE_DIR/overlay-test.html" --js-ready --seconds 2 --tag cold 2>/dev/null \
  | grep -a RESULT | python3 -c 'import sys,json; r=json.loads(sys.stdin.read()[7:]); print("COLD first_buffer_wall=%.3f first_buffer_after_probe_start_s=%s" % (r["first_buffer_wall"], r["first_buffer_s"]))'
echo "COLD done uptime_s=$(cut -d' ' -f1 /proc/uptime)"
exec sleep infinity
