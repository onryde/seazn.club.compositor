# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 4, dest-disconnect (C14 brief): `netem out loss 100%` at 20 s, cleared at 50 s; 80 s live.
#
# The output returns within BUDGET_OUTPUT_RECONNECT_MS of the clear; the programme lost per outage
# (the counter gap across it, in camera time, less its wall span: `output_outages[].lost_ms`) is at
# most BUDGET_OUTPUT_LOST_MAX_MS; the input leg is untouched (every beat reads input connected) and
# no pipeline is rebuilt. Anti-vacuity: the netem qdisc dropped packets, and an outage spans the cut.

CASE_LIVE_S=80
CASE_KEY=output_reconnect

case_drive() {
  at_s 20
  mark cut
  "$HERE/netem.sh" out loss 100%
  at_s 50
  mark netem_out "$("$HERE/netem.sh" stats out)"
  "$HERE/netem.sh" clear out
  mark clear
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=11
case_verdict() {
  require fault_applied 'mark_value("netem_out") | .present and .dropped > 0' 'mark_value("netem_out")'
  require_output_recovers
  require input_untouched 'beats_between($run.drive_start_ms; $run.end_request_ms) | length >= 1 and all(.body.input == "connected")' \
    'beats_between($run.drive_start_ms; $run.end_request_ms) | map(.body.input) | group_by(.) | map({(.[0]): length}) | add'
  require pipeline_rebuilds_unchanged 'beats_between($run.drive_start_ms; $run.end_request_ms) | map(.body.pipelineRebuilds) | unique | length == 1' \
    'beats_between($run.drive_start_ms; $run.end_request_ms) | map(.body.pipelineRebuilds) | unique'
  require_no_false_restart
}
