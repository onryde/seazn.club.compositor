# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 13, cef-gpu-zygote-killed (C14 brief, amendment 2026-10-20; C11b): at 20 s the compositor's
# zygote that parents the `--type=gpu-process` child is SIGKILLed (read from /proc in the container:
# ppid the compositor, `--type=zygote`; harness/in-container.sh); 70 s live.
#
# The log has `cef health zygote_gone=<g>` within BUDGET_ZYGOTE_VERDICT_MS of the kill; the final
# beat's exitDetail is `restart in place: a CEF zygote is gone (gen <g>)`; the die is 75 and
# RestartCount rises by exactly 1; Chromium never aborts; the output returns, the same session, no
# counter regression, no core; the outage and its parts are measured and judged (3 runs).
# Anti-vacuity: the killed pid (printed before the kill) was a zygote of the compositor that parented
# the GPU process, and the kill's status, when the exec lived to print it, is 0.
# Mutant 6 (restart_cause None for a zygote) must red `died_in_place`.

CASE_LIVE_S=70
CASE_KEY=outage_ms
ZYGOTE=gpu
ZYGOTE_CHILD=gpu-process

case_drive() {
  at_s 20
  in_compositor kill-zygote "$ZYGOTE" | mark_lines kill || true
  case_after_kill
  at_s "$CASE_LIVE_S"
}
case_after_kill() { :; }

# The zygote verdict line after the kill: the decision.
ZYGOTE_LINE='([.log.cef_health[] | select(.zygote_gone != null and .at_ms >= mark_at("kill"))] | first)'

zygote_verdict() {
  require killed_the_zygote "(mark_value(\"kill\") | .pid != null and .type == \"zygote\" and .ppid == .compositor
    and (.child_types | any(. == \"$ZYGOTE_CHILD\"))) and (mark(\"kill_rc\") | all(.value.rc == 0))" \
    'mark_value("kill") | {pid, type, ppid, compositor, child_types}'
  require zygote_verdict "$ZYGOTE_LINE as \$z | \$z != null and (\$z.at_ms - mark_at(\"kill\")) <= \$t.budget_zygote_verdict_ms" \
    "{verdict_ms: ($ZYGOTE_LINE.at_ms - mark_at(\"kill\")), gen: $ZYGOTE_LINE.zygote_gone, budget: \$t.budget_zygote_verdict_ms}"
  require_in_place_restart 'mark_at("kill")' "\"restart in place: a CEF zygote is gone (gen \\($ZYGOTE_LINE.zygote_gone))\""
  require_restart_outage "$ZYGOTE_LINE.at_ms"
  require_same_session_and_no_core '(starts_after(mark_at("kill")) | first | .at_ms)'
}

CASE_CHECKS=18
case_verdict() {
  zygote_verdict
}
