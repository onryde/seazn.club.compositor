#!/usr/bin/env bash
# The soak's verdict (C15): harness/soak/verdict.jq over one run's files, with the thresholds, the
# soak's own numbers (soak.json) and the schedule (schedule.jq) bound. soak.yml and test-verdict.sh
# both call it, so the run and the fixtures are judged by one command.
#
#   harness/soak/verdict.sh --dir <run dir> --page real|test --drive-min <N> [--thresholds <file>] [--jq <jq>]
#
# <run dir> holds report.json, actions.jsonl, control-state.json, deliveries.jsonl,
# overlay-samples.jsonl and logscan.json; a missing file reads as empty, so the check that needs it
# fails (never passes on nothing). Prints the verdict as JSON on stdout and a table on stderr.
# Exit 0 on pass or plumbing (the test page: checks (a) to (d) pending), 1 on fail, 64 on a usage
# error, 2 when jq itself failed.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
DIR=""
PAGE=""
DRIVE_MIN=""
THRESHOLDS="$HERE/../thresholds.json"
JQ="jq"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir) DIR="${2:?}"; shift 2 ;;
    --page) PAGE="${2:?}"; shift 2 ;;
    --drive-min) DRIVE_MIN="${2:?}"; shift 2 ;;
    --thresholds) THRESHOLDS="${2:?}"; shift 2 ;;
    --jq) JQ="${2:?}"; shift 2 ;;
    *) echo "verdict.sh: unknown argument $1" >&2; exit 64 ;;
  esac
done
[ -d "$DIR" ] || { echo "verdict.sh: --dir names no directory" >&2; exit 64; }
case "$PAGE" in real | test) ;; *) echo "verdict.sh: --page must be real or test" >&2; exit 64 ;; esac
case "$DRIVE_MIN" in '' | *[!0-9]*) echo "verdict.sh: --drive-min must be whole minutes" >&2; exit 64 ;; esac
[ -f "$THRESHOLDS" ] || { echo "verdict.sh: no thresholds file $THRESHOLDS" >&2; exit 64; }

EMPTY="$(mktemp)"
trap 'rm -f "$EMPTY"' EXIT
file() { if [ -f "$DIR/$1" ]; then printf '%s\n' "$DIR/$1"; else printf '%s\n' "$EMPTY"; fi; }

out="$("$JQ" -n -L "$HERE" \
  --slurpfile report "$(file report.json)" --slurpfile actions "$(file actions.jsonl)" \
  --slurpfile control "$(file control-state.json)" --slurpfile deliveries "$(file deliveries.jsonl)" \
  --slurpfile samples "$(file overlay-samples.jsonl)" --slurpfile logscan "$(file logscan.json)" \
  --slurpfile t "$THRESHOLDS" --slurpfile soak "$HERE/soak.json" \
  --argjson drive_min "$DRIVE_MIN" --arg page "$PAGE" -f "$HERE/verdict.jq")" || {
  echo "verdict.sh: jq failed" >&2
  exit 2
}
printf '%s\n' "$out"
"$JQ" -r '"soak verdict: \(.status) (page \(.page), \(.drive_min) min drive, \(.checked) checks)",
  (.checks[] | "  \(.status | ascii_upcase) \(.id) \(.name)\(if .reason then ": \(.reason)" else "" end)  \(.measured | tojson)")' \
  <<<"$out" >&2
status="$("$JQ" -r '.status' <<<"$out")"
case "$status" in
  pass | plumbing) exit 0 ;;
  *) exit 1 ;;
esac
