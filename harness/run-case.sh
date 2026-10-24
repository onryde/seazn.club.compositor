#!/usr/bin/env bash
# Runs one harness case (C13; extended by C14): `harness/run-case.sh <case> [variant]`, with
# `harness/cases/<case>.sh`.
#
#   RELAY_IMAGE=<relay image> HARNESS_IMAGE=<harness image> [CASE_RUN=<n>] harness/run-case.sh smoke
#
# Its outputs go to harness/out/<case>[-<variant>][-r<n>] (the smoke case's to out/smoke).
# 1. brings the stack up fresh (`down -v` first) with a random JOB_TOKEN, SESSION_ID and SRT
#    passphrase, exported for compose and never printed or put in argv;
# 2. waits for a heartbeat whose programmePhase is live, starts the score timeline, and runs the
#    case's `case_drive` (its controls and its duration);
# 3. asks the run to end (desiredState ending) and waits for the compositor to exit: the final
#    hold (`final_hold_ms`) and the drain (`drain_timeout_ms`) run after the request, and the
#    request only reaches the compositor on its next beat (plan amendment, Owed 5);
# 4. saves the logs, brings the stack down, and runs the checker on what MediaMTX recorded;
# 5. runs the case's own verdict (`case_verdict`, `jq` over out/<run>/report.json), and writes
#    out/<run>/verdict.json (what run-chaos.sh's table reads);
# 6. on EVERY exit, early ones too (the EXIT trap): checks that no credential reached any file in
#    out/<run>, its own console (out/<run>/console.log) among them, and writes the scan's counts
#    to out/<run>/secrets.json. The workflows upload out/ only when that file says 0 leaks.
#
# C14 adds what its cases read (the coordinator's ruling: C14 owns these harness extensions):
#   - the compositor's log with `docker logs --timestamps` (the checker's `log` facts are dated on
#     the clock `docker events` uses);
#   - `docker events` for the stack (die, start, restart, kill, oom, stop), reduced to
#     out/<run>/docker-events.jsonl and run.json's `events`;
#   - the compositor's RestartCount when the run went live and at its end, fake-control's
#     /control/state at the end, and the cores check (`docker diff` of the compositor's container,
#     and the host's crash directories before and after); that diff's names are kept in
#     out/<run>/container-diff.txt;
#   - the case's marks (`mark <name> [json]`, out/<run>/marks.jsonl), which the verdict reads as
#     `$case`; and the controls a case drives: `camera`, `mtx` (MediaMTX's API), `in_compositor`
#     (harness/in-container.sh, as root in the compositor's container), `at_s` (the case's clock);
#   - the verdict's helpers: `require <name> <filter> [<measure>]` (a fact; FAIL prints the
#     filter), `require_run`, `measure <name> <filter>` (printed and kept, never judged) and
#     `owed <name> <owed ref> <filter>` (reported, never a failure: a product gap the case cannot
#     judge yet). Every filter is prefixed with harness/cases/lib.jq and has $t
#     (harness/thresholds.json), $run, $case and $timeline bound: no number is typed into a case.
#
# A case file defines `case_drive` and `case_verdict`, and may set CASE_DEADLINE_S, CAMERA_SIZE,
# CAMERA_PATTERN, CHECKER_RECORDINGS, CASE_LIVE_S, CASE_KEY (the measure run-chaos.sh's table
# shows), CASE_TEARDOWN (a function run at every exit) and CASE_NOT_RUNNABLE (why the case cannot
# run: it is then reported, never faked, and run-case.sh exits 3). It sets CASE_CHECKS to how many
# checks its verdict calls: a verdict that ran fewer than it declares fails.
# shellcheck disable=SC2016 # the verdict helpers' jq filters keep their $ for jq
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CASE="${1:?usage: run-case.sh <case> [variant]}"
CASE_VARIANT="${2:-}"
CASE_RUN="${CASE_RUN:-}"
for part in "$CASE" "$CASE_VARIANT"; do
  [[ "$part" =~ ^[a-z0-9-]*$ ]] || { echo "run-case: bad name '$part'" >&2; exit 64; }
done
[[ "$CASE_RUN" =~ ^[0-9]*$ ]] || { echo "run-case: CASE_RUN must be a number" >&2; exit 64; }
CASE_FILE="$HERE/cases/$CASE.sh"
[ -f "$CASE_FILE" ] || { echo "run-case: no case file $CASE_FILE" >&2; exit 64; }
CASE_REJUDGE="${CASE_REJUDGE:-0}"
if [ "$CASE_REJUDGE" != 1 ]; then
  : "${RELAY_IMAGE:?RELAY_IMAGE names the relay image under test}"
  : "${HARNESS_IMAGE:?HARNESS_IMAGE names the harness image}"
  export RELAY_IMAGE HARNESS_IMAGE
fi
THRESHOLDS="$HERE/thresholds.json"
[ -f "$THRESHOLDS" ] || { echo "run-case: no $THRESHOLDS" >&2; exit 64; }
JQ_LIB="$(cat "$HERE/cases/lib.jq")"

RUN_NAME="$CASE${CASE_VARIANT:+-$CASE_VARIANT}${CASE_RUN:+-r$CASE_RUN}"
# CASE_OUT_ROOT moves out/ (the verdict tests' temporary directories); the workflows never set it.
OUT="${CASE_OUT_ROOT:-$HERE/out}/$RUN_NAME"
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
CASE_KEY=""
CASE_NOT_RUNNABLE=""
CASE_TEARDOWN=""
LIVE_WAIT_S=180
EXIT_WAIT_S=120

compose() { docker compose -f "$HERE/compose.yml" "$@"; }
# This host's wall clock in ms (bash 5's EPOCHREALTIME: no fork per call; the clock `docker
# events` and `docker logs --timestamps` read).
now_ms() {
  local t=$EPOCHREALTIME
  t=${t//[.,]/}
  echo $((10#$t / 1000))
}
log() { echo "run-case[$RUN_NAME]: $*"; }

# A control request inside the stack (no port reaches the host).
control() { compose exec -T fake-control curl -fsS -X "$1" "http://localhost:7000$2"; }
# fake-camera's controls (`camera POST /pause`), from fake-control's container.
camera() { compose exec -T fake-control curl -fsS -X "$1" "http://fake-camera:7001$2"; }
# MediaMTX's API, from netem-in (MediaMTX's own network namespace).
mtx() { compose exec -T netem-in curl -fsS -X "$1" "http://localhost:9997$2"; }
# The compositor's SRT reader connections at MediaMTX (remoteAddr 172.28.0.10, path cam; F-021), as
# a JSON array of {id, created, state, packetsRetrans, packetsReceivedLoss}.
mtx_readers() {
  mtx GET /v3/srt/conns/list | jq -c '[.items[] | select((.remoteAddr // "" | startswith("172.28.0.10:")) and .path == "cam")
    | {id, created, state, packetsRetrans, packetsReceivedLoss}]'
}
# A threshold or budget from harness/thresholds.json (`threshold cef_gpu_deaths_to_restart`).
threshold() { jq -e --arg k "$1" '.[$k]' "$THRESHOLDS"; }
# harness/in-container.sh, as root in the compositor's container (C14: the relay image is never
# changed for the harness; it has bash and /proc, no procps).
in_compositor() { docker exec -i -u 0 "$cid" bash -s -- "$@" <"$HERE/in-container.sh"; }
# A case's mark: `mark <name> [<json value>]`, stamped now (or at MARK_AT_MS, when set).
mark() {
  local at="${MARK_AT_MS:-$(now_ms)}"
  printf '{"name":"%s","at_ms":%s,"value":%s}\n' "$1" "$at" "${2:-null}" >>"$OUT/marks.jsonl"
}
# Each JSON line of in_compositor's output as a mark, stamped with the line's own time (the
# container's clock is this host's): an `at_ms` line (a kill's targets, printed before it kills; a
# tree line; a loop's kill) is a mark of <name>, a `rc_at_ms` line (a kill's status, after it) one
# of <name>_rc, and a `done_at_ms` line (a loop's end) one of <name>_done. Prints them.
mark_lines() {
  local name="$1" line kind_at
  while IFS= read -r line; do
    printf '%s\n' "$line"
    kind_at="$(printf '%s\n' "$line" | jq -r 'if .at_ms then "\(.at_ms)" elif .rc_at_ms then "_rc \(.rc_at_ms)"
      elif .done_at_ms then "_done \(.done_at_ms)" else empty end' 2>/dev/null || true)"
    case "$kind_at" in
      "") ;;
      _*) MARK_AT_MS="${kind_at#* }" mark "$name${kind_at%% *}" "$line" ;;
      *) MARK_AT_MS="$kind_at" mark "$name" "$line" ;;
    esac
  done
}
# Sleeps until <ms> on this host's wall clock.
until_ms() {
  local n
  n=$(now_ms)
  if [ "$n" -lt "$1" ]; then
    sleep "$(awk -v d=$(($1 - n)) 'BEGIN { printf "%.3f", d / 1000 }')"
  fi
}
# Sleeps until <seconds> after the case's drive started.
at_s() { until_ms $((DRIVE_START_MS + $1 * 1000)); }

checks_run=0
failures=0
failed_names=()
# A filter with the case library and its bindings: `jq_case <filter> <input> [jq flags...]`.
jq_case() {
  local filter="$1" input="$2"
  shift 2
  jq "$@" --slurpfile t "$THRESHOLDS" --slurpfile run "$OUT/run.json" \
    --slurpfile case "$OUT/case.json" --slurpfile timeline "$OUT/timeline.json" \
    "(\$t[0]) as \$t | (\$run[0]) as \$run | (\$case[0]) as \$case | (\$timeline[0] // {}) as \$timeline |
     $JQ_LIB
     $filter" "$input"
}
record_measure() {
  local v
  v="$(jq_case "$2" "$3" -c 2>/dev/null | head -n 1 || true)"
  jq -cn --arg n "$1" --argjson v "${v:-null}" '{name: $n, value: $v}' >>"$OUT/measures.jsonl" 2>/dev/null ||
    jq -cn --arg n "$1" '{name: $n, value: null}' >>"$OUT/measures.jsonl"
}
judge() {
  local input="$1" name="$2" filter="$3" shown="${4:-}" m=""
  checks_run=$((checks_run + 1))
  if [ -n "$shown" ]; then
    m="$(jq_case "$shown" "$input" -c 2>&1 | tr '\n' ' ' | cut -c1-300 || true)"
    record_measure "$name" "$shown" "$input"
  fi
  if jq_case "$filter" "$input" -e >/dev/null 2>&1; then
    echo "PASS $name${m:+: $m}"
  else
    echo "FAIL $name ($filter)${m:+: measured $m}"
    failures=$((failures + 1))
    failed_names+=("$name")
  fi
}
require() { judge "$OUT/report.json" "$@"; }
require_run() { judge "$OUT/run.json" "$@"; }
measure() {
  checks_run=$((checks_run + 1))
  record_measure "$1" "$2" "$OUT/report.json"
  echo "MEASURE $1: $(jq_case "$2" "$OUT/report.json" -c 2>&1 | tr '\n' ' ' | cut -c1-300 || true)"
}
owed_lines=()
owed() {
  local name="$1" ref="$2" filter="$3" holds=false
  checks_run=$((checks_run + 1))
  if jq_case "$filter" "$OUT/report.json" -e >/dev/null 2>&1; then holds=true; fi
  echo "OWED $name ($ref): $([ "$holds" = true ] && echo holds || echo "does not hold") ($filter)"
  owed_lines+=("$(jq -cn --arg n "$name" --arg r "$ref" --argjson h "$holds" '{name: $n, ref: $r, holds: $h}')")
}

# The false-positive sweep (C14 brief, amendment 2026-10-20; Owed 70 (f)), for every case without a
# CEF kill: since C11b a false zygote verdict, or a GPU death counted that did not happen, costs an
# outage. 4 checks.
fp_swept=false
require_no_false_restart() {
  fp_swept=true
  require fp_swept_inputs '.log.lines > 0 and (starts_after(0) | length) >= 1' \
    '{log_lines: .log.lines, cef_health_lines: (.log.cef_health | length), starts: (starts_after(0) | length)}'
  require fp_no_zygote_verdict '[.log.cef_health[] | select(.zygote_gone != null)] == []'
  require fp_no_gpu_deaths '[.log.cef_health[] | select(.gpu_process_deaths != 0)] == []'
  require fp_no_exit_75 '[dies_after(0)[] | select(.exit_code == 75)] == []' '[dies_after(0)[] | .exit_code]'
}

# The restart's outage and its parts (C14 brief, amendment 2026-10-20, "The outage measurement"),
# from <decision> (a jq expression for the decision's time: the kill, or the cef health line that
# decided the restart): the decision to the die (the final beat's flush), the die to the start,
# the start to the first programme packet, and the outage itself, judged against
# BUDGET_PROCESS_RESTART_OUTPUT_MS. 6 checks.
require_restart_outage() {
  local d="$1" die start
  die="(dies_after($d) | first | .at_ms)"
  start="(starts_after($die) | first | .at_ms)"
  measure decision_to_die_ms "$die - $d"
  measure die_to_start_ms "$start - $die"
  measure start_to_first_packet_ms "restart_outage($die; $start).to_wall_ms - $start"
  measure outage_ms "restart_outage($die; $start) | .to_wall_ms - .from_wall_ms"
  require restart_outage_found "restart_outage($die; $start) != null" \
    "restart_outage($die; $start) | {from_wall_ms, to_wall_ms, counter_before, counter_after}"
  require restart_outage_in_budget \
    "restart_outage($die; $start) | (.to_wall_ms - .from_wall_ms) <= \$t.budget_process_restart_output_ms" \
    "{outage_ms: (restart_outage($die; $start) | .to_wall_ms - .from_wall_ms), budget: \$t.budget_process_restart_output_ms}"
}

# The output's checks after a fault cleared at mark "clear" (cases 4, 5 and 12), and the clear's
# phase against the last `sink swap` at or before it (the re-review's I-5; Owed 84): a sink built
# less than OUTPUT_CONNECT_TIMEOUT_MS before the clear was still connecting when the path came back,
# so it connects after the clear with the fault's backlog, which is the burst Owed 84 names; a clear
# after that connect timed out builds a fresh sink and never takes that path. `connect_pending` is
# printed so a run says which path it took. It was Owed 84's signature in chaos.json until that
# row's fix (PR #57) merged; a red on these checks now reads NOT OWED whatever the phase. 5 checks.
require_output_recovers() {
  measure clear_phase '([.log.sink_swaps[] | select(.at_ms <= mark_at("clear"))] | last) as $sw
    | (if $sw == null or mark_at("clear") == null then null else mark_at("clear") - $sw.at_ms end) as $ago
    | {swap_gen: ($sw.gen // null), swap_to_clear_ms: $ago,
       swaps_in_fault: ([.log.sink_swaps[] | select(.at_ms > mark_at("cut") and .at_ms <= mark_at("clear"))] | length),
       connect_pending: ($ago != null and $ago < $t.output_connect_timeout_ms),
       connect_timeout_ms: $t.output_connect_timeout_ms}'
  require outage_spans_fault '[.output_outages[] | select(.to_wall_ms > mark_at("cut") and .from_wall_ms < mark_at("clear"))] | length >= 1' \
    '[.output_outages[] | {from_wall_ms, to_wall_ms}]'
  require output_reconnect 'outage_across(mark_at("clear")) as $o | $o != null and $o.from_wall_ms <= mark_at("clear")
    and ($o.to_wall_ms - mark_at("clear")) <= $t.budget_output_reconnect_ms' \
    '{resume_ms: (outage_across(mark_at("clear")).to_wall_ms - mark_at("clear")), budget: $t.budget_output_reconnect_ms}'
  require output_lost '[.output_outages[] | select(.to_wall_ms > mark_at("cut")
      and .from_wall_ms < mark_at("clear") + $t.budget_output_reconnect_ms) | .lost_ms]
    | length >= 1 and all(. != null and . <= $t.budget_output_lost_max_ms)' \
    '{lost_ms: [.output_outages[] | .lost_ms], budget: $t.budget_output_lost_max_ms}'
  require no_outage_outside_fault '[.output_outages[] | select(.to_wall_ms < mark_at("cut")
      or .from_wall_ms > mark_at("clear") + $t.budget_output_reconnect_ms)] == []'
}

# The overlay is back on air after a process restart (C14 fix round 3, item 3: the coordinator's
# decision; spec §9.4 "CEF killed: overlay back", and §6's process-crash target "output back
# ≤15 s", whose overlay is part of that output): the first recorded frame after the container's
# <start> whose score strip reads comes at most BUDGET_PROCESS_RESTART_OUTPUT_MS after the restart's
# outage began (`restart_outage(<die>; <start>)`, its last packet). <die> and <start> are jq
# expressions. The budget is timed from that packet, as spec §6's "output back" is (the re-review's
# m-2: timed from the die it was up to the final beat's flush looser). The search stays after
# <start>, not after the outage's end as m-2 also proposed: where the outage spans the start no
# frame lies between the two, so the forms agree, and where it ends before the start (a frame dated
# between the die and the start, which only the old process can have made) the outage's end would
# read that frame as the new process's. No outage, or no frame back, is a FAIL. Owed 85's restarts,
# whose CEF never started, red here; PR #54 fixed that cause (main 263b08a), so no owed tag excuses
# such a red now. Its evidence is still measured beside it: `graphics_failed_after_start`, the
# `overlay hidden reason=failed` lines after <start> (graphics gave up after CEF_FAIL_COUNT builds;
# the re-review's I-3). 2 checks.
require_overlay_back() {
  local die="$1" start="$2"
  measure graphics_failed_after_start "($start) as \$s
    | if \$s == null then null else [.log.overlay_hidden[] | select(.reason == \"failed\" and .at_ms > \$s)] | length end"
  require overlay_back "restart_outage($die; $start) as \$o | ($start) as \$s
    | (if \$s == null then null else [.frames_table[] | select(.[0] > \$s and .[2] != null)] | first | .[0] end) as \$back
    | \$o != null and \$back != null and (\$back - \$o.from_wall_ms) <= \$t.budget_process_restart_output_ms" \
    "restart_outage($die; $start) as \$o | ($start) as \$s
    | {back_ms: less(if \$s == null then null else [.frames_table[] | select(.[0] > \$s and .[2] != null)] | first | .[0] end;
         \$o.from_wall_ms), budget: \$t.budget_process_restart_output_ms}"
}

# C11b's in-place restart after <after> (a jq expression): the final beat says why, with
# `exitReason` "restart" (Owed 66, closed: web's word; the brief's "fatal" is a false premise) and
# `exitDetail` equal to <detail> (a jq string expression, the domain's `RestartCause::detail`), the
# container dies 75 once and compose starts it again, Chromium never aborts, and the overlay is back
# (`require_overlay_back`, over the restart's outage). 6 checks.
require_in_place_restart() {
  local after="$1" detail="$2"
  require final_beat_detail "($detail) as \$want
    | ([beats[] | select(.received_ms >= $after and .body.state == \"stopped\")] | first) as \$b
    | \$b.body.exitReason == \"restart\" and \$b.body.exitDetail == \$want" \
    "($detail) as \$want | ([beats[] | select(.received_ms >= $after and .body.state == \"stopped\")] | first) as \$b
    | {exitReason: \$b.body.exitReason, exitDetail: \$b.body.exitDetail, want: \$want}"
  require died_in_place "(dies_after($after) | first | .exit_code) == 75" "dies_after($after) | first | .exit_code"
  require_run restarted_once '.compositor.restarts - .restarts_at_live == 1' '{at_live: .restarts_at_live, at_end: .compositor.restarts}'
  require no_abort '.log.gpu_unusable == []' '.log.gpu_unusable | length'
  require_overlay_back "(dies_after($after) | first | .at_ms)" \
    "(starts_after(dies_after($after) | first | .at_ms) | first | .at_ms)"
}

# After a process restart at <after> (a jq expression): the same session, the beats back, no
# counter regression and no core. 4 checks.
require_same_session_and_no_core() {
  local after="$1"
  require_run same_session '.control_state.other_session_requests == 0 and .control_state.unauthorized_requests == 0 and .control_state.session_fetches >= 2' \
    '.control_state | {session_fetches, other_session_requests, unauthorized_requests}'
  require beats_resume "beats_between($after; \$run.end_request_ms + 1) | length >= 1" \
    "beats_between($after; \$run.end_request_ms + 1) | length"
  require counter_regressions '.camera.counter_regressions == 0'
  require_run no_core '.cores.diff_ok and .cores.container_files == 0 and .cores.host_after == .cores.host_before' '.cores'
}

# shellcheck source=/dev/null
. "$CASE_FILE"

verdict_status=""
write_verdict() {
  local status="$1" failed owed measures
  failed="$(printf '%s\n' "${failed_names[@]}" | jq -R . | jq -sc 'map(select(. != ""))')"
  # jq prints its answer and THEN fails on a missing file, so a fallback replaces, never appends.
  owed="$(printf '%s\n' "${owed_lines[@]}" | jq -sc '.' 2>/dev/null)" || owed='[]'
  measures="$(jq -sc 'map({(.name): .value}) | add // {}' "$OUT/measures.jsonl" 2>/dev/null)" || measures='{}'
  [ -n "$owed" ] || owed='[]'
  [ -n "$measures" ] || measures='{}'
  jq -n --arg name "$RUN_NAME" --arg case "$CASE" --arg variant "$CASE_VARIANT" --arg run "$CASE_RUN" \
    --arg status "$status" --argjson declared "$CASE_CHECKS" --argjson ran "$checks_run" \
    --argjson failed "$failed" --argjson owed "$owed" --argjson measures "$measures" \
    --arg key "$CASE_KEY" --argjson fp "$fp_swept" --arg reason "$CASE_NOT_RUNNABLE" \
    '{name: $name, case: $case, variant: $variant, run: $run, status: $status,
      checks_declared: $declared, checks_run: $ran, failed: $failed, owed: $owed,
      measures: $measures, key: $key, fp_swept: $fp,
      not_runnable_reason: (if $reason == "" then null else $reason end)}' >"$OUT/verdict.json"
  verdict_status="$status"
}

# The case's verdict over out/<run>, then verdict.json. `$1`: the checker's exit code.
run_verdict() {
  local checker_exit="$1"
  echo "verdict ($RUN_NAME):"
  case_verdict
  if [ "$CASE_CHECKS" -le 0 ] || [ "$checks_run" -ne "$CASE_CHECKS" ]; then
    echo "FAIL the verdict ran $checks_run checks, the case declares $CASE_CHECKS"
    failures=$((failures + 1))
    failed_names+=("checks_declared")
  fi
  if [ "$checker_exit" -ne 0 ]; then
    echo "FAIL the checker exited $checker_exit"
    failures=$((failures + 1))
    failed_names+=("checker_exit")
  fi
  log "$checks_run checks, $failures failures"
  if [ "$failures" -eq 0 ]; then write_verdict pass; else write_verdict fail; fi
}

# Re-judging (CASE_REJUDGE=1): the verdict alone, over an out/<run> a run left (a CI artifact, for
# instance), with no stack, no checker and no new secrets: what a verdict change is checked on.
if [ "$CASE_REJUDGE" = 1 ]; then
  mkdir -p "$OUT"
  if [ -n "$CASE_NOT_RUNNABLE" ]; then
    echo "NOT RUNNABLE $RUN_NAME: $CASE_NOT_RUNNABLE"
    write_verdict not_runnable
    exit 3
  fi
  for f in report.json run.json case.json timeline.json; do
    [ -f "$OUT/$f" ] || { echo "run-case: CASE_REJUDGE needs $OUT/$f" >&2; exit 64; }
  done
  : >"$OUT/measures.jsonl"
  run_verdict 0
  [ "$failures" -eq 0 ] || exit 1
  exit 0
fi

# A fresh stack and a fresh out/<run> (no stack at all for a case that cannot run).
[ -n "$CASE_NOT_RUNNABLE" ] || compose down -v --remove-orphans >/dev/null 2>&1 || true
rm -rf "$OUT"
mkdir -p "$OUT/rec" "$CHECKER_RECORDINGS"
: >"$OUT/emit.jsonl"
: >"$OUT/heartbeats.jsonl"
: >"$OUT/marks.jsonl"
: >"$OUT/measures.jsonl"
# The console, kept beside the outputs so the secrets scan reads it too (fds 3 and 4 keep the
# originals, so the EXIT trap can close tee's pipe and wait for its last line before scanning).
exec 3>&1 4>&2
exec > >(tee -a "$OUT/console.log") 2>&1
console_pid=$!

RELAY_DEADLINE_AT="$(python3 -I -c 'import datetime,sys; print((datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=int(sys.argv[1]))).strftime("%Y-%m-%dT%H:%M:%SZ"))' "$CASE_DEADLINE_S")"
export RELAY_DEADLINE_AT

# The host's crash directories (Owed 68), counted before and after the run.
host_cores() {
  local n=0 d
  for d in /var/crash /var/lib/apport/coredump /var/lib/systemd/coredump; do
    if [ -d "$d" ]; then n=$((n + $(find "$d" -type f 2>/dev/null | wc -l))); fi
  done
  echo "$n"
}

stack_up=0
collected=0
events_pid=""
cid=""
cores_before=0
collect() {
  [ "$collected" -eq 1 ] && return 0
  collected=1
  [ -n "$cid" ] || cid="$(compose ps -a -q compositor 2>/dev/null || true)"
  if [ -n "$cid" ]; then
    docker logs --timestamps "$cid" >"$OUT/compositor.log" 2>&1 || true
    # Cores: a file named core or core.<pid> that the container's filesystem gained (`docker diff`
    # reads a stopped container too), and the host's crash directories.
    local diff_ok=false files=0 diff
    if diff="$(docker diff "$cid" 2>/dev/null)"; then
      diff_ok=true
      # The names alone (C14 fix round 2, Owed 85): what a restarted container inherits from the
      # run before it on its writable layer.
      printf '%s\n' "$diff" >"$OUT/container-diff.txt"
      files="$(printf '%s\n' "$diff" | awk '{ print $2 }' | grep -cE '(^|/)core(\.[0-9]+)?$' || true)"
    fi
    jq -n --argjson ok "$diff_ok" --argjson files "${files:-0}" --argjson before "$cores_before" \
      --argjson after "$(host_cores)" --arg pattern "$(cat /proc/sys/kernel/core_pattern 2>/dev/null || echo unknown)" \
      '{diff_ok: $ok, container_files: $files, host_before: $before, host_after: $after, core_pattern: $pattern}' \
      >"$OUT/cores.json"
    control GET /control/state >"$OUT/control-state.json" 2>/dev/null || echo '{}' >"$OUT/control-state.json"
  else
    : >"$OUT/compositor.log"
  fi
  compose logs --no-color mediamtx fake-camera fake-control >"$OUT/services.log" 2>&1 || true
  if [ -n "$events_pid" ]; then
    kill "$events_pid" 2>/dev/null || true
    wait "$events_pid" 2>/dev/null || true
    events_pid=""
  fi
  # docker events, reduced to what a verdict reads (the raw lines are not kept).
  if [ -f "$OUT/docker-events.raw" ]; then
    jq -c '{at_ms: ((.timeNano / 1000000) | floor), action: .Action,
            service: (.Actor.Attributes["com.docker.compose.service"] // null),
            exit_code: (.Actor.Attributes.exitCode // null | if . == null then null else tonumber end)}' \
      "$OUT/docker-events.raw" >"$OUT/docker-events.jsonl" 2>/dev/null || : >"$OUT/docker-events.jsonl"
    rm -f "$OUT/docker-events.raw"
  fi
}
teardown() {
  if [ "$stack_up" -eq 1 ]; then
    collect
    compose down -v --remove-orphans >/dev/null 2>&1 || true
    stack_up=0
  fi
  if [ -n "$events_pid" ]; then
    kill "$events_pid" 2>/dev/null || true
    events_pid=""
  fi
  if [ -n "$CASE_TEARDOWN" ]; then
    "$CASE_TEARDOWN" || true
  fi
}

# No credential in any file the run produced. The values go to grep on a pipe, never in argv.
# Files a short run never wrote are skipped, not read as clean: the count of files scanned is
# what secrets.json reports, and 0 is a failure.
present() { local f; for f in "$@"; do [ -f "$OUT/$f" ] && printf '%s\n' "$OUT/$f"; done; return 0; }
leaks=0
scanned=0
scan_secrets() {
  local name f files=()
  scanned="$(find "$OUT" -type f ! -name secrets.json | wc -l | tr -d ' ')"
  for name in JOB_TOKEN HARNESS_SRT_PASSPHRASE; do
    if printf '%s\n' "${!name}" | grep -rqF -f - "$OUT"; then
      log "FAIL $name appears in out/$RUN_NAME"
      leaks=$((leaks + 1))
    fi
  done
  # The page token: the compositor scrubs it from its log, and nothing else logs a query.
  mapfile -t files < <(present compositor.log services.log checker.log console.log container-diff.txt)
  if [ "${#files[@]}" -gt 0 ] && printf '%s\n' "$PAGE_TOKEN" | grep -qF -f - "${files[@]}"; then
    log "FAIL the page token appears in a log"
    leaks=$((leaks + 1))
  fi
  # SESSION_ID: never in the harness's own files. The compositor's log is reported, not judged,
  # and so is the console, which tails that log when the run never goes live.
  for f in services.log checker.log emit.jsonl heartbeats.jsonl timeline.json run.json report.json \
    docker-events.jsonl marks.jsonl case.json measures.jsonl verdict.json control-state.json cores.json \
    container-diff.txt; do
    [ -f "$OUT/$f" ] || continue
    if printf '%s\n' "$SESSION_ID" | grep -qF -f - "$OUT/$f"; then
      log "FAIL SESSION_ID appears in $f"
      leaks=$((leaks + 1))
    fi
  done
  local sid_lines=0
  if [ -f "$OUT/compositor.log" ]; then
    sid_lines="$(printf '%s\n' "$SESSION_ID" | grep -cF -f - "$OUT/compositor.log" || true)"
  fi
  log "secrets checked: $scanned files, $leaks leaks (SESSION_ID on $sid_lines compositor log lines, reported only)"
  if [ "$scanned" -eq 0 ]; then
    log "FAIL no file was scanned for secrets"
  fi
  jq -n --argjson scanned "$scanned" --argjson leaks "$leaks" '{scanned: $scanned, leaks: $leaks}' \
    >"$OUT/secrets.json"
}

# Every exit, early ones too: the logs, the stack down, the verdict, then the scan over a flushed
# console.
on_exit() {
  local rc=$?
  set +e
  teardown
  if [ -z "$verdict_status" ]; then
    if [ -n "$CASE_NOT_RUNNABLE" ]; then write_verdict not_runnable; else write_verdict error; fi
  fi
  exec 1>&3 2>&4 3>&- 4>&-
  wait "$console_pid" 2>/dev/null
  scan_secrets
  if [ "$leaks" -gt 0 ] || [ "$scanned" -eq 0 ]; then
    if [ "$rc" -eq 0 ] || [ "$rc" -eq 3 ]; then rc=1; fi
  fi
  exit "$rc"
}
trap on_exit EXIT

# A case that cannot run is reported, never faked (C14 brief: case 16 without its test hook).
if [ -n "$CASE_NOT_RUNNABLE" ]; then
  log "NOT RUNNABLE: $CASE_NOT_RUNNABLE"
  exit 3
fi

cores_before="$(host_cores)"
# docker events for this project's containers, from before the stack comes up.
docker events --format '{{json .}}' --filter "label=com.docker.compose.project=seazn-harness" \
  --filter type=container --filter event=die --filter event=start --filter event=restart \
  --filter event=kill --filter event=oom --filter event=stop >"$OUT/docker-events.raw" 2>/dev/null &
events_pid=$!

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
live_ms="$(now_ms)"
restarts_at_live="$(docker inspect --format '{{.RestartCount}}' "$cid")"
log "live after $(wc -l <"$OUT/heartbeats.jsonl" | tr -d ' ') beats"
control POST /control/timeline-start >/dev/null

DRIVE_START_MS="$(now_ms)"
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

jq -s 'reduce .[] as $m ({}; .[$m.name] += [{at_ms: $m.at_ms, value: $m.value}])' "$OUT/marks.jsonl" \
  >"$OUT/case.json"
jq -n --argjson end "$end_request_ms" --argjson exit_ms "$exit_ms" --arg status "$c_status" \
  --argjson code "$c_code" --argjson restarts "$c_restarts" --arg case "$CASE" \
  --arg variant "$CASE_VARIANT" --arg run "$CASE_RUN" --arg name "$RUN_NAME" \
  --argjson live "$live_ms" --argjson drive "$DRIVE_START_MS" --argjson at_live "$restarts_at_live" \
  --slurpfile beats "$OUT/heartbeats.jsonl" --slurpfile events "$OUT/docker-events.jsonl" \
  --slurpfile state "$OUT/control-state.json" --slurpfile cores "$OUT/cores.json" \
  '{case: $case, variant: $variant, run: $run, name: $name, live_ms: $live, drive_start_ms: $drive,
    end_request_ms: $end, exit_seen_ms: $exit_ms,
    compositor: {status: $status, exit_code: $code, restarts: $restarts}, restarts_at_live: $at_live,
    last_beat: ($beats | last | .body // null), events: $events,
    control_state: ($state[0] // {}), cores: ($cores[0] // null)}' >"$OUT/run.json"
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
     output_outages: (.output_outages | length), segment_overlaps: .segment_overlaps,
     strip_frames: .camera.strip_frames,
     slate_frames: .camera.slate_frames, regressions: .camera.counter_regressions,
     latency_ms: .camera.latency_ms, cadence: .camera.cadence,
     score_changes: (.overlay.score_index_changes | length),
     av_offset_ms: .audio.av_offset_ms, beats: .heartbeats.count, rungs: .heartbeats.rungs,
     rung_sequence: .heartbeats.rung_sequence,
     restarts_without_crash: .graphics.restarts_without_crash, checker_failures}' "$OUT/report.json"
echo "docker events (compositor): $(jq -c '[.events[] | select(.service == "compositor") | "\(.action)\(if .exit_code == null then "" else " \(.exit_code)" end)"]' "$OUT/run.json")"

# The case's verdict (the secrets scan follows, at exit).
run_verdict "$checker_exit"
[ "$failures" -eq 0 ] || exit 1
