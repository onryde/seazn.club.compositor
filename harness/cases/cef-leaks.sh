# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 16, cef-leaks (C14 brief, amendment 2026-10-20; C11b): 3 teardowns leaked at once (variant
# `three`: the restart, `restart in place: 3 CEF teardowns leaked`), then 2 (`two`: no restart,
# `leaked_teardowns=2` logged).
#
# NOT RUNNABLE, reported, never faked (the brief's own condition). Holding a teardown needs C11's
# `hold_teardowns` hook (crates/adapters, feature `test-hooks`), which the release image does not
# carry, and a control that reaches it in a running compositor, which does not exist: building one
# is a change to crates/compositor and crates/adapters, outside this task (the dispatch). Owed: a
# harness-only relay build with the hook and a control (`docs/_INDEX.md`).

CASE_LIVE_S=60
CASE_KEY=""
case "$CASE_VARIANT" in
  three | two) ;;
  *)
    echo "cef-leaks: the variant is three or two (got '$CASE_VARIANT')" >&2
    exit 64
    ;;
esac
CASE_NOT_RUNNABLE="needs C11's hold_teardowns hook (adapters feature test-hooks) and a control to reach it in a running compositor; the release image has neither (Owed, docs/_INDEX.md)"

case_drive() { :; }
CASE_CHECKS=1
case_verdict() { :; }
