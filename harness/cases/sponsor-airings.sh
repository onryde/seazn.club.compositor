# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# R4 task SC1 (fix round 1, I3 c): sponsor airings ride the heartbeat end to end, 30 s live. The
# compositor runs with RELAY_AIRINGS=1 (compose.sponsor-airings.yml; off in every other case). 5 s
# into the drive fake-control arms the page's sponsor card (`POST /control/page-sponsor`), and the
# page takes it on its next /page-control.json poll: one start line, and its end a few seconds later
# (harness/pages/overlay-test.html). A query switch cannot reach the page (cef-hung's note), so the
# knob is fake-control's. CEF's console channel, the adapter's reader, the airings buffer, the
# control worker and the HTTP plane carry both lines to fake-control's beats, which the checker's
# heartbeat series holds.
#
# The verdict: the card's start and its end each reach a beat exactly once (a 2xx clears what a beat
# carried, so a line is never sent twice), in order, nothing else rides, nothing is dropped, and the
# run ends as smoke's does. Anti-vacuity: the arming mark and fake-control's served stamp. Not a
# chaos case (case_verdicts.rs's NOT_CHAOS): harness.yml runs it on every pull request, beside smoke.

CASE_LIVE_S=30
CASE_KEY=airings_relayed
CASE_COMPOSE=compose.sponsor-airings.yml

case_drive() {
  at_s 5
  mark armed "$(control POST /control/page-sponsor)"
  at_s "$CASE_LIVE_S"
}

# Every airing a beat carried, in receipt order, with that beat's receipt time; the page's card
# (its moment is the page's SPONSOR_MOMENT), and its starts and ends.
AIRED='[.heartbeats.series[] | .received_ms as $r | (.body.airings // [])[] | . + {received_ms: $r}] as $aired
  | [$aired[] | select(.momentId == "harness-sponsor-1")] as $card
  | [$card[] | select(.kind == "start")] as $starts
  | [$card[] | select(.kind == "end")] as $ends'

CASE_CHECKS=11
case_verdict() {
  require armed '$case.armed[0].value.page_sponsor_armed == true' '$case.armed'
  require_run sponsor_served '.control_state.page_sponsor_served_ms != null' \
    '.control_state.page_sponsor_served_ms'
  require start_relayed_once "$AIRED"' | ($starts | length) == 1' "$AIRED"' | $starts'
  require end_relayed_once "$AIRED"' | ($ends | length) == 1 and $ends[0].cutShort == false' \
    "$AIRED"' | $ends'
  require end_after_start "$AIRED"' | $starts[0].at < $ends[0].at
    and $starts[0].received_ms <= $ends[0].received_ms'
  require only_the_card "$AIRED"' | ($aired | length) == 2 and ($card | length) == 2' \
    "$AIRED"' | [$aired[] | {kind, momentId}]'
  # Each beat that carried lines says 0 dropped, and no beat says more.
  require none_dropped '[.heartbeats.series[] | select(.body | has("airings"))] as $c
    | ($c | length) >= 1 and all($c[]; .body.airingsDropped == 0)
    and ([.heartbeats.series[] | select((.body.airingsDropped // 0) > 0)] | length) == 0' \
    '[.heartbeats.series[] | .body.airingsDropped // empty]'
  measure airings_relayed "$AIRED"' | $aired | length'
  # The run ended as asked: one clean exit (0), no restart, and the stopped beat says why.
  require_run compositor_exit '.compositor.status == "exited" and .compositor.exit_code == 0'
  require_run no_restarts '.compositor.restarts == 0'
  require_run ended_beat '.last_beat.state == "stopped" and .last_beat.exitReason == "ended"'
}
