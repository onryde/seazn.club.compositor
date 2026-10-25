# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 18, output-hung (Owed 94; the owner, 2026-10-24, "B"): the output pipeline hangs while the
# encoders keep delivering, so no bus error ever comes, and only the egress backstop can swap the
# sink (domain::output_link: egress flat for output_egress_flat_ms while both encoded tracks grow,
# then the back-off from Connected and a build, its reason line `egress_stalled`). Owed 94's hook,
# compiled in only by the Dockerfile's `relay-test-hooks` stage (RELAY_TEST_HOOKS_IMAGE,
# harness/compose.output-hung.yml), stalls sink 1 OUTPUT_HUNG_AFTER_MS after its build, at the first
# push past it, and logs the plant. Two variants:
#   hold  sink 1's writes are held (a blocking probe on its pad: Owed 94's shape, the pipeline
#         alive and taking nothing);
#   hang  sink 1's streaming thread sleeps holding its stream lock (adapters' SINK_HANG_HOLD), so
#         its teardown does not return before the sleep ends either: the swap must not wait for it.
#
# The arming line and the plant are preconditions (`armed`, `planted`): a leg on the plain relay
# image arms nothing, and would read as "no swap" for the wrong reason were they not reds. The plant
# must fall inside the drive with room before the end (`planted_in_drive`, ROOM below), so the
# outage it causes and sink 2's return to `playing` are recorded. Then (`hung_swap_judged`, lib.jq)
# the first swap after the plant is sink 2, its reason `egress_stalled`, no error line of sink 1
# before it, inside the domain's window on the process's own clock; the beats from the backstop's firing until sink 2 could have earned
# Connected read `stalled`, never `playing` over the dead output (`beat_stalled`); the viewer's gap
# is inside the owner's 15 s target (output_gap_budget_ms, `gap_in_budget`); no outage outside it;
# sink 2 is the last swap (`one_swap`); and once sink 2 could have earned Connected the beats read
# `playing` again (`beat_playing_again`). Every number comes from harness/thresholds.json but the
# drive's own schedule (OUTPUT_HUNG_AFTER_MS, CASE_LIVE_S), as in every case.
#
# The destination must outwait the backstop (the PR #76 review's I-1). MediaMTX closes a publisher
# that sends nothing for its readTimeout, 10 s by default in 1.21.2 (harness/mediamtx.yml sets none
# for the other cases): CI measured the close 9 978-9 983 ms after the plant, inside the backstop's
# own window, so a destination deadline, not the backstop, could swap sink 1 (reason `error`). This
# case alone (harness/compose.output-hung.yml) raises it to MTX_READ_TIMEOUT_MS, derived from the
# owner's outage target. `destination_outwaits` reads MediaMTX's own: the drive asks its running
# config (`GET /v3/config/global/get`, the `mtx_read_timeout` mark), so a setting MediaMTX ignored (a
# renamed key, an image bump: its 10 s default) reds there, not as a swap with the wrong reason (the
# PR #76 confirm pass's n-1: the check compared two numbers the case derived, true by construction).
# That what the case asks outwaits the window, at this threshold and at a whole budget's, is
# case_verdicts.rs's to pin: both numbers come from harness/thresholds.json.
#
# The programme lost beyond the hang's outage is inside spec §6's output stall row (`output_lost`,
# budget_output_lost_max_ms; the confirm pass's n-2), as cases 4, 5 and 12 judge theirs, with no
# floor. CI read -2 002, -1 992, +56 and +84 ms (runs 40154724972, 40154728288): Owed 92's dating of
# a connection's first segment (F-028), about -2 s when that segment's head shows the camera, about 0
# when MediaMTX's recorder discarded a sample and its head is the frames before the first keyframe.

case "$CASE_VARIANT" in
  hold | hang) ;;
  *)
    echo "output-hung: the variant is hold or hang (got '$CASE_VARIANT')" >&2
    exit 64
    ;;
esac
CASE_LIVE_S=70
CASE_KEY=gap_ms
# Sink 1 is built when the programme starts, before the run goes live: 25 s after it lands the plant
# about 15-24 s into the drive, and leaves the swap, sink 2's Connected and the hang's 30 s sleep
# room before the end.
OUTPUT_HUNG_AFTER_MS=25000
if [ "$CASE_VARIANT" = hold ]; then
  export COMPOSITOR_TEST_HOLD_SINK_AFTER_MS=$OUTPUT_HUNG_AFTER_MS COMPOSITOR_TEST_HANG_SINK_AFTER_MS=
else
  export COMPOSITOR_TEST_HOLD_SINK_AFTER_MS= COMPOSITOR_TEST_HANG_SINK_AFTER_MS=$OUTPUT_HUNG_AFTER_MS
fi
CASE_COMPOSE=compose.output-hung.yml
# Four of the owner's 15 s outage targets: past the hang's whole hold (two, adapters'
# SINK_HANG_HOLD) and past any backstop threshold the budget allows (under one), so MediaMTX closes
# no idle publisher of this case's run.
MTX_READ_TIMEOUT_MS=$((4 * $(threshold output_gap_budget_ms)))
export OUTPUT_HUNG_MTX_READTIMEOUT="${MTX_READ_TIMEOUT_MS}ms"
if [ "$CASE_REJUDGE" != 1 ]; then
  RELAY_IMAGE="${RELAY_TEST_HOOKS_IMAGE:?RELAY_TEST_HOOKS_IMAGE names the relay-test-hooks image (case 18)}"
fi

case_drive() {
  # MediaMTX's readTimeout as its running config reports it (redacted by MediaMTX; only the one
  # field is kept). An unread value is a null mark, and `destination_outwaits` reds on it.
  mark mtx_read_timeout "$(mtx GET /v3/config/global/get | jq -c '.readTimeout')"
  at_s "$CASE_LIVE_S"
}

PLANTED='((.log.sink_stall_planted // []) | first)'
# The room the plant needs before the end request: the outage's whole budget, or the latest swap
# the window accepts plus `beat_playing_again`'s wait and a heartbeat for its beat, whichever is
# longer (the second, with any threshold: its connect, bound and heartbeat alone fill the budget).
ROOM='([$t.output_gap_budget_ms, hung_swap.ceiling_ms + $t.output_connect_timeout_ms
  + $t.output_connected_after_ms + 2 * $t.tick_widest_ms + $t.heartbeat_interval_ms] | max)'
# The outage the hang caused: the one spanning the plant and the swap (none without both: jq reads
# `x < null` false).
GAP="hung_swap as \$s | [.output_outages[] | select(.to_wall_ms > \$s.planted_ms and .from_wall_ms < \$s.swap_ms)]"

# MediaMTX's reading, in ms (null when unread or not a duration: jq sorts it below every number).
MTX_READ='(mark_value("mtx_read_timeout") | mtx_duration_ms)'

CASE_CHECKS=19
case_verdict() {
  # A guard, judged on every run: MediaMTX applied at least the idle deadline the case asks for,
  # which is past the latest swap the window accepts, so only the backstop (or its absence) decides
  # `backstop_swap`.
  require destination_outwaits "($MTX_READ) >= $MTX_READ_TIMEOUT_MS" \
    "{read: mark_value(\"mtx_read_timeout\"), read_ms: ($MTX_READ), asked_ms: $MTX_READ_TIMEOUT_MS,
      ceiling_ms: hung_swap.ceiling_ms}"
  require armed "(.log.sink_stall_armed | length) == 1 and .log.sink_stall_armed[0].stall == \"$CASE_VARIANT\"
    and .log.sink_stall_armed[0].after_ms == $OUTPUT_HUNG_AFTER_MS" \
    "{armed: .log.sink_stall_armed, boots: (.log.boots | length), want: {stall: \"$CASE_VARIANT\", after_ms: $OUTPUT_HUNG_AFTER_MS}}"
  require planted "(.log.sink_stall_planted | length) == 1 and $PLANTED.planted and $PLANTED.stall == \"$CASE_VARIANT\"" \
    '.log.sink_stall_planted'
  require planted_in_drive "$PLANTED.at_ms > \$run.drive_start_ms
    and $PLANTED.at_ms + ($ROOM) < \$run.end_request_ms" \
    "{planted_ms: $PLANTED.at_ms, drive_start_ms: \$run.drive_start_ms, end_request_ms: \$run.end_request_ms,
      room_ms: ($ROOM)}"
  require backstop_swap 'hung_swap_judged' 'hung_swap'
  # From the backstop's firing (one back-off before the swap) until sink 2 could have earned
  # Connected (output_connected_after_ms of growth after its build), a tick in from each end for the
  # beat's way to fake-control and the log's to docker.
  require beat_stalled 'hung_swap as $s | [beats[] | select($s.swap_ms != null
      and .received_ms >= $s.swap_ms - $t.output_backoff_base_ms + 2 * $t.tick_widest_ms
      and .received_ms < $s.swap_ms + $t.output_connected_after_ms - 2 * $t.tick_widest_ms)] as $b
    | ($b | length) >= 1 and all($b[]; .body.state == "stalled")' \
    'hung_swap as $s | [beats[] | select($s.swap_ms != null
      and .received_ms >= $s.swap_ms - $t.output_backoff_base_ms
      and .received_ms < $s.swap_ms + $t.output_connected_after_ms)
      | {received_ms, state: .body.state, output: .body.output, sinkSwaps: .body.sinkSwaps}]'
  require gap_in_budget "($GAP) as \$g | (\$g | length) == 1
    and (\$g[0].to_wall_ms - \$g[0].from_wall_ms) <= \$t.output_gap_budget_ms" \
    "($GAP) | map({from_wall_ms, to_wall_ms, gap_ms: (.to_wall_ms - .from_wall_ms), lost_ms})"
  require no_outage_outside_hang 'hung_swap as $s | [.output_outages[] | select(.to_wall_ms <= $s.planted_ms
      or .from_wall_ms >= $s.swap_ms + $t.budget_output_reconnect_ms)] == []' \
    '[.output_outages[] | {from_wall_ms, to_wall_ms}]'
  require one_swap 'hung_swap.swaps_after == 1' 'hung_swap | {swaps_after, swap_gen}'
  # The PR #76 review's m-3 (Owed 94's plan (5), "and `playing` again"): from when sink 2 could have
  # earned Connected at the latest (its whole connect, then output_connected_after_ms of growth, a
  # tick for each) until the end request, at least one beat, and every one reads `playing`.
  require beat_playing_again 'hung_swap as $s | [beats[] | select($s.swap_ms != null
      and .received_ms >= $s.swap_ms + $t.output_connect_timeout_ms + $t.output_connected_after_ms
        + 2 * $t.tick_widest_ms
      and .received_ms < $run.end_request_ms)] as $b
    | ($b | length) >= 1 and all($b[]; .body.state == "playing")' \
    'hung_swap as $s | [beats[] | select($s.swap_ms != null and .received_ms >= $s.swap_ms
      and .received_ms < $run.end_request_ms) | {received_ms, state: .body.state, output: .body.output}]'
  # Spec §6's output stall row, "≤2 s of programme lost" (the header's n-2). No outage, or one with
  # no camera counter on a side, reads null, and null passes `<=` in jq: the first conjunct reds it.
  require output_lost "(($GAP) | first | .lost_ms) as \$l | \$l != null and \$l <= \$t.budget_output_lost_max_ms" \
    "{lost_ms: (($GAP) | first | .lost_ms), budget: \$t.budget_output_lost_max_ms}"
  measure gap_ms "($GAP) | first | .to_wall_ms - .from_wall_ms"
  measure lost_ms "($GAP) | first | .lost_ms"
  require_no_false_restart
  require_run no_restarts '.compositor.restarts == 0'
  require_run ended_beat '.last_beat.state == "stopped" and .last_beat.exitReason == "ended"' '.last_beat | {state, exitReason}'
}
