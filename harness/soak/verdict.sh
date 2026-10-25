#!/usr/bin/env bash
# The soak's verdict (C15): harness/soak/verdict.jq over one run's files, with the thresholds, the
# soak's own numbers (soak.json) and the schedule (schedule.jq) bound. soak.yml and test-verdict.sh
# both call it, so the run and the fixtures are judged by one command.
#
#   harness/soak/verdict.sh --dir <run dir> --page real|test --drive-min <N> [--schedule soak|dropouts]
#                           [--thresholds <file>] [--jq <jq>]
#
# <run dir> holds report.json, actions.jsonl, control-state.json, deliveries.jsonl,
# overlay-samples.jsonl, logscan.json and compositor.log; a missing file reads as empty, so the
# check that needs it fails (never passes on nothing). --schedule is what the driver was to do
# (schedule.jq's schedule_plan; default soak). Prints the verdict as JSON on stdout and a table on
# stderr, then the memory split's MEASURE lines (evidence, not a gate: verdict.jq's header).
# Exit 0 on pass or plumbing (the test page: checks (a) to (d) pending), 1 on fail, 64 on a usage
# error, 2 when jq itself failed.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
DIR=""
PAGE=""
DRIVE_MIN=""
SCHEDULE=soak
THRESHOLDS="$HERE/../thresholds.json"
JQ="jq"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir) DIR="${2:?}"; shift 2 ;;
    --page) PAGE="${2:?}"; shift 2 ;;
    --drive-min) DRIVE_MIN="${2:?}"; shift 2 ;;
    --schedule) SCHEDULE="${2:?}"; shift 2 ;;
    --thresholds) THRESHOLDS="${2:?}"; shift 2 ;;
    --jq) JQ="${2:?}"; shift 2 ;;
    *) echo "verdict.sh: unknown argument $1" >&2; exit 64 ;;
  esac
done
[ -d "$DIR" ] || { echo "verdict.sh: --dir names no directory" >&2; exit 64; }
case "$PAGE" in real | test) ;; *) echo "verdict.sh: --page must be real or test" >&2; exit 64 ;; esac
case "$DRIVE_MIN" in '' | *[!0-9]*) echo "verdict.sh: --drive-min must be whole minutes" >&2; exit 64 ;; esac
case "$SCHEDULE" in soak | dropouts) ;; *) echo "verdict.sh: --schedule must be soak or dropouts" >&2; exit 64 ;; esac
[ -f "$THRESHOLDS" ] || { echo "verdict.sh: no thresholds file $THRESHOLDS" >&2; exit 64; }

EMPTY="$(mktemp)"
trap 'rm -f "$EMPTY"' EXIT
file() { if [ -f "$DIR/$1" ]; then printf '%s\n' "$DIR/$1"; else printf '%s\n' "$EMPTY"; fi; }

out="$("$JQ" -n -L "$HERE" \
  --slurpfile report "$(file report.json)" --slurpfile actions "$(file actions.jsonl)" \
  --slurpfile control "$(file control-state.json)" --slurpfile deliveries "$(file deliveries.jsonl)" \
  --slurpfile samples "$(file overlay-samples.jsonl)" --slurpfile logscan "$(file logscan.json)" \
  --rawfile complog "$(file compositor.log)" \
  --slurpfile t "$THRESHOLDS" --slurpfile soak "$HERE/soak.json" \
  --argjson drive_min "$DRIVE_MIN" --arg page "$PAGE" --arg schedule "$SCHEDULE" -f "$HERE/verdict.jq")" || {
  echo "verdict.sh: jq failed" >&2
  exit 2
}
printf '%s\n' "$out"
"$JQ" -r '"soak verdict: \(.status) (page \(.page), \(.drive_min) min drive, \(.checked) checks)",
  (.checks[] | "  \(.status | ascii_upcase) \(.id) \(.name)\(if .reason then ": \(.reason)" else "" end)  \(.measured | tojson)")' \
  <<<"$out" >&2
"$JQ" -r '.measures.memory_split as $m | ($m.compositor.growth) as $c | ($m.cef.growth) as $g | ($m.steps) as $st
  | def mb: if . == null then "-" else "\(if . > 0 then "+" else "" end)\(.) MB" end;
  "  MEASURE memory split: \($m.lines) lines, \($m.lines_in_drive) in the drive (the last part against the first, medians)",
  "  MEASURE compositor only: rss \($c.rss_mb | mb), anon \($c.anon_mb | mb), file \($c.file_mb | mb), shmem \($c.shmem_mb | mb), threads \($c.threads // "-")  \($m.compositor | tojson)",
  "  MEASURE cef only: pss \($g.cef_mb | mb) (renderer \($g.cef_renderer_mb | mb), gpu-process \($g.cef_gpu_process_mb | mb), utility \($g.cef_utility_mb | mb), zygote \($g.cef_zygote_mb | mb), other \($g.cef_other_mb | mb))  \($m.cef | tojson)",
  "  MEASURE per-dropout step: \($st.measured) of \($st.dropouts) dropouts measured; rss mean \($st.rss_mean_mb | mb), median \($st.rss_median_mb | mb), sum \($st.rss_sum_mb | mb) (first half mean \($st.rss_first_half_mean_mb | mb), second half \($st.rss_second_half_mean_mb | mb)); anon mean \($st.anon_mean_mb | mb); cef mean \($st.cef_mean_mb | mb); input rebuilds \($st.rebuilds // "-")",
  ($m.dropouts[] | "  MEASURE dropout \(.slot): rss \(.rss_step_mb | mb), anon \(.anon_step_mb | mb), cef \(.cef_step_mb | mb), input_gen \(.gen_before // "-") -> \(.gen_after // "-")")' \
  <<<"$out" >&2 || echo "verdict.sh: the memory split's measures did not print (the verdict above stands)" >&2
status="$("$JQ" -r '.status' <<<"$out")"
case "$status" in
  pass | plumbing) exit 0 ;;
  *) exit 1 ;;
esac
