# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 14, cef-renderer-zygote-killed (C14 brief, amendment 2026-10-20; C11b): as case 13, the zygote
# that parents a `--type=renderer` child. After the restart no process from the old tree is left:
# every `--type=` process in the container started after the restart. With no kill mark,
# `starts_after(null)` is the run's first start, which every process is younger than: so the kill
# mark is required (C14 fix round 2).
#
# The restart is the sampled container's own init (pid 1 of the same `tree`, the same clock), which
# must itself have started after the kill (no init: `null > n` is false, so a missing one reds too);
# the sample must come after the restart's `start` event (its lines' `at_ms`, this host's wall
# clock, as docker's). Fix round 3: CI's 6 of 6 reds were the old start_ms clock (btime, 173 to
# 1 080 ms early: in-container.sh), never a surviving process: every typed process was parented to
# the NEW compositor and started 130 to 450 ms after the new init. The `start` event is the daemon's
# report of the start, so it is no anchor for a process that starts a hundred ms into the
# container: the init is.

# shellcheck source=/dev/null
. "$HERE/cases/cef-gpu-zygote-killed.sh"

CASE_LIVE_S=70
ZYGOTE=renderer
ZYGOTE_CHILD=renderer

case_after_kill() {
  at_s 60
  in_compositor tree | mark_lines tree_after || true
}

CASE_CHECKS=18
case_verdict() {
  zygote_verdict
  require old_tree_gone '[mark("tree_after")[] | .value] as $all
    | [$all[] | select(.type != null)] as $p
    | ([$all[] | select(.pid == 1)] | first) as $init
    | (starts_after(mark_at("kill")) | first | .at_ms) as $start
    | mark_at("kill") != null and $start != null and ($all | all(.at_ms > $start))
      and $init.start_ms > mark_at("kill")
      and ($p | length) >= 1 and ($p | all(.start_ms > $init.start_ms))' \
    '[mark("tree_after")[] | .value] as $all | ([$all[] | select(.pid == 1)] | first) as $init
      | (starts_after(mark_at("kill")) | first | .at_ms) as $start
      | {cef_processes: ([$all[] | select(.type != null)] | length),
         init_after_kill_ms: less($init.start_ms; mark_at("kill")),
         oldest_after_init_ms: less([$all[] | select(.type != null) | .start_ms] | min; $init.start_ms),
         init_minus_start_event_ms: less($init.start_ms; $start),
         sampled_after_start_ms: less([$all[] | .at_ms] | min; $start)}'
}
