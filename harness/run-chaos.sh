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
#       (the case says why) or MISSING, with the case's key measure. Then the worst outage of each
#       repeated case, how many legs ran the false-positive sweep, and the wave gate 3 line: a run
#       counts toward the gate only when every selected leg PASSED. Exits 1 on any FAIL, ERROR or
#       MISSING, on an empty selection, on a sweep that some leg calling it did not run, and on any
#       NOT RUNNABLE leg: an unrun leg is never green (the PR #52 review's I6; case 16's are
#       Owed 82, which only the owner's ruling closes). Only a whole manifest's run, on its first
#       attempt (GITHUB_RUN_ATTEMPT 1), with every leg PASS, counts: a selection or a re-run is
#       green for what it ran and says it never counts (the re-review's I-2 and m-7).
#
# A FAIL row names, beside each failed check, the docs/_INDEX.md Owed row chaos.json's `owed` puts
# behind it for that leg (a row, or a list tried in order), or NOT OWED; under the table, the red
# legs every failed check of which is owed, the reds no Owed row explains, and the title of each row
# named (C14 fix round 3, item 4). A row is named only when its signature (`owed_signatures`, a jq
# predicate over the leg's verdict.json) holds in that run: the evidence of the row's own cause,
# not the check's name (the re-review's I-3). A listed check whose rows' signatures are all absent
# reads `NOT OWED: Owed <r>'s signature absent`, and counts as unexplained. That is reading aid
# only: an owed red is as red as any other, and the exit status and the gate line never read it.
#
# The table only reads what each run judged. It never re-judges: run-case.sh's verdict is the one
# authority, and `CASE_REJUDGE=1 harness/run-case.sh <case>` re-judges a downloaded out/<run>.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
# CHAOS_MANIFEST: another manifest, for chaos_table.rs alone. Its list of rows behind one check is
# pinned there, since no leg of chaos.json carries one now that Owed 85 has left it. chaos.yml never
# sets it.
MANIFEST="${CHAOS_MANIFEST:-$HERE/chaos.json}"

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

# The `owed` map of a leg (<case>[-<variant>]) in chaos.json, each value a list: {check: [Owed
# row, ...]}, or {}.
owed_of() {
  jq -c --arg leg "$1" '[.legs[] | select((.case + (if (.variant // "") == "" then "" else "-" + .variant end)) == $leg)
    | .owed // {}] | first // {} | map_values(if type == "array" then . else [.] end)' "$MANIFEST"
}

# Whether each Owed row the failed checks of <verdict.json> name has its signature in that verdict:
# {row: true|false}. A row with no signature, or one whose filter errs, is false: absent evidence is
# never a cause.
signatures_held() {
  local f="$1" owed="$2" held='{}' r sig h
  while IFS= read -r r; do
    sig="$(jq -r --arg r "$r" '.owed_signatures[$r] // ""' "$MANIFEST")"
    h=false
    if [ -n "$sig" ] && jq -e "$sig" "$f" >/dev/null 2>&1; then h=true; fi
    held="$(jq -c --arg r "$r" --argjson h "$h" '. + {($r): $h}' <<<"$held")"
  done < <(jq -r --argjson o "$owed" '[.failed[] | $o[.] // empty | .[]] | unique | .[]' "$f")
  printf '%s\n' "$held"
}

# The arguments after the first, joined by it; "none" when there are none.
join_by() {
  local sep="$1" out=""
  shift
  [ "$#" -gt 0 ] || { echo none; return 0; }
  out="$1"
  shift
  for x in "$@"; do out="$out$sep$x"; done
  printf '%s\n' "$out"
}

table() {
  local dir="$1" want="${2:-all}" selected n=0 red=0 not_runnable=0 swept=0 sweeps=0 passed=0
  local leg name case_ f status key measured row owed held tagged unowed unrun=() owed_reds=() unowed_reds=() rows_named=()
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
      pass)
        passed=$((passed + 1))
        echo "| $name | PASS | $row | ${key:--} | \`$measured\` |"
        ;;
      not_runnable)
        not_runnable=$((not_runnable + 1))
        unrun+=("$name")
        echo "| $name | NOT RUNNABLE | - | - | $(jq -r '.not_runnable_reason // ""' "$f" | cut -c1-160) |"
        ;;
      fail)
        red=$((red + 1))
        owed="$(owed_of "$(jq -r .leg <<<"$leg")")"
        held="$(signatures_held "$f" "$owed")"
        # Each failed check's row: the first of its rows whose signature held, or none.
        tagged="$(jq -c --argjson o "$owed" --argjson h "$held" '[.failed[] | . as $c | ($o[$c] // []) as $rows
          | {check: $c, rows: $rows, row: ([$rows[] | select($h[.] == true)] | first)}]' "$f")"
        echo "| $name | FAIL | $row | ${key:--} | failed: $(jq -r 'map(if .row != null then "\(.check) (Owed \(.row))"
          elif .rows == [] then "\(.check) (NOT OWED)"
          else "\(.check) (NOT OWED: \(.rows | map("Owed \(.)'"'"'s") | join(" and ")) signature absent)" end) | join(", ")' <<<"$tagged") |"
        unowed="$(jq -r 'map(select(.row == null) | .check) | if . == [] then "" else join(", ") end' <<<"$tagged")"
        if [ "$(jq 'length' <<<"$tagged")" -eq 0 ]; then unowed="no failed check named"; fi
        if [ -n "$unowed" ]; then unowed_reds+=("$name ($unowed)"); else owed_reds+=("$name"); fi
        while IFS= read -r r; do rows_named+=("$r"); done < <(jq -r '.[] | .row // empty' <<<"$tagged")
        ;;
      *)
        red=$((red + 1))
        echo "| $name | ERROR ($status) | $row | ${key:--} | the run ended before its verdict |"
        ;;
    esac
  done < <(jq -c '.[]' <<<"$selected")
  echo
  if [ $((${#owed_reds[@]} + ${#unowed_reds[@]})) -gt 0 ]; then
    echo "red, every failed check owed: $(join_by ", " "${owed_reds[@]}")"
    echo "red on a check no Owed row names: $(join_by "; " "${unowed_reds[@]}")"
    while IFS= read -r r; do
      echo "Owed $r: $(jq -r --arg r "$r" '.owed_titles[$r] // "(no title in chaos.json)"' "$MANIFEST")"
    done < <(printf '%s\n' "${rows_named[@]}" | sort -u -n)
    echo
  fi
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
  echo "legs: $n, pass: $passed, red: $red, not runnable: $not_runnable; the false-positive sweep ran on $swept of the $sweeps legs that call it"
  local ok=1
  if [ "$n" -eq 0 ]; then
    echo "FAIL no leg was selected"
    ok=0
  fi
  if [ "$swept" -ne "$sweeps" ]; then
    echo "FAIL the false-positive sweep ran on $swept of the $sweeps legs that call it"
    ok=0
  fi
  [ "$red" -eq 0 ] || ok=0
  # Which run this is (the re-review's m-7): a re-run attempt reads like a fresh run otherwise, and
  # the gate's three-run ledger has nothing to audit.
  local total attempt="${GITHUB_RUN_ATTEMPT:-}" head="${CHAOS_HEAD_SHA:-${GITHUB_SHA:-}}"
  total="$(legs all | jq length)"
  echo "run: attempt ${attempt:-none (not a CI run)}, head ${head:-unknown}, legs $n of $total"
  if [ "$ok" -eq 1 ] && [ "$not_runnable" -eq 0 ]; then
    # Only a whole first attempt counts (the re-review's I-2 and m-7): a selection of legs, or a
    # re-run of failed jobs, is green for what it ran and never a gate run.
    if [ "$n" -ne "$total" ]; then
      echo "WAVE GATE 3: a partial run ($n of $total legs) never counts ($passed of $n legs PASS)"
      return 0
    fi
    if [ "$attempt" != 1 ]; then
      echo "WAVE GATE 3: attempt ${attempt:-none} never counts: only a run's first attempt does ($passed of $n legs PASS)"
      return 0
    fi
    echo "WAVE GATE 3: this run counts ($passed of $n legs PASS)"
    return 0
  fi
  if [ "$not_runnable" -gt 0 ]; then
    echo "WAVE GATE 3: BLOCKED: $not_runnable NOT RUNNABLE (${unrun[*]}; Owed 82, the owner's ruling)"
  fi
  echo "WAVE GATE 3: this run does not count ($passed of $n legs PASS)"
  exit 1
}

case "${1:-}" in
  --legs) legs "${2:?--legs <all|leg,...>}" ;;
  --table) table "${2:?--table <dir> [<all|leg,...>]}" "${3:-all}" ;;
  *)
    echo "usage: run-chaos.sh --legs <all|leg,...> | --table <dir> [<all|leg,...>]" >&2
    exit 64
    ;;
esac
