#!/usr/bin/env bash
# A cost leg's verdict (C15): harness/soak/cost/cost.jq over one leg's files, with soak.json bound.
# soak.yml and test-cost.sh both call it, so the run and the fixtures are judged by one command.
#
#   harness/soak/cost/verdict.sh --dir <leg dir> --leg A|B|C|D --camera <picture> --guest <guest> [--jq <jq>]
#
# <leg dir> holds cost-samples.jsonl, cost-kill.jsonl, heartbeats.jsonl, actions.jsonl and
# deliveries.jsonl; a missing file reads as empty, so the check that needs it fails (never passes on
# nothing). Prints the verdict as JSON on stdout and a table on stderr. Exit 0 on pass, 1 on fail, 64
# on a usage error, 2 when jq itself failed.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
DIR=""
LEG=""
CAMERA=""
GUEST=""
JQ="jq"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dir) DIR="${2:?}"; shift 2 ;;
    --leg) LEG="${2:?}"; shift 2 ;;
    --camera) CAMERA="${2:?}"; shift 2 ;;
    --guest) GUEST="${2:?}"; shift 2 ;;
    --jq) JQ="${2:?}"; shift 2 ;;
    *) echo "cost verdict: unknown argument $1" >&2; exit 64 ;;
  esac
done
SOAK_JSON="$HERE/../soak.json"
[ -d "$DIR" ] || { echo "cost verdict: --dir names no directory" >&2; exit 64; }
jq -e --arg l "$LEG" '.legs | has($l)' "$SOAK_JSON" >/dev/null || { echo "cost verdict: --leg must be one of soak.json's legs" >&2; exit 64; }
jq -e --arg g "$GUEST" '.compositor_guest | has($g)' "$SOAK_JSON" >/dev/null || { echo "cost verdict: --guest must be one of soak.json's compositor_guest" >&2; exit 64; }
jq -e --arg c "$CAMERA" '.leg_cameras | index($c) != null' "$SOAK_JSON" >/dev/null || { echo "cost verdict: --camera must be one of soak.json's leg_cameras" >&2; exit 64; }

# A sample file can end on a cut line (the session dropped mid-write): only whole JSON objects are
# read, everything else (flyctl's own lines among them) dropped and counted.
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
objects() { # <name>: the file's lines that parse as JSON objects, into $W/<name>
  : >"$W/$1"
  [ -f "$DIR/$1" ] || return 0
  python3 -I -c '
import json, sys
bad = 0
with open(sys.argv[1], "rb") as f, open(sys.argv[2], "w") as out:
    for raw in f:
        try:
            v = json.loads(raw)
        except ValueError:
            bad += 1
            continue
        if isinstance(v, dict):
            out.write(json.dumps(v, separators=(",", ":")) + "\n")
        else:
            bad += 1
if bad:
    print("cost verdict: %d line(s) of %s not a JSON object: dropped" % (bad, sys.argv[1].rsplit("/", 1)[-1]), file=sys.stderr)
' "$DIR/$1" "$W/$1"
}
for f in cost-samples.jsonl cost-kill.jsonl heartbeats.jsonl actions.jsonl deliveries.jsonl; do objects "$f"; done

out="$("$JQ" -n --slurpfile samples "$W/cost-samples.jsonl" --slurpfile kills "$W/cost-kill.jsonl" \
  --slurpfile beats "$W/heartbeats.jsonl" --slurpfile actions "$W/actions.jsonl" --slurpfile deliveries "$W/deliveries.jsonl" \
  --slurpfile soak "$SOAK_JSON" --arg leg "$LEG" --arg camera "$CAMERA" --arg guest "$GUEST" -f "$HERE/cost.jq")" || {
  echo "cost verdict: jq failed" >&2
  exit 2
}
printf '%s\n' "$out"
"$JQ" -r '"cost leg \(.leg) (\(.output), \(.template) on \(.sport), camera \(.camera), \(.guest)): \(.status) (\(.checked) checks)",
  (.checks[] | "  \(.status | ascii_upcase) \(.id) \(.name)\(if .reason then ": \(.reason)" else "" end)  \(.measured | tojson)"),
  "  row \(.row | tojson)",
  "  owed93 \(.owed93 | tojson)"' <<<"$out" >&2
[ "$("$JQ" -r '.status' <<<"$out")" = pass ]
