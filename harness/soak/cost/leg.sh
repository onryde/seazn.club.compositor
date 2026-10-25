#!/usr/bin/env bash
# A cost leg's runner side (C15): soak.yml starts it in the background, in its own process group,
# once both Machines are launched; the destroy step stops the group. It:
#
# 1. puts in-compositor.sh on the compositor Machine (flyctl ssh sftp) and starts its sampler over
#    `flyctl ssh console -C` (one JSON line a second to --out's cost-samples.jsonl), restarted if
#    the session drops, until the compositor Machine is gone or --until-ms;
# 2. waits for the driver's drive start (the first line of the harness's /out/actions.jsonl);
# 3. at the drive's midpoint, kills the CEF renderer once (in-compositor.sh kill-renderer, over
#    `flyctl machine exec`), and writes its lines to cost-kill.jsonl with the planned time. A kill
#    that never reached the Machine (no `at_ms` line) is tried again, at most kill_tries times and
#    only while a try can land inside the verdict's window (soak.json cost_kill_late_max_s, less
#    the 15 s a try takes to reach the Machine); one that printed its targets never is. A try whose
#    reply was lost after the kill gets the first kill's lines back (in-compositor.sh's mark), so
#    the product sees one crash a leg either way.
#
# Writes under --out: cost-samples.jsonl, cost-samples.err, cost-kill.jsonl, cost-leg.log. Nothing
# here reads or writes a credential: flyctl takes FLY_API_TOKEN from the environment.
#
#   harness/soak/cost/leg.sh --app <app> --harness <id> --compositor <id> --out <dir> --until-ms <ms>
#
# LEG_POLL_S (default 5), LEG_KILL_TRIES (default 3) and LEG_KILL_WINDOW_S (default soak.json's
# cost_kill_late_max_s) move its pace (test-cost.sh, on a stub flyctl); the workflow sets none.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
arg() { # <name>: the value after --name
  local n="$1" i
  for ((i = 1; i < ${#ARGS[@]}; i++)); do
    if [ "${ARGS[i - 1]}" = "--$n" ]; then printf '%s' "${ARGS[i]}"; return 0; fi
  done
  echo "leg: missing --$n" >&2
  exit 64
}
ARGS=("$@")
APP="$(arg app)"
HID="$(arg harness)"
CID="$(arg compositor)"
OUT="$(arg out)"
UNTIL_MS="$(arg until-ms)"
POLL_S="${LEG_POLL_S:-5}"
KILL_TRIES="${LEG_KILL_TRIES:-3}"
KILL_WINDOW_MS=$((${LEG_KILL_WINDOW_S:-$(jq -r .cost_kill_late_max_s "$HERE/../soak.json")} * 1000))
KILL_LAND_MS=15000 # a try's exec reaches the Machine within this (flyctl's connect)
[[ "$UNTIL_MS" =~ ^[0-9]+$ ]] || { echo "leg: --until-ms is epoch ms" >&2; exit 64; }
mkdir -p "$OUT"
exec >>"$OUT/cost-leg.log" 2>&1

now_ms() { python3 -I -c 'import time; print(int(time.time() * 1000))'; }
log() { echo "cost-leg $(date -u +%H:%M:%S) $*"; }
gone() { # the compositor Machine is no longer running (or no longer listed)
  local state
  state="$(flyctl machine list -a "$APP" --json 2>/dev/null | jq -r --arg id "$CID" '(. // [])[] | select(.id == $id) | .state' 2>/dev/null || true)"
  case "$state" in "" | stopped | destroying | destroyed | failed) return 0 ;; esac
  return 1
}

# 1. The sampler.
put=0
while [ "$(now_ms)" -lt "$UNTIL_MS" ]; do
  if flyctl ssh sftp put "$HERE/in-compositor.sh" /tmp/seazn-cost.sh -a "$APP" --machine "$CID" -q >/dev/null 2>>"$OUT/cost-samples.err"; then
    put=1
    break
  fi
  gone && break
  sleep "$POLL_S"
done
if [ "$put" -eq 1 ]; then
  log "in-compositor.sh on the compositor Machine"
else
  log "FAIL in-compositor.sh never reached the compositor Machine"
fi
sampler_pid=""
if [ "$put" -eq 1 ]; then
  (
    starts=0
    while :; do
      left=$(((UNTIL_MS - $(now_ms)) / 1000))
      [ "$left" -gt 0 ] || break
      starts=$((starts + 1))
      echo "cost-leg $(date -u +%H:%M:%S) sampler start $starts (max ${left} s)"
      flyctl ssh console -a "$APP" --machine "$CID" -q -C "bash /tmp/seazn-cost.sh sample $left" \
        >>"$OUT/cost-samples.jsonl" 2>>"$OUT/cost-samples.err" </dev/null || true
      gone && { echo "cost-leg $(date -u +%H:%M:%S) sampler: the compositor Machine is gone"; break; }
      sleep 2
    done
  ) &
  sampler_pid=$!
fi
trap '[ -z "$sampler_pid" ] || kill "$sampler_pid" 2>/dev/null || true' EXIT

# 2. The drive's start.
start=""
while [ "$(now_ms)" -lt "$UNTIL_MS" ]; do
  line="$(flyctl machine exec "$HID" 'head -n 1 /out/actions.jsonl' -a "$APP" --timeout 20 2>/dev/null | grep -a '^{' | head -n 1 || true)"
  if jq -e '.kind == "start" and (.at_ms | type) == "number" and (.drive_s | type) == "number"' >/dev/null 2>&1 <<<"$line"; then
    start="$line"
    break
  fi
  gone && break
  sleep "$POLL_S"
done
if [ -z "$start" ]; then
  log "FAIL no drive start: no kill"
  [ -z "$sampler_pid" ] || wait "$sampler_pid" || true
  exit 0
fi
at="$(jq -r .at_ms <<<"$start")"
drive_s="$(jq -r .drive_s <<<"$start")"
planned=$((at + drive_s * 500))
log "drive start $at, $drive_s s: the kill at $planned"

# 3. The kill, at the midpoint.
while [ "$(now_ms)" -lt "$planned" ]; do
  w=$(((planned - $(now_ms)) / 1000))
  [ "$w" -le 0 ] && break
  [ "$w" -gt "$POLL_S" ] && w="$POLL_S"
  sleep "$w"
done
tries=0
while [ "$tries" -lt "$KILL_TRIES" ]; do
  if [ $(($(now_ms) + KILL_LAND_MS)) -gt $((planned + KILL_WINDOW_MS)) ]; then
    log "kill: no try left inside the window ($((KILL_WINDOW_MS / 1000)) s from the midpoint)"
    break
  fi
  tries=$((tries + 1))
  out="$(flyctl machine exec "$CID" 'bash /tmp/seazn-cost.sh kill-renderer' -a "$APP" --timeout 30 2>/dev/null | grep -a '^{' || true)"
  if grep -q '"at_ms"' <<<"$out"; then
    jq -c --argjson p "$planned" --argjson t "$tries" '. + {planned_ms: $p, try: $t}' <<<"$out" >>"$OUT/cost-kill.jsonl"
    log "kill: $(paste -sd ' ' - <<<"$out")"
    break
  fi
  log "kill try $tries reached nothing"
  gone && break
  sleep 2
done
grep -q '"at_ms"' "$OUT/cost-kill.jsonl" 2>/dev/null || log "FAIL the kill never ran"

[ -z "$sampler_pid" ] || wait "$sampler_pid" || true
log "done"
