# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 5, dest-stall (C14 brief, amended 2026-10-18 for Owed 13/15): `netem out rate 1kbit` at
# 20 s (the link accepts, nothing moves), cleared at 50 s; 80 s live.
#
# A sinkSwaps rise appears in a beat within output_stall_after_ms + output_progress_poll_ms +
# heartbeat_interval_ms (all from thresholds.json), timed from the last beat whose egressBytes grew:
# Owed 39, the kernel's send buffer keeps the total growing ~4 s after the cut, so a bound timed
# from the cut is wrong by the drain (`stall_swap`, harness/cases/lib.jq). The cut-timed figure and
# the log's swap line are printed. The output then recovers as in case 4.

CASE_LIVE_S=80
CASE_KEY=stall_swap
NETEM_RATE=1kbit

case_drive() {
  at_s 20
  mark cut
  "$HERE/netem.sh" out rate "$NETEM_RATE"
  at_s 50
  mark netem_out "$("$HERE/netem.sh" stats out)"
  "$HERE/netem.sh" clear out
  mark clear
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=11
case_verdict() {
  require fault_applied 'mark_value("netem_out") | .present and .sent_pkts > 0' 'mark_value("netem_out")'
  require stall_swap 'stall_swap(mark_at("cut")) | .swap_ms != null and .last_growth_ms != null and .from_growth_ms <= .bound_ms' \
    'stall_swap(mark_at("cut")) | del(.egress_between)'
  measure swap_log_from_cut_ms '[.log.sink_swaps[] | select(.at_ms > mark_at("cut"))] | first | .at_ms - mark_at("cut")'
  require_output_recovers
  require_no_false_restart
}
