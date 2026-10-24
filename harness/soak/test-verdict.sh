#!/usr/bin/env bash
# The soak verdict's tests (C15 brief, Steps 1, 2 and 6): harness/soak/verdict.jq over
# harness/soak/fixtures/green (a 30-minute miniature run, fixtures/make-green.py) and over red
# copies of it, each changed in ONE way. Run from anywhere: `harness/soak/test-verdict.sh`
# (check.yml runs it). bash 3.2 or later (macOS's /bin/bash runs it too); JQ=<jq> picks the jq.
#
# - The green reads `pass`, and runs every check the soak declares (a check dropped from verdict.jq
#   fails here, not only on its red).
# - The brief's five reds (memory drift, an unexplained gap, two session GETs, a missing dropout
#   action, a stepped beat) each fail on exactly their criterion, with their reason.
# - Guard reds: every other conjunct verdict.jq judges has a red of its own that breaks only it
#   (a guard nothing kills is not tested), including the real-page checks (a) to (d).
# - The test page: the same green with `page=test` reads `plumbing`, its checks (a) to (d) pending
#   on the page source, never green.
# - The schedule (schedule.jq): 36 dropouts and 3 disconnects, at minutes 30, 90 and 150, in 3 h
#   (the brief's numbers), the rehearsal's 25 minutes, and a drive too short to schedule refused.
#
# Prints one line per case and `checked: N cases, M checks`; zero of either is a FAIL.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
JQ="${JQ:-jq}"
GREEN="$HERE/fixtures/green"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cases=0
checks=0
failures=0
fail() {
  echo "FAIL $*"
  failures=$((failures + 1))
}

# The green's six inputs as one JSON bundle; a red is a jq filter over it.
"$JQ" -n --slurpfile report "$GREEN/report.json" --slurpfile actions "$GREEN/actions.jsonl" \
  --slurpfile control "$GREEN/control-state.json" --slurpfile deliveries "$GREEN/deliveries.jsonl" \
  --slurpfile samples "$GREEN/overlay-samples.jsonl" --slurpfile logscan "$GREEN/logscan.json" \
  '{report: $report[0], actions: $actions, control: $control[0], deliveries: $deliveries,
    samples: $samples, logscan: $logscan[0]}' >"$WORK/green.json"

# `run_verdict <bundle> <page> <drive_min> <out>`: the bundle back into files, then verdict.jq.
run_verdict() {
  local b="$1" page="$2" drive="$3" out="$4" d
  d="$(mktemp -d "$WORK/run.XXXXXX")"
  "$JQ" -c '.report' "$b" >"$d/report.json"
  "$JQ" -c '.actions[]' "$b" >"$d/actions.jsonl"
  "$JQ" -c '.control' "$b" >"$d/control-state.json"
  "$JQ" -c '.deliveries[]' "$b" >"$d/deliveries.jsonl"
  "$JQ" -c '.samples[]' "$b" >"$d/overlay-samples.jsonl"
  "$JQ" -c '.logscan' "$b" >"$d/logscan.json"
  local rc=0
  "$HERE/verdict.sh" --dir "$d" --page "$page" --drive-min "$drive" --jq "$JQ" >"$out" 2>"$out.err" || rc=$?
  echo "$rc" >"$out.rc"
}

# Every check verdict.jq declares, in order: a dropped one fails the green.
ALL='inputs,1,2,3,4,5.beats,5.dropouts,5.disconnects,5a,5b,5c,5d,6'

# `case_ <name> <page> <want status> <want failed ids, sorted, comma-joined> <reason> <filter>`
case_() {
  local name="$1" page="$2" want="$3" want_failed="$4" reason="$5" filter="$6" b out got failed ids n
  cases=$((cases + 1))
  b="$WORK/case-$name.json"
  if ! "$JQ" "$filter" "$WORK/green.json" >"$b" 2>"$WORK/$name.patch.err"; then
    fail "$name: the patch does not apply: $(head -c 300 "$WORK/$name.patch.err")"
    return
  fi
  if [ "$want" = fail ] && cmp -s "$b" "$WORK/green.json"; then
    fail "$name: the patch changed nothing (a red identical to the green proves nothing)"
    return
  fi
  out="$WORK/case-$name.out"
  run_verdict "$b" "$page" 30 "$out"
  if ! got="$("$JQ" -er '.status' "$out" 2>/dev/null)"; then
    fail "$name: no verdict ($(head -c 300 "$out.err"))"
    return
  fi
  failed="$("$JQ" -r '[.checks[] | select(.status == "fail") | .id] | sort | join(",")' "$out")"
  ids="$("$JQ" -r '[.checks[].id] | join(",")' "$out")"
  n="$("$JQ" -r '.checks | length' "$out")"
  checks=$((checks + n))
  if [ "$ids" != "$ALL" ]; then
    fail "$name: the verdict ran checks [$ids], the soak declares [$ALL]"
    return
  fi
  if [ "$got" != "$want" ] || [ "$failed" != "$want_failed" ]; then
    fail "$name: want $want failing [$want_failed], got $got failing [$failed]: $("$JQ" -c '[.checks[] | select(.status == "fail") | {id, reason}]' "$out" | head -c 600)"
    return
  fi
  # The reason: "<id>:<text>", where check <id> must fail with a reason containing <text>; a bare
  # "<text>" is the one failing check's.
  if [ -n "$reason" ]; then
    local rid="${want_failed}" rtext="$reason"
    case "$reason" in *:*) rid="${reason%%:*}"; rtext="${reason#*:}" ;; esac
    if ! "$JQ" -e --arg id "$rid" --arg r "$rtext" '[.checks[] | select(.id == $id and .status == "fail") | .reason | contains($r)] == [true]' "$out" >/dev/null; then
      fail "$name: check $rid does not fail with \"$rtext\": $("$JQ" -c '[.checks[] | select(.status == "fail") | {id, reason}]' "$out" | head -c 600)"
      return
    fi
  fi
  # verdict.sh's exit: 0 for pass and plumbing (the workflow warns on plumbing), 1 for fail.
  local want_rc=0
  [ "$want" = fail ] && want_rc=1
  if [ "$(cat "$out.rc")" != "$want_rc" ]; then
    fail "$name: verdict.sh exited $(cat "$out.rc") on a $got verdict, want $want_rc"
    return
  fi
  echo "PASS $name: $got${failed:+ [$failed]}${reason:+ \"$reason\"}"
}

# --- the green, and the test page's plumbing read of it ------------------------------------------
case_ green real pass '' '' '.'
case_ test-page-plumbing test plumbing '' '' '.'
if ! "$JQ" -e '[.checks[] | select(.status == "pending") | .id] == ["5a","5b","5c","5d"]
    and ([.checks[] | select(.status == "pending") | .reason | startswith("pending: page source")] | all)' \
  "$WORK/case-test-page-plumbing.out" >/dev/null; then
  fail "test-page-plumbing: (a) to (d) are not all 'pending: page source': $("$JQ" -c '[.checks[] | {id, status}]' "$WORK/case-test-page-plumbing.out")"
fi

# --- the brief's five reds -------------------------------------------------------------------------
# Memory: hour 3's beats 25 % above hour 1's (the drive's last third against its first).
case_ memory-drift real fail '1' 'memory drift' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .report.heartbeats.series |= map(if .received_ms >= $s + 1200000 and (.body.memMb | type) == "number"
       then .body.memMb = (.body.memMb * 1.25 | floor) else . end)'
# An output outage at minute 10, which no disconnect explains.
case_ unexplained-gap real fail '2' 'unexplained output gap' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .report.output_outages += [{from_wall_ms: ($s + 600000), to_wall_ms: ($s + 603000), kind: "between",
       counter_before: null, counter_after: null, counter_gap_ms: null, lost_ms: 900}]'
# Two session GETs: the process restarted.
case_ two-session-gets real fail '3' 'programme rebuilt' '.control.session_fetches = 2'
# The fourth dropout never happened.
case_ missing-dropout real fail '5.dropouts' 'dropout actions' \
  'del(.actions[] | select(.kind == "dropout" and .slot == 3))'
# One beat stepped down (criterion 6, amended 2026-10-18).
case_ stepped-beat real fail '6' '6:sizing alarm: the guest is short of CPU' \
  '.report.heartbeats.series[100].body.rung = "overlay_half_rate"'

# --- guard reds: each other conjunct, alone -------------------------------------------------------
# No report: every check that reads it fails ("no report"), never passes on nothing.
case_ no-report real fail '1,2,4,5.beats,5.disconnects,5a,5b,5c,6,inputs' 'inputs:no report' '.report = null'
case_ no-frames real fail 'inputs' 'no frames' '.report.frames = 0'
# No start action: the drive's windows are unknown, so every windowed check fails too.
case_ no-start real fail '1,5.beats,5a,5c,inputs' 'inputs:one start and one end action' 'del(.actions[] | select(.kind == "start"))'
case_ drive-mismatch real fail 'inputs' 'drive' '(.actions[] | select(.kind == "start") | .drive_s) = 1500'
case_ not-ended real fail 'inputs' 'did not end as asked' '.report.heartbeats.series[-1].body.exitReason = "deadline"'
case_ checker-failure real fail 'inputs' 'checker failed' '.report.checker_failures = ["a rung outside the declared ones"]'
case_ memory-no-beats real fail '1' 'no memMb' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .report.heartbeats.series |= map(if .received_ms >= $s + 1200000 then del(.body.memMb) else . end)'
# The three anti-vacuity guards nothing else reaches alone: a renamed beat field must fail, never
# pass on nothing (hour 1's memMb; droppedFrames and rung on every beat).
case_ memory-no-beats-hour1 real fail '1' 'no memMb in hour 1' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .report.heartbeats.series |= map(if .received_ms < $s + 600000 then del(.body.memMb) else . end)'
case_ no-dropped-frames-field real fail '4' 'no beat carried droppedFrames' '.report.heartbeats.series |= map(del(.body.droppedFrames))'
case_ no-rung-field real fail '6' 'no beat carried a rung' '.report.heartbeats.series |= map(del(.body.rung))'
case_ pts-gap real fail '2' 'pts gaps' '.report.pts_gaps = [{at_ms: 1792800400000, gap_ms: 400}]'
case_ late-reconnect real fail '2' 'after its disconnect ended' \
  '.report.output_outages[1].to_wall_ms = ((.actions[] | select(.kind == "disconnect" and .slot == 1) | .end_ms) + 8000)'
case_ dropped-frames real fail '4' 'dropped frames' '.report.heartbeats.series[-2].body.droppedFrames = 60'
case_ few-beats real fail '5.beats' 'heartbeats' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .report.heartbeats.series |= [to_entries[] | select(.key % 4 != 1 or .value.received_ms < $s + 600000 or .value.received_ms >= $s + 1200000) | .value]
   | .report.heartbeats.count = (.report.heartbeats.series | length)'
case_ dropout-failed real fail '5.dropouts' 'dropout actions' '(.actions[] | select(.kind == "dropout" and .slot == 2) | .ok) = false'
case_ missing-disconnect real fail '5.disconnects' 'disconnect actions' \
  'del(.actions[] | select(.kind == "disconnect" and .slot == 2)) | del(.report.output_outages[2])'
case_ disconnect-not-applied real fail '5.disconnects' 'dropped no packet' \
  '(.actions[] | select(.kind == "disconnect" and .slot == 0) | .dropped_packets) = 0'
case_ disconnect-not-felt real fail '5.disconnects' 'no output outage' 'del(.report.output_outages[0])'
case_ few-cef-alive real fail '5a' 'cef alive' '.report.log.cef_alive |= [to_entries[] | select(.key % 5 != 0) | .value]'
case_ graphics-failed real fail '5b' 'graphics read failed' '.report.heartbeats.series[200].body.graphics = "failed"'
case_ hidden-failed real fail '5b' 'reason=failed' '.report.log.overlay_hidden[0].reason = "failed"'
case_ hidden-bare real fail '5b' 'without a reason' '.logscan.overlay_hidden_lines = 2'
case_ no-log real fail '5b' 'no compositor log' '.report.log.lines = 0 | .logscan.lines = 0'
case_ delivery-gap real fail '5c' 'deliveries' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .deliveries |= map(select(.at_ms < $s + 420000 or .at_ms >= $s + 480000))'
case_ delivery-refused real fail '5c' 'deliveries' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .deliveries |= map(if .at_ms >= $s + 420000 and .at_ms < $s + 480000 then .ok = false | .status = 409 else . end)'
# (c) judges what the page received: one drive beat with scoreStale true (the page went its stale
# budget without a good poll), and no beat carrying scoreStale at all (the page never said).
case_ score-stale real fail '5c' '5c:score stale: 1 drive beat' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | (.report.heartbeats.series | map(.received_ms >= $s + 900000) | index(true)) as $i
   | .report.heartbeats.series[$i].body.scoreStale = true'
case_ score-stale-unreported real fail '5c' 'no drive beat carried scoreStale' '.report.heartbeats.series |= map(del(.body.scoreStale))'
case_ overlay-off-air real fail '5d' 'overlay' '.samples[9].differs = false'
case_ overlay-unsampled real fail '5d' 'overlay' 'del(.samples[9])'

# --- the schedule ----------------------------------------------------------------------------------
plan() {
  "$JQ" -n -L "$HERE" --slurpfile soak "$HERE/soak.json" --argjson d "$1" -c \
    'include "schedule"; soak_plan($soak[0]; $d) | {dropouts: (.dropouts | length), first: .dropouts[0], last: .dropouts[-1], disconnects}'
}
schedule_case() {
  local name="$1" drive_s="$2" want="$3" got
  cases=$((cases + 1))
  checks=$((checks + 1))
  if got="$(plan "$drive_s" 2>&1)" && [ "$got" = "$want" ]; then
    echo "PASS schedule $name: $got"
  else
    fail "schedule $name: want $want, got $got"
  fi
}
schedule_case "3 h (the brief's 36 dropouts; disconnects at minutes 30, 90, 150)" 10800 \
  '{"dropouts":36,"first":150,"last":10650,"disconnects":[1800,5400,9000]}'
schedule_case "the rehearsal's 25 min" 1500 '{"dropouts":5,"first":150,"last":1350,"disconnects":[300,900,1200]}'
schedule_case "the fixture's 30 min" 1800 '{"dropouts":6,"first":150,"last":1650,"disconnects":[300,900,1500]}'
cases=$((cases + 1))
checks=$((checks + 1))
if out="$(plan 600 2>&1)"; then
  fail "schedule 10 min: a drive under min_drive_min was planned: $out"
elif printf '%s' "$out" | grep -q 'under min_drive_min'; then
  echo "PASS schedule 10 min: refused (under min_drive_min)"
else
  fail "schedule 10 min: refused for the wrong reason: $out"
fi

echo "checked: $cases cases, $checks checks, $failures failures (jq: $("$JQ" --version); thresholds: $ROOT/harness/thresholds.json)"
if [ "$cases" -eq 0 ] || [ "$checks" -eq 0 ]; then
  echo "FAIL nothing was checked"
  exit 1
fi
[ "$failures" -eq 0 ]
