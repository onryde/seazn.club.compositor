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
# - The memory split (soak 40139563554's drift): check 1.split fails on a log with no line in the
#   drive, none with the RSS, none with a CEF poll; the measures read the fixture's planted steps
#   (make-green.py: 9 MB of RSS, 8 of anon and 1 of CEF's renderer per dropout, typed here from its
#   constants, never from verdict.jq), a missing line leaves its dropout unmeasured, and the
#   dropout-only schedule's green (fixtures/green-dropouts) passes, a disconnect in it failing.
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
  "$HERE/verdict.sh" --dir "$d" --page "$page" --drive-min "$drive" --schedule "$SCHEDULE" --jq "$JQ" >"$out" 2>"$out.err" || rc=$?
  echo "$rc" >"$out.rc"
}

# Every check verdict.jq declares, in order: a dropped one fails the green.
ALL='inputs,1,1.split,2,3,4,5.beats,5.dropouts,5.disconnects,5a,5b,5c,5d,6'

# `case_ <name> <page> <want status> <want failed ids, sorted, comma-joined> <reason> <filter>`
case_() {
  local name="$1" page="$2" want="$3" want_failed="$4" reason="$5" filter="$6" b out got failed ids n
  cases=$((cases + 1))
  b="$WORK/case-$name.json"
  if ! "$JQ" "$filter" "$BASE" >"$b" 2>"$WORK/$name.patch.err"; then
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
case_ no-start real fail '1,1.split,5.beats,5a,5c,inputs' 'inputs:one start and one end action' 'del(.actions[] | select(.kind == "start"))'
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

# --- the memory split (soak 40139563554's drift) ----------------------------------------------------
# Check 1.split: zero lines read fails, and so do lines only outside the drive, lines without the
# compositor's RSS, and lines without a CEF poll (the live CEF reader never wired).
case_ split-none real fail '1.split' 'no memory split line in the drive (0 in the whole' '.complog = ""'
case_ split-outside-drive real fail '1.split' 'no memory split line in the drive (2 in the whole' \
  'def at: capture("ts=(?<t>[^ ]+)").t | sub("[.][0-9]+Z$"; "Z") | fromdateiso8601 * 1000;
   (.actions[] | select(.kind == "start") | .at_ms) as $s
   | .complog |= ([split("\n")[] | select((test(" msg=memory split ") | not) or at < $s or at >= $s + 1800000)] | join("\n"))'
case_ split-no-rss real fail '1.split' "carries the compositor's RSS" '.complog |= gsub(" rss_kb=[0-9]+ "; " rss_kb=- ")'
case_ split-no-cef real fail '1.split' 'carries a CEF poll' '.complog |= gsub(" cef_poll=[0-9]+ "; " cef_poll=- ")'
# The driver's start action names the schedule it ran; another one than the verdict's fails the run.
case_ schedule-mismatch real fail 'inputs' 'the driver ran the dropouts schedule, the verdict judges soak' \
  '(.actions[] | select(.kind == "start") | .schedule) = "dropouts"'
# A driver older than the field ran the soak.
case_ start-without-schedule real pass '' '' 'del(.actions[] | select(.kind == "start") | .schedule)'

# `measure <name> <out> <jq test> <what it shows>`: a measure the verdict printed, against the
# fixture's planted numbers (make-green.py: STEP_RSS_KB 9216 = 9 MB, STEP_ANON_KB 8192 = 8 MB,
# STEP_CEF_KB 1024 = 1 MB, one input rebuild per dropout; file, shmem and threads constant).
measure() {
  local name="$1" out="$2" test="$3"
  cases=$((cases + 1))
  checks=$((checks + 1))
  if "$JQ" -e ".measures.memory_split | $test" "$out" >/dev/null 2>&1; then
    echo "PASS measure $name"
  else
    fail "measure $name: $test, got $("$JQ" -c '.measures.memory_split | del(.dropouts)' "$out" 2>/dev/null | head -c 700)"
  fi
}
G="$WORK/case-green.out"
# The soak green: 6 dropouts, every one measured at the planted step. The drive's first part
# (minutes 0-10) has lines at minute k + 5 s, k = 0..9, after 0,0,0,1,1,1,1,1,2,2 dropouts (ends at
# 160 and 460 s): median 1 step; the last part (minutes 20-30) after 4,4,4,5,5,5,5,5,6,6: median 5.
measure "soak green: 30 lines in the drive, of 32" "$G" '.lines == 32 and .lines_in_drive == 30'
measure "soak green: 6 of 6 dropouts measured, each +9 MB rss, +8 anon, +1 CEF, one rebuild" "$G" \
  '.steps.dropouts == 6 and .steps.measured == 6 and .steps.unmeasured == 0
   and ([.dropouts[] | [.rss_step_mb, .anon_step_mb, .file_step_mb, .cef_step_mb, .gen_after - .gen_before]] | unique) == [[9, 8, 0, 1, 1]]
   and .steps.rss_median_mb == 9 and .steps.rss_sum_mb == 54 and .steps.rebuilds == 6
   and .steps.rss_first_half_mean_mb == 9 and .steps.rss_second_half_mean_mb == 9'
measure "soak green: compositor-only growth 4 steps (36 MB rss, 32 anon, file/shmem/threads 0)" "$G" \
  '.compositor.growth == {rss_mb: 36, anon_mb: 32, file_mb: 0, shmem_mb: 0, threads: 0} and .compositor.first_part.threads == 87'
measure "soak green: CEF-only growth 4 MB, all of it the renderer's" "$G" \
  '.cef.growth == {cef_mb: 4, cef_renderer_mb: 4, cef_gpu_process_mb: 0, cef_utility_mb: 0, cef_zygote_mb: 0, cef_other_mb: 0}
   and .cef.first_part.cef_gpu_process_mb == 87.9'
# The empty case: no line, so no measure (null, never 0), and every dropout unmeasured.
measure "no lines: nothing measured, nulls not zeros" "$WORK/case-split-none.out" \
  '.lines == 0 and .steps.measured == 0 and .steps.unmeasured == 6 and .steps.rss_median_mb == null
   and .compositor.growth.rss_mb == null and .cef.growth.cef_mb == null'
# Lines without a CEF poll: CEF's part is unknown (null), never a sum of what the line still names.
measure "no CEF poll: CEF growth and steps null, the compositor's still measured" "$WORK/case-split-no-cef.out" \
  '.cef.first_part.cef_mb == null and .cef.growth.cef_renderer_mb == null and .steps.cef_mean_mb == null
   and .steps.measured == 6 and .compositor.growth.rss_mb == 36'
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
measure "no line between dropouts 3 and 4: both unmeasured" "$WORK/case-split-gap-between.out" \
  '.steps.measured == 4 and ([.dropouts[] | select(.rss_step_mb == null) | .slot] == [3, 4])'
# A CEF poll that found no child of a kind: that kind adds 0 to CEF's total, never drops the line.
# The first part's median total is then the renderer after one dropout, the GPU process and the
# zygotes: (201024 + 90000 + 15000) kB = 298.85 MB, 298.9 to one decimal.
case_ split-cef-kind-absent real pass '' '' '.complog |= gsub(" cef_utility_kb=40000 cef_utility_n=2 "; " cef_utility_kb=- cef_utility_n=0 ")'
measure "a kind with no child: CEF total without it" "$WORK/case-split-cef-kind-absent.out" \
  '.cef.first_part.cef_utility_mb == 0 and .cef.first_part.cef_mb == 298.9 and .steps.cef_mean_mb == 1'

# The dropout-only schedule's green (soak.json's dropout_only: every 2 min from minute 1, no
# disconnect): 14 dropouts in the fixture's 30 minutes, every one measured. First part after
# 0,0,1,1,2,2,3,3,4,4 dropouts (ends at 70 + 120k s): median 2; last part 10,10,11,11,12,12,13,13,14,14:
# median 12, so 10 steps.
BASE="$WORK/green-dropouts.json"
SCHEDULE=dropouts
case_ dropouts-green real pass '' '' '.'
measure "dropouts green: 14 of 14 measured at +9 MB; growth 10 steps (90 MB rss, 10 CEF)" "$WORK/case-dropouts-green.out" \
  '.steps.dropouts == 14 and .steps.measured == 14 and .steps.rss_median_mb == 9 and .steps.rebuilds == 14
   and .compositor.growth.rss_mb == 90 and .cef.growth.cef_mb == 10'
if ! "$JQ" -e '.plan == {dropouts: 14, disconnects: [], part_s: 600} and .schedule == "dropouts"' "$WORK/case-dropouts-green.out" >/dev/null; then
  fail "dropouts-green: the plan is not dropout_plan's: $("$JQ" -c '.plan' "$WORK/case-dropouts-green.out")"
fi
case_ dropouts-disconnect real fail '5.disconnects' '5.disconnects:disconnect actions the plan does not have: slots [0]' \
  '(.actions[] | select(.kind == "start") | .at_ms) as $s
   | .actions += [{kind: "disconnect", slot: 0, planned_ms: ($s + 300000), at_ms: ($s + 300004), end_ms: ($s + 330006), ok: true,
       dropped_packets: 412, families: ["ipv4"]}]
   | .report.output_outages += [{from_wall_ms: ($s + 300350), to_wall_ms: ($s + 332600), kind: "between",
       counter_before: null, counter_after: null, counter_gap_ms: null, lost_ms: 1200}]'
case_ dropouts-missing-dropout real fail '5.dropouts' '5.dropouts:dropout actions: 13 of 14' 'del(.actions[] | select(.kind == "dropout" and .slot == 9))'
# The soak's run judged as the dropout experiment: the start action says soak.
case_ dropouts-judging-a-soak-run real fail 'inputs' 'the driver ran the soak schedule, the verdict judges dropouts' \
  '(.actions[] | select(.kind == "start") | .schedule) = "soak"'
BASE="$WORK/green.json"
SCHEDULE=soak

# verdict.sh refuses a schedule it does not know (64), before any jq.
cases=$((cases + 1))
checks=$((checks + 1))
rc=0
"$HERE/verdict.sh" --dir "$GREEN" --page real --drive-min 30 --schedule weekly --jq "$JQ" >"$WORK/bad-schedule.out" 2>&1 || rc=$?
if [ "$rc" -eq 64 ] && grep -q 'schedule must be soak or dropouts' "$WORK/bad-schedule.out"; then
  echo "PASS verdict.sh --schedule weekly: refused (64)"
else
  fail "verdict.sh --schedule weekly: exit $rc: $(head -c 300 "$WORK/bad-schedule.out")"
fi

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
# dropout every 2 minutes from minute 1, each owning its whole period, and no disconnect; the soak
# by name is soak_plan's; an unknown name and a short drive are refused.
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
splan_case "dropouts, the experiment's 25 min (12 dropouts, 30 an hour)" dropouts 1500 '{"dropouts":12,"first":60,"last":1380,"disconnects":[]}'
splan_case "dropouts, the fixture's 30 min" dropouts 1800 '{"dropouts":14,"first":60,"last":1620,"disconnects":[]}'
splan_case "dropouts, 3 h" dropouts 10800 '{"dropouts":89,"first":60,"last":10620,"disconnects":[]}'
splan_case "soak by name: soak_plan's 25 min" soak 1500 '{"dropouts":5,"first":150,"last":1350,"disconnects":[300,900,1200]}'
for refused in "weekly 1500 no schedule" "dropouts 600 under min_drive_min"; do
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
