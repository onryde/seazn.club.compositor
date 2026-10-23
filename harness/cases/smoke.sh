# shellcheck shell=bash disable=SC2034 # sourced by run-case.sh, which reads CASE_CHECKS
# The smoke case (C13 brief, Step 5): 60 s live, no impairment. Sourced by run-case.sh.
#
# Both netem legs carry a pass-through netem (no delay, no loss) for the whole run, and each must
# show packets through it: proof, on this host's kernel, that C14's faults will land where
# netem.sh says. The verdict's facts are anti-vacuity guards: a harness that recorded nothing, or
# read nothing, fails here.

CASE_LIVE_S=60

case_drive() {
  "$HERE/netem.sh" in limit 10000
  "$HERE/netem.sh" out limit 10000
  sleep "$CASE_LIVE_S"
  "$HERE/netem.sh" check in
  "$HERE/netem.sh" check out
}

CASE_CHECKS=17
case_verdict() {
  # The brief's seven.
  require frames '.frames >= 1700'
  require pts_gaps '.pts_gaps == []'
  require counter_regressions '.camera.counter_regressions == 0'
  require latency_p95 '.camera.latency_ms.p95 > 0'
  require score_index_changes '(.overlay.score_index_changes | length) >= 1'
  require heartbeats '.heartbeats.count >= 10'
  require heartbeat_fields '.heartbeats.fields_seen | has("programmePhase") and has("input") and has("output") and has("graphics") and has("rung")'
  # The 2026-10-18 amendment's two.
  require restarts_without_crash '.graphics.restarts_without_crash == 0'
  require rungs '.heartbeats.rungs - ["full", "overlay_half_rate"] == []'
  # No impairment: no slate inside the run, no output outage, every segment decoded.
  require no_interior_slates '.camera.slate_frames == 0'
  require no_output_outages '.output_outages == []'
  # A backlog overlap (Owed 80) is judged by the camera counters across it, never by the segment
  # names alone: each must carry its content gap (a gap past the outage threshold is already an
  # output outage, which no_output_outages refuses).
  require overlaps_judged_by_content '[.segment_overlaps[] | .content_gap_ms != null] | all'
  require no_decode_errors '.inputs.decode_errors == []'
  # The run ended as asked: one clean exit (0), no restart, and the stopped beat says why.
  require_run compositor_exit '.compositor.status == "exited" and .compositor.exit_code == 0'
  require_run no_restarts '.compositor.restarts == 0'
  require_run ended_beat '.last_beat.state == "stopped" and .last_beat.exitReason == "ended"'
  require camera_strips '.camera.strip_frames >= 1700'
}
