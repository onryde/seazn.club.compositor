# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 15, cef-gpu-deaths (C14 brief, amendment 2026-10-20; C11b, Owed 70 (a)): from 20 s the
# `--type=gpu-process` child is SIGKILLed again and again (harness/in-container.sh `gpu-loop`); 90 s
# live. Three variants (`run-case.sh cef-gpu-deaths <variant>`):
#   threshold  cef_gpu_deaths_to_restart kills, each once a NEW pid has replaced the last and at
#              least KILL_SPACING_MS after the one before: the restart as in case 13
#              (`restart in place: <n> CEF GPU-process deaths`), never Chromium's abort;
#   one-fewer  one kill fewer: no restart and no exit in the run, `gpu_process_deaths=<n-1>` logged,
#              the page reloaded after each death and the overlay back after each kill within the
#              reload's budget (Owed 96), and every score change scheduled after the last kill's
#              reload window shown (the re-review's m-1);
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
#
# Owed 96 (owner ruling 2026-10-25 "A": reload the page after each counted GPU-process death; CI's
# run 40148980537 had the overlay off air 82.6 s after a counted death while every beat read
# `graphics: ready`): in every variant each count the `cef health` lines rise to below
# cef_gpu_deaths_to_restart is reloaded once (`gpu_death_reloads`), and the count at it reloads
# nothing (the in-place restart does). In one-fewer the overlay is back after each kill
# (`overlay_back_after_each_kill`), never hidden after the first (`no_hide_after_kill`), and stays on
# air after the last (`stays_on_air`).

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

# Owed 96: the reloads. The counts the `cef health` lines rose to below cef_gpu_deaths_to_restart
# (`want`; a count read twice, or by the periodic line, is one), against the `cef reload ... deaths=<n>`
# lines (`got`), in log order: one reload a count, none at the restart's. Each reload follows its
# count's first health line (the driver writes both in the tick that read it; docker stamps lines in
# the order it reads them) and within one child poll of it.
RELOADS='. as $r
  | ([.log.cef_health[] | .gpu_process_deaths | select(. > 0 and . < $t.cef_gpu_deaths_to_restart)] | unique) as $want
  | {want: $want, got: [.log.cef_reloads[] | .deaths],
     rows: [.log.cef_reloads[] | . as $x
       | ([$r.log.cef_health[] | select(.gpu_process_deaths == $x.deaths)] | first | .at_ms) as $h
       | {gen: $x.gen, deaths: $x.deaths, reason: $x.reason, after_health_ms: less($x.at_ms; $h)}]}'
require_reloads() {
  require gpu_death_reloads "$RELOADS"' | (.want | length) >= 1 and .got == .want
    and all(.rows[]; .reason == "gpu_process_death" and .after_health_ms != null
      and .after_health_ms >= 0 and .after_health_ms <= $t.cef_child_poll_ms)' "$RELOADS"
}

# Owed 96, one-fewer: the overlay back after each kill. Per kill: the reload it caused (the first
# `cef reload` after it), the first `cef ready` of that generation or a later one at or after that
# reload, and the back frame, the first strip frame at or after that ready. That frame can still be
# the held one (the slot keeps it until the new build's first sample, and `.[2] != null` cannot tell
# the two apart): that the strip is FRESH is `scheduled_changes_shown`'s to witness, never this
# check's (the PR #87 review's m-5). Each back frame lies within `budget_gpu_death_reload_ms`
# (harness::thresholds, derived: the child poll, a tick, the domain's load timeout, the frame to the
# encoder) of the LAST kill at or before it: a kill while a reload loads reloads again (the domain's
# rule), so the window restarts at that kill. No reload, no ready or no frame is no back.
# What it judges (the PR #87 review's m-4): KILL_SPACING_MS (2 s) is shorter than a reload (3 to 5 s),
# so on the real stack every reload but the last is superseded by the next kill, every kill shares
# the last back frame, and the check judges the last kill's reload. A kill's own back is judged only
# when its reload readies before the next kill, which only case_verdicts.rs's spaced worlds drive.
BACKS='[mark("kill")[] | .at_ms] as $ks
  | [$ks[] as $k
     | ([.log.cef_reloads[] | select(.at_ms > $k)] | first) as $rl
     | (if $rl == null then null else [.log.cef_ready[] | select(.gen >= $rl.gen and .at_ms >= $rl.at_ms)] | first end) as $rd
     | (if $rd == null then null else [.frames_table[] | select(.[0] >= $rd.at_ms and .[2] != null)] | first | .[0] end) as $back
     | ([$ks[] | select(. <= ($back // $k))] | max) as $from
     | {kill_ms: $k, reload_gen: $rl.gen, ready_gen: $rd.gen, back_ms: $back, back_after_ms: less($back; $from)}]'

# The score changes due after the last kill's reload window (case 15 one-fewer), as `$due`, with `$k`
# the last kill and `$from` the window's end: the overlay's back frame after that kill, or the
# reload budget after the kill when that comes first or there is no back frame, so the due set allows
# the reload and nothing more (Owed 96). Up to the last programme frame or the end request, whichever
# is first, less the frame-to-encoder budget. With no kill, no end request (or no frame) a bound is
# null and nothing is due: a red that prints `end_request_ms: null`, never a jq error with a null
# measure (the PR #79 confirm pass's C-3). A null back is handled before `min`, which would take it
# for the smallest.
DUE_AFTER_RELOAD='(mark("kill") | last | .at_ms) as $k
  | ('"$BACKS"' | last | .back_ms) as $back
  | (if $k == null then null elif $back == null then $k + $t.budget_gpu_death_reload_ms
     else [$back, $k + $t.budget_gpu_death_reload_ms] | min end) as $from
  | ([($r.frames_table | last | .[0]), $run.end_request_ms] | min) as $last
  | [scheduled[] | select($from != null and $last != null and .due_ms > $from and .due_ms <= $last - $t.budget_frame_to_encoder_ms) | .index] as $due'

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
  require_reloads
}

case "$CASE_VARIANT" in
  threshold) CASE_CHECKS=22 ;;
  one-fewer) CASE_CHECKS=15 ;;
  fast) CASE_CHECKS=23 ;;
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
      require_reloads
      # Owed 96: back after each kill, held for `resume_after_ms` from the last one's back frame
      # (lib.jq's on_air_for: a page that paints one frame and goes off air is not back), and on air
      # from there to the end request (lib.jq's stays_on_air_from, its allowance unchanged: the
      # reload window ends at the back frame, so none of it is excused there).
      require overlay_back_after_each_kill "$BACKS"' as $b | ($b | length) >= 1
        and all($b[]; .back_ms != null and .back_after_ms <= $t.budget_gpu_death_reload_ms)
        and on_air_for($b | last | .back_ms; $t.resume_after_ms)' \
        "$BACKS"' as $b | {kills: $b, budget: $t.budget_gpu_death_reload_ms}
          + hold_evidence($b | last | .back_ms; $t.resume_after_ms)'
      require stays_on_air '('"$BACKS"' | last | .back_ms) as $back | stays_on_air_from($back) | stays_on_air_ok' \
        '('"$BACKS"' | last | .back_ms) as $back | {back_ms: $back} + stays_on_air_from($back)'
      # No hide from the first kill to the end request (the PR #87 review's m-2): the domain holds a
      # reloaded page's last frame on air to its bound, and a reload ready inside it never hides, so
      # any `overlay hidden` line here is a reload that outlasted the bound (`reason=reload`), a hang,
      # a crash or a failure. The back budget alone would pass a reload ready 8 to 11 s after a kill
      # that took the overlay off air at the bound on the way.
      require no_hide_after_kill '(mark("kill") | first | .at_ms) as $k0
        | $k0 != null and $run.end_request_ms != null
          and [.log.overlay_hidden[] | select(.at_ms > $k0 and .at_ms <= $run.end_request_ms)] == []' \
        '(mark("kill") | first | .at_ms) as $k0
        | {first_kill_ms: $k0, end_request_ms: $run.end_request_ms,
           hides: [.log.overlay_hidden[] | select($k0 != null and .at_ms > $k0)
             | {at_ms, reason, gen, after_kill_ms: (.at_ms - $k0)}]}'
      # No restart, and the page paints after the last kill's reload: every score change the
      # timeline schedules after the reload window shows (the re-review's m-1: round 3's
      # `overlay_back` here passed on the held frame, CI's `back_ms` 20 and 22, a frame that never
      # left; a page that stops painting while it beats keeps a stale score on air, and the hang rule
      # never hides a page that beats). Due up to the last PROGRAMME frame, never the last frame whose
      # overlay read: that bound fell back to the kill when the page went off air (run 40148980537
      # read "0 due"; 22 fell due before its end request, every one missing), and passed a page that
      # went off air later. And no later than the end request: what the page shows in the final hold
      # is not this check's (the PR #79 review's m-1). `due_from_ms` is the reload window's end the
      # due set starts after. The measure's `off_air` (Owed 96's signature evidence) left with the
      # fix: the back and stays-on-air checks above measure the page that stops painting, and Owed 96
      # left chaos.json, so a red here reads NOT OWED.
      require scheduled_changes_shown '. as $r | '"$DUE_AFTER_RELOAD"'
        | ($r.overlay.score_index_changes | map(.index)) as $shown
        | ($due | length) >= 1 and ($due - $shown) == []' \
        '. as $r | '"$DUE_AFTER_RELOAD"'
        | ($due - ($r.overlay.score_index_changes | map(.index))) as $missing
        | {due_after_reload: ($due | length), due_from_ms: $from, missing: $missing, end_request_ms: $run.end_request_ms,
           lag_ms: [$r.overlay.score_index_changes[] | select(.due_wall_ms != null and $from != null and .due_wall_ms > $from)
             | .frame_wall_ms - .due_wall_ms] | {min: min, max: max}}'
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
