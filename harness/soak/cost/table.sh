#!/usr/bin/env bash
# The cost legs' table (C15, F-0xx "real overlay cost"): F-005's table shape, one column per leg, from
# each leg's verdict (verdict.sh's JSON). soak.yml writes it to the run's summary, a column per leg
# job and the whole table from the cost-table job.
#
#   harness/soak/cost/table.sh <verdict.json>...
#
# Prints markdown on stdout; a file that is not a verdict is named on stderr and left out. Exit 1
# when none is.
set -euo pipefail
[ "$#" -gt 0 ] || { echo "table: no verdict file" >&2; exit 64; }
W="$(mktemp)"
trap 'rm -f "$W"' EXIT
for f in "$@"; do
  if jq -e 'type == "object" and has("leg") and has("row") and has("checks")' "$f" >/dev/null 2>&1; then
    jq -c . "$f" >>"$W"
  else
    echo "table: $f is not a cost verdict: left out" >&2
  fi
done
[ -s "$W" ] || { echo "table: no cost verdict among the files" >&2; exit 1; }
jq -r -s '
  def n: if . == null then "—" else tostring end;
  def m($id): (.checks[] | select(.id == $id) | .measured);
  sort_by(.leg, .camera) as $v
  | def line($label; f): "| \($label) | " + ([$v[] | f] | join(" | ")) + " |";
  line("Leg"; "**\(.leg)** \(.output) \(.template) on \(.sport), \(.camera), \(.guest)"),
  "|---|" + ([$v[] | "---"] | join("|")) + "|",
  line("**Machine cores avg / p95 / max (of N)**"; "**\(.row.machine.avg | n) / \(.row.machine.p95 | n) / \(.row.machine.max | n) (of \(.cpus); p95 \(.row.machine.p95_pct | n) %)**"),
  line("Process tree cores avg"; .row.tree_avg | n),
  line("…compositor (GStreamer host, CEF browser)"; .row.cats_avg.compositor | n),
  line("…CEF renderer + gpu + utility + zygote"; "\(.row.cats_avg.renderer | n) + \(.row.cats_avg.gpu | n) + \(.row.cats_avg.utility | n) + \(.row.cats_avg.zygote | n)"),
  line("…Xvfb"; .row.cats_avg.xvfb | n),
  line("The sampler itself / the rest"; "\(.row.cats_avg.self | n) / \(.row.cats_avg.other | n)"),
  line("Busiest compositor thread, p95 / max"; if .row.top_thread == null then "—" else "\(.row.top_thread.name) \(.row.top_thread.p95 | n) / \(.row.top_thread.max | n)" end),
  line("Encoder speed, median / worst beat"; "\(.row.encoder_speed.median | n) / \(.row.encoder_speed.min | n)"),
  line("Output fps, median beat"; .row.fps_median | n),
  line("Final droppedFrames"; .row.dropped_final | n),
  line("Rungs (beats)"; .row.rungs | if . == null then "—" else to_entries | map("\(.key) \(.value)") | join(", ") end),
  line("Moments (by the page'"'"'s feed), a minute"; "\(.row.moments.n | n), \(.row.moments.per_min | n)/min"),
  line("Renderer kill, s into the drive / graphics ready after, s"; m("reload") | "\(.kill_at_s | n) / \(.ready_after_s | n)"),
  line("Machine memory used, max MB"; .row.mem_used_max_mb | n),
  line("Samples / expected"; "\(.row.samples.n) / \(.row.samples.expected)"),
  line("Owed 93: p95 + two held pages (2 × renderer) vs the size rule"; if .owed93 == null then "—" else "\(.owed93.p95_plus_two_held) vs \(.owed93.size_rule_max) (\(if .owed93.within then "within" else "OVER" end))" end),
  line("**Size rule verdict**"; if .status == "pass" then "**PASS**" else "**FAIL**: " + ([.checks[] | select(.status == "fail") | .id] | join(", ")) end)
' "$W"
