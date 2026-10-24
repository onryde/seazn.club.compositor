# shellcheck shell=bash disable=SC2034 # sourced by run-case.sh, which reads CASE_CHECKS
# Owed 81's fix candidate: the smoke case's run (60 s live, pass-through netem on both legs, audio
# on) with the camera's mux at one packet a buffer, so no frame's last packets wait for the next
# frame's to fill a buffer of seven (F-029). Compare its `camera.cadence` and its wire.jsonl /
# tap.jsonl with a smoke run's. Measured (dispatch 40090232700): not the fix. The mux still held
# 37.5 % of the frames, waiting for the audio, and no reader decoded a video frame (F-029).
# Sourced by run-case.sh.

CASE_LIVE_S=60
export CAMERA_TS_ALIGNMENT=1

case_drive() {
  "$HERE/netem.sh" in limit 10000
  "$HERE/netem.sh" out limit 10000
  sleep "$CASE_LIVE_S"
  "$HERE/netem.sh" check in
  "$HERE/netem.sh" check out
}

CASE_CHECKS=5
case_verdict() {
  # Anti-vacuity: the run recorded and read the camera, and ended as asked. The cadence itself is
  # the measurement, not a gate (Owed 81: no cadence gate until the owner rules).
  require frames '.frames >= 1700'
  require camera_strips '.camera.strip_frames >= 1700'
  require cadence_pairs '.camera.cadence.pairs >= 1700'
  require no_interior_slates '.camera.slate_frames == 0'
  require_run compositor_exit '.compositor.status == "exited" and .compositor.exit_code == 0'
}
