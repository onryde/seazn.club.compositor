# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 2, camera-flap (C14 brief): the camera pauses and resumes every 5 s for 120 s (12 flaps, from
# 5 s; 125 s live).
#
# Hysteresis from the strip, never the beats (a 5 s beat cannot see a 1 s rule): every resume holds
# the camera off for at least resume_after_ms less one frame, and returns within
# BUDGET_RETURN_TO_LIVE_MS. Mutant 1 (resume_after_ms 0 in a scratch build) reds `flap_hysteresis`.

CASE_LIVE_S=125
CASE_KEY=flap_hysteresis
FLAPS=12

case_drive() {
  local i
  for ((i = 0; i < FLAPS; i++)); do
    at_s $((5 + i * 10))
    mark pause
    camera POST /pause >/dev/null
    at_s $((10 + i * 10))
    mark resume
    camera POST /resume >/dev/null
  done
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=8
case_verdict() {
  # Anti-vacuity: 120 s of 5 s flaps leave at least 10 resumes the strip judged.
  require flaps_seen "(mark(\"pause\") | length) == $FLAPS and (mark(\"resume\") | length) == $FLAPS
    and ([.camera.resumes[] | select(.suppressed_ms != null)] | length) >= 10" \
    '{pauses: (mark("pause") | length), resumes: (.camera.resumes | length),
      judged: ([.camera.resumes[] | select(.suppressed_ms != null)] | length)}'
  require flap_hysteresis '(.camera.resumes | length) >= 1 and ([.camera.resumes[] | .suppressed_ms]
    | all(. != null and . >= ($t.resume_after_ms - $t.frame_period_ms) and . <= $t.budget_return_to_live_ms))' \
    '[.camera.resumes[] | .suppressed_ms] | {min: min, max: max, floor: ($t.resume_after_ms - $t.frame_period_ms), budget: $t.budget_return_to_live_ms}'
  require no_output_outages '.output_outages == []' '.output_outages | length'
  require counter_regressions '.camera.counter_regressions == 0'
  require_no_false_restart
}
