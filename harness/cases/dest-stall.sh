# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 5, dest-stall (C14 brief, amended 2026-10-18 for Owed 13/15): `netem out rate 1kbit` at
# 20 s (the link accepts, nothing moves), cleared at 50 s; 80 s live.
#
# The log's `sink swap` line comes within output_stall_after_ms + output_progress_poll_ms (both from
# thresholds.json) of the last beat whose egressBytes grew, and a beat carries the raised sinkSwaps:
# Owed 39, the kernel's send buffer keeps the total growing after the cut, so a bound timed from the
# cut is wrong by the drain (`stall_swap`, harness/cases/lib.jq; the PR #52 review's I4: a bound
# that also took a beat interval let a detector twice as slow pass). The cut-timed figure is
# printed. The output then recovers as in case 4.
#
# The swap must be the stall rule's (`cause`, C14 fix round 3): a swap that came through the sink's
# own bus error is another rule's, however fast. In CI (run 40056474269) rtmp2sink's socket timeout
# (its `timeout` property, 5 s, never set by the product) logged `output gen=1 error: … Socket I/O
# timed out` 5.75 s after the cut and the swap followed 520 ms later (the back-off): the stall rule
# never fired. That red is Owed 87's, and is never loosened.
#
# Anti-vacuity (the review's I1): the netem qdisc holds a backlog at the clear, so the rate limit
# held the leg's packets. Its Sent count is no witness: CI read 0 sent and 0 dropped in 30 s at
# 1 kbit while the stall applied (the TSO/GSO segments already queued each take minutes).

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

CASE_CHECKS=12
case_verdict() {
  require fault_applied 'mark_value("netem_out") | .backlog_bytes > 0' 'mark_value("netem_out")'
  require stall_swap 'stall_swap(mark_at("cut")) | .swap_ms != null and .swap_beat_ms != null and .last_growth_ms != null
    and .from_growth_ms <= .bound_ms and .cause == "stall"' 'stall_swap(mark_at("cut"))'
  measure swap_log_from_cut_ms '[.log.sink_swaps[] | select(.at_ms > mark_at("cut"))] | first | .at_ms - mark_at("cut")'
  require_output_recovers
  require_no_false_restart
}
