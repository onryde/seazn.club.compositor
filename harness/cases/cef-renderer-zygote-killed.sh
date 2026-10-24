# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 14, cef-renderer-zygote-killed (C14 brief, amendment 2026-10-20; C11b): as case 13, the zygote
# that parents a `--type=renderer` child. After the restart no process from the old tree is left:
# every `--type=` process in the container started after the restart's `start` event.

# shellcheck source=/dev/null
. "$HERE/cases/cef-gpu-zygote-killed.sh"

CASE_LIVE_S=70
ZYGOTE=renderer
ZYGOTE_CHILD=renderer

case_after_kill() {
  at_s 60
  in_compositor tree | mark_lines tree_after || true
}

CASE_CHECKS=17
case_verdict() {
  zygote_verdict
  require old_tree_gone '[mark("tree_after")[] | .value | select(.type != null)] as $p
    | (starts_after(mark_at("kill")) | first | .at_ms) as $start
    | ($p | length) >= 1 and $start != null and ($p | all(.start_ms > $start))' \
    '{cef_processes: ([mark("tree_after")[] | .value | select(.type != null)] | length),
      oldest_ms_after_start: ([mark("tree_after")[] | .value | select(.type != null) | .start_ms] | min) - (starts_after(mark_at("kill")) | first | .at_ms)}'
}
