#!/usr/bin/env bash
# Runs one harness case (C13): `harness/run-case.sh <case>`, with `harness/cases/<case>.sh`.
#
#   RELAY_IMAGE=<relay image> HARNESS_IMAGE=<harness image> harness/run-case.sh smoke
#
# 1. brings the stack up fresh (`down -v` first) with a random JOB_TOKEN, SESSION_ID and SRT
#    passphrase, exported for compose and never printed or put in argv;
# 2. waits for a heartbeat whose programmePhase is live, starts the score timeline, and runs the
#    case's `case_drive` (its controls and its duration);
# 3. asks the run to end (desiredState ending) and waits for the compositor to exit: the final
#    hold (`final_hold_ms`) and the drain (`drain_timeout_ms`) run after the request, and the
#    request only reaches the compositor on its next beat (plan amendment, Owed 5);
# 4. saves the logs, brings the stack down, and runs the checker on what MediaMTX recorded;
# 5. checks that no credential reached any log or output file;
# 6. exits with the case's own verdict (`case_verdict`, `jq` over out/<case>/report.json).
#
# A case file defines `case_drive` and `case_verdict`, and may set CASE_DEADLINE_S, CAMERA_SIZE,
# CAMERA_PATTERN and CHECKER_RECORDINGS. Its verdict calls `require <name> <jq filter>` (on
# report.json) and `require_run <name> <jq filter>` (on run.json) once a fact, and sets
# CASE_CHECKS to how many it calls: a verdict that ran fewer checks than it declares fails.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CASE="${1:?usage: run-case.sh <case>}"
CASE_FILE="$HERE/cases/$CASE.sh"
[ -f "$CASE_FILE" ] || { echo "run-case: no case file $CASE_FILE" >&2; exit 64; }
: "${RELAY_IMAGE:?RELAY_IMAGE names the relay image under test}"
: "${HARNESS_IMAGE:?HARNESS_IMAGE names the harness image}"
export RELAY_IMAGE HARNESS_IMAGE

OUT="$HERE/out/$CASE"
export HARNESS_OUT="$OUT"
HARNESS_UID="$(id -u)"
HARNESS_GID="$(id -g)"
export HARNESS_UID HARNESS_GID

# The run's credentials: random, exported for compose only. Never echoed, never in argv.
rand_hex() { od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'; }
JOB_TOKEN="hjt_$(rand_hex 24)"
SESSION_ID="$(python3 -I -c 'import uuid; print(uuid.uuid4())')"
HARNESS_SRT_PASSPHRASE="$(rand_hex 16)"
export JOB_TOKEN SESSION_ID HARNESS_SRT_PASSPHRASE
# The fixture's page token (contracts/fixtures/relay-job.v1/session-composed-720p.json): fixed, but
# a credential to the compositor, which must scrub it from every line it logs.
PAGE_TOKEN="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["pageToken"])' \
  "$HERE/../contracts/fixtures/relay-job.v1/session-composed-720p.json")"

# The case's knobs, before it is sourced.
CASE_DEADLINE_S=900
CHECKER_RECORDINGS="$OUT/rec"
CASE_CHECKS=0
LIVE_WAIT_S=180
EXIT_WAIT_S=120

compose() { docker compose -f "$HERE/compose.yml" "$@"; }
now_ms() { python3 -I -c 'import time; print(int(time.time() * 1000))'; }
log() { echo "run-case[$CASE]: $*"; }

# A control request inside the stack (no port reaches the host).
control() { compose exec -T fake-control curl -fsS -X "$1" "http://localhost:7000$2"; }

checks_run=0
failures=0
require() {
  checks_run=$((checks_run + 1))
  if jq -e "$2" "$OUT/report.json" >/dev/null 2>&1; then
    echo "PASS $1"
  else
    echo "FAIL $1 ($2)"
    failures=$((failures + 1))
  fi
}
require_run() {
  checks_run=$((checks_run + 1))
  if jq -e "$2" "$OUT/run.json" >/dev/null 2>&1; then
    echo "PASS $1"
  else
    echo "FAIL $1 ($2)"
    failures=$((failures + 1))
  fi
}

# shellcheck source=/dev/null
. "$CASE_FILE"

# A fresh stack and a fresh out/<case>.
compose down -v --remove-orphans >/dev/null 2>&1 || true
rm -rf "$OUT"
mkdir -p "$OUT/rec" "$CHECKER_RECORDINGS"
: >"$OUT/emit.jsonl"
: >"$OUT/heartbeats.jsonl"

RELAY_DEADLINE_AT="$(python3 -I -c 'import datetime,sys; print((datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=int(sys.argv[1]))).strftime("%Y-%m-%dT%H:%M:%SZ"))' "$CASE_DEADLINE_S")"
export RELAY_DEADLINE_AT

stack_up=0
collected=0
collect() {
  [ "$collected" -eq 1 ] && return 0
  collected=1
  local cid
  cid="$(compose ps -a -q compositor 2>/dev/null || true)"
  if [ -n "$cid" ]; then
    docker logs "$cid" >"$OUT/compositor.log" 2>&1 || true
  else
    : >"$OUT/compositor.log"
  fi
  compose logs --no-color mediamtx fake-camera fake-control >"$OUT/services.log" 2>&1 || true
}
teardown() {
  if [ "$stack_up" -eq 1 ]; then
    collect
    compose down -v --remove-orphans >/dev/null 2>&1 || true
    stack_up=0
  fi
}
trap teardown EXIT

log "up: relay $RELAY_IMAGE, harness $HARNESS_IMAGE, deadline $RELAY_DEADLINE_AT"
stack_up=1
compose up -d --quiet-pull

# The compositor's environment must not turn on CEF's or GStreamer's logs (Owed 52, F-013). Names
# only are read here: the values never leave docker.
cid="$(compose ps -q compositor)"
env_names="$(docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$cid" | cut -d= -f1)"
for v in GST_DEBUG GST_CEF_LOG_SEVERITY; do
  if printf '%s\n' "$env_names" | grep -qx "$v"; then
    log "FAIL the compositor's environment sets $v"
    exit 1
  fi
done

# Live: a heartbeat whose programmePhase is live.
deadline=$(($(date +%s) + LIVE_WAIT_S))
until grep -q '"programmePhase":"live"' "$OUT/heartbeats.jsonl"; do
  state="$(docker inspect --format '{{.State.Status}}' "$cid")"
  if [ "$state" = exited ] || [ "$(date +%s)" -ge "$deadline" ]; then
    log "FAIL no live heartbeat within $LIVE_WAIT_S s (compositor $state; $(wc -l <"$OUT/heartbeats.jsonl") beats)"
    collect
    tail -n 40 "$OUT/compositor.log" || true
    exit 1
  fi
  sleep 1
done
log "live after $(wc -l <"$OUT/heartbeats.jsonl" | tr -d ' ') beats"
control POST /control/timeline-start >/dev/null

case_drive

# The end: the request, then the compositor's own exit.
end_request_ms="$(now_ms)"
control POST '/control/desired?state=ending' >/dev/null
control GET /control/timeline >"$OUT/timeline.json"
deadline=$(($(date +%s) + EXIT_WAIT_S))
until [ "$(docker inspect --format '{{.State.Status}}' "$cid")" = exited ]; do
  if [ "$(date +%s)" -ge "$deadline" ]; then
    log "the compositor did not exit within $EXIT_WAIT_S s of the end request"
    break
  fi
  sleep 1
done
exit_ms="$(now_ms)"
read -r c_status c_code c_restarts <<<"$(docker inspect --format '{{.State.Status}} {{.State.ExitCode}} {{.RestartCount}}' "$cid")"
collect
compose down -v --remove-orphans >/dev/null 2>&1 || true
stack_up=0

jq -n --argjson end "$end_request_ms" --argjson exit_ms "$exit_ms" --arg status "$c_status" \
  --argjson code "$c_code" --argjson restarts "$c_restarts" --arg case "$CASE" \
  --slurpfile beats "$OUT/heartbeats.jsonl" \
  '{case: $case, end_request_ms: $end, exit_seen_ms: $exit_ms,
    compositor: {status: $status, exit_code: $code, restarts: $restarts},
    last_beat: ($beats | last | .body // null)}' >"$OUT/run.json"
log "compositor $c_status, exit $c_code, $c_restarts restarts, $(( (exit_ms - end_request_ms) / 1000 )) s after the end request"

# The checker, on what MediaMTX recorded (or what the case points it at).
checker_exit=0
docker run --rm --user "$HARNESS_UID:$HARNESS_GID" -e XDG_CACHE_HOME=/tmp \
  -v "$OUT:/out" -v "$CHECKER_RECORDINGS:/recordings:ro" "$HARNESS_IMAGE" \
  checker --recordings /recordings --emit /out/emit.jsonl --heartbeats /out/heartbeats.jsonl \
  --timeline /out/timeline.json --compositor-log /out/compositor.log \
  --end-request-ms "$end_request_ms" --out /out/report.json >"$OUT/checker.log" 2>&1 || checker_exit=$?
cat "$OUT/checker.log"
if [ ! -f "$OUT/report.json" ]; then
  log "FAIL the checker wrote no report (exit $checker_exit)"
  exit 1
fi
jq '{frames, duration_ms, fps_mean, pts_gaps: (.pts_gaps | length), segments: (.segments | length),
     output_outages: (.output_outages | length), strip_frames: .camera.strip_frames,
     slate_frames: .camera.slate_frames, regressions: .camera.counter_regressions,
     latency_ms: .camera.latency_ms, score_changes: (.overlay.score_index_changes | length),
     av_offset_ms: .audio.av_offset_ms, beats: .heartbeats.count, rungs: .heartbeats.rungs,
     restarts_without_crash: .graphics.restarts_without_crash, checker_failures}' "$OUT/report.json"

# No credential in any file the run produced. The values go to grep on a pipe, never in argv.
leaks=0
scanned="$(find "$OUT" -type f | wc -l | tr -d ' ')"
for name in JOB_TOKEN HARNESS_SRT_PASSPHRASE; do
  if printf '%s\n' "${!name}" | grep -rqF -f - "$OUT"; then
    log "FAIL $name appears in out/$CASE"
    leaks=$((leaks + 1))
  fi
done
# The page token: the compositor scrubs it from its log, and nothing else logs a query.
if printf '%s\n' "$PAGE_TOKEN" | grep -qF -f - "$OUT/compositor.log" "$OUT/services.log" "$OUT/checker.log"; then
  log "FAIL the page token appears in a log"
  leaks=$((leaks + 1))
fi
# SESSION_ID: never in the harness's own files; the compositor's log is reported, not judged.
for f in services.log checker.log emit.jsonl heartbeats.jsonl timeline.json run.json report.json; do
  if printf '%s\n' "$SESSION_ID" | grep -qF -f - "$OUT/$f"; then
    log "FAIL SESSION_ID appears in $f"
    leaks=$((leaks + 1))
  fi
done
sid_lines="$(printf '%s\n' "$SESSION_ID" | grep -cF -f - "$OUT/compositor.log" || true)"
log "secrets checked: $scanned files, $leaks leaks (SESSION_ID on $sid_lines compositor log lines, reported only)"
if [ "$scanned" -eq 0 ]; then
  log "FAIL no file was scanned for secrets"
  exit 1
fi

# The case's verdict.
echo "verdict ($CASE):"
case_verdict
if [ "$CASE_CHECKS" -le 0 ] || [ "$checks_run" -ne "$CASE_CHECKS" ]; then
  echo "FAIL the verdict ran $checks_run checks, the case declares $CASE_CHECKS"
  failures=$((failures + 1))
fi
if [ "$leaks" -gt 0 ]; then
  failures=$((failures + 1))
fi
if [ "$checker_exit" -ne 0 ]; then
  echo "FAIL the checker exited $checker_exit"
  failures=$((failures + 1))
fi
log "$checks_run checks, $failures failures"
[ "$failures" -eq 0 ]
