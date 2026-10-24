# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 11, cef-hung (C14 brief, amendment 2026-10-18; Owed 18): at 20 s the harness page hangs
# (`while (true) {}` after ready); 60 s live. The brief's `?crash=1&crash_after_ms=N` cannot reach
# the page (the session's overlayUrl is the fixture's, and the rewrite refuses a query change: a
# false premise), so fake-control arms the hang (`POST /control/page-hang`) and the page takes it on
# its next /page-control.json poll, once.
#
# The evidence comes from the compositor's structured log, never a beat in a window: exactly one
# `overlay hidden reason=hang gen=g`, then `graphics restart gen=g+1`, then `cef ready gen=g+1`, and no
# other hide. The strip, frame-exact: the overlay holds one index from the hang while the timeline
# schedules a change (the frame is held, not idle), then shows no overlay for at least one frame,
# then the overlay again after the new generation's ready. The strip's gap starts within
# cef_hang_after_ms + one frame period of the page's last `seazn:alive` (the brief's bound: CI run
# 40045826841 read the strip 6 ms behind the log, so no transport term is owed; the PR #52 review's
# m1). Camera frames continue, the output never drops, and every beat in the span (at least one)
# reads ready, restarting or starting, never failed.
# Anti-vacuity: the page's own hang line, at least 2 alive lines before it and none after.

CASE_LIVE_S=60
CASE_KEY=detect_ms

case_drive() {
  at_s 20
  mark armed "$(control POST /control/page-hang)"
  at_s "$CASE_LIVE_S"
}

# The pieces every check reads, as one jq prefix.
HUNG='(.log.page_hangs[0]) as $h
  | ([.log.cef_alive[] | select(.gen == $h.gen and .at_ms <= $h.at_ms)] | last) as $alive
  | ([.log.overlay_hidden[] | select(.reason == "hang")] | first) as $hid
  | ([.log.graphics_restart[] | select(.gen == $h.gen + 1)] | first) as $restart
  | ([.log.cef_ready[] | select(.gen == $h.gen + 1)] | first) as $ready
  | ([.overlay.runs[] | select(.index == null and .from_ms >= $h.at_ms)] | first) as $gap'

CASE_CHECKS=16
case_verdict() {
  require hang_started '(.log.page_hangs | length) == 1 and $run.control_state.page_hang_served_ms != null' \
    '{page_hangs: (.log.page_hangs | length), served: $run.control_state.page_hang_served_ms}'
  require alive_before_not_after "$HUNG"' | ([.log.cef_alive[] | select(.gen == $h.gen and .at_ms < $h.at_ms)] | length) >= 2
    and ([.log.cef_alive[] | select(.gen == $h.gen and .at_ms > $h.at_ms)] | length) == 0' \
    "$HUNG"' | {before: ([.log.cef_alive[] | select(.gen == $h.gen and .at_ms < $h.at_ms)] | length),
      after: ([.log.cef_alive[] | select(.gen == $h.gen and .at_ms > $h.at_ms)] | length)}'
  require one_hang_hide "$HUNG"' | ([.log.overlay_hidden[] | select(.reason == "hang")] | length) == 1 and $hid.gen == $h.gen' \
    '[.log.overlay_hidden[] | {reason, gen}]'
  require no_other_hide '[.log.overlay_hidden[] | select(.reason != "hang")] == []'
  require restart_then_ready "$HUNG"' | $restart != null and $ready != null and $restart.at_ms >= $hid.at_ms and $ready.at_ms >= $restart.at_ms' \
    "$HUNG"' | {hidden_after_hang_ms: ($hid.at_ms - $h.at_ms), restart_after_hide_ms: ($restart.at_ms - $hid.at_ms), ready_after_restart_ms: ($ready.at_ms - $restart.at_ms)}'
  require overlay_gap "$HUNG"' | $gap != null and $gap.frames >= 1' "$HUNG"' | $gap'
  require held_frame "$HUNG"' | [.overlay.runs[] | select(.to_ms > $h.at_ms and .from_ms < $gap.from_ms)] as $held
    | ($held | length) >= 1 and ($held | map(.index) | unique | length) == 1 and $held[0].index != null
    and ([scheduled[] | select(.due_ms > $h.at_ms and .due_ms < $gap.from_ms)] | length) >= 1' \
    "$HUNG"' | {held: [.overlay.runs[] | select(.to_ms > $h.at_ms and .from_ms < $gap.from_ms) | .index],
      scheduled_meanwhile: ([scheduled[] | select(.due_ms > $h.at_ms and .due_ms < $gap.from_ms)] | length)}'
  require detect_ms "$HUNG"' | $alive != null and ($gap.from_ms - $alive.at_ms) <= ($t.cef_hang_after_ms + $t.frame_period_ms)' \
    "$HUNG"' | {strip_from_last_alive_ms: ($gap.from_ms - $alive.at_ms), log_from_last_alive_ms: ($hid.at_ms - $alive.at_ms),
      bound: ($t.cef_hang_after_ms + $t.frame_period_ms)}'
  require overlay_back "$HUNG"' | $ready != null and $gap.to_ms >= $ready.at_ms
    and ([.overlay.runs[] | select(.from_ms >= $gap.to_ms and .index != null)] | length) >= 1' \
    "$HUNG"' | {gap_to_ms: $gap.to_ms, ready_ms: $ready.at_ms}'
  require camera_continues "$HUNG"' | [.camera.freezes[] | select(.from_ms >= $h.at_ms and .from_ms <= $gap.to_ms + $t.budget_return_to_live_ms and .frames > 2)] == []' \
    "$HUNG"' | [.camera.freezes[] | select(.from_ms >= $h.at_ms and .from_ms <= $gap.to_ms + $t.budget_return_to_live_ms) | .frames]'
  require no_output_outages '.output_outages == []' '.output_outages | length'
  require beats_never_failed "$HUNG"' | beats_between($h.at_ms; $ready.at_ms + 1)
    | length >= 1 and all(.body.graphics == "ready" or .body.graphics == "restarting" or .body.graphics == "starting")' \
    "$HUNG"' | beats_between($h.at_ms; $ready.at_ms + 1) | map(.body.graphics)'
  require_no_false_restart
}
