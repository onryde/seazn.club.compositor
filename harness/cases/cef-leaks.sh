# shellcheck shell=bash disable=SC2034,SC2016,SC2154 # sourced by run-case.sh (its cid and restarts_at_live: SC2154); jq filters keep their $ (SC2016)
# Case 16, cef-leaks (C14 brief, amendment 2026-10-20; C11b; amendment 2026-10-24, "Owed 82's
# handoff"): the compositor holds its next N retired CEF teardowns for good (Owed 82's hook,
# COMPOSITOR_TEST_HOLD_TEARDOWNS, compiled in only by the Dockerfile's `relay-test-hooks` stage:
# RELAY_TEST_HOOKS_IMAGE, harness/compose.cef-leaks.yml), and from 20 s the current build's renderer
# is SIGKILLed (harness/in-container.sh `kill-renderer`: the highest `--renderer-client-id`) once
# its `cef ready` is in, until N are done. Each kill is a CEF crash, so a retirement (the third in
# five minutes is Failed, and the graphics stop that follows is the retirement), and each held
# teardown is killed by the reaper REAPER_KILL_AFTER_MS later and counted leaked
# TEARDOWN_LEAK_AFTER_MS after that. Two variants (owner ruling 4A: 1 to 2 leaked teardowns are
# carried to the session's end, the 3rd restarts in place):
#   three  N = cef_leaks_to_restart: the restart in place, exit 75 and the final beat's
#          `restart in place: <N> CEF teardowns leaked`, read as in case 13 (key: outage_ms);
#   two    N = cef_leaks_to_restart - 1: no restart, on air to the end with `leaked_teardowns=<N>`
#          logged, and the CEF children's CPU printed, never judged (key: retirements).
#
# The arming line is a precondition (`armed`): a leg on the plain relay image arms nothing, and
# would read as "0 leaked" were it not a red. Verdicts are keyed on gen and count RETIREMENTS, not
# kills (a load timeout before the first kill takes one from N too): N held lines counting down,
# each held gen's reaper line naming at least 1 killed pid (the hold comes before
# `set_state(NULL)`, so cefsrc is live: the brief's only witness of that on the real image), and
# the leak count 1..N. The drive leaves REAPER_KILL_AFTER_MS + TEARDOWN_LEAK_AFTER_MS + 10 s (the
# brief's room) before the session's end, and `three` the restart's budget beside it; it stops
# after N kills, so the restarted process (armed again: the budget is per process) is never driven.
# Every number comes from harness/thresholds.json. Never the page-hang driver (one-shot, and a held
# page can take it).

N_RESTART="$(threshold cef_leaks_to_restart)"
case "$CASE_VARIANT" in
  three)
    HOLD=$N_RESTART
    CASE_LIVE_S=80
    CASE_KEY=outage_ms
    ;;
  two)
    HOLD=$((N_RESTART - 1))
    CASE_LIVE_S=60
    CASE_KEY=retirements
    ;;
  *)
    echo "cef-leaks: the variant is three or two (got '$CASE_VARIANT')" >&2
    exit 64
    ;;
esac
# Owner ruling 4A: the restart comes at cef_leaks_to_restart leaked, never before.
RESTART_EXPECTED=$((HOLD >= N_RESTART))
export COMPOSITOR_TEST_HOLD_TEARDOWNS=$HOLD
CASE_COMPOSE=compose.cef-leaks.yml
if [ "$CASE_REJUDGE" != 1 ]; then
  RELAY_IMAGE="${RELAY_TEST_HOOKS_IMAGE:?RELAY_TEST_HOOKS_IMAGE names the relay-test-hooks image (case 16)}"
fi
# The brief's room after the last leak, before the session ends.
ROOM_AFTER_LEAK_MS=10000
# The drive's own patience past a wait's budget (a detection, a docker log read): a drive that
# gives up early leaves fewer retirements, which `held_n` reds; it never makes a pass.
DRIVE_SLACK_S=5

# This process's held lines so far (the restarted process is never driven).
held_so_far() {
  docker logs "$cid" 2>&1 | grep -cE ' msg=WARN test hook \(Owed 82\) gen=[0-9]+: ' || true
}
# Waits up to <s> seconds for a `gen=<g>: loading` line with g above <gen>: prints its docker stamp
# in ms and g, or fails. Lines are read in memory and never printed (await_log's rule).
await_loading() {
  local above="$1" until=$(($(now_ms) + $2 * 1000)) line g
  while [ "$(now_ms)" -lt "$until" ]; do
    line="$(docker logs --timestamps "$cid" 2>&1 | grep -E ' msg=gen=[0-9]+: loading ' | tail -n 1 || true)"
    if [ -n "$line" ]; then
      g="${line#* msg=gen=}"
      g="${g%%:*}"
      if [[ "$g" =~ ^[0-9]+$ ]] && [ "$g" -gt "$above" ]; then
        echo "$(stamp_ms "${line%% *}") $g"
        return 0
      fi
    fi
    sleep 0.2
  done
  return 1
}

case_drive() {
  local end_ms room_ms load_wait ready_wait kills=0 last=0 load_ms g ready_ms stopped=none s
  end_ms=$((DRIVE_START_MS + CASE_LIVE_S * 1000))
  room_ms=$(($(threshold reaper_kill_after_ms) + $(threshold teardown_leak_after_ms) + ROOM_AFTER_LEAK_MS))
  if [ "$RESTART_EXPECTED" = 1 ]; then room_ms=$((room_ms + $(threshold budget_process_restart_output_ms))); fi
  # A crash's next build comes after its back-off (at most the cap), and its ready within the load
  # timeout of its load.
  load_wait=$((($(threshold cef_backoff_cap_ms) + $(threshold cef_load_timeout_ms)) / 1000 + DRIVE_SLACK_S))
  ready_wait=$(($(threshold cef_load_timeout_ms) / 1000 + DRIVE_SLACK_S))
  at_s 20
  while [ "$kills" -lt "$HOLD" ]; do
    if [ "$(held_so_far)" -ge "$HOLD" ]; then stopped=held_already; break; fi
    if [ "$(docker inspect --format '{{.RestartCount}}' "$cid")" != "$restarts_at_live" ]; then
      stopped=restarted
      break
    fi
    # The current build: the newest `loading` line's gen, never any `cef ready` (a held page's
    # reload logs `cef ready gen=<old>`).
    if ! read -r load_ms g < <(await_loading "$last" "$load_wait"); then stopped=no_build; break; fi
    if ! read -r ready_ms _ < <(await_log "$load_ms" "cef ready gen=$g\$" "$ready_wait"); then
      stopped=not_ready
      break
    fi
    if [ "$(held_so_far)" -ge "$HOLD" ]; then stopped=held_already; break; fi
    if [ $(($(now_ms) + room_ms)) -gt "$end_ms" ]; then stopped=no_room; break; fi
    MARK_AT_MS="$ready_ms" mark ready "{\"gen\":$g,\"loading_ms\":$load_ms}"
    in_compositor kill-renderer | mark_lines kill || true
    kills=$((kills + 1))
    last=$g
  done
  mark drive_done "{\"kills\":$kills,\"stopped\":\"$stopped\"}"
  if [ "$CASE_VARIANT" = two ]; then
    # The CEF children's CPU, twice, after the leaks: printed, never judged (no budget exists).
    for s in 40 55; do
      at_s "$s"
      mark cef_cpu "$(in_compositor cef-cpu | jq -ce 'select(.children)' 2>/dev/null || echo null)"
    done
  fi
  at_s "$CASE_LIVE_S"
}

# The first process's window: its log lines before the second boot (`three` restarts; the
# restarted process arms N again and its session-end stop is held, never this case's).
P1='(.log.boots | if length > 1 then .[1] else 1e18 end) as $p1'
HELD="$P1"' | [.log.teardowns_held[] | select(.at_ms < $p1)]'
# Each kill and the build it hit: the newest `gen=<g>: loading` before it, and that gen's `cef ready`
# after the load and before the kill.
KILLS='. as $r | [mark("kill")[] | . as $k
  | ([$r.log.cef_loading[] | select(.at_ms < $k.at_ms)] | last) as $l
  | {at_ms, pid: .value.pid, client_id: .value.client_id,
     newest_id: ([.value.renderers[]?.client_id] | max), renderers: ([.value.renderers[]?] | length), gen: $l.gen,
     ready_ms: (if $l == null then null
       else [$r.log.cef_ready[] | select(.gen == $l.gen and .at_ms > $l.at_ms and .at_ms < $k.at_ms)] | first | .at_ms end)}]'
RETIREMENTS="($HELD) as \$h | ($KILLS) as \$k | {held: (\$h | length), kills: (\$k | length), want: $HOLD,
  held_gens: (\$h | map(.gen)), killed_gens: (\$k | map(.gen)), drive: mark_value(\"drive_done\")}"
# The cef health line that decided the restart (`three`).
DECIDING='([.log.cef_health[] | select(.leaked_teardowns >= $t.cef_leaks_to_restart and .at_ms >= mark_at("kill"))] | first)'

# The checks both variants run. 7 checks.
require_common() {
  require armed ".log.boots as \$b | .log.hold_armed as \$a
    | (\$b | length) >= 1 and (\$a | length) == (\$b | length)
    and all(range(\$a | length); \$a[.].n == $HOLD and \$a[.].at_ms <= \$b[.] and (. == 0 or \$a[.].at_ms > \$b[. - 1]))" \
    "{armed: .log.hold_armed, boots: .log.boots, want: $HOLD}"
  require killed_one_current "($KILLS) as \$k | (\$k | length) >= 1 and (\$k | length) <= $HOLD
    and all(\$k[]; .client_id == .newest_id and .ready_ms != null)
    and (mark(\"kill_rc\") | length) == (\$k | length) and all(mark(\"kill_rc\")[]; .value.rc == 0)" \
    "{kills: ($KILLS), rc: (mark(\"kill_rc\") | map(.value.rc))}"
  require held_n "($HELD) as \$h | ($KILLS) as \$k
    | (\$h | map(.left)) == [range($HOLD - 1; -1; -1)]
    and (\$h | map(.gen) | unique | length) == (\$h | length) and ((\$k | map(.gen)) - (\$h | map(.gen))) == []" \
    "$RETIREMENTS + {lefts: (($HELD) | map(.left))}"
  require reaper_killed ". as \$r | $P1 | ($HELD) as \$h
    | (\$h | length) >= 1 and all(\$h[]; . as \$x
      | ([\$r.log.reaper_kills[] | select(.gen == \$x.gen and .at_ms < \$p1)] | first) as \$k
      | (\$k.killed | length) >= 1 and \$k.after_ms == \$t.reaper_kill_after_ms)" \
    "$P1 | ($HELD) as \$h | {held_gens: (\$h | map(.gen)), reaper: [.log.reaper_kills[] | select(.at_ms < \$p1)
      | {gen, after_ms, killed: (.killed | length), of: (.of | length)}], after_ms: \$t.reaper_kill_after_ms}"
  require leaks_counted ". as \$r | $P1 | ($HELD) as \$h | [.log.teardown_leaks[] | select(.at_ms < \$p1)] as \$l
    | (\$l | map(.leaked)) == [range(1; $HOLD + 1)] and (\$l | map(.gen)) == (\$h | map(.gen))
    and all(\$l[]; .at_ms < \$run.end_request_ms and (. as \$x
      | ([\$r.log.reaper_kills[] | select(.gen == \$x.gen)] | first | .at_ms) as \$k | \$k != null and \$k <= \$x.at_ms))" \
    "$P1 | ($HELD) as \$h | {leaks: [.log.teardown_leaks[] | select(.at_ms < \$p1) | . as \$x | {gen, leaked,
      after_held_ms: less(.at_ms; ([\$h[] | select(.gen == \$x.gen)] | first | .at_ms))}],
      want: $HOLD, reaper_plus_leak_ms: (\$t.reaper_kill_after_ms + \$t.teardown_leak_after_ms),
      end_request_ms: \$run.end_request_ms}"
  require restart_as_ruled "[dies_after(mark_at(\"kill\") // 0)[] | select(.exit_code == 75)] | length == $RESTART_EXPECTED" \
    "{exit_75: [dies_after(mark_at(\"kill\") // 0)[] | select(.exit_code == 75) | .at_ms], expected: $RESTART_EXPECTED,
      held: $HOLD, restart_at: \$t.cef_leaks_to_restart}"
  measure retirements "$RETIREMENTS"
}

if [ "$RESTART_EXPECTED" = 1 ]; then CASE_CHECKS=24; else CASE_CHECKS=16; fi
case_verdict() {
  require_common
  if [ "$RESTART_EXPECTED" = 1 ]; then
    require leaks_health "$DECIDING | .leaked_teardowns == \$t.cef_leaks_to_restart
      and .zygote_gone == null and .gpu_process_deaths == 0" \
      "{line: $DECIDING, counted_max: ([.log.cef_health[] | .leaked_teardowns] | max), needs: \$t.cef_leaks_to_restart}"
    require_in_place_restart 'mark_at("kill")' "\"restart in place: \\($DECIDING.leaked_teardowns) CEF teardowns leaked\""
    require_restart_outage "$DECIDING.at_ms"
    require_same_session_and_no_core '(starts_after(mark_at("kill")) | first | .at_ms)'
  else
    # On air to the end at `cef health zygote_gone=- leaked_teardowns=<N> gpu_process_deaths=0`.
    require leaked_logged "([.log.cef_health[] | select(.at_ms < \$run.end_request_ms)] | last) as \$l
      | \$l != null and \$l.leaked_teardowns == $HOLD and \$l.zygote_gone == null and \$l.gpu_process_deaths == 0" \
      "{last: ([.log.cef_health[] | select(.at_ms < \$run.end_request_ms)] | last), want: $HOLD}"
    # The build after the last kill is on air: its ready after that kill, then every score change
    # the timeline schedules after it shown (case 15's one-fewer: a page that stops painting and
    # keeps beating holds a stale score on air), and graphics never gave up. Due up to the last
    # PROGRAMME frame, never the last frame whose overlay read: that bound passed a page that went
    # off air partway, and read "0 due" for one that went off at once (case 15's Owed 96).
    local newest='(mark("kill") | last | .at_ms) as $k
      | ([.log.cef_loading[] | select($k != null and .at_ms > $k)] | last) as $n
      | (if $n == null then null else [.log.cef_ready[] | select(.gen == $n.gen and .at_ms > $n.at_ms)] | first | .at_ms end) as $ready'
    require overlay_on_air "$newest"' | . as $r
      | ($r.overlay.score_index_changes | map(.index)) as $shown
      | ($r.frames_table | last | .[0]) as $last
      | [scheduled[] | select($ready != null and .due_ms > $ready and .due_ms <= $last - $t.budget_frame_to_encoder_ms) | .index] as $due
      | ($due | length) >= 1 and ($due - $shown) == []
      and ([.log.overlay_hidden[] | select(.reason == "failed")] == [])' \
      "$newest"' | . as $r | ($r.frames_table | last | .[0]) as $last
      | [scheduled[] | select($ready != null and .due_ms > $ready and .due_ms <= $last - $t.budget_frame_to_encoder_ms) | .index] as $due
      | {newest_gen: $n.gen, ready_ms: $ready, due_after_ready: ($due | length),
         missing: ($due - ($r.overlay.score_index_changes | map(.index))),
         failed: ([.log.overlay_hidden[] | select(.reason == "failed")] | length)}'
    require on_air_to_end '.output_outages == []' '.output_outages | length'
    require_run no_restart '.compositor.restarts == .restarts_at_live' '{at_live: .restarts_at_live, at_end: .compositor.restarts}'
    require no_abort '.log.gpu_unusable == []' '.log.gpu_unusable | length'
    require_run ended_beat '.last_beat.state == "stopped" and .last_beat.exitReason == "ended"' '.last_beat | {state, exitReason}'
    require counter_regressions '.camera.counter_regressions == 0'
    require_run no_core '.cores.diff_ok and .cores.container_files == 0 and .cores.host_after == .cores.host_before' '.cores'
    # Each CEF child's CPU between the two samples, ms per s (the brief: printed, never judged).
    measure cef_cpu '(mark("cef_cpu") | map(.value) | map(select(. != null))) as $s
      | if ($s | length) < 2 then {samples: ($s | length)} else ($s | first) as $a | ($s | last) as $b
        | {samples: ($s | length), interval_ms: ($b.sample_ms - $a.sample_ms),
           ms_per_s: [$b.children[] | . as $c | ([$a.children[] | select(.pid == $c.pid)] | first) as $o
             | {type, client_id, ms_per_s: (if $o == null then null
                 else ((.cpu_ms - $o.cpu_ms) * 1000 / ($b.sample_ms - $a.sample_ms) | floor) end)}]} end'
  fi
}
