# shellcheck shell=bash disable=SC2034 # sourced by run-case.sh, which reads CASE_CHECKS
# Owed 81's alignment-1 control (red: it loses the video, F-029): the smoke case's run (60 s live,
# pass-through netem on both legs, audio on) with the camera's mux at one packet a buffer, so no
# frame's last packets wait to fill a buffer of seven. Measured (dispatch 40090232700): the mux
# still held 37.5 % of the frames, waiting for the audio, and no reader got a video frame:
# MediaMTX sends a reader no H264 until it has seen a random-access unit, and here it never did,
# for a reason not read (its publisher's SRT loss counters would tell). It never goes live, so the
# verdict below fails by design; read its wire.jsonl with scripts/cadence_stages.py.
# Sourced by run-case.sh.

CASE_LIVE_S=60
# Owed 81's input-tap (compose's `probe` profile): tap.jsonl, read by scripts/cadence_stages.py.
export COMPOSE_PROFILES=probe
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
