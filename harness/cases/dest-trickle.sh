# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 12, dest-trickle (C14 brief, amendment 2026-10-18; Owed 16(b)): `netem out rate 8kbit` at
# 20 s (the link stays open and bytes still trickle), cleared at 50 s; 80 s live.
#
# A trickle is a stall, not progress: a sinkSwaps rise within case 5's bound (`stall_swap`). Then
# the output recovers as in case 4. Anti-vacuity: the beats' egressBytes take at least 2 distinct
# values between the cut and the swap, so the bytes really trickled and this is not case 5 again.
# The brief's "compositor log OutputProgress totals" do not exist (the compositor logs no progress
# total: a false premise), so the beats' totals stand in. A red here is the witness Owed 16 asks
# for, and reopens C04 for a rate floor; it is never loosened. Mutant 5 (the netem line commented
# out) must red `trickled`.

CASE_LIVE_S=80
CASE_KEY=stall_swap
NETEM_RATE=8kbit

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
  require trickled 'mark_value("netem_out").present and mark_value("netem_out").sent_bytes > 0
    and (stall_swap(mark_at("cut")) | .swap_ms != null and (.egress_between | map(select(. != null)) | unique | length) >= 2)' \
    '{netem: mark_value("netem_out"), egress_values: (stall_swap(mark_at("cut")).egress_between | unique | length)}'
  require stall_swap 'stall_swap(mark_at("cut")) | .swap_ms != null and .last_growth_ms != null and .from_growth_ms <= .bound_ms' \
    'stall_swap(mark_at("cut")) | del(.egress_between)'
  measure swap_log_from_cut_ms '[.log.sink_swaps[] | select(.at_ms > mark_at("cut"))] | first | .at_ms - mark_at("cut")'
  measure netem_out 'mark_value("netem_out")'
  require_output_recovers
  require_no_false_restart
}
