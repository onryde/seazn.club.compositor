#!/usr/bin/env bash
# The soak driver (C15): the harness Machine's main process, under tini (harness/soak/Dockerfile).
# It runs MediaMTX, fake-camera and fake-control on this Machine, waits for the compositor Machine's
# first live beat, drives the schedule (schedule.jq: a camera dropout every 5 minutes, a destination
# disconnect at the middle of each third of the drive), ends the run, takes the compositor's log
# from the job, runs the checker, and writes /out/done. soak.yml polls /out/status.json, uploads
# /out/compositor.log (then /out/log-ready) once /out/ended exists, and fetches the outputs after
# /out/done.
#
# Environment (the Machine's; set by soak.yml):
#   SESSION_ID, JOB_TOKEN   the run's (JOB_TOKEN is random per run and never printed or written)
#   SOAK_DRIVE_MIN          the drive, in minutes (the verdict's duration)
#   SOAK_DEADLINE_MS        the compositor's RELAY_DEADLINE_AT, epoch ms (a guard: the drive must end
#                           before it)
#   SOAK_HARD_STOP_MS       epoch ms: this process exits by then whatever happened, so the Machine
#                           stops even if the job's cleanup never runs
#   SOAK_PAGE               test (C14's overlay-test.html, served by fake-control: plumbing) or real
#   SOAK_OVERLAY_URL        real only: the production overlay page's full URL, which fake-control
#                           hands the compositor unchanged; the page source is this one input
#   CAMERA_PATTERN          fake-camera's picture (ball, snow or noise; default ball)
#   FLY_MACHINE_ID, FLY_APP_NAME   set by the platform: this Machine's private name
#
# Every scheduled action is a line of /out/actions.jsonl with its wall time (ms, this Machine's
# clock: the clock the heartbeats' received_ms and the recordings' segment names use too).
set -euo pipefail

# SOAK_ROOT and SOAK_OUT move the driver's files and its outputs (test-driver.sh, under stubs);
# the Machine uses the defaults.
SOAK="${SOAK_ROOT:-/soak}"
OUT="${SOAK_OUT:-/out}"
mkdir -p "$OUT/rec"
exec > >(tee -a "$OUT/driver.log") 2>&1

now_ms() {
  local t=$EPOCHREALTIME
  t=${t//[.,]/}
  echo $((10#$t / 1000))
}
log() { echo "soak-driver $(date -u +%H:%M:%S) $*"; }
status() {
  jq -cn --arg phase "$1" --arg note "${2:-}" --argjson at "$(now_ms)" \
    --argjson actions "$(wc -l <"$OUT/actions.jsonl" 2>/dev/null || echo 0)" \
    --argjson beats "$(wc -l <"$OUT/heartbeats.jsonl" 2>/dev/null || echo 0)" \
    '{phase: $phase, note: $note, at_ms: $at, actions: $actions, beats: $beats}' >"$OUT/status.json.tmp"
  mv "$OUT/status.json.tmp" "$OUT/status.json"
}
action() { jq -cn "$@" >>"$OUT/actions.jsonl"; }

: "${SESSION_ID:?}" "${JOB_TOKEN:?}" "${SOAK_DRIVE_MIN:?}" "${SOAK_DEADLINE_MS:?}" "${SOAK_HARD_STOP_MS:?}"
: "${FLY_MACHINE_ID:?}" "${FLY_APP_NAME:?}"
SOAK_PAGE="${SOAK_PAGE:-test}"
CAMERA_PATTERN="${CAMERA_PATTERN:-ball}"
case "$SOAK_PAGE" in
  test) [ -z "${SOAK_OVERLAY_URL:-}" ] || { log "FAIL SOAK_OVERLAY_URL is set on the test page"; exit 64; } ;;
  real) [[ "${SOAK_OVERLAY_URL:-}" =~ ^https?://[^/?#]+/[^?#]*\?[^#]+$ ]] || { log "FAIL SOAK_PAGE=real needs SOAK_OVERLAY_URL with a query"; exit 64; } ;;
  *) log "FAIL SOAK_PAGE must be test or real"; exit 64 ;;
esac
[[ "$SOAK_DRIVE_MIN" =~ ^[0-9]+$ ]] || { log "FAIL SOAK_DRIVE_MIN must be whole minutes"; exit 64; }
HOST="$FLY_MACHINE_ID.vm.$FLY_APP_NAME.internal"
DRIVE_S=$((SOAK_DRIVE_MIN * 60))
soak() { jq -r --arg k "$1" '.[$k]' "$SOAK/soak.json"; }
PLAN="$(jq -c -n -L "$SOAK" --slurpfile soak "$SOAK/soak.json" --argjson d "$DRIVE_S" \
  'include "schedule"; soak_plan($soak[0]; $d)')"
: >"$OUT/actions.jsonl"
: >"$OUT/heartbeats.jsonl"
: >"$OUT/emit.jsonl"
status booting "plan $(jq -c '{dropouts: (.dropouts | length), disconnects}' <<<"$PLAN")"
log "boot: drive $SOAK_DRIVE_MIN min, page $SOAK_PAGE, camera $CAMERA_PATTERN, host $HOST, plan $(jq -c '{dropouts: (.dropouts | length), disconnects}' <<<"$PLAN")"
log "disk: $(df -h "$OUT" | tail -n 1 | awk '{print $2" total, "$4" free"}')"

# The SRT passphrase: made here, handed to MediaMTX, fake-camera and fake-control (which puts it in
# the session the compositor fetches) through the environment only.
HARNESS_SRT_PASSPHRASE="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
export HARNESS_SRT_PASSPHRASE

pids=()
done_written=0
stopping=0
finish() {
  local rc=$?
  set +e
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done
  if [ "$done_written" -eq 0 ]; then
    status error "the driver exited ($rc) before done"
    jq -cn --argjson rc "$rc" '{ok: false, driver_exit: $rc}' >"$OUT/done"
    # Keep /out for the job's fetch (it reads phase error and fetches at once), unless the Machine is
    # being stopped: the hard stop still bounds it.
    if [ "$stopping" -eq 0 ]; then
      log "error: holding /out for the job until the hard stop"
      while [ "$(now_ms)" -lt "$SOAK_HARD_STOP_MS" ]; do sleep 2; done
    fi
  fi
}
trap finish EXIT
trap 'stopping=1; log "TERM: stopping"; exit 143' TERM INT

# The dest disconnect's firewall: whichever iptables backend this kernel takes, for each family.
FW4=""
FW6=""
for b in iptables-nft iptables-legacy; do
  if "$b" -w -L INPUT -n >/dev/null 2>&1; then FW4="$b"; break; fi
done
for b in ip6tables-nft ip6tables-legacy; do
  if "$b" -w -L INPUT -n >/dev/null 2>&1; then FW6="$b"; break; fi
done
log "firewall: ipv4 ${FW4:-none}, ipv6 ${FW6:-none}"

# The scripted score timeline the test page shows (fake-control's --score): an event every 3 s, for
# the drive and the end, so the overlay repaints through the whole run (harness/pages/score.json's
# pace; its 50 events would end at 150 s).
jq -n --argjson n $(((DRIVE_S + 900) / 3)) '{events: [range(1; $n + 1) | {index: ., offset_ms: (. * 3000)}]}' >"$OUT/score.json"
# The session fake-control hands out: the vendored composed one, with the production page's URL when
# there is one (fake-control rewrites its origin to itself, so it is passed as the overlay origin
# and stays unchanged).
if [ "$SOAK_PAGE" = real ]; then
  jq --arg u "$SOAK_OVERLAY_URL" '.overlayUrl = $u' "$SOAK/session-composed-720p.json" >"$OUT/session.json"
  OVERLAY_ORIGIN="$(sed -E 's#^(https?://[^/?#]+).*#\1#' <<<"$SOAK_OVERLAY_URL")"
else
  cp "$SOAK/session-composed-720p.json" "$OUT/session.json"
  OVERLAY_ORIGIN="http://$HOST:7000"
fi

TZ=UTC MTX_PATHS_CAM_SRTREADPASSPHRASE="$HARNESS_SRT_PASSPHRASE" MTX_PATHS_CAM_SRTPUBLISHPASSPHRASE="$HARNESS_SRT_PASSPHRASE" \
  env -u JOB_TOKEN -u SESSION_ID mediamtx "$SOAK/mediamtx.yml" >"$OUT/mediamtx.log" 2>&1 &
pids+=($!)
mtx_pid=$!
XDG_CACHE_HOME=/tmp env -u JOB_TOKEN -u SESSION_ID fake-camera --emit "$OUT/emit.jsonl" --listen 127.0.0.1:7001 --srt-host 127.0.0.1 \
  --srt-port 8890 --stream-id publish:cam --size 1280x720 --pattern "$CAMERA_PATTERN" \
  >"$OUT/fake-camera.log" 2>&1 &
pids+=($!)
fake-control --fixture "$OUT/session.json" --page "$SOAK/pages/overlay-test.html" --score "$OUT/score.json" \
  --beats "$OUT/heartbeats.jsonl" --listen "[::]:7000" --input-origin "srt://$HOST:8890" \
  --target-origin "rtmp://$HOST:1935" --overlay-origin "$OVERLAY_ORIGIN" --stream-id read:cam --stream-key dest \
  >"$OUT/fake-control.log" 2>&1 &
pids+=($!)
control() { curl -fsS -m 10 -X "$1" "http://127.0.0.1:7000$2"; }
# fake-camera's control's HTTP status, as a number (0 when nothing answered).
camera() {
  local code
  code="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:7001$1" 2>/dev/null)" || true
  [[ "$code" =~ ^[0-9]{3}$ ]] || code=0
  echo $((10#$code))
}
for _ in $(seq 1 "$(soak control_wait_s)"); do control GET /healthz >/dev/null 2>&1 && break; sleep 1; done
control GET /healthz >/dev/null 2>&1 || { log "FAIL fake-control never answered"; tail -n 20 "$OUT/fake-control.log"; exit 1; }

# The last beat's receipt time (LAST_BEAT_MS), kept when the file's last line is still being written.
LAST_BEAT_MS=0
read_last_beat() {
  local v
  v="$(tail -n 1 "$OUT/heartbeats.jsonl" 2>/dev/null | jq -er '.received_ms' 2>/dev/null)" || return 0
  [[ "$v" =~ ^[0-9]+$ ]] && LAST_BEAT_MS="$v"
  return 0
}

# Live: a heartbeat whose programmePhase is live.
status waiting_live
live_deadline=$(($(now_ms) + $(soak live_wait_s) * 1000))
until grep -q '"programmePhase":"live"' "$OUT/heartbeats.jsonl"; do
  if [ "$(now_ms)" -ge "$live_deadline" ]; then
    log "FAIL no live beat within $(soak live_wait_s) s ($(wc -l <"$OUT/heartbeats.jsonl") beats)"
    break
  fi
  sleep 1
done
went_live=0
grep -q '"programmePhase":"live"' "$OUT/heartbeats.jsonl" && went_live=1
control POST /control/timeline-start >/dev/null || log "timeline-start refused"
start_ms="$(now_ms)"
end_ms=$((start_ms + DRIVE_S * 1000))
ending_budget_ms=$(($(soak end_wait_s) * 1000))
if [ $((end_ms + ending_budget_ms)) -gt "$SOAK_DEADLINE_MS" ]; then
  log "WARN the drive ends $(((end_ms + ending_budget_ms - SOAK_DEADLINE_MS) / 1000)) s after the compositor's deadline allows"
fi

# The schedule, in time order: [{kind, slot, at_s}], dropouts and disconnects (never overlapping:
# schedule.jq refuses a plan where they would).
mapfile -t steps < <(jq -c '[(.dropouts | to_entries[] | {kind: "dropout", slot: .key, at_s: .value}),
  (.disconnects | to_entries[] | {kind: "disconnect", slot: .key, at_s: .value})] | sort_by(.at_s)[]' <<<"$PLAN")
lost_beats_ms=$(($(soak lost_beats_end_s) * 1000))
ended_early=""
if [ "$went_live" -eq 1 ]; then
  action --argjson at "$start_ms" --argjson d "$DRIVE_S" '{kind: "start", at_ms: $at, drive_s: $d}'
  read_last_beat
  log "live: the drive starts, $DRIVE_S s, ${#steps[@]} actions"
else
  ended_early="the compositor never went live"
fi

# Sleeps until <ms>, watching the beats: false (and ended_early set) when they stopped.
wait_until() {
  local target="$1" n lb
  while :; do
    n="$(now_ms)"
    [ "$n" -ge "$target" ] && return 0
    if [ "$n" -ge "$SOAK_HARD_STOP_MS" ]; then
      ended_early="the hard stop came before the drive ended"
      return 1
    fi
    read_last_beat
    lb="$LAST_BEAT_MS"
    if [ $((n - lb)) -gt "$lost_beats_ms" ]; then
      ended_early="no beat for $(((n - lb) / 1000)) s"
      return 1
    fi
    if [ $((n / 60000)) -ne "${last_status_min:-0}" ]; then
      last_status_min=$((n / 60000))
      status driving "minute $(((n - start_ms) / 60000)) of $SOAK_DRIVE_MIN"
    fi
    sleep "$(awk -v d=$((target - n)) 'BEGIN { s = d / 1000; if (s > 5) s = 5; printf "%.3f", s }')"
  done
}

# The DROP rule's packets for one family (`$1`: the backend), or 0.
dropped() {
  [ -n "$1" ] || { echo 0; return; }
  "$1" -w -L INPUT -v -n -x 2>/dev/null | awk '$3 == "DROP" && /dpt:1935/ { s += $1 } END { print s + 0 }'
}
dropout() {
  local slot="$1" planned="$2" at p r end
  at="$(now_ms)"
  p="$(camera /pause)"
  sleep "$(soak dropout_pause_s)"
  r="$(camera /resume)"
  end="$(now_ms)"
  action --argjson slot "$slot" --argjson planned "$planned" --argjson at "$at" --argjson end "$end" \
    --argjson p "$p" --argjson r "$r" \
    '{kind: "dropout", slot: $slot, planned_ms: $planned, at_ms: $at, end_ms: $end, ok: ($p == 200 and $r == 200), pause: $p, resume: $r}'
  log "dropout $slot: pause $p, resume $r"
}
disconnect() {
  local slot="$1" planned="$2" at end ok=true fams=() n4 n6 fw
  at="$(now_ms)"
  for fw in "$FW4" "$FW6"; do
    if [ -n "$fw" ] && "$fw" -w -I INPUT -p tcp --dport 1935 -j DROP; then fams+=("$fw"); else ok=false; fi
  done
  sleep "$(soak disconnect_hold_s)"
  n4="$(dropped "$FW4")"
  n6="$(dropped "$FW6")"
  for fw in "${fams[@]}"; do "$fw" -w -D INPUT -p tcp --dport 1935 -j DROP || ok=false; done
  end="$(now_ms)"
  action --argjson slot "$slot" --argjson planned "$planned" --argjson at "$at" --argjson end "$end" \
    --argjson ok "$ok" --argjson n4 "$n4" --argjson n6 "$n6" --arg fams "${fams[*]}" \
    '{kind: "disconnect", slot: $slot, planned_ms: $planned, at_ms: $at, end_ms: $end, ok: $ok,
      dropped_packets: ($n4 + $n6), dropped_ipv4: $n4, dropped_ipv6: $n6,
      families: ($fams | split(" ") | map(select(. != "") | if startswith("ip6") then "ipv6" else "ipv4" end))}'
  log "disconnect $slot: ok $ok, dropped ipv4 $n4, ipv6 $n6 (${fams[*]:-no rule})"
}

if [ -z "$ended_early" ]; then
  status driving "minute 0 of $SOAK_DRIVE_MIN"
  for s in "${steps[@]}"; do
    kind="$(jq -r .kind <<<"$s")"
    slot="$(jq -r .slot <<<"$s")"
    planned=$((start_ms + $(jq -r .at_s <<<"$s") * 1000))
    wait_until "$planned" || break
    "$kind" "$slot" "$planned"
  done
  [ -n "$ended_early" ] || wait_until "$end_ms" || true
fi

# The end: the request, then the compositor's stopped beat.
end_request_ms="$(now_ms)"
[ "$went_live" -eq 1 ] && action --argjson at "$end_request_ms" --arg why "$ended_early" \
  '{kind: "end", at_ms: $at} + (if $why == "" then {} else {ended_early: $why} end)'
log "end requested${ended_early:+ early: $ended_early}"
status ending "${ended_early}"
control POST '/control/desired?state=ending' >/dev/null || log "the end request was refused"
stop_deadline=$(($(now_ms) + ending_budget_ms))
until grep -q '"state":"stopped"' "$OUT/heartbeats.jsonl"; do
  [ "$(now_ms)" -ge "$stop_deadline" ] && { log "no stopped beat within $(soak end_wait_s) s"; break; }
  sleep 1
done
control GET /control/timeline >"$OUT/timeline.json" || echo '{"start_wall_ms":0,"delay_ms":0,"events":[]}' >"$OUT/timeline.json"
control GET /control/state >"$OUT/control-state.json" || echo '{}' >"$OUT/control-state.json"
log "control state: $(jq -c '{session_fetches, beats, other_session_requests, unauthorized_requests}' "$OUT/control-state.json")"

# The compositor's log comes from the job (the platform's log stream; this Machine cannot read it).
jq -cn --argjson at "$end_request_ms" '{end_request_ms: $at}' >"$OUT/ended"
status awaiting_log
log_deadline=$(($(now_ms) + $(soak log_wait_s) * 1000))
until [ -f "$OUT/log-ready" ]; do
  if [ "$(now_ms)" -ge "$log_deadline" ]; then
    log "no compositor log within $(soak log_wait_s) s: checking without one"
    : >"$OUT/compositor.log"
    break
  fi
  sleep 2
done
[ -f "$OUT/compositor.log" ] || : >"$OUT/compositor.log"

# The recordings closed (MediaMTX's last segment ends on its own exit), the fakes stopped, then the
# checker.
for p in "${pids[@]}"; do [ "$p" = "$mtx_pid" ] || kill "$p" 2>/dev/null || true; done
# TERM, not INT: a non-interactive shell's background jobs start with SIGINT ignored. MediaMTX
# closes its recordings on TERM; a bounded wait, then KILL.
kill -TERM "$mtx_pid" 2>/dev/null || true
for _ in $(seq 1 30); do kill -0 "$mtx_pid" 2>/dev/null || break; sleep 1; done
kill -KILL "$mtx_pid" 2>/dev/null || true
wait "$mtx_pid" 2>/dev/null || true
status checking
checker_exit=0
XDG_CACHE_HOME=/tmp env -u JOB_TOKEN -u SESSION_ID -u HARNESS_SRT_PASSPHRASE checker --recordings "$OUT/rec" --emit "$OUT/emit.jsonl" --heartbeats "$OUT/heartbeats.jsonl" \
  --timeline "$OUT/timeline.json" --compositor-log "$OUT/compositor.log" --end-request-ms "$end_request_ms" \
  --out "$OUT/report.json" >"$OUT/checker.log" 2>&1 || checker_exit=$?
tail -n 5 "$OUT/checker.log"

# No credential in any file this run wrote (the recordings are binary and hold none: skipped). The
# values go to grep on a pipe, never in argv.
scanned=0
leaks=0
mapfile -t files < <(find "$OUT" -path "$OUT/rec" -prune -o -type f -print)
scanned="${#files[@]}"
for name in JOB_TOKEN HARNESS_SRT_PASSPHRASE; do
  if printf '%s\n' "${!name}" | grep -qF -f - "${files[@]}"; then
    log "FAIL $name appears in /out"
    leaks=$((leaks + 1))
  fi
done
jq -cn --argjson s "$scanned" --argjson l "$leaks" '{scanned: $s, leaks: $l}' >"$OUT/secrets.json"
log "secrets checked: $scanned files, $leaks leaks"

jq -cn --argjson c "$checker_exit" --argjson live "$went_live" --arg why "$ended_early" --argjson l "$leaks" \
  '{ok: ($l == 0), checker_exit: $c, went_live: ($live == 1), ended_early: (if $why == "" then null else $why end), leaks: $l}' \
  >"$OUT/done"
done_written=1
status done "checker exit $checker_exit"
log "done: checker exit $checker_exit; waiting for the job to fetch /out"

# Stay up for the job's fetch, then stop (the Machine stops with this process): a lost cleanup
# never leaves the harness running past the hard stop.
while [ "$(now_ms)" -lt "$SOAK_HARD_STOP_MS" ]; do sleep 10; done
log "hard stop"
