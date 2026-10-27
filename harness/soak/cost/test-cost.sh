#!/usr/bin/env bash
# The cost legs' own parts (C15), off Fly:
#
# - in-compositor.sh's sampler on three fake /proc trees: the whole Machine from /proc/stat, each
#   category of the tree, the renderer by its client id, the compositor's busiest thread, memory,
#   a process that died, one whose pid was reused, and nothing printed for the first read; the
#   expected cores are worked by hand from the trees' jiffies (CPU-seconds over wall seconds);
# - its kill-renderer on this test's own sleeps: the newest renderer (the highest client id) dies,
#   the other renderers and the GPU process live, a second call repeats the first from its mark and
#   kills nothing, and a tree with no numbered renderer fails; never before its not_before by its own
#   clock (a call that lands early waits, up to 10 s: run 40153136892's kill 80 ms early, through
#   the verdict), and one more than 10 s early is refused;
# - leg.sh on a stub flyctl that runs the script it was sent: the script sent after a refused try,
#   the sampler restarted after its session drops, one kill at the drive's midpoint (an unreachable
#   try is tried again, a reached one never; a try sent before the midpoint held on the Machine
#   until it), and an exit once the compositor Machine is gone;
# - the verdict (verdict.sh, cost.jq) on fixture.py's green leg, worked by hand, and the reds: one or
#   more per check (every guard's own), each failing that check alone, by its reason; the reload
#   judged from the compositor's own log; and table.sh over two verdicts;
#
#   HARNESS_BASH=<bash 5> harness/soak/cost/test-cost.sh        (in-compositor.sh needs bash 5)
#
# Needs jq. Prints `checked: N` and fails on zero.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
BASH5="${HARNESS_BASH:-bash}"
"$BASH5" -c '[ "${BASH_VERSINFO[0]}" -ge 5 ]' || { echo "FAIL test-cost.sh needs bash 5 (HARNESS_BASH)"; exit 1; }
IN="${COST_IN_COMPOSITOR:-$HERE/in-compositor.sh}"
W="$(mktemp -d)"
kids=()
cleanup() {
  for p in "${kids[@]:-}"; do [ -z "$p" ] || kill -KILL "$p" 2>/dev/null || true; done
  rm -rf "$W"
}
trap cleanup EXIT

checked=0
failures=0
pass() { checked=$((checked + 1)); echo "PASS $1"; }
fail() { checked=$((checked + 1)); failures=$((failures + 1)); echo "FAIL $1${2:+: $2}"; }
# expect <name> <file> <jq filter, true to pass>
expect() {
  if jq -e -s "$3" "$2" >/dev/null 2>&1; then pass "$1"; else fail "$1" "$(head -c 600 "$2")"; fi
}

# --- the sampler -------------------------------------------------------------------------------
# proc <tree> <pid> <comm> <utime> <stime> <start> [cmdline with \0]
proc() {
  local d="$1/$2"
  mkdir -p "$d/task"
  printf '%s\n' "$3" >"$d/comm"
  # pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt utime stime
  # cutime cstime priority nice threads itrealvalue starttime ...
  printf '%s (%s) S 1 1 1 0 -1 0 0 0 0 0 %s %s 0 0 20 0 1 0 %s 0 0\n' "$2" "$3" "$4" "$5" "$6" >"$d/stat"
  printf '%b' "${7:-}" >"$d/cmdline"
}
# thread <tree> <pid> <tid> <comm> <ticks>
thread() {
  local d="$1/$2/task/$3"
  mkdir -p "$d"
  printf '%s\n' "$4" >"$d/comm"
  printf '%s (%s) S 1 1 1 0 -1 0 0 0 0 0 %s 0 0 0 20 0 1 0 50 0 0\n' "$3" "$4" "$5" >"$d/stat"
}
machine() { # <tree> <now_ms> <user> <system> <idle> <iowait> <mem_avail_kb>
  mkdir -p "$1"
  echo "$2" >"$1/now_ms"
  {
    echo "cpu  $3 0 $4 $5 $6 0 0 0 0 0"
    for c in 0 1 2 3; do echo "cpu$c 1 0 1 1 0 0 0 0 0 0"; done
    echo "intr 0"
    echo "ctxt 0"
  } >"$1/stat"
  printf 'MemTotal:        8000000 kB\nMemFree:          100 kB\nMemAvailable:    %s kB\n' "$7" >"$1/meminfo"
}
T="$W/seq"
# Read 0 (t 1000 ms): the baseline.
machine "$T/0" 1000 1000 500 8000 100 7000000
proc "$T/0" 1 init 10 0 1
proc "$T/0" 100 compositor 30 20 50
thread "$T/0" 100 100 compositor 50
thread "$T/0" 100 101 'x264enc:src' 100
thread "$T/0" 100 102 'cef (main)' 20
proc "$T/0" 200 cefsrc 20 10 60 '--type=renderer\0--renderer-client-id=7\0'
proc "$T/0" 201 cefsrc 15 5 60 '--type=gpu-process\0'
proc "$T/0" 202 cefsrc 5 0 60 '--type=zygote\0'
proc "$T/0" 203 'chrome' 1 0 60 '/opt/cef/chrome --type=utility --utility-sub-type=network\0\0\0\0'
proc "$T/0" 300 Xvfb 30 10 20
proc "$T/0" 400 bash 2 0 70
# Read 1 (t 2000 ms): busy +200 of +400 jiffies on 4 CPUs = 2.000; the compositor +80 = 0.800 (its
# threads +10, +60, +10: x264enc:src the busiest, 0.600); the renderer +12 = 0.120; the GPU +5;
# the zygote +0; the utility +1; Xvfb +3; the sampler (400) +2; init +1 (other).
machine "$T/1" 2000 1150 550 8190 110 7000000
proc "$T/1" 1 init 11 0 1
proc "$T/1" 100 compositor 90 40 50
thread "$T/1" 100 100 compositor 60
thread "$T/1" 100 101 'x264enc:src' 160
thread "$T/1" 100 102 'cef (main)' 30
proc "$T/1" 200 cefsrc 30 12 60 '--type=renderer\0--renderer-client-id=7\0'
proc "$T/1" 201 cefsrc 20 5 60 '--type=gpu-process\0'
proc "$T/1" 202 cefsrc 5 0 60 '--type=zygote\0'
proc "$T/1" 203 'chrome' 2 0 60 '/opt/cef/chrome --type=utility --utility-sub-type=network\0\0\0\0'
proc "$T/1" 300 Xvfb 33 10 20
proc "$T/1" 400 bash 4 0 70
# Read 2 (t 3000 ms): busy +100 of +400 = 1.000; the renderer 200 is gone and a new one (210, the
# same client id, started within the interval) counts its whole 9 jiffies; pid 300 is reused by a
# new Xvfb (start 2500 ticks) whose 4 jiffies count whole, never 4 less the old 43.
machine "$T/2" 3000 1220 580 8480 120 6000000
proc "$T/2" 1 init 11 0 1
proc "$T/2" 100 compositor 120 50 50
thread "$T/2" 100 100 compositor 70
thread "$T/2" 100 101 'x264enc:src' 170
thread "$T/2" 100 102 'cef (main)' 50
proc "$T/2" 210 cefsrc 6 3 250 '--type=renderer\0--renderer-client-id=7\0'
proc "$T/2" 201 cefsrc 20 5 60 '--type=gpu-process\0'
proc "$T/2" 202 cefsrc 5 0 60 '--type=zygote\0'
proc "$T/2" 203 'chrome' 2 0 60 '/opt/cef/chrome --type=utility --utility-sub-type=network\0\0\0\0'
proc "$T/2" 300 Xvfb 4 0 2500
proc "$T/2" 400 bash 5 0 70

rc=0
COST_PROC_SEQ="$T" COST_CLK_TCK=100 COST_SELF_PID=400 "$BASH5" "$IN" sample 60 >"$W/samples.jsonl" 2>"$W/samples.err" || rc=$?
[ "$rc" -eq 0 ] && pass "sample: exit 0 at the end of the trees" || fail "sample: exit $rc" "$(head -c 300 "$W/samples.err")"
expect "sample: two lines (the first read prints nothing)" "$W/samples.jsonl" 'length == 2'
expect "sample 1: the Machine, 200 busy of 400 jiffies on 4 CPUs = 2.000 cores, dt 1000 ms" "$W/samples.jsonl" \
  '.[0] | .t_ms == 2000 and .dt_ms == 1000 and .ncpu == 4 and .machine == 2'
expect "sample 1: each category, by hand" "$W/samples.jsonl" \
  '.[0].cats == {compositor: 0.8, renderer: 0.12, gpu: 0.05, utility: 0.01, zygote: 0, cef_other: 0, xvfb: 0.03, self: 0.02, other: 0.01}'
expect "sample 1: the tree is the compositor, its CEF children and Xvfb (1.010), never the sampler or init" "$W/samples.jsonl" \
  '.[0].tree == 1.01'
expect "sample 1: the renderer by pid and client id" "$W/samples.jsonl" '.[0].renderers == [{pid: 200, client: 7, cores: 0.12}]'
expect "sample 1: the busiest compositor thread (x264enc:src, 0.600)" "$W/samples.jsonl" \
  '.[0].top_thread == {name: "x264enc:src", cores: 0.6}'
expect "sample 1: memory used (MemTotal less MemAvailable) 976 MB, 8 processes" "$W/samples.jsonl" \
  '.[0].mem_used_mb == 976 and .[0].procs == 8'
expect "sample 2: the Machine 1.000; a new renderer counts its whole time (0.090)" "$W/samples.jsonl" \
  '.[1] | .machine == 1 and .renderers == [{pid: 210, client: 7, cores: 0.09}] and .cats.renderer == 0.09'
expect "sample 2: a reused pid (a new start time) counts its whole time (Xvfb 0.040, never negative)" "$W/samples.jsonl" \
  '.[1].cats.xvfb == 0.04'
expect "sample 2: the compositor +40 = 0.400, its busiest thread now 'cef (main)' (+20)" "$W/samples.jsonl" \
  '.[1].cats.compositor == 0.4 and .[1].top_thread == {name: "cef (main)", cores: 0.2} and .[1].mem_used_mb == 1953'
# The real clock, on one unchanging tree: a line a second (the FIFO's timed read is the sleep), and
# the run ends at max_s.
rc=0
t_start=$(date +%s)
COST_PROC="$T/0" COST_CLK_TCK=100 "$BASH5" "$IN" sample 3 >"$W/cadence.jsonl" 2>"$W/cadence.err" || rc=$?
took=$(($(date +%s) - t_start))
# The reads keep to the first read's whole seconds (a late one is followed by a shorter wait, never
# a drift), so each line's time is the first line's plus a whole number of seconds, within 250 ms
# (a loaded runner's jitter; the 600 ms of a missed second would fail).
if jq -e -s 'length >= 2 and length <= 3 and all(.machine == 0 and .tree == 0 and .dt_ms > 0)
    and (.[0].t_ms as $f | to_entries | all(((.value.t_ms - $f) - .key * 1000) | fabs <= 250))' "$W/cadence.jsonl" >/dev/null 2>&1; then
  pass "sample on the real clock: $(jq -s length "$W/cadence.jsonl") lines a second apart on the first read's grid (dt $(jq -s -c 'map(.dt_ms)' "$W/cadence.jsonl") ms), zero cores on an unchanging tree"
else
  fail "sample on the real clock: the cadence" "$(jq -s -c 'map({t_ms, dt_ms, machine, tree})' "$W/cadence.jsonl" 2>/dev/null)"
fi
[ "$rc" -eq 0 ] && [ "$took" -ge 2 ] && [ "$took" -le 5 ] && pass "sample 3: exit 0 after about 3 s ($took s)" \
  || fail "sample 3" "exit $rc after $took s: $(head -c 300 "$W/cadence.err")"
rc=0
"$BASH5" "$IN" sample >"$W/usage.out" 2>&1 || rc=$?
[ "$rc" -eq 64 ] && grep -qF '"error"' "$W/usage.out" && pass "sample without max_s: usage, exit 64" || fail "sample without max_s" "exit $rc"

# --- the renderer kill ---------------------------------------------------------------------------
# The newest page's renderer only (the highest client id, as C14's in-container.sh: client ids only
# grow), once: a second call (leg.sh's retry when a reply was lost) repeats the first call's lines
# from the mark and kills nothing.
K="$W/kill"
sleep 300 & r1=$!
sleep 300 & r2=$!
sleep 300 & r3=$!
sleep 300 & g=$!
kids+=("$r1" "$r2" "$r3" "$g")
mkdir -p "$K/0"
# The highest client id on the LOWER pid, so a pick by listing order (the last one read) is wrong.
proc "$K/0" "$r1" cefsrc 1 0 60 '/opt/cef/chrome --type=renderer --renderer-client-id=9\0\0'
proc "$K/0" "$r2" cefsrc 1 0 60 '--type=renderer\0--renderer-client-id=7\0'
proc "$K/0" "$r3" cefsrc 1 0 60 '--type=renderer\0'
proc "$K/0" "$g" cefsrc 1 0 60 '--type=gpu-process\0'
echo 5000 >"$K/0/now_ms"
MARK="$W/kill-mark.jsonl"
rc=0
COST_PROC_SEQ="$K" COST_KILL_MARK="$MARK" "$BASH5" "$IN" kill-renderer 5000 >"$W/kill.out" 2>&1 || rc=$?
sleep 0.3
alive() { kill -0 "$1" 2>/dev/null; }
[ "$rc" -eq 0 ] && pass "kill-renderer: exit 0" || fail "kill-renderer: exit $rc" "$(cat "$W/kill.out")"
if jq -e -s --argjson a "$r1" --argjson b "$r2" --argjson c "$r3" 'length == 2 and .[0].at_ms == 5000
     and .[0].killed == [$a] and .[0].clients == [9]
     and (.[0].renderers | sort_by(.pid)) == ([{pid: $a, client: 9}, {pid: $b, client: 7}, {pid: $c, client: null}] | sort_by(.pid))
     and .[1].rc == 0' "$W/kill.out" >/dev/null; then
  pass "kill-renderer: the highest client id's renderer (9 of 7, 9, none) by pid, every renderer seen, then rc 0"
else
  fail "kill-renderer: the target" "$(cat "$W/kill.out")"
fi
if ! alive "$r1" && alive "$r2" && alive "$r3" && alive "$g"; then
  pass "kill-renderer: that renderer dead; the other renderers and the GPU process alive"
else
  fail "kill-renderer: who died" "r1 $(alive "$r1" && echo alive || echo dead), r2 $(alive "$r2" && echo alive || echo dead), r3 $(alive "$r3" && echo alive || echo dead), gpu $(alive "$g" && echo alive || echo dead)"
fi
cmp -s <(head -n 2 "$W/kill.out") "$MARK" && pass "kill-renderer: its lines kept in the mark" || fail "kill-renderer: the mark" "$(cat "$MARK" 2>/dev/null)"
# The retry: the reply was lost, the call is made again on a tree whose newest renderer is r2 now.
rm -rf "${K:?}/0/$r1"
echo 9000 >"$K/0/now_ms"
rc=0
COST_PROC_SEQ="$K" COST_KILL_MARK="$MARK" "$BASH5" "$IN" kill-renderer 9000 >"$W/kill-again.out" 2>&1 || rc=$?
sleep 0.3
if [ "$rc" -eq 0 ] && cmp -s <(head -n 2 "$W/kill-again.out") "$MARK" && jq -e -s '.[2] == {repeat_at_ms: 9000} and length == 3' "$W/kill-again.out" >/dev/null \
  && alive "$r2" && alive "$r3"; then
  pass "kill-renderer again: the first call's lines repeated from the mark, a repeat line, nothing killed (one kill a leg)"
else
  fail "kill-renderer again" "exit $rc, $(cat "$W/kill-again.out"); r2 $(alive "$r2" && echo alive || echo dead)"
fi
rm -rf "${K:?}/0/$r2"
rc=0
COST_PROC_SEQ="$K" COST_KILL_MARK="$W/kill-mark2.jsonl" "$BASH5" "$IN" kill-renderer 9000 >"$W/kill2.out" 2>&1 || rc=$?
if [ "$rc" -ne 0 ] && jq -e -s '.[0].killed == [] and .[0].renderers == [{pid: '"$r3"', client: null}] and .[1].rc == 1' "$W/kill2.out" >/dev/null \
  && alive "$r3" && alive "$g" && [ ! -e "$W/kill-mark2.jsonl" ] && [ ! -e "$W/kill-mark2.jsonl.claim" ]; then
  pass "kill-renderer with no renderer that has a client id: fails, rc 1, kills nothing, leaves no mark or claim"
else
  fail "kill-renderer with no numbered renderer" "exit $rc, $(cat "$W/kill2.out")"
fi

# Never before not_before by the Machine's own clock (the clock at_ms is read from, so the verdict's
# at_ms - planned_ms cannot be negative): run 40153136892's A:noise leg went red on a kill 80 ms
# before the midpoint (leg.sh's wait ended up to a second early, on the runner's clock). Here the
# call lands at P - 80 by the Machine's clock (tree 0) and waits; the next read (tree 1) is P + 4.
# Its lines, through the verdict on fixture.py's green leg, pass the reload check.
P=1000300000 # fixture.py's midpoint: S + DRIVE * 500
KE="$W/kill-early"
sleep 300 & re=$!
kids+=("$re")
for t in 0 1; do mkdir -p "$KE/$t"; proc "$KE/$t" "$re" cefsrc 1 0 60 '--type=renderer\0--renderer-client-id=5\0'; done
echo $((P - 80)) >"$KE/0/now_ms"
echo $((P + 4)) >"$KE/1/now_ms"
rc=0
COST_PROC_SEQ="$KE" COST_KILL_MARK="$W/kill-early-mark.jsonl" "$BASH5" "$IN" kill-renderer "$P" >"$W/kill-early.out" 2>&1 || rc=$?
sleep 0.3
if [ "$rc" -eq 0 ] && jq -e -s --argjson p "$P" --argjson r "$re" 'length == 2 and .[0].at_ms == $p + 4 and .[0].killed == [$r] and .[1].rc == 0' "$W/kill-early.out" >/dev/null \
  && ! alive "$re"; then
  pass "kill-renderer landing 80 ms before not_before by its own clock: waits, kills at P + 4 (the next read)"
else
  fail "kill-renderer 80 ms early" "exit $rc, $(cat "$W/kill-early.out")"
fi
python3 -I "$HERE/fixture.py" "$W/legs/kill-early-driven"
jq -c --argjson p "$P" '. + {planned_ms: $p, try: 1}' "$W/kill-early.out" >"$W/legs/kill-early-driven/cost-kill.jsonl"
rc=0
"$HERE/verdict.sh" --dir "$W/legs/kill-early-driven" --leg A --camera snow --guest performance-4x >"$W/kill-early-driven.json" 2>/dev/null || rc=$?
if [ "$rc" -eq 0 ] && jq -e '.status == "pass" and (.checks[] | select(.id == "reload") | .status == "pass" and .measured.kill_at_s == 300.004)' "$W/kill-early-driven.json" >/dev/null; then
  pass "verdict on that kill's own lines: the reload check passes, the kill 300.004 s in (never -0.08 s from the midpoint)"
else
  fail "verdict on the 80 ms-early landing" "exit $rc, $(jq -c '[.checks[] | select(.status == "fail") | {id, reason}]' "$W/kill-early-driven.json" 2>/dev/null)"
fi
# The wait is bounded: a call that lands more than 10 s early (the caller's clock is off) is refused
# and kills nothing, so it never waits out flyctl's 30 s exec timeout into a retry's second kill.
# One that lands exactly 10 s early waits.
KF="$W/kill-far"
mkdir -p "$KF/0" "$KF/1"
sleep 300 & rf=$!
kids+=("$rf")
for t in 0 1; do proc "$KF/$t" "$rf" cefsrc 1 0 60 '--type=renderer\0--renderer-client-id=6\0'; done
echo $((P - 10001)) >"$KF/0/now_ms"
echo $((P + 1)) >"$KF/1/now_ms"
rc=0
COST_PROC_SEQ="$KF" COST_KILL_MARK="$W/kill-far-mark.jsonl" "$BASH5" "$IN" kill-renderer "$P" >"$W/kill-far.out" 2>&1 || rc=$?
sleep 0.3
if [ "$rc" -eq 1 ] && jq -e -s --argjson p "$P" 'length == 1 and .[0] == {early_at_ms: ($p - 10001), not_before_ms: $p, killed: []}' "$W/kill-far.out" >/dev/null \
  && alive "$rf" && [ ! -e "$W/kill-far-mark.jsonl" ] && [ ! -e "$W/kill-far-mark.jsonl.claim" ]; then
  pass "kill-renderer landing 10.001 s early: refused (early_at_ms), exit 1, nothing killed, no mark, its claim released"
else
  fail "kill-renderer 10.001 s early" "exit $rc, $(cat "$W/kill-far.out")"
fi
echo $((P - 10000)) >"$KF/0/now_ms"
rc=0
COST_PROC_SEQ="$KF" COST_KILL_MARK="$W/kill-far-mark.jsonl" "$BASH5" "$IN" kill-renderer "$P" >"$W/kill-far2.out" 2>&1 || rc=$?
sleep 0.3
if [ "$rc" -eq 0 ] && jq -e -s --argjson p "$P" --argjson r "$rf" '.[0].at_ms == $p + 1 and .[0].killed == [$r]' "$W/kill-far2.out" >/dev/null && ! alive "$rf"; then
  pass "kill-renderer landing exactly 10 s early: waits, kills at P + 1"
else
  fail "kill-renderer 10 s early" "exit $rc, $(cat "$W/kill-far2.out")"
fi
rc=0
COST_PROC_SEQ="$KE" COST_KILL_MARK="$W/kill-usage.jsonl" "$BASH5" "$IN" kill-renderer >"$W/kill-usage.out" 2>&1 || rc=$?
rc2=0
COST_PROC_SEQ="$KE" COST_KILL_MARK="$W/kill-usage.jsonl" "$BASH5" "$IN" kill-renderer 12x >>"$W/kill-usage.out" 2>&1 || rc2=$?
[ "$rc" -eq 64 ] && [ "$rc2" -eq 64 ] && [ "$(grep -c '"error"' "$W/kill-usage.out")" -eq 2 ] \
  && pass "kill-renderer without a numeric not_before: usage, exit 64 (twice)" || fail "kill-renderer usage" "exit $rc and $rc2: $(cat "$W/kill-usage.out")"
# The test clock: a wait with no later tree is the test's own fault, named, never a kill.
KN="$W/kill-notree"
mkdir -p "$KN/0"
echo $((P - 5)) >"$KN/0/now_ms"
rc=0
COST_PROC_SEQ="$KN" COST_KILL_MARK="$W/kill-notree.jsonl" "$BASH5" "$IN" kill-renderer "$P" >"$W/kill-notree.out" 2>&1 || rc=$?
[ "$rc" -eq 70 ] && grep -qF '"error"' "$W/kill-notree.out" && [ ! -e "$W/kill-notree.jsonl" ] && [ ! -e "$W/kill-notree.jsonl.claim" ] \
  && pass "kill-renderer (test clock) waiting with no later tree: exit 70, named" || fail "kill-renderer with no later tree" "exit $rc: $(cat "$W/kill-notree.out")"

# One kill a leg while a call waits: the first call claims the kill (mkdir "$mark.claim") before it
# waits for not_before, so a retry that lands during that wait (try 1's reply lost while it still
# waits) waits for the first call's lines and repeats them, never a second kill. On the real clock:
# the first call waits 1.5 s, the second lands 0.3 s after it.
KR="$W/kill-race"
sleep 300 & rr=$!
kids+=("$rr")
proc "$KR" "$rr" cefsrc 1 0 60 '--type=renderer\0--renderer-client-id=2\0'
nb=$(($(python3 -I -c 'import time; print(int(time.time() * 1000))') + 1500))
COST_PROC="$KR" COST_KILL_MARK="$W/kill-race.jsonl" "$BASH5" "$IN" kill-renderer "$nb" >"$W/kill-race-1.out" 2>&1 &
ka=$!
sleep 0.3
COST_PROC="$KR" COST_KILL_MARK="$W/kill-race.jsonl" "$BASH5" "$IN" kill-renderer "$nb" >"$W/kill-race-2.out" 2>&1 &
kb=$!
rca=0; wait "$ka" || rca=$?
rcb=0; wait "$kb" || rcb=$?
if [ "$rca" -eq 0 ] && [ "$rcb" -eq 0 ] && jq -e -s --argjson nb "$nb" --argjson r "$rr" 'length == 2 and .[0].at_ms >= $nb and .[0].killed == [$r] and .[1].rc == 0' "$W/kill-race-1.out" >/dev/null \
  && cmp -s "$W/kill-race-1.out" <(head -n 2 "$W/kill-race-2.out") && jq -e -s 'length == 3 and (.[2] | keys) == ["repeat_at_ms"]' "$W/kill-race-2.out" >/dev/null \
  && ! alive "$rr"; then
  pass "kill-renderer: a second call landing during the first's wait repeats the first's lines (one kill, the claim held)"
else
  fail "kill-renderer: a call during the first's wait" "exit $rca/$rcb; first $(cat "$W/kill-race-1.out"); second $(cat "$W/kill-race-2.out")"
fi
# A claim whose lines never come (a first call that died holding it) is waited for at most 15 s
# (the first call's 10 s wait, its scan and its kill), then refused: nothing killed, no at_ms.
KB="$W/kill-busy"
sleep 300 & rb=$!
kids+=("$rb")
for t in 0 1; do mkdir -p "$KB/$t"; proc "$KB/$t" "$rb" cefsrc 1 0 60 '--type=renderer\0--renderer-client-id=2\0'; done
echo "$P" >"$KB/0/now_ms"
echo $((P + 15001)) >"$KB/1/now_ms"
mkdir "$W/kill-busy.jsonl.claim"
rc=0
COST_PROC_SEQ="$KB" COST_KILL_MARK="$W/kill-busy.jsonl" "$BASH5" "$IN" kill-renderer "$P" >"$W/kill-busy.out" 2>&1 || rc=$?
sleep 0.3
if [ "$rc" -eq 1 ] && jq -e -s --argjson p "$P" 'length == 1 and .[0] == {busy_at_ms: ($p + 15001), killed: []}' "$W/kill-busy.out" >/dev/null \
  && alive "$rb" && [ ! -e "$W/kill-busy.jsonl" ] && [ -d "$W/kill-busy.jsonl.claim" ]; then
  pass "kill-renderer finding another's claim and no lines for 15.001 s: refused (busy_at_ms), exit 1, nothing killed, the claim left to its holder"
else
  fail "kill-renderer with a claim and no lines" "exit $rc, $(cat "$W/kill-busy.out")"
fi

# --- leg.sh, on a stub flyctl ---------------------------------------------------------------------
# The stub keeps its state under $FS: the script it was sent, each sampler session (the first drops
# after two lines, the second runs until the Machine is gone), each kill (the first unreachable),
# and the Machine's state (destroyed once $FS/gone exists). A kill that reaches the Machine runs the
# script leg.sh sent, with leg.sh's arguments, on $FS/proc (this test's own sleep as the renderer)
# and the real clock: the one leg.sh's now_ms reads, as two Machines' synced clocks would be.
FS="$W/fly"
mkdir -p "$FS/bin"
cat >"$FS/bin/flyctl" <<'STUB'
#!/usr/bin/env bash
FS="${STUB_FS:?}"
ms() { python3 -I -c 'import time; print(int(time.time() * 1000))'; }
echo "$*" >>"$FS/calls"
case "$1 $2" in
  "ssh sftp")
    n=$(cat "$FS/sftp-fail" 2>/dev/null || echo 0)
    if [ "$n" -gt 0 ]; then echo $((n - 1)) >"$FS/sftp-fail"; echo "sftp: not yet" >&2; exit 1; fi
    cp "$4" "$FS/sent-script"; echo "$5" >"$FS/sent-to" ;;
  "ssh console")
    n=$(($(cat "$FS/consoles" 2>/dev/null || echo 0) + 1)); echo "$n" >"$FS/consoles"
    for a in "$@"; do last="$a"; done
    echo "$last" >>"$FS/console-commands"
    if [ "$n" -eq 1 ]; then echo "{\"t_ms\":$(ms),\"session\":1}"; echo "{\"t_ms\":$(ms),\"session\":1}"; exit 1; fi
    while [ ! -f "$FS/gone" ]; do echo "{\"t_ms\":$(ms),\"session\":$n}"; sleep 0.5; done ;;
  "machine exec")
    case "$3 $4" in
      "h1 head -n 1 /out/actions.jsonl") cat "$FS/actions-first" 2>/dev/null || true ;;
      "c1 bash /tmp/seazn-cost.sh kill-renderer"*)
        n=$(cat "$FS/kill-unreachable" 2>/dev/null || echo 0)
        if [ "$n" -gt 0 ]; then echo $((n - 1)) >"$FS/kill-unreachable"; echo "Error: timeout" >&2; exit 1; fi
        echo "$4" >>"$FS/kill-commands"
        echo "$(ms)" >>"$FS/kill-landed"
        # shellcheck disable=SC2086 # leg.sh's arguments, split as the Machine's shell splits them
        COST_PROC="$FS/proc" COST_KILL_MARK="$FS/kill-mark.jsonl" "${STUB_BASH:?}" "$FS/sent-script" ${4#bash /tmp/seazn-cost.sh }
        krc=$?
        echo "$(ms)" >>"$FS/kills"
        exit "$krc" ;;
      *) echo "unexpected exec $3 $4" >&2; exit 1 ;;
    esac ;;
  "machine list")
    if [ -f "$FS/gone" ]; then echo '[{"id":"c1","state":"destroyed"},{"id":"h1","state":"started"}]'
    else echo '[{"id":"c1","state":"started"},{"id":"h1","state":"started"}]'; fi ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
STUB
chmod +x "$FS/bin/flyctl"
ms() { python3 -I -c 'import time; print(int(time.time() * 1000))'; }
echo 1 >"$FS/sftp-fail"
echo 1 >"$FS/kill-unreachable"
sleep 300 & lr=$!
kids+=("$lr")
proc "$FS/proc" "$lr" cefsrc 1 0 60 '--type=renderer\0--renderer-client-id=3\0'
L="$W/leg-out"
t0=$(ms)
start_at=$((t0 + 1000))
rc=0
PATH="$FS/bin:$PATH" STUB_FS="$FS" STUB_BASH="$BASH5" LEG_POLL_S=1 LEG_KILL_TRIES=3 \
  "$BASH5" "$HERE/leg.sh" --app soak --harness h1 --compositor c1 --out "$L" --until-ms $((t0 + 30000)) &
leg=$!
kids+=("$leg")
sleep 1
echo "{\"kind\":\"start\",\"at_ms\":$start_at,\"drive_s\":4}" >"$FS/actions-first"
# The Machine goes once the kill is in (the midpoint is 2 s into the 4 s drive) and a second has
# passed.
for _ in $(seq 1 100); do [ -s "$FS/kills" ] && break; sleep 0.1; done
sleep 1
touch "$FS/gone"
for _ in $(seq 1 100); do kill -0 "$leg" 2>/dev/null || break; sleep 0.1; done
if kill -0 "$leg" 2>/dev/null; then fail "leg.sh: still running 10 s after the Machine went"; kill "$leg"; else
  wait "$leg" || rc=$?
  [ "$rc" -eq 0 ] && pass "leg.sh: exits 0 once the compositor Machine is gone" || fail "leg.sh: exit $rc" "$(tail -n 5 "$L/cost-leg.log")"
fi
cmp -s "$FS/sent-script" "$IN" && [ "$(cat "$FS/sent-to")" = /tmp/seazn-cost.sh ] \
  && pass "leg.sh: in-compositor.sh sent to /tmp/seazn-cost.sh, after one refused try" \
  || fail "leg.sh: the script sent" "$(cat "$FS/sent-to" 2>/dev/null)"
[ "$(cat "$FS/consoles")" -ge 2 ] && grep -qE '^bash /tmp/seazn-cost.sh sample [0-9]+$' "$FS/console-commands" \
  && pass "leg.sh: the sampler restarted after its session dropped ($(cat "$FS/consoles") sessions)" \
  || fail "leg.sh: sampler sessions" "$(cat "$FS/consoles" 2>/dev/null)"
expect "leg.sh: both sessions' lines in cost-samples.jsonl" "$L/cost-samples.jsonl" \
  '(map(select(.session == 1)) | length) == 2 and (map(select(.session == 2)) | length) >= 2'
[ "$(grep -c . "$FS/kills")" -eq 1 ] && pass "leg.sh: one kill reached the Machine (the first try was unreachable)" \
  || fail "leg.sh: kills" "$(grep -c . "$FS/kills")"
if jq -e -s --argjson p $((start_at + 2000)) --argjson r "$lr" \
    'length == 2 and all(.planned_ms == $p and .try == 2) and .[0].at_ms >= $p and .[0].killed == [$r] and .[1].rc == 0' \
    "$L/cost-kill.jsonl" >/dev/null && ! alive "$lr" \
  && [ "$(sort -u "$FS/kill-commands")" = "bash /tmp/seazn-cost.sh kill-renderer $((start_at + 2000))" ]; then
  pass "leg.sh: the kill at the midpoint or after by the Machine's clock, kill-renderer <planned_ms>, its lines kept with planned_ms and try 2"
else
  fail "leg.sh: cost-kill.jsonl" "$(cat "$L/cost-kill.jsonl" 2>/dev/null); $(cat "$FS/kill-commands" 2>/dev/null)"
fi
# A try sent before the midpoint (leg.sh sends it 2 s ahead, so the exec's latency does not delay
# the kill) is held on the Machine until the midpoint by the Machine's own clock: the stub's exec
# lands at once, so the kill's at_ms is the Machine's wait, not the runner's. Without the hold it
# would read about -2 s from the midpoint (run 40153136892's A:noise read -0.08 s).
FS3="$W/fly3"
mkdir -p "$FS3"
cp "$FS/bin/flyctl" "$FS3/flyctl"
sleep 300 & lr3=$!
kids+=("$lr3")
proc "$FS3/proc" "$lr3" cefsrc 1 0 60 '--type=renderer\0--renderer-client-id=4\0'
L3="$W/leg-out3"
t0=$(ms)
# The drive starts 3 s out (leg.sh reads it within about a second), so the try goes at the lead.
p3=$((t0 + 3000 + 2000))
echo "{\"kind\":\"start\",\"at_ms\":$((t0 + 3000)),\"drive_s\":4}" >"$FS3/actions-first"
rc=0
PATH="$FS3:$PATH" STUB_FS="$FS3" STUB_BASH="$BASH5" LEG_POLL_S=1 LEG_KILL_TRIES=3 \
  "$BASH5" "$HERE/leg.sh" --app soak --harness h1 --compositor c1 --out "$L3" --until-ms $((t0 + 30000)) &
leg=$!
kids+=("$leg")
for _ in $(seq 1 100); do [ -s "$FS3/kills" ] && break; sleep 0.1; done
touch "$FS3/gone"
for _ in $(seq 1 100); do kill -0 "$leg" 2>/dev/null || break; sleep 0.1; done
kill "$leg" 2>/dev/null || true
landed="$(head -n 1 "$FS3/kill-landed" 2>/dev/null || echo 0)"
if jq -e -s --argjson p "$p3" --argjson r "$lr3" --argjson l "$landed" \
    'length == 2 and all(.planned_ms == $p and .try == 1) and .[0].at_ms >= $p and .[0].at_ms < $p + 1000
     and .[0].killed == [$r] and .[1].rc == 0 and $l > 0 and $l <= $p - 500' "$L3/cost-kill.jsonl" >/dev/null && ! alive "$lr3"; then
  pass "leg.sh: a try that reached the Machine $((p3 - landed)) ms before the midpoint held there, the kill $(($(jq -s '.[0].at_ms' "$L3/cost-kill.jsonl") - p3)) ms after it (0 to 1000), try 1"
else
  fail "leg.sh: the try held to the midpoint" "landed $((landed - p3)) ms from it; $(cat "$L3/cost-kill.jsonl" 2>/dev/null); $(tail -n 3 "$L3/cost-leg.log" 2>/dev/null)"
fi
# A try is started only while its kill can land inside the verdict's window (cost_kill_late_max_s,
# less the 15 s a try's exec takes to reach the Machine): here a 15 s window, the first try sent 2 s
# before the midpoint and every try unreachable, so one try and no second (soak.json's window holds
# three).
FS2="$W/fly2"
mkdir -p "$FS2"
cp "$FS/bin/flyctl" "$FS2/flyctl"
echo 5 >"$FS2/kill-unreachable"
L2="$W/leg-out2"
t0=$(ms)
# The drive starts 3 s out, as the held try's: the first try goes at the lead, 2 s before the
# midpoint, whenever leg.sh reads the start within those 3 s.
echo "{\"kind\":\"start\",\"at_ms\":$((t0 + 3000)),\"drive_s\":4}" >"$FS2/actions-first"
rc=0
PATH="$FS2:$PATH" STUB_FS="$FS2" STUB_BASH="$BASH5" LEG_POLL_S=1 LEG_KILL_TRIES=3 LEG_KILL_WINDOW_S=15 \
  "$BASH5" "$HERE/leg.sh" --app soak --harness h1 --compositor c1 --out "$L2" --until-ms $((t0 + 30000)) &
leg=$!
kids+=("$leg")
for _ in $(seq 1 100); do grep -q 'no try left inside the window' "$L2/cost-leg.log" 2>/dev/null && break; sleep 0.1; done
touch "$FS2/gone"
for _ in $(seq 1 100); do kill -0 "$leg" 2>/dev/null || break; sleep 0.1; done
kill "$leg" 2>/dev/null || true
tries=$(grep -c 'kill-renderer' "$FS2/calls" 2>/dev/null) || true # grep -c prints 0 and exits 1 on none
if [ "$tries" -eq 1 ] && grep -q 'no try left inside the window' "$L2/cost-leg.log" && grep -q 'FAIL the kill never ran' "$L2/cost-leg.log"; then
  pass "leg.sh: no kill try past the window (1 try in a 15 s window, then none), the kill named as never run"
else
  fail "leg.sh: the window" "$tries tries; $(grep -E 'kill' "$L2/cost-leg.log" 2>/dev/null | tail -n 4)"
fi
w=$(jq -r .cost_kill_late_max_s "$HERE/../soak.json")
[ "$w" -ge $((15 + 2 * (30 + 2))) ] && pass "soak.json: the kill's window ($w s) holds three tries (15 s to land, two failed ones of 30 s + 2 s before it)" \
  || fail "soak.json: the kill window" "$w s"

# --- the verdict ---------------------------------------------------------------------------------
# fixture.py's green leg (its docstring: the numbers worked by hand), and one red per mutation.
legdir() { python3 -I "$HERE/fixture.py" "$@"; } # <dir> [mutation...]
V="$HERE/verdict.sh"
legdir "$W/legs/green"
rc=0
"$V" --dir "$W/legs/green" --leg A --camera snow --guest performance-4x >"$W/green.json" 2>"$W/green.txt" || rc=$?
[ "$rc" -eq 0 ] && pass "verdict: the green leg passes (exit 0)" || fail "verdict: the green leg" "exit $rc: $(grep -E 'FAIL|error' "$W/green.txt" | head -n 5)"
expect "verdict green: 8 checks, all pass; the leg's spec from soak.json" "$W/green.json" \
  '.[0] | .status == "pass" and .checked == 8 and all(.checks[]; .status == "pass")
   and .output == "720p" and .template == "bar" and .sport == "cricket" and .cpus == 4'
expect "verdict green: the window's samples only (585 of 585: not the early one, the duplicate, flyctl's line or a cut one)" "$W/green.json" \
  '.[0].row.samples == {n: 585, expected: 585}'
expect "verdict green: Machine avg 603/585, p95 by nearest rank 1.0 (25 %), max 1.9" "$W/green.json" \
  '.[0].row.machine == {avg: 1.031, p95: 1, max: 1.9, p95_pct: 25}'
expect "verdict green: 100 moments in 10 min, never more than 6 s apart; the kill 302 s in, the compositor's own cef crashed gen=1 0.4 s and cef ready gen=2 4 s after it" "$W/green.json" \
  '.[0] | .row.moments == {n: 100, per_min: 10, kinds: {four: 100}, longest_gap_s: 6}
   and (.checks[] | select(.id == "reload") | .measured) == {kill_at_s: 302, killed: 1, crashes: 1, crash_gen: 1, crash_after_s: 0.4, ready_after_s: 4}'
expect "verdict green: one renderer before the kill and one after (no held page painting beside it); 8 stills of 8" "$W/green.json" \
  '.[0].row | .renderers == {before_max: 1, after_max: 1} and .stills == {written: 8, asked: 8}'
expect "verdict green: the tree, the renderer, the busiest thread, the encoder" "$W/green.json" \
  '.[0].row | .cats_avg.renderer == 0.05 and .top_thread == {name: "x264enc:src", p95: 0.4, max: 0.4}
   and .encoder_speed == {median: 1, min: 1} and .dropped_final == 0'
expect "verdict green: Owed 93, two held pages at the live renderer's cost: 1.0 + 2 x 0.05 within 3.0" "$W/green.json" \
  '.[0].owed93 == {renderer_avg: 0.05, p95_plus_two_held: 1.1, size_rule_max: 3, within: true}'
grep -qF "line(s) of cost-samples.jsonl not a JSON object: dropped" "$W/green.txt" \
  && pass "verdict green: the lines that are not JSON objects are dropped and counted" || fail "verdict green: dropped lines" "$(head -n 3 "$W/green.txt")"
legdir "$W/legs/at-max" cpu-at-max
"$V" --dir "$W/legs/at-max" --leg A --camera snow --guest performance-4x >"$W/at-max.json" 2>/dev/null \
  && expect "verdict: a p95 of exactly 3.0 cores passes (at most 75 %)" "$W/at-max.json" '.[0].row.machine.p95 == 3 and .[0].status == "pass"' \
  || fail "verdict: a p95 of exactly 3.0" "$(jq -c '.checks[] | select(.status == "fail")' "$W/at-max.json")"
"$V" --dir "$W/legs/green" --leg A --camera snow --guest performance-8x >"$W/8x.json" 2>/dev/null && rc=0 || rc=$?
expect "verdict: the same samples judged as performance-8x fail on the CPU count (4 read, 8 asked)" "$W/8x.json" \
  '.[0] | .status == "fail" and [.checks[] | select(.status == "fail") | .id] == ["samples"]'
# red <name> <mutation> <failing check id> <reason fragment>: that check fails, alone, by that reason
red() {
  local rc=0
  legdir "$W/legs/$2" "$2"
  "$V" --dir "$W/legs/$2" --leg A --camera snow --guest performance-4x >"$W/$2.json" 2>"$W/$2.txt" || rc=$?
  if [ "$rc" -eq 1 ] && jq -e --arg id "$3" --arg r "$4" '.status == "fail" and ([.checks[] | select(.status == "fail") | .id] == [$id])
      and ((.checks[] | select(.id == $id) | .reason) | contains($r))' "$W/$2.json" >/dev/null; then
    pass "verdict red: $1 ($3)"
  else
    fail "verdict red: $1" "exit $rc, $(jq -c '[.checks[] | select(.status == "fail") | {id, reason}]' "$W/$2.json" 2>/dev/null)"
  fi
}
# red_among: that check fails by that reason, whatever else fails with it (no start: no window at all)
red_among() {
  local rc=0
  legdir "$W/legs/$2" "$2"
  "$V" --dir "$W/legs/$2" --leg A --camera snow --guest performance-4x >"$W/$2.json" 2>"$W/$2.txt" || rc=$?
  if [ "$rc" -eq 1 ] && jq -e --arg id "$3" --arg r "$4" '.status == "fail" and ((.checks[] | select(.id == $id)) | .status == "fail" and (.reason | contains($r)))' \
      "$W/$2.json" >/dev/null; then
    pass "verdict red: $1 ($3, among $(jq -c '[.checks[] | select(.status == "fail") | .id]' "$W/$2.json"))"
  else
    fail "verdict red: $1" "exit $rc, $(jq -c '[.checks[] | select(.status == "fail") | {id, reason}]' "$W/$2.json" 2>/dev/null)"
  fi
}
red "a p95 over 3.0 cores" cpu-over cpu "p95 3.1 cores, over 3"
red "a stepped beat" stepped rung "1 beat(s) off full (overlay_half_rate"
red "pre-encoder drops" drops drops "droppedFrames reached 3"
red "fewer than one moment a minute" few-moments moments "9 moments in 10 min"
red "the feed carried no event's moments" unread moments "the page's feed carried none of the 100 events"
red "no kill" no-kill reload "no kill ran"
red "two kills" two-kills reload "2 kills: one a leg"
red "a kill 110 s after the midpoint" kill-late reload "the kill came 110 s from the midpoint (0 to 100)"
red "a kill 80 ms before the midpoint (run 40153136892's A:noise)" kill-early-80ms reload "the kill came -0.08 s from the midpoint (0 to 100)"
red "a kill that found no renderer" no-renderer reload "the kill found no renderer"
red "a kill whose status is 1" kill-rc reload "the kill's status: [1]"
red "a kill with no status line" kill-no-rc reload "no status line from the kill"
# The reload, by the compositor's own log (the beats every 5 s seldom see it).
red "no compositor log" no-log reload "no compositor.log: the reload cannot be witnessed"
red "the kill did nothing the compositor saw" no-crash reload 'no `cef crashed` line in the drive'
red "two crashes (a retried kill, a second reload)" two-crashes reload '2 `cef crashed` lines in the drive (gens 1, 2): one reload a leg'
red "the drive's one crash came before the kill" crash-before-kill reload 'the only `cef crashed` (gen 1) came 60 s before the kill'
red "the page never came back" no-ready reload 'no `cef ready gen=2` after `cef crashed gen=1`'
red "only the old page came back (an in-place reload, no new build)" ready-same-gen reload 'no `cef ready gen=2` after `cef crashed gen=1`'
# The beats' fields, the actions and the stills.
red "beats with no rung field" no-rung-field rung "no beat carried a rung"
red "beats with no droppedFrames field" no-dropped-field drops "no beat carried droppedFrames"
red "two start actions" two-starts run "want one start and one end action, got 2 and 1"
red "no end action" no-end run "want one start and one end action, got 1 and 0"
red "a drive under leg_min_drive_min" short-drive run "a 590 s drive is under leg_min_drive_min (10 min)"
red_among "no start action" no-start run "want one start and one end action, got 0 and 1"
red "no still written" no-stills stills "0 of 8 stills written"
red "a sample missing every fifth second" few-samples samples "468 samples of 585 (under 0.9)"
# The table: one column per verdict, in leg order; a file that is not a verdict left out, none at all
# refused.
"$HERE/table.sh" "$W/green.json" "$W/cpu-over.json" /etc/hosts >"$W/table.md" 2>"$W/table.err" && rc=0 || rc=$?
if [ "$rc" -eq 0 ] && [ "$(grep -c '^| ' "$W/table.md")" -eq 20 ] && grep -qF '| **Size rule verdict** | **PASS** | **FAIL**: cpu |' "$W/table.md" \
  && grep -qF '| **Machine cores avg / p95 / max (of N)** | **1.031 / 1 / 1.9 (of 4; p95 25 %)** |' "$W/table.md" \
  && grep -qF 'is not a cost verdict: left out' "$W/table.err"; then
  pass "table: two legs' columns, F-005's rows, the failing check named, a non-verdict left out"
else
  fail "table" "exit $rc: $(head -c 600 "$W/table.md") $(cat "$W/table.err")"
fi
"$HERE/table.sh" /etc/hosts >/dev/null 2>&1 && rc=0 || rc=$?
[ "$rc" -eq 1 ] && pass "table: no verdict among the files is refused (exit 1)" || fail "table with no verdict" "exit $rc"
rc=0
"$V" --dir "$W/legs/green" --leg E --camera snow --guest performance-4x >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 64 ] && pass "verdict: an unknown leg is a usage error (64)" || fail "verdict: an unknown leg" "exit $rc"
rc=0
"$V" --dir "$W/legs/green" --leg A --camera ball --guest performance-4x >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 64 ] && pass "verdict: a camera outside leg_cameras is a usage error (64)" || fail "verdict: camera ball" "exit $rc"

echo "checked: $checked"
[ "$checked" -gt 0 ] || { echo "FAIL nothing checked"; exit 1; }
[ "$failures" -eq 0 ]
