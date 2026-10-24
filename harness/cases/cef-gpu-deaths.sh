# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 15, cef-gpu-deaths (C14 brief, amendment 2026-10-20; C11b, Owed 70 (a)): from 20 s the
# `--type=gpu-process` child is SIGKILLed again and again (harness/in-container.sh `gpu-loop`); 90 s
# live. Three variants (`run-case.sh cef-gpu-deaths <variant>`):
#   threshold  cef_gpu_deaths_to_restart kills, each once a NEW pid has replaced the last and at
#              least KILL_SPACING_MS after the one before: the restart as in case 13
#              (`restart in place: <n> CEF GPU-process deaths`), never Chromium's abort;
#   one-fewer  one kill fewer: no restart and no exit in the run, `gpu_process_deaths=<n-1>` logged;
#   fast       the fast loop (the PR #34 review's gap): each kill the moment a new pid appears, until
#              the process exits or chromium_gpu_deaths_abort - 1 are done (one short of the abort).
#
# Every kill is stamped on this host's clock (the one `docker logs --timestamps` uses), and the
# verdict splits the kills at the `cef health ... gpu_process_deaths=<n>` line that decided the
# restart: `unread` = the kills at or before that line, less n (how far the 1 s child poll lags a
# real loop), and `at_exit` = the kills after that line and before the die (deaths during the final
# beat's flush). Owed 70 (a) budgets one survivable death to each (BUDGET_GPU_DEATHS_UNREAD,
# BUDGET_GPU_DEATHS_AT_EXIT): over either is a red and a finding for the owner, as is the loop's
# last kill before the die, or unread below 0 (the log counted a death no kill made).
# Anti-vacuity: each kill's pid differs from the one before. Mutant 7 (cef_gpu_deaths_to_restart 6
# in a scratch build) must red the threshold run's `died_in_place`.

CASE_LIVE_S=90
KILL_SPACING_MS=2000

case "$CASE_VARIANT" in
  threshold | fast) CASE_KEY=gpu_death_split ;;
  one-fewer) CASE_KEY=deaths_logged ;;
  *)
    echo "cef-gpu-deaths: the variant is threshold, one-fewer or fast (got '$CASE_VARIANT')" >&2
    exit 64
    ;;
esac

case_drive() {
  local n
  n="$(threshold cef_gpu_deaths_to_restart)"
  at_s 20
  case "$CASE_VARIANT" in
    threshold) in_compositor gpu-loop "$n" "$KILL_SPACING_MS" | mark_lines kill || true ;;
    one-fewer) in_compositor gpu-loop $((n - 1)) "$KILL_SPACING_MS" | mark_lines kill || true ;;
    fast) in_compositor gpu-loop $(($(threshold chromium_gpu_deaths_abort) - 1)) 0 | mark_lines kill || true ;;
  esac
  at_s "$CASE_LIVE_S"
}

# The cef health line that decided the restart, and the kills split at it.
DECIDING='([.log.cef_health[] | select(.gpu_process_deaths >= $t.cef_gpu_deaths_to_restart and .at_ms >= mark_at("kill"))] | first)'
SPLIT="$DECIDING"' as $line | (dies_after(mark_at("kill")) | first | .at_ms) as $die
  | {kills: (mark("kill") | length), deaths_named: $line.gpu_process_deaths,
     unread: (if $line == null then null else (mark("kill") | map(select(.at_ms <= $line.at_ms)) | length) - $line.gpu_process_deaths end),
     at_exit: (if $line == null then null else mark("kill") | map(select(.at_ms > $line.at_ms and ($die == null or .at_ms < $die))) | length end)}'

require_split() {
  require gpu_death_split "$SPLIT"' | .unread != null and .unread >= 0 and .unread <= $t.budget_gpu_deaths_unread
    and .at_exit <= $t.budget_gpu_deaths_at_exit' \
    "$SPLIT"' + {budget_unread: $t.budget_gpu_deaths_unread, budget_at_exit: $t.budget_gpu_deaths_at_exit}'
}

require_restarted() {
  require deaths_logged "$DECIDING != null" "$DECIDING"
  require_in_place_restart 'mark_at("kill")' "\"restart in place: \\($DECIDING.gpu_process_deaths) CEF GPU-process deaths\""
  require_split
}

case "$CASE_VARIANT" in
  threshold) CASE_CHECKS=17 ;;
  one-fewer) CASE_CHECKS=9 ;;
  fast) CASE_CHECKS=18 ;;
esac
case_verdict() {
  case "$CASE_VARIANT" in
    threshold)
      require kills_distinct '(mark("kill") | length) == $t.cef_gpu_deaths_to_restart and (mark("kill") | map(.value.pid) | repeats) == []' \
        'mark("kill") | map(.value.pid)'
      require_restarted
      require_restart_outage "$DECIDING.at_ms"
      require_same_session_and_no_core '(starts_after(mark_at("kill")) | first | .at_ms)'
      ;;
    one-fewer)
      require kills_distinct '(mark("kill") | length) == ($t.cef_gpu_deaths_to_restart - 1) and (mark("kill") | map(.value.pid) | repeats) == []' \
        'mark("kill") | map(.value.pid)'
      require deaths_logged '([.log.cef_health[] | .gpu_process_deaths] | max // 0) == ($t.cef_gpu_deaths_to_restart - 1)' \
        '[.log.cef_health[] | .gpu_process_deaths]'
      require_run no_restart '.compositor.restarts == .restarts_at_live' '{at_live: .restarts_at_live, at_end: .compositor.restarts}'
      require no_die_before_end '[dies_after(mark_at("kill"))[] | select(.at_ms < $run.end_request_ms)] == []'
      require no_abort '.log.gpu_unusable == []' '.log.gpu_unusable | length'
      require_run ended_beat '.last_beat.state == "stopped" and .last_beat.exitReason == "ended"' '.last_beat | {state, exitReason}'
      require counter_regressions '.camera.counter_regressions == 0'
      require_run no_core '.cores.diff_ok and .cores.container_files == 0 and .cores.host_after == .cores.host_before' '.cores'
      require no_output_outages '.output_outages == []' '.output_outages | length'
      ;;
    fast)
      require kills_distinct '(mark("kill") | length) >= 1 and (mark("kill") | map(.value.pid) | repeats) == []' \
        'mark("kill") | map(.value.pid)'
      require_restarted
      require last_kill_not_before_die '(dies_after(mark_at("kill")) | first | .at_ms) as $die
        | [mark("kill")[] | select(.value.n >= ($t.chromium_gpu_deaths_abort - 1) and ($die == null or .at_ms < $die))] == []' \
        '{kills: (mark("kill") | length), die_ms: (dies_after(mark_at("kill")) | first | .at_ms)}'
      require_restart_outage "$DECIDING.at_ms"
      require_same_session_and_no_core '(starts_after(mark_at("kill")) | first | .at_ms)'
      ;;
  esac
}
