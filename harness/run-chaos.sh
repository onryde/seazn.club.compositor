#!/usr/bin/env bash
# The chaos suite's plan and its table (C14), over harness/chaos.json:
#
#   harness/run-chaos.sh --legs <all|leg,leg,...>
#       the legs as a JSON array of {name, leg, case, variant, run}: chaos.yml's matrix. A leg is
#       <case>[-<variant>], and selecting one selects all its repeats (name <leg>-r<n>). An unknown
#       leg is refused (exit 64), never dropped.
#   harness/run-chaos.sh --table <dir> [<all|leg,leg,...>]
#       one row per selected leg, from <dir>/<name>/verdict.json (a local harness/out) or
#       <dir>/verdict-<name>/verdict.json (chaos.yml's artifacts): PASS, FAIL, ERROR, NOT RUNNABLE
#       (reported, never red: the case says why) or MISSING, with the case's key measure. Then the
#       worst outage of each repeated case, and how many legs ran the false-positive sweep. Exits 1
#       on any FAIL, ERROR or MISSING, on an empty selection, and on a sweep that ran on no leg
#       although a selected case calls it.
#
# The table only reads what each run judged. It never re-judges: run-case.sh's verdict is the one
# authority, and `CASE_REJUDGE=1 harness/run-case.sh <case>` re-judges a downloaded out/<run>.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
MANIFEST="$HERE/chaos.json"

legs() {
  local want="$1" all unknown
  all="$(jq -c '[.legs[] | . as $l | ($l.variant // "") as $v
      | ($l.case + (if $v == "" then "" else "-" + $v end)) as $leg
      | (if $l.runs > 1 then (range(1; $l.runs + 1) | tostring) else "" end) as $r
      | {name: ($leg + (if $r == "" then "" else "-r" + $r end)), leg: $leg, case: $l.case,
         variant: $v, run: $r}]' "$MANIFEST")"
  if [ "$want" = all ]; then
    printf '%s\n' "$all"
    return 0
  fi
  unknown="$(jq -rn --argjson all "$all" --arg want "$want" \
    '($want | split(",") | map(gsub("^ +| +$"; "")) | map(select(. != ""))) - ($all | map(.leg)) | join(", ")')"
  if [ -n "$unknown" ]; then
    echo "run-chaos: no leg $unknown in harness/chaos.json" >&2
    exit 64
  fi
  jq -cn --argjson all "$all" --arg want "$want" \
    '($want | split(",") | map(gsub("^ +| +$"; ""))) as $w | [$all[] | select(.leg as $n | any($w[]; . == $n))]'
}

table() {
  local dir="$1" want="${2:-all}" selected n=0 red=0 not_runnable=0 swept=0 sweeps=0
  local leg name case_ f status key measured row
  selected="$(legs "$want")"
  echo "| leg | verdict | checks | key | measured |"
  echo "|---|---|---|---|---|"
  while IFS= read -r leg; do
    name="$(jq -r .name <<<"$leg")"
    case_="$(jq -r .case <<<"$leg")"
    n=$((n + 1))
    if grep -q 'require_no_false_restart' "$HERE/cases/$case_.sh"; then sweeps=$((sweeps + 1)); fi
    f=""
    for c in "$dir/$name/verdict.json" "$dir/verdict-$name/verdict.json"; do
      if [ -f "$c" ]; then f="$c"; break; fi
    done
    if [ -z "$f" ]; then
      echo "| $name | MISSING | - | - | no verdict.json |"
      red=$((red + 1))
      continue
    fi
    status="$(jq -r '.status // "error"' "$f")"
    key="$(jq -r '.key // ""' "$f")"
    measured="$(jq -c --arg k "$key" 'if $k == "" then null else .measures[$k] end' "$f" | cut -c1-160)"
    row="$(jq -r '"\(.checks_run)/\(.checks_declared)"' "$f")"
    if [ "$(jq -r '.fp_swept' "$f")" = true ]; then swept=$((swept + 1)); fi
    case "$status" in
      pass) echo "| $name | PASS | $row | ${key:--} | \`$measured\` |" ;;
      not_runnable)
        not_runnable=$((not_runnable + 1))
        echo "| $name | NOT RUNNABLE | - | - | $(jq -r '.not_runnable_reason // ""' "$f" | cut -c1-160) |"
        ;;
      fail)
        red=$((red + 1))
        echo "| $name | FAIL | $row | ${key:--} | failed: $(jq -r '.failed | join(", ")' "$f" | cut -c1-160) |"
        ;;
      *)
        red=$((red + 1))
        echo "| $name | ERROR ($status) | $row | ${key:--} | the run ended before its verdict |"
        ;;
    esac
  done < <(jq -c '.[]' <<<"$selected")
  echo
  # The worst outage of each repeated case (each run already judged its own against the budget).
  local budget repeated
  budget="$(jq '.budget_process_restart_output_ms' "$HERE/thresholds.json")"
  repeated="$(jq -r '[group_by(.leg)[] | select(length > 1) | .[0].leg] | .[]' <<<"$selected")"
  for leg in $repeated; do
    local worst="" runs=0 v
    while IFS= read -r name; do
      for c in "$dir/$name/verdict.json" "$dir/verdict-$name/verdict.json"; do
        [ -f "$c" ] || continue
        v="$(jq -r '.measures.outage_ms // empty | numbers' "$c")"
        if [ -n "$v" ]; then
          runs=$((runs + 1))
          if [ -z "$worst" ] || [ "$v" -gt "$worst" ]; then worst="$v"; fi
        fi
        break
      done
    done < <(jq -r --arg l "$leg" '.[] | select(.leg == $l) | .name' <<<"$selected")
    echo "worst outage_ms, $leg: ${worst:-none} over $runs runs (budget $budget)"
  done
  echo "legs: $n, red: $red, not runnable: $not_runnable; the false-positive sweep ran on $swept of the $sweeps legs that call it"
  if [ "$n" -eq 0 ]; then
    echo "FAIL no leg was selected"
    exit 1
  fi
  if [ "$sweeps" -gt 0 ] && [ "$swept" -eq 0 ]; then
    echo "FAIL the false-positive sweep ran on no leg"
    exit 1
  fi
  [ "$red" -eq 0 ] || exit 1
}

case "${1:-}" in
  --legs) legs "${2:?--legs <all|leg,...>}" ;;
  --table) table "${2:?--table <dir> [<all|leg,...>]}" "${3:-all}" ;;
  *)
    echo "usage: run-chaos.sh --legs <all|leg,...> | --table <dir> [<all|leg,...>]" >&2
    exit 64
    ;;
esac
