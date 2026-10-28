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
# - The memory split (soak 40139563554's drift): the measures read the fixture's planted steps
#   (make-green.py: 9 MB of RSS, 8 of anon, 1 of CEF's renderer and 10 of the tree per dropout, typed
#   here from its constants, never from verdict.jq), a missing line leaves its dropout unmeasured,
#   and the soak's run is never gated on them (its page=real log may be withheld). On the
#   experiment's schedules (fixtures/green-dropouts, fixtures/green-none: its control, nothing in
#   the drive) check 1.split fails on a log with no line in the drive, none with the RSS, none with
#   a CEF poll, no PSS read, a kind listed and unread, a poll that does not advance, and no step
#   measured from the lines or from the beats' windows. A run with steady growth and no dropout
#   effect reads its steps equal to the control between them (excess 0), and the windowed step the
#   same on the dropouts run and on the none run.
# - Check 1's drift bound (owner, 2026-10-25: soak.json's memory_drift_max, read here, never typed):
#   0.1 percentage point under it and exactly at it pass, 0.1 over it fails (the gate is `>`); the
#   3 h soak 40139563554's 22.7 % and memory arm 1 40166664556's 13.2 % pass.
# - Check 1.projection (owner ruling A, 2026-10-25): the drive's memMb fitted against time and
#   projected to soak.json's session cap, within its budget fraction of the Machine's memory
#   (--memory-mb, the production guest's from soak.json). A flat run and 104 MB/h from 954 MB pass,
#   a steep run reds, and too few beats, none, or one (no spread in time) red. The expected fits are
#   the planted lines' own constants (a perfect line fits itself), never verdict.jq's output.
#
# Prints one line per case and `checked: N cases, M checks`; zero of either is a FAIL.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
JQ="${JQ:-jq}"
GREEN="$HERE/fixtures/green"
# The compositor Machine's memory the verdict is told (soak.yml passes its C_MEM): by default the
# production guest's (soak.json's memory_projection.production_guest, soak.yml's default guest;
# test-workflow.sh pins both). The check 1.projection section moves it for one case at a time.
MEM_PROD="$("$JQ" -er '.compositor_guest[.memory_projection.production_guest].memory_mb' "$HERE/soak.json")"
MEM="$MEM_PROD"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cases=0
checks=0
failures=0
fail() {
  echo "FAIL $*"
  failures=$((failures + 1))
}

# A green's seven inputs as one JSON bundle; a red is a jq filter over it.
bundle() {
  local g="$1"
  "$JQ" -n --slurpfile report "$g/report.json" --slurpfile actions "$g/actions.jsonl" \
    --slurpfile control "$g/control-state.json" --slurpfile deliveries "$g/deliveries.jsonl" \
    --slurpfile samples "$g/overlay-samples.jsonl" --slurpfile logscan "$g/logscan.json" \
    --rawfile complog "$g/compositor.log" \
    '{report: $report[0], actions: $actions, control: $control[0], deliveries: $deliveries,
      samples: $samples, logscan: $logscan[0], complog: $complog}'
}
bundle "$GREEN" >"$WORK/green.json"
bundle "$HERE/fixtures/green-dropouts" >"$WORK/green-dropouts.json"
bundle "$HERE/fixtures/green-none" >"$WORK/green-none.json"
# What case_ patches, and the schedule the verdict judges it by (the dropouts section moves both).
BASE="$WORK/green.json"
SCHEDULE=soak

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
  "$JQ" -j '.complog // ""' "$b" >"$d/compositor.log"
  local rc=0
  "$HERE/verdict.sh" --dir "$d" --page "$page" --drive-min "$drive" --schedule "$SCHEDULE" --memory-mb "$MEM" --jq "$JQ" >"$out" 2>"$out.err" || rc=$?
  echo "$rc" >"$out.rc"
}

# Every check verdict.jq declares, in order: a dropped one fails the green. Check 1.split is the
# memory experiment's (schedules dropouts and none), never the soak's.
ALL_SOAK='inputs,1,1.projection,2,3,4,5.beats,5.dropouts,5.disconnects,5a,5b,5c,5d,5e,6'
ALL_EXPERIMENT='inputs,1,1.projection,1.split,2,3,4,5.beats,5.dropouts,5.disconnects,5a,5b,5c,5d,5e,6'

# `case_ <name> <page> <want status> <want failed ids, sorted, comma-joined> <reason> <filter>`
case_() {
  local name="$1" page="$2" want="$3" want_failed="$4" reason="$5" filter="$6" b out got failed ids n
  cases=$((cases + 1))
  b="$WORK/case-$name.json"
  # A filter may read soak.json as $soak[0] (the drift cases' bound).
  if ! "$JQ" --slurpfile soak "$HERE/soak.json" "$filter" "$BASE" >"$b" 2>"$WORK/$name.patch.err"; then
    fail "$name: the patch does not apply: $(head -c 300 "$WORK/$name.patch.err")"
    return
  fi
  if [ "$want" = fail ] && cmp -s "$b" "$BASE"; then
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
  local all="$ALL_SOAK"
  [ "$SCHEDULE" = soak ] || all="$ALL_EXPERIMENT"
  if [ "$ids" != "$all" ]; then
    fail "$name: the verdict ran checks [$ids], the $SCHEDULE schedule declares [$all]"
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
# Memory (owner, 2026-10-25: the bound is soak.json's memory_drift_max, read here as $max, never
# typed): every memMb in the drive's first two parts m1 and in its last part m3, so part 1's median
# is exactly m1 and part 3's exactly m3 (the drive's last third against its first). m1 = 500 MB, so
# 0.5 MB is 0.1 percentage point, and a step at the bound does not also project over check
# 1.projection's budget in a 30-minute fixture (the same 40 % step on 1000 MB fits about
# 1 070 MB/h: 6 200 MB at 5 h). The red is 0.1 point over the bound.
DRIFT='(.actions[] | select(.kind == "start") | .at_ms) as $s | ($soak[0].memory_drift_max) as $max
  | .report.heartbeats.series |= map(if (.body.memMb | type) == "number"
      then .body.memMb = (if .received_ms >= $s + 1200000 then m3($max) else m1 end) else . end)'
DRIFT_OVER_REASON="$("$JQ" -r '.memory_drift_max as $max
  | "memory drift: hour 3'"'"'s median \(500 + 500 * $max + 0.5) MB is \($max * 100 + 0.1) % from hour 1'"'"'s 500 MB (at most \($max * 100) %)"' \
  "$HERE/soak.json")"
case_ memory-drift real fail '1' "1:$DRIFT_OVER_REASON" 'def m1: 500; def m3($max): m1 + m1 * $max + 0.5; '"$DRIFT"
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
case_ no-report real fail '1,1.projection,2,4,5.beats,5.disconnects,5a,5b,5c,5e,6,inputs' 'inputs:no report' '.report = null'
case_ no-frames real fail 'inputs' 'no frames' '.report.frames = 0'
# No start action: the drive's windows are unknown, so every windowed check fails too.
case_ no-start real fail '1,1.projection,5.beats,5a,5c,inputs' 'inputs:one start and one end action' 'del(.actions[] | select(.kind == "start"))'
case_ drive-mismatch real fail 'inputs' 'drive' '(.actions[] | select(.kind == "start") | .drive_s) = 1500'
case_ not-ended real fail 'inputs' 'did not end as asked' '.report.heartbeats.series[-1].body.exitReason = "deadline"'
case_ checker-failure real fail 'inputs' 'checker failed' '.report.checker_failures = ["a rung outside the declared ones"]'
# A third of the drive's beats without memMb is also under check 1.projection's floor of beats.
case_ memory-no-beats real fail '1,1.projection' '1:no memMb' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .report.heartbeats.series |= map(if .received_ms >= $s + 1200000 then del(.body.memMb) else . end)'
# The three anti-vacuity guards nothing else reaches alone: a renamed beat field must fail, never
# pass on nothing (hour 1's memMb; droppedFrames and rung on every beat).
case_ memory-no-beats-hour1 real fail '1,1.projection' '1:no memMb in hour 1' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .report.heartbeats.series |= map(if .received_ms < $s + 600000 then del(.body.memMb) else . end)'
case_ no-dropped-frames-field real fail '4' 'no beat carried droppedFrames' '.report.heartbeats.series |= map(del(.body.droppedFrames))'
case_ no-rung-field real fail '6' 'no beat carried a rung' '.report.heartbeats.series |= map(del(.body.rung))'
case_ pts-gap real fail '2' 'pts gaps' '.report.pts_gaps = [{at_ms: 1792800400000, gap_ms: 400}]'
case_ late-reconnect real fail '2' 'after its disconnect ended' \
  '.report.output_outages[1].to_wall_ms = ((.actions[] | select(.kind == "disconnect" and .slot == 1) | .end_ms) + 8000)'
case_ dropped-frames real fail '4' 'dropped frames' '.report.heartbeats.series[-2].body.droppedFrames = 60'
# Fewer beats than 5.beats's floor are fewer memMb beats than check 1.projection fits too.
case_ few-beats real fail '1.projection,5.beats' '5.beats:heartbeats' \
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
case_ no-log real fail '5b,5e' '5e:no compositor log' '.report.log.lines = 0 | .logscan.lines = 0'
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
# (e), Owed 99 (the PR #92 review's I-1): a blank-page reload, on either page (the test page judges it
# too); a page whose first ready has no first frame at or after it; none checked; and a report with no
# blank-reload facts. A held teardown's late ready of its own generation is no page.
BLANK_RELOAD='.report.log.blank_reloads = [{at_ms: (.report.log.cef_ready[1].at_ms + 10000), gen: 3, blank_pages: 1}]'
case_ blank-reload real fail '5e' 'blank-page reload gen=3 blank_pages=1' "$BLANK_RELOAD"
case_ blank-reload-test-page test fail '5e' 'blank-page reload gen=3' "$BLANK_RELOAD"
case_ page-unpainted real fail '5e' 'page gen=2 ready at' '.report.log.cef_painted |= map(select(.gen != 2))'
case_ painted-before-ready real fail '5e' 'reported no first frame' '.report.log.cef_painted[1].at_ms -= 1'
case_ no-page-checked real fail '5e' 'no page checked' '.report.log.cef_ready = []'
case_ no-blank-facts real fail '5e' 'no blank-page reload facts' 'del(.report.log.blank_reloads)'
case_ late-ready-no-page real pass '' '' '.report.log.cef_ready += [{at_ms: (.report.log.cef_ready[1].at_ms + 5000), gen: 2}]'

# --- check 1's bound and check 1.projection (owner rulings, 2026-10-25) -----------------------------
# `measured <name> <out> <check id> <jq test>`: one check's `measured`, with soak.json as $soak, the
# thresholds as $t, the Machine's memory the case ran with as $mem and the production guest's as
# $prod, against numbers the case planted.
measured() {
  local name="$1" out="$2" id="$3" test="$4"
  cases=$((cases + 1))
  checks=$((checks + 1))
  if "$JQ" -e --arg id "$id" --slurpfile soak "$HERE/soak.json" --slurpfile t "$ROOT/harness/thresholds.json" \
    --argjson mem "$MEM" --argjson prod "$MEM_PROD" \
    '$soak[0] as $soak | $t[0] as $t | [.checks[] | select(.id == $id) | .measured] as $m | ($m | length) == 1 and ($m[0] | '"$test"')' \
    "$out" >/dev/null 2>&1; then
    echo "PASS measured $name"
  else
    fail "measured $name: $test, got $("$JQ" -c --arg id "$id" '.checks[] | select(.id == $id) | .measured' "$out" 2>/dev/null | head -c 600)"
  fi
}
# Check 1 at the bound: 0.1 point under it and exactly at it pass (the gate is `>`), and each reads
# the drift it was planted with (the red 0.1 over it is the brief's red above).
case_ drift-under-bound real pass '' '' 'def m1: 500; def m3($max): m1 + m1 * $max - 0.5; '"$DRIFT"
case_ drift-at-bound real pass '' '' 'def m1: 500; def m3($max): m1 + m1 * $max; '"$DRIFT"
measured "0.1 point under the bound: part 1 500 MB, part 3 0.5 MB under the bound" "$WORK/case-drift-under-bound.out" 1 \
  '.hour1_median_mb == 500 and .hour3_median_mb == 500 + 500 * $soak.memory_drift_max - 0.5 and (.drift_pct * 10 | round) == ($soak.memory_drift_max * 1000 - 1 | round)'
measured "exactly at the bound" "$WORK/case-drift-at-bound.out" 1 \
  '.hour1_median_mb == 500 and .hour3_median_mb == 500 + 500 * $soak.memory_drift_max and (.drift_pct * 10 | round) == ($soak.memory_drift_max * 1000 | round)'
measured "0.1 point over the bound" "$WORK/case-memory-drift.out" 1 \
  '.hour1_median_mb == 500 and .hour3_median_mb == 500 + 500 * $soak.memory_drift_max + 0.5 and (.drift_pct * 10 | round) == ($soak.memory_drift_max * 1000 + 1 | round)'
# The two readings the owner ruled on: the 3 h soak 40139563554 (hour 1's median 994 MB, hour 3's
# 1220 MB: 22.7 %) and memory arm 1 40166664556 (13.2 %). Both pass. Check 1 reads a ratio, so each
# is planted on about 500 MB, like the bound's cases: the soak's medians halved (497 and 610 MB,
# still 22.7 %), arm 1's on 500 MB (its medians are not in this repo). The reason (PR #88 review
# m3): these are whole-verdict cases, and in a 30-minute fixture a part-3 step fits about 6 times
# the real run's slope (the step comes 20 minutes in, not 2 hours). On the real 994 MB the soak's
# step projects 3932 MB, 36 % under check 1.projection's 6144 MB budget (4 % under the 4096 MB it
# was before 2026-10-27), so a smaller guest, cap or fraction could red a case named for check 1;
# halved it projects about 1966 MB (the real run fits 108.4 MB/h from 945.9 MB: 1487.8 MB). The
# bound's own cases project about 3100 MB.
case_ drift-soak-40139563554 real pass '' '' 'def m1: 497; def m3($max): 610; '"$DRIFT"
measured "the 3 h soak's 22.7 % passes (its medians halved)" "$WORK/case-drift-soak-40139563554.out" 1 \
  '.hour1_median_mb == 497 and .hour3_median_mb == 610 and .drift_pct == 22.7'
# The coupling, watched: the case keeps a quarter of the budget to spare, or this names it.
measured "the 3 h soak's case is not carried by a near miss on the budget (a quarter to spare)" "$WORK/case-drift-soak-40139563554.out" 1.projection \
  '.projected_mb <= .budget_mb * 0.75'
case_ drift-arm1-40166664556 real pass '' '' 'def m1: 500; def m3($max): 566; '"$DRIFT"
measured "memory arm 1's 13.2 % passes" "$WORK/case-drift-arm1-40166664556.out" 1 \
  '.hour1_median_mb == 500 and .hour3_median_mb == 566 and .drift_pct == 13.2'

# Check 1.projection: memMb = a + b * h over the drive (h: hours since the start action), and 99999
# on the beats outside it (12 before, 4 after), which the fit must not read. A perfect line fits
# itself, so the fit is (b MB/h, a MB) and the projection a + b * session_cap_min / 60, typed here
# from the planted constants and soak.json.
PROJ='(.actions[] | select(.kind == "start") | .at_ms) as $s
  | .report.heartbeats.series |= map(if (.body.memMb | type) == "number"
      then .body.memMb = (if .received_ms >= $s and .received_ms < $s + 1800000 then a + b * (.received_ms - $s) / 3600000 else 99999 end)
      else . end)'
# The drive's beats carrying memMb in a case's bundle, counted here (never read from the verdict).
fitted() {
  "$JQ" '(.actions[] | select(.kind == "start") | .at_ms) as $s
    | [.report.heartbeats.series[] | select(.received_ms >= $s and .received_ms < $s + 1800000 and (.body.memMb | type) == "number")] | length' "$1"
}
# The floor: check 5.beats's (the drive's beats at heartbeat_interval_ms, times heartbeat_min_ratio).
PROJ_FLOOR='.want_beats == 1800000 / $t.heartbeat_interval_ms * $soak.heartbeat_min_ratio'
# The budget: the fraction of the smaller of the Machine's memory and the production guest's.
PROJ_BUDGET='.budget_mb == ([$mem, $prod] | min) * $soak.memory_projection.memory_budget_fraction and .machine_memory_mb == $mem
  and .production_memory_mb == $prod and .production_guest == $soak.memory_projection.production_guest
  and .session_cap_min == $soak.memory_projection.session_cap_min'
# Flat: 0 MB/h, so the projection is the level.
case_ projection-flat real pass '' '' 'def a: 1000; def b: 0; '"$PROJ"
measured "a flat run: 0 MB/h from 1000 MB projects 1000 MB, every drive beat fitted" "$WORK/case-projection-flat.out" 1.projection \
  ".slope_mb_per_h == 0 and .intercept_mb == 1000 and .projected_mb == 1000 and .beats_fitted == $(fitted "$WORK/case-projection-flat.json") and $PROJ_FLOOR and $PROJ_BUDGET"
# 104 MB/h from 954 MB (the coordinator's example: about 1474 MB at 300 min): under the budget.
case_ projection-104 real pass '' '' 'def a: 954; def b: 104; '"$PROJ"
measured "104 MB/h from 954 MB projects 954 + 104 * cap / 60 MB, the beats outside the drive unread" "$WORK/case-projection-104.out" 1.projection \
  ".slope_mb_per_h == 104 and .intercept_mb == 954 and .projected_mb == 954 + 104 * \$soak.memory_projection.session_cap_min / 60
   and .beats_fitted == $(fitted "$WORK/case-projection-104.json") and $PROJ_BUDGET"
# Steep: 1000 MB/h from 1500 MB projects 6500 MB at the cap, over the budget (performance-6x's half
# is 6144 MB), while check 1 still passes (part 1's median about 1583 MB, part 3's about 1916: 21.1 %),
# so only the projection reds.
STEEP_REASON="$("$JQ" -r --argjson prod "$MEM_PROD" '.memory_projection as $p
  | "reaches \(1500 + 1000 * $p.session_cap_min / 60) MB at the session cap of \($p.session_cap_min) min, over the budget of \($prod * $p.memory_budget_fraction) MB"' \
  "$HERE/soak.json")"
case_ projection-steep real fail '1.projection' "1.projection:$STEEP_REASON" 'def a: 1500; def b: 1000; '"$PROJ"
measured "a steep run: 1000 MB/h from 1500 MB, its projection over the budget" "$WORK/case-projection-steep.out" 1.projection \
  '.slope_mb_per_h == 1000 and .intercept_mb == 1500 and .projected_mb == 1500 + 1000 * $soak.memory_projection.session_cap_min / 60
   and .projected_mb > .budget_mb and '"$PROJ_BUDGET"
# The budget's edge (PR #88 review m1): the projection is judged as it is reported, to 0.1 MB, and
# the gate is `>`. 500 MB/h from the budget less 500 * cap / 60, plus d: 0.1 under the budget and
# exactly at it pass, 0.1 over it fails (check 1 reads about 10 %). The budget here is the
# production guest's, from soak.json.
AT_BUDGET='$soak[0] as $k | ($k.compositor_guest[$k.memory_projection.production_guest].memory_mb * $k.memory_projection.memory_budget_fraction) as $budget
  | def b: 500; def a: $budget - b * $k.memory_projection.session_cap_min / 60 + d; '
case_ projection-under-budget real pass '' '' 'def d: -0.1; '"$AT_BUDGET$PROJ"
case_ projection-at-budget real pass '' '' 'def d: 0; '"$AT_BUDGET$PROJ"
case_ projection-over-budget real fail '1.projection' \
  "1.projection:over the budget of $("$JQ" -r '.compositor_guest[.memory_projection.production_guest].memory_mb * .memory_projection.memory_budget_fraction' "$HERE/soak.json") MB" \
  'def d: 0.1; '"$AT_BUDGET$PROJ"
measured "0.1 MB under the budget" "$WORK/case-projection-under-budget.out" 1.projection \
  '(.projected_mb * 10 | round) == (.budget_mb * 10 - 1) and .budget_mb == $prod * $soak.memory_projection.memory_budget_fraction'
measured "exactly at the budget passes" "$WORK/case-projection-at-budget.out" 1.projection \
  '.projected_mb == .budget_mb and .budget_mb == $prod * $soak.memory_projection.memory_budget_fraction and .slope_mb_per_h == 500'
measured "0.1 MB over the budget" "$WORK/case-projection-over-budget.out" 1.projection \
  '(.projected_mb * 10 | round) == (.budget_mb * 10 + 1)'
# The guest (the coordinator's decision, 2026-10-25): a soak on a bigger guest budgets the production
# guest's memory, never its own. performance-8x's 16384 MB still budgets production's fraction, so
# the steep run still reds. A smaller Machine than production's (performance-4x's, from soak.json;
# the smaller side of the rule) budgets its own: a flat run at three quarters of production's budget
# reds on performance-4x's memory.
MEM_4X="$("$JQ" -er '.compositor_guest["performance-4x"].memory_mb' "$HERE/soak.json")"
MEM_8X="$("$JQ" -er '.compositor_guest["performance-8x"].memory_mb' "$HERE/soak.json")"
cases=$((cases + 1))
checks=$((checks + 1))
if [ "$MEM_8X" -gt "$MEM_PROD" ]; then echo "PASS the 8x guest is bigger than production's ($MEM_8X > $MEM_PROD MB)"
else fail "the 8x guest's $MEM_8X MB is not bigger than production's $MEM_PROD MB: the next cases prove nothing"; fi
MEM="$MEM_8X"
case_ projection-steep-8x real fail '1.projection' "1.projection:$STEEP_REASON" 'def a: 1500; def b: 1000; '"$PROJ"
measured "an 8x run (C_MEM $MEM_8X) still budgets production's memory" "$WORK/case-projection-steep-8x.out" 1.projection \
  '.machine_memory_mb == $mem and $mem > $prod and .budget_mb == $prod * $soak.memory_projection.memory_budget_fraction
   and .projected_mb > .budget_mb and '"$PROJ_BUDGET"
cases=$((cases + 1))
checks=$((checks + 1))
if [ "$MEM_4X" -lt "$MEM_PROD" ]; then echo "PASS the 4x guest is smaller than production's ($MEM_4X < $MEM_PROD MB)"
else fail "the 4x guest's $MEM_4X MB is not smaller than production's $MEM_PROD MB: the next case proves nothing"; fi
MEM="$MEM_4X"
case_ projection-small-guest real fail '1.projection' \
  "1.projection:over the budget of $("$JQ" -r --argjson m "$MEM" '$m * .memory_projection.memory_budget_fraction' "$HERE/soak.json") MB" \
  '$soak[0] as $k | ($k.compositor_guest[$k.memory_projection.production_guest].memory_mb * $k.memory_projection.memory_budget_fraction * 0.75) as $level
   | def a: $level; def b: 0; '"$PROJ"
measured "a Machine smaller than production's (performance-4x, C_MEM $MEM_4X) budgets its own memory" "$WORK/case-projection-small-guest.out" 1.projection \
  '.machine_memory_mb == $mem and $mem < $prod and .budget_mb == $mem * $soak.memory_projection.memory_budget_fraction
   and .projected_mb < $prod * $soak.memory_projection.memory_budget_fraction and '"$PROJ_BUDGET"
MEM="$MEM_PROD"
# The floor's edge (PR #88 review m2): memMb on exactly the floor's beats (check 5.beats's, rounded
# up to a whole beat) passes; one fewer fails. The beats after the kept ones lose memMb.
FLOOR_N="$("$JQ" -n --slurpfile t "$ROOT/harness/thresholds.json" --slurpfile s "$HERE/soak.json" \
  '1800000 / $t[0].heartbeat_interval_ms * $s[0].heartbeat_min_ratio | ceil')"
KEEP='(.actions[] | select(.kind == "start") | .at_ms) as $s
  | ([.report.heartbeats.series[] | select(.received_ms >= $s and .received_ms < $s + 1800000 and (.body.memMb | type) == "number")][keep].received_ms) as $cut
  | .report.heartbeats.series |= map(if .received_ms >= $cut and .received_ms < $s + 1800000 then del(.body.memMb) else . end)'
case_ projection-at-floor real pass '' '' "def keep: $FLOOR_N; $KEEP"
case_ projection-under-floor real fail '1.projection' "1.projection:$((FLOOR_N - 1)) drive beats carry memMb, want at least" \
  "def keep: $((FLOOR_N - 1)); $KEEP"
measured "exactly the floor's beats" "$WORK/case-projection-at-floor.out" 1.projection \
  ".beats_fitted == $FLOOR_N and .beats_fitted == $(fitted "$WORK/case-projection-at-floor.json") and .beats_fitted >= .want_beats and $PROJ_FLOOR"
measured "one beat under the floor" "$WORK/case-projection-under-floor.out" 1.projection \
  ".beats_fitted == $((FLOOR_N - 1)) and .beats_fitted == $(fitted "$WORK/case-projection-under-floor.json")"
# A production_guest soak.json's compositor_guest lacks is a named red, never a jq error or a pass:
# the verdict run from a copy of the harness whose soak.json names performance-2x.
cases=$((cases + 1))
checks=$((checks + 1))
ALT="$WORK/alt-harness/soak"
mkdir -p "$ALT"
cp "$HERE/verdict.sh" "$HERE/verdict.jq" "$HERE/schedule.jq" "$ALT/"
"$JQ" '.memory_projection.production_guest = "performance-2x"' "$HERE/soak.json" >"$ALT/soak.json"
rc=0
"$ALT/verdict.sh" --dir "$GREEN" --page real --drive-min 30 --memory-mb "$MEM" --thresholds "$ROOT/harness/thresholds.json" --jq "$JQ" \
  >"$WORK/alt.out" 2>"$WORK/alt.err" || rc=$?
if [ "$rc" -eq 1 ] && "$JQ" -e '[.checks[] | select(.status == "fail") | .id] == ["1.projection"]
    and ([.checks[] | select(.id == "1.projection") | .reason | contains("no budget: soak.json'"'"'s memory_projection.production_guest (\"performance-2x\") names no compositor_guest")] == [true])
    and ([.checks[] | select(.id == "1.projection") | .measured.budget_mb] == [null])' "$WORK/alt.out" >/dev/null 2>&1; then
  echo "PASS a production_guest not in compositor_guest: 1.projection fails by name, no budget"
else
  fail "a production_guest not in compositor_guest: exit $rc: $(head -c 300 "$WORK/alt.err") $("$JQ" -c '[.checks[] | select(.status == "fail") | {id, reason}]' "$WORK/alt.out" 2>/dev/null | head -c 400)"
fi
# Anti-vacuity: memMb on every fourth drive beat only (check 1 still has both parts' medians) is
# under the floor; none fits nothing; one has no spread in time to fit (and check 1 has no part 3).
case_ projection-few real fail '1.projection' '1.projection:drive beats carry memMb, want at least' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .report.heartbeats.series |= [to_entries[] | if .value.received_ms >= $s and .value.received_ms < $s + 1800000 and .key % 4 != 0
       then del(.value.body.memMb) else . end | .value]'
measured "every fourth beat: the beats fitted counted, under the floor" "$WORK/case-projection-few.out" 1.projection \
  ".beats_fitted == $(fitted "$WORK/case-projection-few.json") and .beats_fitted > 0 and .beats_fitted < .want_beats and $PROJ_FLOOR"
case_ projection-none real fail '1,1.projection' '1.projection:0 drive beats carry memMb' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .report.heartbeats.series |= map(if .received_ms >= $s and .received_ms < $s + 1800000 then del(.body.memMb) else . end)'
measured "no memMb in the drive: 0 fitted, no slope, no projection (null, never 0)" "$WORK/case-projection-none.out" 1.projection \
  '.beats_fitted == 0 and .slope_mb_per_h == null and .intercept_mb == null and .projected_mb == null'
case_ projection-one real fail '1,1.projection' '1.projection:no spread in time' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | (.report.heartbeats.series | map(.received_ms >= $s) | index(true)) as $first
   | .report.heartbeats.series |= [to_entries[] | if .value.received_ms >= $s and .value.received_ms < $s + 1800000 and .key != $first
       then del(.value.body.memMb) else . end | .value]'
measured "one memMb beat: 1 fitted, no slope" "$WORK/case-projection-one.out" 1.projection \
  '.beats_fitted == 1 and .slope_mb_per_h == null and .projected_mb == null'
# The green's own fit: every drive beat, under the budget.
measured "the green: every drive beat fitted, under the budget" "$WORK/case-green.out" 1.projection \
  ".beats_fitted == $(fitted "$WORK/case-green.json") and .projected_mb < .budget_mb and $PROJ_FLOOR and $PROJ_BUDGET"

# --- the memory split (soak 40139563554's drift) ----------------------------------------------------
# The soak is never gated on the memory split (PR #82 review m1: on page=real the runner withholds a
# compositor log the harness did not scan): no log at all still passes the soak.
case_ soak-log-withheld real pass '' '' '.complog = ""'
# The driver's start action names the schedule it ran; another one than the verdict's fails the run.
case_ schedule-mismatch real fail 'inputs' 'the driver ran the dropouts schedule, the verdict judges soak' \
  '(.actions[] | select(.kind == "start") | .schedule) = "dropouts"'
# A driver older than the field ran the soak.
case_ start-without-schedule real pass '' '' 'del(.actions[] | select(.kind == "start") | .schedule)'

# `measure <name> <out> <jq test>`: a measure the verdict printed, against the fixture's planted
# numbers (make-green.py: STEP_RSS_KB 9216 = 9 MB, STEP_ANON_KB 8192 = 8 MB, STEP_CEF_KB 1024 = 1 MB,
# STEP_TREE_MB 10, one input rebuild per dropout; file, shmem and threads constant; the beats'
# noise's median exactly 1 in every window).
measure() {
  local name="$1" out="$2" test="$3"
  cases=$((cases + 1))
  checks=$((checks + 1))
  if "$JQ" -e ".measures.memory_split | $test" "$out" >/dev/null 2>&1; then
    echo "PASS measure $name"
  else
    fail "measure $name: $test, got $("$JQ" -c '.measures.memory_split | del(.dropouts, .gaps)' "$out" 2>/dev/null | head -c 900)"
  fi
}
G="$WORK/case-green.out"
# The soak green: 6 dropouts, every one measured at the planted step. Lines come at minute k + 5 s.
# The drive's first part (minutes 0-10) holds lines after 0,0,0,1,1,1,1,1,2,2 dropouts (ends at 160
# and 460 s): median 1 step; the last part (minutes 20-30) after 4,4,4,5,5,5,5,5,6,6: median 5. Each
# step's lines are 60 s apart (the last line at least 10 s before the dropout, the first at least
# 30 s after it). Two gaps lie between two dropouts with no disconnect between (dropouts 1-2 and
# 3-4), each with two lines 240 s apart and no step in between: control 0, excess the whole step.
measure "soak green: 30 lines in the drive, of 32" "$G" '.lines == 32 and .lines_in_drive == 30'
measure "soak green: 6 of 6 dropouts measured over 60 s, each +9 MB rss, +8 anon, +1 CEF and children, one rebuild" "$G" \
  '.steps.of == "dropouts" and .steps.instants == 6 and .steps.measured == 6 and .steps.unmeasured == 0 and .steps.span_mean_s == 60
   and ([.dropouts[] | [.rss_step_mb, .anon_step_mb, .file_step_mb, .cef_step_mb, .children_step_mb, .gen_after - .gen_before, .span_s]] | unique) == [[9, 8, 0, 1, 1, 1, 60]]
   and .steps.rss_median_mb == 9 and .steps.rss_sum_mb == 54 and .steps.rebuilds == 6
   and .steps.rss_first_half_mean_mb == 9 and .steps.rss_second_half_mean_mb == 9'
measure "soak green: compositor-only growth 4 steps (36 MB rss, 32 anon, file/shmem/threads 0)" "$G" \
  '.compositor.growth == {rss_mb: 36, anon_mb: 32, file_mb: 0, shmem_mb: 0, threads: 0} and .compositor.first_part.threads == 87'
measure "soak green: CEF-only growth 4 MB, all of it the renderer's, and the children's RSS the same 4" "$G" \
  '.cef.growth == {cef_mb: 4, cef_renderer_mb: 4, cef_gpu_process_mb: 0, cef_utility_mb: 0, cef_zygote_mb: 0, cef_other_mb: 0, cef_children_rss_mb: 4}
   and .cef.first_part.cef_gpu_process_mb == 87.9'
measure "soak green: the control, 2 of 2 gaps over 240 s, no growth: excess the whole step (9, 8, 1)" "$G" \
  '.control.gaps == 2 and .control.measured == 2 and .control.span_mean_s == 240
   and .control.rss == {gaps: 2, rate_mb_per_min: 0, step_mean_mb: 9, at_step_span_mb: 0, excess_mb: 9}
   and .control.anon.excess_mb == 8 and .control.cef.excess_mb == 1'
measure "soak green: the windowed step on memMb, 6 of 6 at the tree's planted 10 MB" "$G" \
  '.window == {before_s: [-90, -10], after_s: [30, 110], measured: 6, mean_mb: 10, median_mb: 10, sum_mb: 60, first_half_mean_mb: 10, second_half_mean_mb: 10}
   and ([.dropouts[] | [.beats_before, .beats_after]] | unique) == [[16, 16]]'
# The empty case: no line, so no measure (null, never 0), and every dropout unmeasured.
measure "no lines: nothing measured from the lines, nulls not zeros; the windows still read the beats" "$WORK/case-soak-log-withheld.out" \
  '.lines == 0 and .steps.measured == 0 and .steps.unmeasured == 6 and .steps.rss_median_mb == null and .steps.span_mean_s == null
   and .compositor.growth.rss_mb == null and .cef.growth.cef_mb == null and .control.measured == 0 and .control.rss.excess_mb == null
   and .window.measured == 6'
# A dropout whose following gap lost its lines is unmeasured, the others still measured.
case_ split-gap real pass '' '' \
  '(.actions[] | select(.kind == "dropout" and .slot == 2)) as $d
   | def at: capture("ts=(?<t>[^ ]+)").t | sub("[.][0-9]+Z$"; "Z") | fromdateiso8601 * 1000;
     .complog |= ([split("\n")[] | select((test(" msg=memory split ") | not) or at < $d.end_ms or at >= $d.end_ms + 140000)] | join("\n"))'
measure "a gap with no line after dropout 2: it alone unmeasured" "$WORK/case-split-gap.out" \
  '.steps.measured == 5 and .steps.unmeasured == 1 and ([.dropouts[] | select(.rss_step_mb == null) | .slot] == [2])'
# No line between dropouts 3 and 4: neither is measured (3 has no line after it, 4 none before it
# that is after 3 ended), never a line from before dropout 3 standing in for 4's.
case_ split-gap-between real pass '' '' \
  '(.actions[] | select(.kind == "dropout" and .slot == 3)) as $d3 | (.actions[] | select(.kind == "dropout" and .slot == 4)) as $d4
   | def at: capture("ts=(?<t>[^ ]+)").t | sub("[.][0-9]+Z$"; "Z") | fromdateiso8601 * 1000;
     .complog |= ([split("\n")[] | select((test(" msg=memory split ") | not) or at < $d3.end_ms or at >= $d4.at_ms)] | join("\n"))'
measure "no line between dropouts 3 and 4: both unmeasured, and that gap no control" "$WORK/case-split-gap-between.out" \
  '.steps.measured == 4 and ([.dropouts[] | select(.rss_step_mb == null) | .slot] == [3, 4]) and .control.measured == 1'

# The dropout-only schedule's green (soak.json's dropout_only: every 125 s from minute 1, no
# disconnect): 13 dropouts in the fixture's 30 minutes, every one measured. Its lines, at minute
# k + 5 s, fall 120 s apart around 9 dropouts and 60 s around 4 (mean span 101.5 s), and 5 of the 12
# gaps hold two lines (60 s apart). First part after 0,0,1,1,2,2,3,3,4,4 dropouts (ends at 70 + 125j
# s): median 2; last part 10,10,11,11,11,12,12,13,13,13: median 11.5, so 9.5 steps of growth.
BASE="$WORK/green-dropouts.json"
SCHEDULE=dropouts
D="$WORK/case-dropouts-green.out"
case_ dropouts-green real pass '' '' '.'
measure "dropouts green: 13 of 13 measured at +9 MB over a mean 101.5 s; growth 9.5 steps (85.5 MB rss, 9.5 CEF)" "$D" \
  '.steps.instants == 13 and .steps.measured == 13 and .steps.rss_median_mb == 9 and .steps.rebuilds == 13 and .steps.span_mean_s == 101.5
   and ([.dropouts[] | .span_s] | (map(select(. == 120)) | length) == 9 and (map(select(. == 60)) | length) == 4)
   and .compositor.growth.rss_mb == 85.5 and .cef.growth.cef_mb == 9.5'
measure "dropouts green: the control, 5 of 12 gaps with two lines 60 s apart, no growth: excess the whole step" "$D" \
  '.control.gaps == 12 and .control.measured == 5 and .control.span_mean_s == 60 and .control.rss.excess_mb == 9 and .control.rss.rate_mb_per_min == 0'
measure "dropouts green: the windowed step, 13 of 13 at the tree's 10 MB" "$D" '.window.measured == 13 and .window.mean_mb == 10 and .window.median_mb == 10'
if ! "$JQ" -e '.plan == {dropouts: 13, disconnects: [], part_s: 600} and .schedule == "dropouts"' "$D" >/dev/null; then
  fail "dropouts-green: the plan is not dropout_plan's: $("$JQ" -c '.plan' "$D")"
fi
if ! "$JQ" -e '[.checks[] | select(.id == "1.split") | .measured]
    == [{lines: 32, in_drive: 30, with_rss: 30, with_cef_poll: 30, with_cef_pss: 30, unread_lines: 0, stale_pairs: 0, instants: 13, steps_measured: 13, windows_measured: 13}]' "$D" >/dev/null; then
  fail "dropouts-green: 1.split did not read every line: $("$JQ" -c '.checks[] | select(.id == "1.split") | .measured' "$D")"
fi
case_ dropouts-disconnect real fail '5.disconnects' '5.disconnects:disconnect actions the plan does not have: slots [0]' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .actions += [{kind: "disconnect", slot: 0, planned_ms: ($s + 300000), at_ms: ($s + 300004), end_ms: ($s + 330006), ok: true,
       dropped_packets: 412, families: ["ipv4"]}]
   | .report.output_outages += [{from_wall_ms: ($s + 300350), to_wall_ms: ($s + 332600), kind: "between",
       counter_before: null, counter_after: null, counter_gap_ms: null, lost_ms: 1200}]'
case_ dropouts-missing-dropout real fail '5.dropouts' '5.dropouts:dropout actions: 12 of 13' 'del(.actions[] | select(.kind == "dropout" and .slot == 9))'
# The soak's run judged as the dropout experiment: the start action says soak.
case_ dropouts-judging-a-soak-run real fail 'inputs' 'the driver ran the soak schedule, the verdict judges dropouts' \
  '(.actions[] | select(.kind == "start") | .schedule) = "soak"'
# A dropout that failed (ok false) is not a step: it bounds its neighbours and is left out (review m3).
case_ dropouts-one-failed real fail '5.dropouts' '5.dropouts:dropout actions: 12 of 13' '(.actions[] | select(.kind == "dropout" and .slot == 2) | .ok) = false'
measure "a failed dropout: 12 instants, slot 2 not among them, the rest measured" "$WORK/case-dropouts-one-failed.out" \
  '.steps.instants == 12 and .steps.measured == 12 and ([.dropouts[] | .slot] == [0, 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])'
# Check 1.split: zero lines read fails, and so do lines only outside the drive, lines without the
# compositor's RSS, and lines without a CEF poll (the live CEF reader never wired).
case_ split-none real fail '1.split' 'no memory split line in the drive (0 in the whole compositor log; the log is empty or missing' '.complog = ""'
case_ split-outside-drive real fail '1.split' 'no memory split line in the drive (2 in the whole compositor log)' \
  'def at: capture("ts=(?<t>[^ ]+)").t | sub("[.][0-9]+Z$"; "Z") | fromdateiso8601 * 1000;
   (.actions[] | select(.kind == "start") | .at_ms) as $s
   | .complog |= ([split("\n")[] | select((test(" msg=memory split ") | not) or at < $s or at >= $s + 1800000)] | join("\n"))'
case_ split-no-rss real fail '1.split' "carries the compositor's RSS" '.complog |= gsub(" rss_kb=[0-9]+ "; " rss_kb=- ")'
case_ split-no-cef real fail '1.split' 'carries a CEF poll' '.complog |= gsub(" cef_poll=[0-9]+ "; " cef_poll=- ")'
# Lines without a CEF poll: CEF's part is unknown (null), never a sum of what the line still names.
measure "no CEF poll: CEF growth and steps null, the compositor's still measured" "$WORK/case-split-no-cef.out" \
  '.cef.first_part.cef_mb == null and .cef.growth.cef_renderer_mb == null and .steps.cef_mean_mb == null
   and .steps.measured == 13 and .compositor.growth.rss_mb == 85.5'
# Review I2's probe: every kind listed and none read (`kb=- n=0`, the listed counts kept) used to
# read as a flat CEF and pass. It fails twice: no PSS read in the drive, and kinds listed unread.
case_ split-cef-unread-all real fail '1.split' "1.split:no memory split line in the drive carries a CEF child's PSS (30 polled, none read" \
  '.complog |= gsub(" cef_(?<k>[a-z_]+)_kb=[0-9]+ cef_[a-z_]+_n=[0-9]+ "; " cef_\(.k)_kb=- cef_\(.k)_n=0 ")'
if ! "$JQ" -e '[.checks[] | select(.id == "1.split") | .reason | contains("CEF listed children it read no PSS of in 30 of 30 drive lines")] == [true]' \
  "$WORK/case-split-cef-unread-all.out" >/dev/null; then
  fail "split-cef-unread-all: the unread kinds are not named: $("$JQ" -c '.checks[] | select(.id == "1.split") | .reason' "$WORK/case-split-cef-unread-all.out")"
fi
measure "every kind unread: CEF growth, steps and control null, never 0" "$WORK/case-split-cef-unread-all.out" \
  '.cef.growth.cef_mb == null and .cef.growth.cef_renderer_mb == null and .cef.first_part.cef_utility_mb == null
   and .steps.cef_mean_mb == null and .control.cef.excess_mb == null and .steps.measured == 13'
# One line where the renderer was listed and not read: that alone fails, by kind and count.
case_ split-cef-unread-one real fail '1.split' '1.split:CEF listed children it read no PSS of in 1 of 30 drive lines (first at 1792800665000: renderer listed 1, read 0)' \
  '.complog |= sub("(?<a>ts=2026-10-24T00:11:05[.]000Z [^\n]* )cef_renderer_kb=[0-9]+ cef_renderer_n=1 "; "\(.a)cef_renderer_kb=- cef_renderer_n=0 ")'
# A kind the poll listed none of: it reads 0 and adds 0, never drops the line. The first part's
# median total is then the renderer after two dropouts, the GPU process and the zygotes:
# (202048 + 90000 + 15000) kB = 299.85 MB, 299.9 to one decimal.
case_ split-cef-kind-absent real pass '' '' \
  '.complog |= gsub(" cef_utility_kb=40000 cef_utility_n=2 cef_utility_listed=2 "; " cef_utility_kb=- cef_utility_n=0 cef_utility_listed=0 ")'
measure "a kind with no child: 0, and CEF's total without it" "$WORK/case-split-cef-kind-absent.out" \
  '.cef.first_part.cef_utility_mb == 0 and .cef.first_part.cef_mb == 299.9 and .steps.cef_mean_mb == 1'
# A CEF poll that does not advance between two lines (minute 11's line, poll 1265, repeats minute
# 10's number):
# a stuck poll thread would read CEF as flat (review m2).
case_ split-stale-poll real fail '1.split' '1.split:the CEF poll did not advance between 1 pair(s) of drive lines (first at 1792800665000: poll 1205, then 1205)' \
  '.complog |= gsub(" cef_poll=1265 "; " cef_poll=1205 ")'
# Lines in the drive, but none where a step can be read: each kept line lies between 10 s before a
# dropout and 30 s after it (review m3: zero steps measured fails).
case_ split-no-step real fail '1.split' '1.split:no dropout step measured: 0 of 13 dropouts have a memory split line on both sides' \
  'def at: capture("ts=(?<t>[^ ]+)").t | sub("[.][0-9]+Z$"; "Z") | fromdateiso8601 * 1000;
   [.actions[] | select(.kind == "dropout") | .at_ms] as $ds
   | .complog |= ([split("\n")[] | select((test(" msg=memory split ") | not) or (at as $t | any($ds[]; $t > . - 10000 and $t < . + 30000)))] | join("\n"))'
# No memMb in any instant's window: the windowed step cannot be read (the rest of the drive's beats
# keep check 1).
case_ split-no-window real fail '1.projection,1.split' '1.split:no windowed step measured: 0 of 13 dropouts have memMb beats in both windows' \
  '[.actions[] | select(.kind == "dropout") | .at_ms] as $ds
   | .report.heartbeats.series |= map(.received_ms as $t
       | if any($ds[]; ($t >= . - 90000 and $t < . - 10000) or ($t >= . + 30000 and $t < . + 110000)) then del(.body.memMb) else . end)'
# Review I3: steady growth with no dropout effect (1 MB a minute of RSS and anon, the tree 1 MB a
# minute in the beats, CEF flat, no planted step). The steps then read the growth over their own span
# (9 of 120 s and 4 of 60 s: a mean 101.5 s, 1.7 MB), the control the same rate (1 MB a minute), so
# the control over the steps' span equals the steps and the excess is 0; the windowed step reads the
# 120 s between its windows' middles: 2 MB.
STEADY='def at: capture("ts=(?<t>[^ ]+)").t | sub("[.][0-9]+Z$"; "Z") | fromdateiso8601 * 1000;
  (.actions[] | select(.kind == "start") | .at_ms) as $s
  | .complog |= ([split("\n")[] | if test(" msg=memory split ") then (((at - $s + 55000) / 60000) as $k
      | sub(" rss_kb=[0-9]+ "; " rss_kb=\(612000 + 1024 * $k) ") | sub(" anon_kb=[0-9]+ "; " anon_kb=\(500000 + 1024 * $k) ")
      | sub(" cef_renderer_kb=[0-9]+ "; " cef_renderer_kb=200000 ")) else . end] | join("\n"))
  | .report.heartbeats.series |= map(if (.body.memMb | type) == "number"
      then ((.received_ms - $s + 60000) / 5000) as $i | .body.memMb = (1400 + ($i % 2) * 2 + ($i / 12 | floor)) else . end)'
case_ dropouts-steady real pass '' '' "$STEADY"
measure "steady growth, no dropout effect: the steps equal the control over their span, excess 0; windowed 2 MB" "$WORK/case-dropouts-steady.out" \
  '.steps.measured == 13 and .steps.rss_mean_mb == 1.7 and .control.measured == 5 and .control.rss.rate_mb_per_min == 1
   and .control.rss.at_step_span_mb == .steps.rss_mean_mb and .control.rss.excess_mb == 0
   and .control.anon.excess_mb == 0 and .control.cef.excess_mb == 0 and .window.mean_mb == 2 and .window.median_mb == 2'

# The control run (schedule none: the same drive, nothing in it): the steps are measured at the
# instants dropout_plan would have had. Its lines fall 120 s apart around 8 of them and 60 s around 5
# (exact instants, so the line at 965 s is 30 s after the instant at 935 s): a mean 96.9 s; 6 of the
# 12 gaps hold two lines.
BASE="$WORK/green-none.json"
SCHEDULE=none
N="$WORK/case-none-green.out"
case_ none-green real pass '' '' '.'
measure "none green: 13 phantoms measured, every step 0 (lines and windows), 6 of 12 gaps, excess 0" "$N" \
  '.steps.of == "phantoms" and .steps.instants == 13 and .steps.measured == 13 and .steps.span_mean_s == 96.9
   and .steps.rss_sum_mb == 0 and .steps.rebuilds == 0 and .control.measured == 6 and .control.rss.excess_mb == 0
   and .window.measured == 13 and .window.sum_mb == 0'
if ! "$JQ" -e '.plan == {dropouts: 0, disconnects: [], part_s: 600} and .schedule == "none"
    and ([.checks[] | select(.id == "5.dropouts") | .measured] == [{done: 0, want: 0}])' "$N" >/dev/null; then
  fail "none-green: the plan is not the control's: $("$JQ" -c '[.plan, (.checks[] | select(.id == "5.dropouts") | .measured)]' "$N")"
fi
case_ none-steady real pass '' '' "$STEADY"
measure "none, steady growth: the control's steps equal its gaps over their span (1.6 MB over 96.9 s), windowed 2 MB" "$WORK/case-none-steady.out" \
  '.steps.rss_mean_mb == 1.6 and .control.rss.at_step_span_mb == .steps.rss_mean_mb and .control.rss.excess_mb == 0
   and .window.mean_mb == 2'
# The windowed step on the dropouts run equals the control run's when dropouts change nothing.
cases=$((cases + 1))
checks=$((checks + 1))
if [ "$("$JQ" '.measures.memory_split.window.mean_mb' "$WORK/case-dropouts-steady.out")" = "$("$JQ" '.measures.memory_split.window.mean_mb' "$WORK/case-none-steady.out")" ] \
  && [ "$("$JQ" '.measures.memory_split.window.measured' "$WORK/case-none-steady.out")" = 13 ]; then
  echo "PASS the windowed step: the dropouts run and the none run read the same under steady growth alone"
else
  fail "the windowed step: dropouts $("$JQ" -c '.measures.memory_split.window' "$WORK/case-dropouts-steady.out"), none $("$JQ" -c '.measures.memory_split.window' "$WORK/case-none-steady.out")"
fi
case_ none-with-a-dropout real fail '5.dropouts' '5.dropouts:dropout actions the plan does not have: slots [0] (the none schedule plans 0)' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .actions += [{kind: "dropout", slot: 0, planned_ms: ($s + 60000), at_ms: ($s + 60003), end_ms: ($s + 70005), ok: true, pause: 200, resume: 200}]
   | .actions |= sort_by(.at_ms)'
case_ none-no-start real fail '1,1.projection,1.split,5.beats,5a,5c,inputs' '1.split:no start action' 'del(.actions[] | select(.kind == "start"))'
# Only the drive's first two lines (at 5 and 65 s): no phantom has a line on both sides.
case_ none-no-step real fail '1.split' '1.split:no phantom step measured: 0 of 13 phantoms' \
  'def at: capture("ts=(?<t>[^ ]+)").t | sub("[.][0-9]+Z$"; "Z") | fromdateiso8601 * 1000;
   (.actions[] | select(.kind == "start") | .at_ms) as $s
   | .complog |= ([split("\n")[] | select((test(" msg=memory split ") | not) or (at >= $s and at < $s + 120000))] | join("\n"))'
BASE="$WORK/green.json"
SCHEDULE=soak

# verdict.sh refuses a schedule it does not know (64), before any jq.
cases=$((cases + 1))
checks=$((checks + 1))
rc=0
"$HERE/verdict.sh" --dir "$GREEN" --page real --drive-min 30 --memory-mb "$MEM" --schedule weekly --jq "$JQ" >"$WORK/bad-schedule.out" 2>&1 || rc=$?
if [ "$rc" -eq 64 ] && grep -q 'schedule must be soak, dropouts or none' "$WORK/bad-schedule.out"; then
  echo "PASS verdict.sh --schedule weekly: refused (64)"
else
  fail "verdict.sh --schedule weekly: exit $rc: $(head -c 300 "$WORK/bad-schedule.out")"
fi
# It refuses a run without the Machine's memory, or one that is not whole MB above 0 (64), before
# any jq: check 1.projection's budget is a fraction of it.
for bad in none "" 0 08 8192.5 8G; do
  cases=$((cases + 1))
  checks=$((checks + 1))
  rc=0
  if [ "$bad" = none ]; then
    "$HERE/verdict.sh" --dir "$GREEN" --page real --drive-min 30 --jq "$JQ" >"$WORK/bad-mem.out" 2>&1 || rc=$?
  else
    "$HERE/verdict.sh" --dir "$GREEN" --page real --drive-min 30 --memory-mb "$bad" --jq "$JQ" >"$WORK/bad-mem.out" 2>&1 || rc=$?
  fi
  if [ "$rc" -eq 64 ] && grep -q 'memory-mb must be whole MB above 0' "$WORK/bad-mem.out"; then
    echo "PASS verdict.sh --memory-mb '$bad': refused (64)"
  else
    fail "verdict.sh --memory-mb '$bad': exit $rc: $(head -c 300 "$WORK/bad-mem.out")"
  fi
done

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
# The dropout-only schedule, by name through schedule_plan (what driver.sh and verdict.jq call): a
# dropout every 125 s from minute 1, each owning its whole period, and no disconnect; none is the
# same drive with nothing in it; the soak by name is soak_plan's; an unknown name and a short drive
# are refused.
splan() {
  "$JQ" -n -L "$HERE" --slurpfile soak "$HERE/soak.json" --argjson d "$2" --arg s "$1" -c \
    'include "schedule"; schedule_plan($soak[0]; $s; $d) | {dropouts: (.dropouts | length), first: .dropouts[0], last: .dropouts[-1], disconnects}'
}
splan_case() {
  local name="$1" sched="$2" drive_s="$3" want="$4" got
  cases=$((cases + 1))
  checks=$((checks + 1))
  if got="$(splan "$sched" "$drive_s" 2>&1)" && [ "$got" = "$want" ]; then
    echo "PASS schedule_plan $name: $got"
  else
    fail "schedule_plan $name: want $want, got $got"
  fi
}
splan_case "dropouts, the experiment's 75 min (an 80-minute run: 35 dropouts)" dropouts 4500 '{"dropouts":35,"first":60,"last":4310,"disconnects":[]}'
splan_case "dropouts, 25 min" dropouts 1500 '{"dropouts":11,"first":60,"last":1310,"disconnects":[]}'
splan_case "dropouts, the fixture's 30 min" dropouts 1800 '{"dropouts":13,"first":60,"last":1560,"disconnects":[]}'
splan_case "dropouts, 3 h" dropouts 10800 '{"dropouts":85,"first":60,"last":10560,"disconnects":[]}'
splan_case "none, the control's 75 min: nothing" none 4500 '{"dropouts":0,"first":null,"last":null,"disconnects":[]}'
splan_case "soak by name: soak_plan's 25 min" soak 1500 '{"dropouts":5,"first":150,"last":1350,"disconnects":[300,900,1200]}'
for refused in "weekly 1500 no schedule" "dropouts 600 under min_drive_min" "none 600 under min_drive_min"; do
  set -- $refused
  cases=$((cases + 1))
  checks=$((checks + 1))
  if out="$(splan "$1" "$2" 2>&1)"; then
    fail "schedule_plan $1 $2: planned: $out"
  elif printf '%s' "$out" | grep -q "$3 $4"; then
    echo "PASS schedule_plan $1 $2: refused ($3 $4)"
  else
    fail "schedule_plan $1 $2: refused for the wrong reason: $out"
  fi
done

echo "checked: $cases cases, $checks checks, $failures failures (jq: $("$JQ" --version); thresholds: $ROOT/harness/thresholds.json)"
if [ "$cases" -eq 0 ] || [ "$checks" -eq 0 ]; then
  echo "FAIL nothing was checked"
  exit 1
fi
[ "$failures" -eq 0 ]
