# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 5, dest-stall (C14 brief, amended 2026-10-18 for Owed 13/15): `netem out rate 1kbit` at
# 20 s (the link accepts, nothing moves), cleared about 50 s in, in the variant's phase (case 4's);
# 80 s live.
#
# The stall is caught by the sink's own error (Owed 87; the owner, 2026-10-24, "A": the output has
# no stall rule): rtmp2sink's socket timeout (`timeout`, output_sink_timeout_ms, which C09 sets)
# logs `output gen=<g> error:`, the swap's reason line reads `error`, and the swap follows the
# domain's back-off from Connected, within one sink timeout, that back-off and a frame of the last
# beat whose egressBytes grew, and a beat carries the raised sinkSwaps (`stall_swap_judged`,
# harness/cases/lib.jq; every number from thresholds.json). Owed 39: the kernel's send buffer keeps
# the total growing after the cut, so a bound timed from the cut is wrong by the drain; the error
# still comes no sooner than one sink timeout, less a frame, after the cut. The cut-timed figure is
# printed. In CI (runs 40056474269, 40106770881) the socket timed out 5.75 s after the cut and the
# swap followed 517-520 ms later. The output then recovers as in case 4.
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

CASE_CHECKS=14
case_verdict() {
  require fault_applied 'mark_value("netem_out") | .backlog_bytes > 0' 'mark_value("netem_out")'
  require stall_swap 'stall_swap_judged(mark_at("cut"))' 'stall_swap(mark_at("cut"))'
  measure swap_log_from_cut_ms '[.log.sink_swaps[] | select(.at_ms > mark_at("cut"))] | first | .at_ms - mark_at("cut")'
  require_output_recovers
  require_clear_phase
  require_no_false_restart
}
