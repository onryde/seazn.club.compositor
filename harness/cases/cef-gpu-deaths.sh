# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 15, cef-gpu-deaths (C14 brief, amendment 2026-10-20; C11b, Owed 70 (a)): from 20 s the
# `--type=gpu-process` child is SIGKILLed again and again (harness/in-container.sh `gpu-loop`); 90 s
# live. Three variants (`run-case.sh cef-gpu-deaths <variant>`):
#   threshold  cef_gpu_deaths_to_restart kills, each once a NEW pid has replaced the last and at
#              least KILL_SPACING_MS after the one before: the restart as in case 13
#              (`restart in place: <n> CEF GPU-process deaths`), never Chromium's abort;
#   one-fewer  one kill fewer: no restart and no exit in the run, `gpu_process_deaths=<n-1>` logged,
#              and every score change scheduled after the last kill shown (the re-review's m-1);
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
# Anti-vacuity: each kill's pid differs from the one before. The loop has a deadline (the PR #52
# review's I8: with no target it ran to the relay's 900 s): BUDGET_PROCESS_RESTART_OUTPUT_MS before
# the drive's end, so a restart it causes still shows its outage and its beats before the end
# request. A loop that reaches it fails (`kill_loop_in_time`). Mutant 7 (cef_gpu_deaths_to_restart
# 6 in a scratch build) must red the threshold run's `died_in_place`.

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
  local n deadline
  n="$(threshold cef_gpu_deaths_to_restart)"
  deadline=$((DRIVE_START_MS + CASE_LIVE_S * 1000 - $(threshold budget_process_restart_output_ms)))
  at_s 20
  case "$CASE_VARIANT" in
    threshold) in_compositor gpu-loop "$n" "$KILL_SPACING_MS" "$deadline" | mark_lines kill || true ;;
    one-fewer) in_compositor gpu-loop $((n - 1)) "$KILL_SPACING_MS" "$deadline" | mark_lines kill || true ;;
    fast) in_compositor gpu-loop $(($(threshold chromium_gpu_deaths_abort) - 1)) 0 "$deadline" | mark_lines kill || true ;;
  esac
  at_s "$CASE_LIVE_S"
}

# The cef health line that decided the restart, and the kills split at it.
DECIDING='([.log.cef_health[] | select(.gpu_process_deaths >= $t.cef_gpu_deaths_to_restart and .at_ms >= mark_at("kill"))] | first)'
SPLIT="$DECIDING"' as $line | (dies_after(mark_at("kill")) | first | .at_ms) as $die
  | {kills: (mark("kill") | length), deaths_named: $line.gpu_process_deaths,
     unread: (if $line == null then null else (mark("kill") | map(select(.at_ms <= $line.at_ms)) | length) - $line.gpu_process_deaths end),
     at_exit: (if $line == null then null else mark("kill") | map(select(.at_ms > $line.at_ms and ($die == null or .at_ms < $die))) | length end)}'

# The score changes due after the last kill (case 15 one-fewer), as `$due`, with `$k` the last kill:
# up to the last programme frame or the end request, whichever is first, less the frame-to-encoder
# budget. With no end request (or no frame) the bound is null and nothing is due: a red that prints
# `end_request_ms: null`, never a jq error with a null measure (the PR #79 confirm pass's C-3).
DUE_AFTER_KILL='(mark("kill") | last | .at_ms) as $k
  | ([($r.frames_table | last | .[0]), $run.end_request_ms] | min) as $last
  | [scheduled[] | select($k != null and $last != null and .due_ms > $k and .due_ms <= $last - $t.budget_frame_to_encoder_ms) | .index] as $due'

require_split() {
  require gpu_death_split "$SPLIT"' | .unread != null and .unread >= 0 and .unread <= $t.budget_gpu_deaths_unread
    and .at_exit <= $t.budget_gpu_deaths_at_exit' \
    "$SPLIT"' + {budget_unread: $t.budget_gpu_deaths_unread, budget_at_exit: $t.budget_gpu_deaths_at_exit}'
}

require_restarted() {
  # Shown: the deciding line, and the kills against the most deaths any line counted (C14 fix round
  # 2: CI's fast loop read 5 kills and 1 counted, which the bare line showed as null).
  require deaths_logged "$DECIDING != null" \
    "{line: $DECIDING, kills: (mark(\"kill\") | length), counted_max: ([.log.cef_health[] | .gpu_process_deaths] | max),
      needs: \$t.cef_gpu_deaths_to_restart}"
  require_in_place_restart 'mark_at("kill")' "\"restart in place: \\($DECIDING.gpu_process_deaths) CEF GPU-process deaths\""
  require_split
}

case "$CASE_VARIANT" in
  threshold) CASE_CHECKS=20 ;;
  one-fewer) CASE_CHECKS=11 ;;
  fast) CASE_CHECKS=21 ;;
esac
case_verdict() {
  # The loop's own end line, when the exec lived to print it (a restart kills the exec with its
  # container): never its deadline.
  require kill_loop_in_time 'mark("kill_done") | all(.value.timed_out == false)' 'mark("kill_done") | map(.value)'
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
      # No restart, and the page still paints after the last kill: every score change the timeline
      # schedules after it shows (the re-review's m-1: round 3's `overlay_back` here passed on the
      # held frame, CI's `back_ms` 20 and 22, a frame that never left; a page that stops painting
      # while it beats keeps a stale score on air, and the hang rule never hides a page that beats).
      # Due up to the last PROGRAMME frame, never the last frame whose overlay read (Owed 96): that
      # bound fell back to the kill when the page went off air (run 40148980537 read "0 due"; 22
      # fell due before its end request, every one missing), and passed a page that went off air
      # later. And no later than the end request:
      # what the page shows in the final hold is not this check's (the PR #79 review's m-1).
      # `off_air` is Owed 96's evidence: the longest strip-less run after the first kill (healthy
      # runs show a 1 or 2 frame blip at a kill), timed from the kill before it; `at_kill` when that
      # is within one child poll (`cef_child_poll_ms`, the cadence the product counts a death at),
      # the hides logged after the first kill, and `covers_missing`: changes are missing and every
      # one fell due inside the run (false with none missing).
      require scheduled_changes_shown '. as $r | '"$DUE_AFTER_KILL"'
        | ($r.overlay.score_index_changes | map(.index)) as $shown
        | ($due | length) >= 1 and ($due - $shown) == []' \
        '. as $r | '"$DUE_AFTER_KILL"'
        | ($due - ($r.overlay.score_index_changes | map(.index))) as $missing
        | (mark("kill") | first | .at_ms) as $k0
        | ([$r.overlay.runs[] | select(.index == null and $k0 != null and .from_ms > $k0)] | max_by(.frames)) as $off
        | {due_after_last_kill: ($due | length), missing: $missing, end_request_ms: $run.end_request_ms,
           lag_ms: [$r.overlay.score_index_changes[] | select(.due_wall_ms != null and $k != null and .due_wall_ms > $k)
             | .frame_wall_ms - .due_wall_ms] | {min: min, max: max},
           off_air: (if $off == null then null
             else less($off.from_ms; [mark("kill")[] | select(.at_ms < $off.from_ms) | .at_ms] | max) as $after
             | {from_ms: $off.from_ms, after_kill_ms: $after, at_kill: ($after <= $t.cef_child_poll_ms),
                ms: ($off.to_ms - $off.from_ms), frames: $off.frames, to_end: ($off == ($r.overlay.runs | last)),
                hidden_after_kill: ([$r.log.overlay_hidden[] | select(.at_ms > $k0)] | length),
                covers_missing: (($missing | length) >= 1
                  and all(scheduled[] | select(.index as $i | any($missing[]; . == $i)) | .due_ms;
                    . + $t.budget_frame_to_encoder_ms >= $off.from_ms and . < $off.to_ms))} end)}'
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
