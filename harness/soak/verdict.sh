#!/usr/bin/env bash
# The soak's verdict (C15): harness/soak/verdict.jq over one run's files, with the thresholds, the
# soak's own numbers (soak.json) and the schedule (schedule.jq) bound. soak.yml and test-verdict.sh
# both call it, so the run and the fixtures are judged by one command.
#
#   harness/soak/verdict.sh --dir <run dir> --page real|test --drive-min <N> --memory-mb <MB>
#                           [--schedule soak|dropouts|none] [--thresholds <file>] [--jq <jq>]
#
# --memory-mb is the compositor Machine's memory (soak.yml passes its C_MEM, soak.json's
# compositor_guest): check 1.projection's budget is soak.json's fraction of it.
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
MEMORY_MB=""
SCHEDULE=soak
THRESHOLDS="$HERE/../thresholds.json"
JQ="jq"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir) DIR="${2:?}"; shift 2 ;;
    --page) PAGE="${2:?}"; shift 2 ;;
    --drive-min) DRIVE_MIN="${2:?}"; shift 2 ;;
    --memory-mb) MEMORY_MB="${2-}"; shift; [ "$#" -eq 0 ] || shift ;;
    --schedule) SCHEDULE="${2:?}"; shift 2 ;;
    --thresholds) THRESHOLDS="${2:?}"; shift 2 ;;
    --jq) JQ="${2:?}"; shift 2 ;;
    *) echo "verdict.sh: unknown argument $1" >&2; exit 64 ;;
  esac
done
[ -d "$DIR" ] || { echo "verdict.sh: --dir names no directory" >&2; exit 64; }
case "$PAGE" in real | test) ;; *) echo "verdict.sh: --page must be real or test" >&2; exit 64 ;; esac
case "$DRIVE_MIN" in '' | *[!0-9]*) echo "verdict.sh: --drive-min must be whole minutes" >&2; exit 64 ;; esac
case "$MEMORY_MB" in '' | *[!0-9]* | 0*) echo "verdict.sh: --memory-mb must be whole MB above 0 (the compositor Machine's memory: soak.yml's C_MEM)" >&2; exit 64 ;; esac
case "$SCHEDULE" in soak | dropouts | none) ;; *) echo "verdict.sh: --schedule must be soak, dropouts or none" >&2; exit 64 ;; esac
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
  --argjson drive_min "$DRIVE_MIN" --argjson memory_mb "$MEMORY_MB" --arg page "$PAGE" --arg schedule "$SCHEDULE" \
  -f "$HERE/verdict.jq")" || {
  echo "verdict.sh: jq failed" >&2
  exit 2
}
printf '%s\n' "$out"
"$JQ" -r '"soak verdict: \(.status) (page \(.page), \(.drive_min) min drive, \(.checked) checks)",
  (.checks[] | "  \(.status | ascii_upcase) \(.id) \(.name)\(if .reason then ": \(.reason)" else "" end)  \(.measured | tojson)")' \
  <<<"$out" >&2
"$JQ" -r '.schedule as $schedule | .measures.memory_split as $m | ($m.compositor.growth) as $c | ($m.cef.growth) as $g
  | ($m.steps) as $st | ($m.control) as $k | ($m.window) as $w | ($st.of | rtrimstr("s")) as $one
  | def mb: if . == null then "-" else "\(if . > 0 then "+" else "" end)\(.) MB" end;
  "  MEASURE memory split: \($m.lines) lines, \($m.lines_in_drive) in the drive (growth: the last part against the first, medians)",
  "  MEASURE compositor only: rss \($c.rss_mb | mb), anon \($c.anon_mb | mb), file \($c.file_mb | mb), shmem \($c.shmem_mb | mb), threads \($c.threads // "-")  \($m.compositor | tojson)",
  "  MEASURE cef only: pss \($g.cef_mb | mb) (renderer \($g.cef_renderer_mb | mb), gpu-process \($g.cef_gpu_process_mb | mb), utility \($g.cef_utility_mb | mb), zygote \($g.cef_zygote_mb | mb), other \($g.cef_other_mb | mb)); children by rss (the tree less the compositor) \($g.cef_children_rss_mb | mb)  \($m.cef | tojson)",
  "  MEASURE per-\($one) step (the lines): \($st.measured) of \($st.instants) \($st.of) measured, mean span \($st.span_mean_s // "-") s; rss mean \($st.rss_mean_mb | mb), median \($st.rss_median_mb | mb), sum \($st.rss_sum_mb | mb) (first half mean \($st.rss_first_half_mean_mb | mb), second half \($st.rss_second_half_mean_mb | mb)); anon mean \($st.anon_mean_mb | mb); cef mean \($st.cef_mean_mb | mb); children by rss mean \($st.children_mean_mb | mb); input rebuilds \($st.rebuilds // "-")",
  (if $k.measured == 0 then "  MEASURE control (the lines, between \($st.of)): NO CONTROL: \($k.gaps) gap(s), none with two distinct lines, so the steps above still carry the growth over time"
   else "  MEASURE control (the lines, between \($st.of)): \($k.measured) of \($k.gaps) gaps with two lines, mean span \($k.span_mean_s) s; rss \($k.rss.rate_mb_per_min // "-") MB/min, \($k.rss.at_step_span_mb | mb) over the steps mean span, so excess \($k.rss.excess_mb | mb) a step (anon excess \($k.anon.excess_mb | mb), cef excess \($k.cef.excess_mb | mb))" end),
  "  MEASURE windowed step (memMb, the diagnosis: median over [\($w.before_s | map(tostring) | join(", "))) s against [\($w.after_s | map(tostring) | join(", "))) s): \($w.measured) of \($st.instants) measured; mean \($w.mean_mb | mb), median \($w.median_mb | mb), sum \($w.sum_mb | mb) (first half mean \($w.first_half_mean_mb | mb), second half \($w.second_half_mean_mb | mb))\(if $schedule == "none" then "; this run is the control: nothing happened at these instants" elif $schedule == "dropouts" then "; its control is a schedule=none run of the same length (here its windows meet the next dropout)" else "" end)",
  ($m.dropouts[] | "  MEASURE \($one) \(.slot): rss \(.rss_step_mb | mb) over \(.span_s // "-") s, anon \(.anon_step_mb | mb), cef \(.cef_step_mb | mb), windowed memMb \(.window_step_mb | mb), input_gen \(.gen_before // "-") -> \(.gen_after // "-")")' \
  <<<"$out" >&2 || echo "verdict.sh: the memory split's measures did not print (the verdict above stands)" >&2
status="$("$JQ" -r '.status' <<<"$out")"
case "$status" in
  pass | plumbing) exit 0 ;;
  *) exit 1 ;;
esac
