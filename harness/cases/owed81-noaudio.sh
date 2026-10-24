# shellcheck shell=bash disable=SC2034 # sourced by run-case.sh, which reads CASE_CHECKS
# Owed 81's control: the smoke case's run (60 s live, pass-through netem on both legs) with the
# camera's audio off, so its MPEG-TS carries the video alone and its mux has nothing to interleave
# the frames with. Compare its `camera.cadence` and its wire.jsonl / tap.jsonl with a smoke run's.
# Sourced by run-case.sh.

CASE_LIVE_S=60
export CAMERA_AUDIO=off

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
