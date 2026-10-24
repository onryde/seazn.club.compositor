# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 10, input-impaired (C14 brief; spec §9.4 "Camera uplink impaired"): `netem in delay 400ms
# 100ms loss 2%` on MediaMTX's SRT media to the compositor from 20 s, cleared at 80 s; 100 s live.
#
# The programme clock holds through a bad camera leg: no PTS gap, fps_mean at least
# BUDGET_FPS_MEAN_MIN, no counter regression, no output outage. SRT's fixed TSBPD latency absorbs
# the delay, so the fault is witnessed where it is injected: (a) the netem qdisc's Sent and dropped
# counts (the brief's u32 filter hit count exists only on a kernel built with CONFIG_CLS_U32_PERF:
# a false premise, C13's as-built; the qdisc's own counts stand in), and (b) MediaMTX's
# packetsRetrans on the compositor's reader connection grew in the window (F-021).
# latency_windows_ms is printed, never judged.

CASE_LIVE_S=100
CASE_KEY=fps_mean

case_drive() {
  at_s 20
  mark readers_before "$(mtx_readers)"
  mark impair
  "$HERE/netem.sh" in delay 400ms 100ms loss 2%
  at_s 80
  mark netem_in "$("$HERE/netem.sh" stats in)"
  mark readers_at_clear "$(mtx_readers)"
  "$HERE/netem.sh" clear in
  mark clear
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=11
case_verdict() {
  require fault_applied 'mark_value("netem_in") | .present and .sent_pkts > 0 and .dropped > 0' 'mark_value("netem_in")'
  require retransmitted '(mark_value("readers_before") | first) as $a | (mark_value("readers_at_clear") | first) as $b
    | $b != null and (if $a != null and $a.id == $b.id then ($b.packetsRetrans - $a.packetsRetrans) else $b.packetsRetrans end) > 0' \
    '{before: (mark_value("readers_before") | first), at_clear: (mark_value("readers_at_clear") | first)}'
  require pts_gaps '.pts_gaps == []' '.pts_gaps | length'
  require fps_mean '.fps_mean >= $t.budget_fps_mean_min' '{fps_mean, floor: $t.budget_fps_mean_min}'
  require counter_regressions '.camera.counter_regressions == 0'
  require no_output_outages '.output_outages == []' '.output_outages | length'
  measure latency_windows_ms '[.camera.latency_windows_ms[] | .p50]'
  require_no_false_restart
}
