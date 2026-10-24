# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 5, dest-stall (C14 brief, amended 2026-10-18 for Owed 13/15): `netem out rate 1kbit` at
# 20 s (the link accepts, nothing moves), cleared about 50 s in, in the variant's phase (case 4's);
# 80 s live.
#
# The log's `sink swap` line comes within output_stall_after_ms + output_progress_poll_ms (both from
# thresholds.json) of the last beat whose egressBytes grew, and a beat carries the raised sinkSwaps:
# Owed 39, the kernel's send buffer keeps the total growing after the cut, so a bound timed from the
# cut is wrong by the drain (`stall_swap`, harness/cases/lib.jq; the PR #52 review's I4: a bound
# that also took a beat interval let a detector twice as slow pass). The cut-timed figure is
# printed. The output then recovers as in case 4.
#
# Either path passes (`stall_swap_judged`, lib.jq; the coordinator's decision on Owed 87,
# 2026-10-24): the stall rule's swap within its budget, or the sink's own error first and its
# replacement after the back-off, inside the spec's output budget. Here the socket errors first: in
# CI (runs 40056474269, 40106770881) rtmp2sink's socket timeout (its `timeout` property, 5 s, never
# set by the product) logged `output gen=1 error: … Socket I/O timed out` 5.75 s after the cut, the
# reason line read `error`, and the swap followed 517-520 ms later (the back-off). No case witnesses
# the stall rule itself: case 17 (dest-stall-rule) was meant to and cannot, as built (Owed 87).
#
# Anti-vacuity (the review's I1): the netem qdisc holds a backlog at the clear, so the rate limit
# held the leg's packets. Its Sent count is no witness: CI read 0 sent and 0 dropped in 30 s at
# 1 kbit while the stall applied (the TSO/GSO segments already queued each take minutes).

CASE_LIVE_S=80
CASE_KEY=stall_swap
# The clear's phase (the re-review's I-5; Owed 84): the variant's (`dest_phase`), from 45 s on, so
# it lands 47-57 s in (one reconnect cycle at most); `require_clear_phase` reds a missed phase.
CLEAR_PHASE="$(dest_phase)"
CLEAR_FROM_S=45
NETEM_RATE=1kbit

case_drive() {
  at_s 20
  mark cut
  "$HERE/netem.sh" out rate "$NETEM_RATE"
  clear_out_in_phase "$CLEAR_PHASE" "$CLEAR_FROM_S"
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=12
case_verdict() {
  require fault_applied 'mark_value("netem_out") | .backlog_bytes > 0' 'mark_value("netem_out")'
  require stall_swap 'stall_swap_judged(mark_at("cut"); ["stall", "error"])' 'stall_swap_shown(mark_at("cut"))'
  measure swap_log_from_cut_ms '[.log.sink_swaps[] | select(.at_ms > mark_at("cut"))] | first | .at_ms - mark_at("cut")'
  require_output_recovers
  require_clear_phase
  require_no_false_restart
}
