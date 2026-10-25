# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 19, cef-blank (Owed 99, option B, applied by the coordinator 2026-10-25: a blank-overlay
# watchdog; the C14 brief's amendment 2026-10-25): the compositor keeps every frame of its first N CEF
# pages out of the overlay slot (Owed 99's hook, COMPOSITOR_TEST_WITHHOLD_PAGES, compiled in only by
# the Dockerfile's `relay-test-hooks` stage: RELAY_TEST_HOOKS_IMAGE, harness/compose.cef-blank.yml),
# so each is a page that loads, signals ready and beats, and puts nothing on air: what the PR #87
# review's I-2 named, a Ready build that writes no overlay frame. The domain calls such a page blank
# at the first tick at or after cef_hang_after_ms past its ready (domain::graphics, `Blank`) and,
# below cef_gpu_deaths_to_restart page faults (blank pages and GPU-process deaths, counted together:
# domain::cef_health::page_faults_restart), reloads it (`cef reload gen=<g> reason=no_frames
# blank_pages=<n>`). N is cef_gpu_deaths_to_restart - 1, so every withheld page is reloaded and the
# page after the last paints: the overlay comes on air within budget_blank_page_reload_ms of the last
# withheld page's ready (harness::thresholds, derived: the bound, a tick, the domain's load timeout,
# the frame to the encoder) and stays there, with every score change scheduled after it shown. 60 s
# live; the drive does nothing (the faults are the hook's, from the first page).
#
# No `threshold` variant (N = cef_gpu_deaths_to_restart, the in-place restart): the hook is armed per
# process, so the restarted process would withhold N pages again and restart again, to the
# 6-restart cap (owner ruling 2A), which tests the cap and not the watchdog. The restart at the count
# is the domain's and the app's tests' (crates/domain/tests/cef_health.rs, crates/app/tests/
# cef_health.rs), and a restart here is a red (`no_restart`, `fp_no_exit_75`).
#
# The detect bound is case 11's (cef-hung.sh): the blank comes at the first tick at or after
# cef_hang_after_ms past the ready, so `reload - ready` lies in [cef_hang_after_ms, + one frame
# period), and one more period each way covers the docker stamps of two lines and a late tick.
# Anti-vacuity: the arming line, N withheld pages counting down, no overlay in any programme frame
# before the last blank reload (the hook really withheld: at least one frame read), and exactly one
# painted page before the end request, the one after the last withheld (the adapter's first-frame
# report, `cef painted gen=<g>`).

CASE_LIVE_S=60
CASE_KEY=overlay_back
WITHHOLD=$(($(threshold cef_gpu_deaths_to_restart) - 1))
export COMPOSITOR_TEST_WITHHOLD_PAGES=$WITHHOLD
CASE_COMPOSE=compose.cef-blank.yml
if [ "$CASE_REJUDGE" != 1 ]; then
  RELAY_IMAGE="${RELAY_TEST_HOOKS_IMAGE:?RELAY_TEST_HOOKS_IMAGE names the relay-test-hooks image (case 19)}"
fi

case_drive() {
  at_s "$CASE_LIVE_S"
}

# The first process's window: its log lines before a second boot (a restart is a red here, and the
# restarted process arms N again).
P1='(.log.boots | if length > 1 then .[1] else 1e18 end) as $p1 | ($t.cef_gpu_deaths_to_restart - 1) as $n'
# Each withheld page: its gen and countdown, its ready (the first `cef ready` of its gen after its
# withheld line), and the blank reload that replaced it: the k-th `cef reload ... reason=no_frames`
# of the first process for the k-th withheld page, paired by order, so `blank_reloads` alone judges
# the gen each names (the PR #92 review's m-6: paired by gen, a reload naming the wrong gen was
# never paired, and the gen clause was dead). Read after P1, whose $p1 it uses, as
# `"$P1 | ($PAGES)" as $p`.
PAGES='. as $r | [.log.blank_reloads[] | select(.at_ms < $p1)] as $bls
  | [.log.pages_withheld[] | select(.at_ms < $p1)] | to_entries | map(.key as $k | .value as $w
  | ([$r.log.cef_ready[] | select(.gen == $w.gen and .at_ms >= $w.at_ms)] | first) as $rd
  | $bls[$k] as $bl
  | {gen: $w.gen, left: $w.left, ready_ms: $rd.at_ms, reload_ms: $bl.at_ms, reload_gen: $bl.gen,
     blank_pages: $bl.blank_pages, reload_after_ready_ms: less($bl.at_ms; $rd.at_ms)})'
# The page after the last withheld one (`$g`), its ready and its painted report, the last withheld
# page's ready (`$from`, the budget's start) and blank reload (`$reload`), and the back frame: the
# first strip frame at or after that ready.
BACK="$P1 | ($PAGES)"' as $p | (($p | last | .gen // 0) + 1) as $g
  | ($p | last | .ready_ms) as $from | ($p | last | .reload_ms) as $reload
  | ([.log.cef_ready[] | select(.gen == $g)] | first) as $rd
  | ([.log.cef_painted[] | select(.gen == $g)] | first) as $pt
  | (if $rd == null then null else [.frames_table[] | select(.[0] >= $rd.at_ms and .[2] != null)] | first | .[0] end) as $back'
# The score changes due after the back frame, up to the last programme frame or the end request,
# whichever is first, less the frame-to-encoder budget (case 15 one-fewer's rule). No back frame or
# no end request: nothing is due, a red that prints it.
DUE_AFTER_BACK="$BACK"' | ([(.frames_table | last | .[0]), $run.end_request_ms] | min) as $last
  | [scheduled[] | select($back != null and $last != null and .due_ms > $back and .due_ms <= $last - $t.budget_frame_to_encoder_ms) | .index] as $due'

CASE_CHECKS=20
case_verdict() {
  require armed "$P1"' | .log.boots as $b | .log.withhold_armed as $a
    | ($b | length) >= 1 and ($a | length) == ($b | length)
    and all(range($a | length); $a[.].n == $n and $a[.].at_ms <= $b[.] and (. == 0 or $a[.].at_ms > $b[. - 1]))' \
    "$P1"' | {armed: .log.withhold_armed, boots: .log.boots, want: $n}'
  # The first N builds, each withheld, the countdown to 0.
  require withheld_n "$P1"' | [.log.pages_withheld[] | select(.at_ms < $p1)] as $w
    | $n >= 1 and ($w | map(.left)) == [range($n - 1; -1; -1)] and ($w | map(.gen)) == [range(1; $n + 1)]' \
    "$P1"' | {withheld: [.log.pages_withheld[] | {gen, left}], want: $n}'
  # Each withheld page reloaded once, as the next gen, the count 1..N, at its bound past its ready.
  require blank_reloads "$P1 | ($PAGES)"' as $p | [.log.blank_reloads[] | select(.at_ms < $p1)] as $b
    | ($p | length) == $n and ($b | map(.blank_pages)) == [range(1; $n + 1)] and ($b | map(.gen)) == ($p | map(.gen + 1))
    and all($p[]; .reload_after_ready_ms != null
      and .reload_after_ready_ms >= ($t.cef_hang_after_ms - $t.frame_period_ms)
      and .reload_after_ready_ms <= ($t.cef_hang_after_ms + 2 * $t.frame_period_ms))' \
    "$P1 | ($PAGES)"' as $p | {pages: $p, reloads: [.log.blank_reloads[] | {gen, blank_pages}],
      bounds: [($t.cef_hang_after_ms - $t.frame_period_ms), ($t.cef_hang_after_ms + 2 * $t.frame_period_ms)]}'
  # The hook withheld: no programme frame before the last blank reload shows the overlay, and at
  # least one was read (no frame of the page after it can exist before it is built).
  require off_air_while_withheld "$BACK"' | [.frames_table[] | select($reload != null and .[0] < $reload)] as $f
    | ($f | length) >= 1 and all($f[]; .[2] == null)' \
    "$BACK"' | [.frames_table[] | select($reload != null and .[0] < $reload)] as $f
    | {last_reload_ms: $reload, frames_before: ($f | length), with_overlay: ([$f[] | select(.[2] != null)] | length)}'
  # The adapter's first-frame report: the page after the last withheld one, once, after its ready;
  # never a withheld page.
  require painted_once "$BACK"' | [.log.cef_painted[] | select($run.end_request_ms != null and .at_ms < $run.end_request_ms)] as $c
    | ($c | map(.gen)) == [$g] and $rd != null and $pt.at_ms >= $rd.at_ms' \
    "$BACK"' | {want_gen: $g, painted: [.log.cef_painted[] | {gen, at_ms}], ready_ms: $rd.at_ms}'
  # Back within the budget from the last withheld page's ready, and held for resume_after_ms (lib.jq's
  # on_air_for: a page that paints one frame and goes off air is not back).
  require overlay_back "$BACK"' | $back != null and $from != null and ($back - $from) <= $t.budget_blank_page_reload_ms
    and on_air_for($back; $t.resume_after_ms)' \
    "$BACK"' | {gen: $g, last_withheld_ready_ms: $from, ready_ms: $rd.at_ms, painted_ms: $pt.at_ms, back_ms: $back,
      back_after_ms: less($back; $from), budget: $t.budget_blank_page_reload_ms} + hold_evidence($back; $t.resume_after_ms)'
  require stays_on_air "$BACK"' | stays_on_air_from($back) | stays_on_air_ok' \
    "$BACK"' | {back_ms: $back} + stays_on_air_from($back)'
  # The page paints what it is sent: every score change scheduled after the back frame shown (a held
  # or frozen frame would pass the two checks above).
  require scheduled_changes_shown '. as $r | '"$DUE_AFTER_BACK"'
    | ($r.overlay.score_index_changes | map(.index)) as $shown
    | ($due | length) >= 1 and ($due - $shown) == []' \
    '. as $r | '"$DUE_AFTER_BACK"'
    | {due_after_back: ($due | length), back_ms: $back, missing: ($due - ($r.overlay.score_index_changes | map(.index))),
       end_request_ms: $run.end_request_ms}'
  # A blank page is reloaded, never hidden: nothing was on air to hide, and the page beats (no hang).
  require no_hide '$run.end_request_ms != null and [.log.overlay_hidden[] | select(.at_ms <= $run.end_request_ms)] == []' \
    '[.log.overlay_hidden[] | {at_ms, reason, gen}]'
  require_run no_restart '.compositor.restarts == .restarts_at_live' '{at_live: .restarts_at_live, at_end: .compositor.restarts}'
  require_run ended_beat '.last_beat.state == "stopped" and .last_beat.exitReason == "ended"' '.last_beat | {state, exitReason}'
  require counter_regressions '.camera.counter_regressions == 0'
  require_run no_core '.cores.diff_ok and .cores.container_files == 0 and .cores.host_after == .cores.host_before' '.cores'
  require no_output_outages '.output_outages == []' '.output_outages | length'
  # What the beats said while the overlay was blank: printed, never judged. The beat carries no
  # blank state (Owed 99: no wire change), so `graphics: ready` here is the residual the owner reads.
  measure beats_while_blank "$BACK"' | ($p | first | .ready_ms) as $a
    | beats_between($a // 0; $back // 0) | map(.body.graphics) | group_by(.) | map({(.[0] // "null"): length}) | add // {}'
  require_no_false_restart
}
