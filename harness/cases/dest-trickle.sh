# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 12, dest-trickle (C14 brief, amendment 2026-10-18; Owed 16(b)): `netem out rate 8kbit` at
# 20 s (the link stays open and bytes still trickle), cleared about 50 s in, in the variant's phase
# (case 4's); 80 s live.
#
# A trickle is a stall, not progress: the sink's own socket timeout swaps it within case 5's bound
# (`stall_swap_judged`; Owed 87, the owner, 2026-10-24, "A": rtmp2sink's `timeout` is the only
# detector of a destination stall). Then the output recovers as in case 4. Anti-vacuity: the netem
# qdisc sent at least 2 packets through the open
# link, so the bytes really trickled and this is not case 5 again (case 5's qdisc sends none), and
# the swap came. The trickle is witnessed where it is injected, as case 10's loss is. The brief's
# "compositor log OutputProgress totals" do not exist (the compositor logs no progress total: a
# false premise), and the beats' totals are no stand-in either (the PR #52 review's I2): a correct
# swap ends the window 5 to 10 s after the cut, one or two 5 s beats, so "2 distinct values" cannot
# be met, and the window cannot be lengthened past the swap that ends it. A red here is the witness
# Owed 16 asks for, and reopens C04 for a rate floor; it is never loosened. Mutant 5 (the netem line
# commented out) must red `trickled`.

CASE_LIVE_S=80
CASE_KEY=stall_swap
# The clear's phase (the re-review's I-5; Owed 84): the variant's (`dest_phase`), from 45 s on, so
# it lands 47-57 s in (one reconnect cycle at most); `require_clear_phase` reds a missed phase.
CLEAR_PHASE="$(dest_phase)"
CLEAR_FROM_S=45
NETEM_RATE=8kbit

case_drive() {
  at_s 20
  mark cut
  "$HERE/netem.sh" out rate "$NETEM_RATE"
  clear_out_in_phase "$CLEAR_PHASE" "$CLEAR_FROM_S"
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=15
case_verdict() {
  require trickled 'mark_value("netem_out") as $n | $n.sent_pkts >= 2
    and stall_swap(mark_at("cut")).swap_ms != null' \
    '{netem: mark_value("netem_out"), swap_ms: stall_swap(mark_at("cut")).swap_ms}'
  require stall_swap 'stall_swap_judged(mark_at("cut"))' 'stall_swap(mark_at("cut"))'
  measure swap_log_from_cut_ms '[.log.sink_swaps[] | select(.at_ms > mark_at("cut"))] | first | .at_ms - mark_at("cut")'
  measure netem_out 'mark_value("netem_out")'
  require_output_recovers
  require_clear_phase
  require_no_false_restart
}
