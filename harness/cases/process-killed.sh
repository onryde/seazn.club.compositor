# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 9, process-killed (C14 brief; amended 2026-10-18, Owed 19): at 20 s the compositor process is
# SIGKILLed (the brief's `pkill -KILL -x compositor`; harness/in-container.sh reads /proc); 70 s live.
#
# What web reads off Fly's Machine in production, read here after the restart, never from a beat in
# the gap: the `die` event's non-zero exitCode and RestartCount up by exactly 1. The output returns
# within BUDGET_PROCESS_RESTART_OUTPUT_MS of the kill, the beats come back, the same session, no
# counter regression, no core; the outage and its parts are measured (3 runs; the table prints the
# worst). No CEF kill here, so the false-positive sweep runs too.
#
# Owed 79 (measured here, reported only): netem-out shares the compositor's network namespace; a
# pass-through netem on the output leg from the start shows whether its qdisc still carries the
# restarted process's packets.

CASE_LIVE_S=70
CASE_KEY=outage_ms

case_drive() {
  "$HERE/netem.sh" out limit 10000
  at_s 18
  mark netem_before "$("$HERE/netem.sh" stats out)"
  at_s 20
  in_compositor kill-compositor | mark_lines kill || true # the container dies under the exec
  at_s 55
  mark netem_after "$("$HERE/netem.sh" stats out || echo '{"present":false,"error":true}')"
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=19
case_verdict() {
  require kill_applied 'mark_value("kill") | .rc == 0 and .pid != null' 'mark_value("kill")'
  require died_non_zero '(dies_after(mark_at("kill")) | first) as $d | $d != null and $d.exit_code != 0' \
    '(dies_after(mark_at("kill")) | first | .exit_code)'
  require_run restarted_once '.compositor.restarts - .restarts_at_live == 1' '{at_live: .restarts_at_live, at_end: .compositor.restarts}'
  require output_resumes_after_kill '(dies_after(mark_at("kill")) | first | .at_ms) as $die | (starts_after($die) | first | .at_ms) as $start
    | restart_outage($die; $start) as $o | $o != null and ($o.to_wall_ms - mark_at("kill")) <= $t.budget_process_restart_output_ms' \
    '(dies_after(mark_at("kill")) | first | .at_ms) as $die | (starts_after($die) | first | .at_ms) as $start | restart_outage($die; $start).to_wall_ms - mark_at("kill")'
  owed netem_out_survives_restart 'Owed 79' 'mark_value("netem_after").present == true
    and mark_value("netem_after").sent_pkts > mark_value("netem_before").sent_pkts'
  require_restart_outage 'mark_at("kill")'
  require_same_session_and_no_core '(starts_after(mark_at("kill")) | first | .at_ms)'
  require_no_false_restart
}
