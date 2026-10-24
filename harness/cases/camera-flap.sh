# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 2, camera-flap (C14 brief): the camera pauses and resumes 12 times, one flap every 10 s from
# 5 s; 125 s live.
#
# Each pause lasts halfway between slate_after_ms and input_stall_after_ms (both from
# thresholds.json): long enough to slate, short of the input's stall rule, so every flap is the same
# flap (the PR #52 review's I5). The brief's "every 5 s" made each pause 5 s, which IS
# input_stall_after_ms: some flaps rebuilt the input and some did not, and CI read 900 to 3 733 ms
# (a false premise, `docs/_INDEX.md`). fake-camera's resume starts with a key frame
# (`camera::Schedule::take_key_due`), so a decoder's wait for one is not the product's hold-off.
#
# Hysteresis from the strip, never the beats (a 5 s beat cannot see a 1 s rule): every resume holds
# the camera off for resume_after_ms, less what the programme's pipeline holds between the input and
# the frame on air (at most BUDGET_FRAME_TO_ENCODER_MS: the hold-off starts at the first frame's
# arrival, and the frame shown at the switch is that much older than the newest; CI's 900 ms flaps),
# and returns within BUDGET_RETURN_TO_LIVE_MS. Mutant 1 (resume_after_ms 0 in a scratch build) reds
# `flap_hysteresis`.

CASE_LIVE_S=125
CASE_KEY=flap_hysteresis
FLAPS=12
FLAP_EVERY_S=10

case_drive() {
  local i pause_ms
  pause_ms=$((($(threshold slate_after_ms) + $(threshold input_stall_after_ms)) / 2))
  for ((i = 0; i < FLAPS; i++)); do
    at_s $((5 + i * FLAP_EVERY_S))
    mark pause "{\"pause_ms\":$pause_ms}"
    camera POST /pause >/dev/null
    sleep "$(awk -v d="$pause_ms" 'BEGIN { printf "%.3f", d / 1000 }')"
    mark resume
    camera POST /resume >/dev/null
  done
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=8
case_verdict() {
  # Anti-vacuity: 12 flaps leave at least 10 resumes the strip judged (each judged resume followed a
  # slate, so each pause was long enough to slate), and no pause reached the input's stall rule.
  require flaps_seen "(mark(\"pause\") | length) == $FLAPS and (mark(\"resume\") | length) == $FLAPS
    and (mark(\"pause\") | all(.value.pause_ms < \$t.input_stall_after_ms))
    and ([.camera.resumes[] | select(.suppressed_ms != null)] | length) >= 10" \
    '{pauses: (mark("pause") | length), pause_ms: mark_value("pause").pause_ms, resumes: (.camera.resumes | length),
      judged: ([.camera.resumes[] | select(.suppressed_ms != null)] | length)}'
  require flap_hysteresis '(.camera.resumes | length) >= 1 and ([.camera.resumes[] | .suppressed_ms]
    | all(. != null and . >= ($t.resume_after_ms - $t.budget_frame_to_encoder_ms) and . <= $t.budget_return_to_live_ms))' \
    '[.camera.resumes[] | .suppressed_ms] | {min: min, max: max, floor: ($t.resume_after_ms - $t.budget_frame_to_encoder_ms), budget: $t.budget_return_to_live_ms}'
  require no_output_outages '.output_outages == []' '.output_outages | length'
  require counter_regressions '.camera.counter_regressions == 0'
  require_no_false_restart
}
