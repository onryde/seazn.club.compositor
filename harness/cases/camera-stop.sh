# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 1, camera-stop (C14 brief): the camera pauses at 20 s and resumes at 30 s; 50 s live.
#
# The slate begins within BUDGET_SLATE_AFTER_LOSS_MS of the last pre-pause frame, and the camera's
# return is judged in camera time: `suppressed_ms`, the camera time after the resume the programme
# never showed. The brief's wall form (the first strip frame after the resume against the camera's
# first post-resume emit) includes the glass-to-glass latency, which the budget does not (a false
# premise, `docs/_INDEX.md`); it is measured and printed, never judged. fake-camera's resume starts
# with a key frame (`camera::Schedule::take_key_due`; the PR #52 review's I5), so the camera time
# judged is the product's: the camera keys once a second, and CI's first build read up to a whole
# GOP of undecodable frames into `suppressed_ms` (1 800 ms against 1 200). The 10 s pause is past
# input_stall_after_ms, so the input is rebuilt here, as a phone that stopped sending is.

CASE_LIVE_S=50
CASE_KEY=return_to_live

case_drive() {
  at_s 20
  mark pause
  camera POST /pause >/dev/null
  at_s 30
  mark resume
  camera POST /resume >/dev/null
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=14
case_verdict() {
  # Anti-vacuity: the pause made exactly one camera gap the checker read, and the programme showed
  # camera frames again after it.
  require camera_paused '(mark("pause") | length) == 1 and (.camera.resumes | length) == 1
    and .camera.resumes[0].gap_from_wall_ms <= mark_at("resume")' \
    '{resumes: (.camera.resumes | length), gap_ms: .camera.resumes[0].gap_ms}'
  require frames_after_resume '(frames_between(mark_at("resume"); $run.end_request_ms) | map(select(.[1] != null)) | length) >= 1' \
    'frames_between(mark_at("resume"); $run.end_request_ms) | map(select(.[1] != null)) | length'
  require slate_after_loss '.camera.resumes[0] | .slate_from_ms != null and .last_shown_wall_ms != null
    and (.slate_from_ms - .last_shown_wall_ms) <= $t.budget_slate_after_loss_ms' \
    '.camera.resumes[0] | {slate_after_ms: (.slate_from_ms - .last_shown_wall_ms), budget: $t.budget_slate_after_loss_ms}'
  require return_to_live '.camera.resumes[0].suppressed_ms != null and .camera.resumes[0].suppressed_ms <= $t.budget_return_to_live_ms' \
    '.camera.resumes[0] | {suppressed_ms, budget: $t.budget_return_to_live_ms}'
  measure return_wall_ms '.camera.resumes[0] | .first_shown_wall_ms - .first_emitted_wall_ms'
  measure latency_ms '.camera.latency_ms'
  require no_output_outages '.output_outages == []' '.output_outages | length'
  require counter_regressions '.camera.counter_regressions == 0'
  require_no_false_restart
}
