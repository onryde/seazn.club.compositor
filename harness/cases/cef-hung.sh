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
# then the overlay again after the new generation's ready. Camera frames continue, the output never
# drops, and every beat in the span (at least one) reads ready, restarting or starting, never failed.
#
# The detect bound (C14 fix round 3's re-review, I-1). The product's rule is judged on the log: the
# hide comes at the first tick at or after cef_hang_after_ms past the last alive
# (domain::graphics), so `hide − alive` lies in [cef_hang_after_ms, + one frame period), and one more
# period each way covers the docker stamps of two lines and a late tick. The strip is judged against
# the logged hide within BUDGET_FRAME_TO_ENCODER_MS (spec §6): a strip time is the MediaMTX
# segment's name plus the frame's offset, and the names step ±17 ms of arrival jitter (F-021). The
# brief's one bound, strip − alive ≤ cef_hang_after_ms + one frame period, left the strip's dating no
# slack: PR run 40065135366 read the log at 10 031 (on the rule) and the strip at 10 037 (a false
# premise row; the round-1 review's m1 rested on one sample).
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

CASE_CHECKS=19
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
  require detect_ms "$HUNG"' | ($hid.at_ms - $alive.at_ms) as $log
    | $log >= ($t.cef_hang_after_ms - $t.frame_period_ms) and $log <= ($t.cef_hang_after_ms + 2 * $t.frame_period_ms)
    and (($gap.from_ms - $hid.at_ms) | abs) <= $t.budget_frame_to_encoder_ms' \
    "$HUNG"' | {log_from_last_alive_ms: less($hid.at_ms; $alive.at_ms), strip_from_log_ms: less($gap.from_ms; $hid.at_ms),
      log_bounds: [($t.cef_hang_after_ms - $t.frame_period_ms), ($t.cef_hang_after_ms + 2 * $t.frame_period_ms)],
      strip_budget: $t.budget_frame_to_encoder_ms}'
  # Back after the new generation's ready, and held for resume_after_ms (lib.jq's on_air_for).
  require overlay_back "$HUNG"' | ([.overlay.runs[] | select(.from_ms >= $gap.to_ms and .index != null)] | first | .from_ms) as $b
    | $ready != null and $gap.to_ms >= $ready.at_ms and $b != null and on_air_for($b; $t.resume_after_ms)' \
    "$HUNG"' | ([.overlay.runs[] | select(.from_ms >= $gap.to_ms and .index != null)] | first | .from_ms) as $b
    | {gap_to_ms: $gap.to_ms, ready_ms: $ready.at_ms, back_ms: $b} + hold_evidence($b; $t.resume_after_ms)'
  # And on air from that frame to the end request (lib.jq's stays_on_air_from; the confirm pass's C-4).
  require stays_on_air "$HUNG"' | ([.overlay.runs[] | select(.from_ms >= $gap.to_ms and .index != null)] | first | .from_ms) as $b
    | stays_on_air_from($b) | stays_on_air_ok' \
    "$HUNG"' | ([.overlay.runs[] | select(.from_ms >= $gap.to_ms and .index != null)] | first | .from_ms) as $b
    | {back_ms: $b} + stays_on_air_from($b)'
  require camera_continues "$HUNG"' | [.camera.freezes[] | select(.from_ms >= $h.at_ms and .from_ms <= $gap.to_ms + $t.budget_return_to_live_ms and .frames > 2)] == []' \
    "$HUNG"' | [.camera.freezes[] | select(.from_ms >= $h.at_ms and .from_ms <= $gap.to_ms + $t.budget_return_to_live_ms) | .frames]'
  require no_output_outages '.output_outages == []' '.output_outages | length'
  require beats_never_failed "$HUNG"' | beats_between($h.at_ms; $ready.at_ms + 1)
    | length >= 1 and all(.body.graphics == "ready" or .body.graphics == "restarting" or .body.graphics == "starting")' \
    "$HUNG"' | beats_between($h.at_ms; $ready.at_ms + 1) | map(.body.graphics)'
  require_no_false_restart
}
