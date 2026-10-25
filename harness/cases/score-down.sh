# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 7, score-down (C14 brief): fake-control's score feed answers 503 for 60 s from 20 s
# (`POST /control/score-down?ms=60000`); 100 s live.
#
# The harness page keeps the real page's stale rule (C14, overlay-test.html: `seazn:score-stale=true`
# after BUDGET_SCORE_STALE_AFTER_MS with no good answer, `=false` on the next good one), and the
# case proves the compositor's relay of it into the beats: `scoreStale: true` within the budget +
# the page's poll + one beat interval, and `false` within the poll + one beat after the feed is
# back. The overlay never shows an event revealed after the feed went down, keeps its last good
# answer on air (no overlay-less frame), changes again after the recovery, and never regresses.

CASE_LIVE_S=100
CASE_KEY=stale_beat
SCORE_DOWN_MS=60000

case_drive() {
  at_s 20
  local answer
  answer="$(control POST "/control/score-down?ms=$SCORE_DOWN_MS")"
  mark down "$answer"
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=13
case_verdict() {
  # Anti-vacuity: the feed refused the page's polls, and the page said so in the log.
  require feed_was_down '$run.control_state.score_refused >= 1 and mark_value("down").score_down_until_ms > mark_at("down")' \
    '{score_refused: $run.control_state.score_refused, down_ms: (mark_value("down").score_down_until_ms - mark_at("down"))}'
  require stale_line '([.log.score_stale[] | select(.value == true and .at_ms >= mark_at("down"))] | first | .at_ms - mark_at("down")) as $s
    | $s != null and $s >= ($t.budget_score_stale_after_ms - $t.harness_page_poll_ms) and $s <= ($t.budget_score_stale_after_ms + $t.harness_page_poll_ms)' \
    '{page_stale_ms: ([.log.score_stale[] | select(.value == true and .at_ms >= mark_at("down"))] | first | .at_ms - mark_at("down")), budget: $t.budget_score_stale_after_ms}'
  require stale_beat '([beats[] | select(.received_ms >= mark_at("down") and .body.scoreStale == true)] | first | .received_ms - mark_at("down")) as $b
    | $b != null and $b <= ($t.budget_score_stale_after_ms + $t.harness_page_poll_ms + $t.heartbeat_interval_ms)' \
    '{stale_beat_ms: ([beats[] | select(.received_ms >= mark_at("down") and .body.scoreStale == true)] | first | .received_ms - mark_at("down")),
      bound: ($t.budget_score_stale_after_ms + $t.harness_page_poll_ms + $t.heartbeat_interval_ms)}'
  require not_stale_before 'beats_between($run.drive_start_ms; mark_at("down")) | length >= 1 and all(.body.scoreStale != true)'
  require fresh_beat 'mark_value("down").score_down_until_ms as $u
    | ([beats[] | select(.received_ms >= $u and .body.scoreStale == false)] | first | .received_ms - $u) as $b
    | $b != null and $b <= ($t.harness_page_poll_ms + $t.heartbeat_interval_ms)' \
    'mark_value("down").score_down_until_ms as $u | {fresh_beat_ms: ([beats[] | select(.received_ms >= $u and .body.scoreStale == false)] | first | .received_ms - $u),
      page_fresh_ms: ([.log.score_stale[] | select(.value == false and .at_ms >= $u)] | first | .at_ms - $u)}'
  require no_new_index_while_down 'mark_value("down").score_down_until_ms as $u
    | ([$timeline.events[] | select(.at <= mark_at("down")) | .index] | max) as $m
    | frames_between(mark_at("down"); $u) | map(.[2] | select(. != null)) | length >= 1 and all(. <= $m)' \
    'mark_value("down").score_down_until_ms as $u | {revealed_max: ([$timeline.events[] | select(.at <= mark_at("down")) | .index] | max),
      shown_max: (frames_between(mark_at("down"); $u) | map(.[2] | select(. != null)) | max)}'
  require last_good_held 'mark_value("down").score_down_until_ms as $u
    | ([$timeline.events[] | select(.at <= mark_at("down") - $t.harness_page_poll_ms) | .index] | max) as $sure
    | (frames_between($u - $t.heartbeat_interval_ms; $u) | map(.[2] | select(. != null)) | max) as $held
    | $held != null and $held >= $sure
    and (frames_between(mark_at("down"); $u) | map(select(.[2] == null)) | length) == 0' \
    'mark_value("down").score_down_until_ms as $u | {held: (frames_between($u - $t.heartbeat_interval_ms; $u) | map(.[2] | select(. != null)) | max),
      no_overlay_frames: (frames_between(mark_at("down"); $u) | map(select(.[2] == null)) | length)}'
  # The first change after the recovery, and the overlay held on air for resume_after_ms from it
  # (lib.jq's on_air_for: one frame and off air is no recovery).
  require changes_after_recovery '([.overlay.score_index_changes[] | select(.frame_wall_ms > mark_value("down").score_down_until_ms)] | first | .frame_wall_ms) as $c
    | $c != null and on_air_for($c; $t.resume_after_ms)' \
    '([.overlay.score_index_changes[] | select(.frame_wall_ms > mark_value("down").score_down_until_ms)]) as $c
    | {changes: ($c | length), first_ms: ($c | first | .frame_wall_ms)}
      + hold_evidence($c | first | .frame_wall_ms; $t.resume_after_ms)'
  require never_regresses 'index_regressions == []' 'index_regressions | length'
  require_no_false_restart
}
