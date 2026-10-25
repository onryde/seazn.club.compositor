# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 8, cef-killed (C14 brief; amended 2026-10-19/20, Owed 62 and 68): at 20 s every CEF child is
# SIGKILLed at once (the brief's `pkill -KILL -f cef`; the relay image has no procps, so
# harness/in-container.sh reads /proc); 70 s live.
#
# Measured on CEF 139 (Owed 62): that kill aborts the compositor process (Chromium's FATAL "GPU
# process isn't usable", SIGTRAP), so the case takes case 9's shape: the container dies non-zero,
# compose's `on-failure:2` restarts it once, the output returns within
# BUDGET_PROCESS_RESTART_OUTPUT_MS of the kill, the same session, no counter regression, and no core
# (Owed 68). Since the zygotes are listed at CEF's first ready (F-022), C11b's zygote verdict can come
# before Chromium's abort, and the process then restarts in place (75) as the owner's ruling "B"
# asks for a dead zygote (Owed 56): CI read 75 in 1 of 6 runs and 133 in the rest (C14 fix round 2;
# the brief's "not 75" was measured before the listing). So the death is either: any non-zero exit,
# and a 75 only with C11b's own final beat (`exitReason` "restart"). The outage and its parts are measured (3 runs; the
# table prints the worst). The restart evidence of Owed 19 is the new process's own `cef ready`
# after the start, the first beat after that ready reads graphics ready, and every beat after the
# kill reads ready, restarting or starting; and the overlay is back on air within
# BUDGET_PROCESS_RESTART_OUTPUT_MS of the output's last packet (`require_overlay_back`, fix rounds 3
# and 4). Anti-vacuity: the kill's targets (printed before it kills) name at least one CEF child, and
# its status, when the exec lived to print it, is 0.

CASE_LIVE_S=70
CASE_KEY=outage_ms

case_drive() {
  at_s 20
  in_compositor kill-cef | mark_lines kill || true # the container may die under the exec
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=20
case_verdict() {
  # Anti-vacuity: the kill reached CEF's children, and the container died.
  require kill_applied '(mark_value("kill") | (.killed | length) >= 1) and (mark("kill_rc") | all(.value.rc == 0))' \
    '{killed: (mark_value("kill").killed | length), types: (mark_value("kill").types | group_by(.) | map({(.[0]): length}) | add),
      rc: [mark("kill_rc")[] | .value.rc]}'
  require died_to_restart '(dies_after(mark_at("kill")) | first) as $d
    | ([beats[] | select(.received_ms >= mark_at("kill") and .body.state == "stopped")] | first) as $b
    | $d != null and $d.exit_code != 0 and ($d.exit_code != 75 or $b.body.exitReason == "restart")' \
    '(dies_after(mark_at("kill")) | first) as $d
    | ([beats[] | select(.received_ms >= mark_at("kill") and .body.state == "stopped")] | first) as $b
    | {exit_code: $d.exit_code, exitReason: $b.body.exitReason, exitDetail: $b.body.exitDetail}'
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
  require_overlay_back '(dies_after(mark_at("kill")) | first | .at_ms)' \
    '(starts_after(dies_after(mark_at("kill")) | first | .at_ms) | first | .at_ms)'
  require beats_never_failed 'beats_between(mark_at("kill"); $run.end_request_ms + 1)
    | length >= 1 and all(.body.state != "playing" or .body.graphics == "ready" or .body.graphics == "restarting" or .body.graphics == "starting")' \
    'beats_between(mark_at("kill"); $run.end_request_ms + 1) | map(.body.graphics) | unique'
  require_restart_outage 'mark_at("kill")'
  require_same_session_and_no_core '(starts_after(mark_at("kill")) | first | .at_ms)'
}
