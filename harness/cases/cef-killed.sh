# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 8, cef-killed (C14 brief; amended 2026-10-19/20, Owed 62 and 68): at 20 s every CEF child is
# SIGKILLed at once (the brief's `pkill -KILL -f cef`; the relay image has no procps, so
# harness/in-container.sh reads /proc); 70 s live.
#
# Measured on CEF 139 (Owed 62): that kill aborts the compositor process (Chromium's FATAL "GPU
# process isn't usable", SIGTRAP), so the case takes case 9's shape: the container dies with a
# non-zero exit that is not C11b's in-place restart (75), compose's `on-failure:2` restarts it once,
# the output returns within BUDGET_PROCESS_RESTART_OUTPUT_MS of the kill, the same session, no
# counter regression, and no core (Owed 68). The outage and its parts are measured (3 runs; the
# table prints the worst). The restart evidence of Owed 19 is the new process's own `cef ready`
# after the start, the first beat after that ready reads graphics ready, and every beat after the
# kill reads ready, restarting or starting. Anti-vacuity: the kill's targets (printed before it
# kills) name at least one CEF child, and its status, when the exec lived to print it, is 0.

CASE_LIVE_S=70
CASE_KEY=outage_ms

case_drive() {
  at_s 20
  in_compositor kill-cef | mark_lines kill || true # the container may die under the exec
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=17
case_verdict() {
  # Anti-vacuity: the kill reached CEF's children, and the container died.
  require kill_applied '(mark_value("kill") | (.killed | length) >= 1) and (mark("kill_rc") | all(.value.rc == 0))' \
    '{killed: (mark_value("kill").killed | length), types: (mark_value("kill").types | group_by(.) | map({(.[0]): length}) | add),
      rc: [mark("kill_rc")[] | .value.rc]}'
  require died_not_in_place '(dies_after(mark_at("kill")) | first) as $d | $d != null and $d.exit_code != 0 and $d.exit_code != 75' \
    '(dies_after(mark_at("kill")) | first | .exit_code)'
  require_run restarted_once '.compositor.restarts - .restarts_at_live == 1' '{at_live: .restarts_at_live, at_end: .compositor.restarts}'
  require output_resumes_after_kill '(dies_after(mark_at("kill")) | first | .at_ms) as $die | (starts_after($die) | first | .at_ms) as $start
    | restart_outage($die; $start) as $o | $o != null and ($o.to_wall_ms - mark_at("kill")) <= $t.budget_process_restart_output_ms' \
    '(dies_after(mark_at("kill")) | first | .at_ms) as $die | (starts_after($die) | first | .at_ms) as $start | restart_outage($die; $start).to_wall_ms - mark_at("kill")'
  measure gpu_unusable_lines '.log.gpu_unusable | length'
  require cef_ready_after_restart '(starts_after(mark_at("kill")) | first | .at_ms) as $s
    | ([.log.cef_ready[] | select(.at_ms > $s)] | first | .at_ms) as $r
    | $s != null and $r != null and ([beats[] | select(.received_ms > $r)] | first | .body.graphics) == "ready"' \
    '(starts_after(mark_at("kill")) | first | .at_ms) as $s | ([.log.cef_ready[] | select(.at_ms > $s)] | first | .at_ms) as $r
      | {ready_after_start_ms: ($r - $s), first_beat_after: ([beats[] | select(.received_ms > $r)] | first | .body.graphics)}'
  require beats_never_failed 'beats_between(mark_at("kill"); $run.end_request_ms + 1)
    | length >= 1 and all(.body.state != "playing" or .body.graphics == "ready" or .body.graphics == "restarting" or .body.graphics == "starting")' \
    'beats_between(mark_at("kill"); $run.end_request_ms + 1) | map(.body.graphics) | unique'
  require_restart_outage 'mark_at("kill")'
  require_same_session_and_no_core '(starts_after(mark_at("kill")) | first | .at_ms)'
}
