# Definitions every case verdict may use (C14). run-case.sh prepends this file to each `require`,
# `measure` and `owed` filter, with these bound:
#   $t     harness/thresholds.json (the domain's thresholds and spec §6's budgets; C14 step 2)
#   $run   out/<run>/run.json (docker events, restart counts, fake-control's state, cores)
#   $case  the case's own marks, by name: {"<name>": [{"at_ms": N, "value": <json>}, ...]}
#   $timeline  out/<run>/timeline.json (fake-control's score timeline, absolute)
# The input is report.json (the checker's), except for `require_run`, whose input is run.json.
# No number is typed here: every one comes from $t, a mark, or the report.

def mark($n): ($case[$n] // []);
def mark_at($n): (mark($n) | first | .at_ms);
def mark_value($n): (mark($n) | first | .value);
# $a - $b, or null when either is missing: a printed measure shows what it can of a red world.
def less($a; $b): if $a == null or $b == null then null else $a - $b end;
def abs: if . < 0 then 0 - . else . end;

# docker events (die, start, restart, kill, oom, stop) of the compositor's container, in time order.
def compositor_events: [$run.events[] | select(.service == "compositor")] | sort_by(.at_ms);
def dies_after($ms): [compositor_events[] | select(.action == "die" and .at_ms >= $ms)];
def starts_after($ms): [compositor_events[] | select(.action == "start" and .at_ms >= $ms)];

# The beats, in order, each {received_ms, body}.
def beats: .heartbeats.series;
def beats_between($a; $b): [beats[] | select(.received_ms >= $a and .received_ms < $b)];

# The recording's frames, each [wall_ms, counter, score] (null where a strip did not read).
def frames_between($a; $b): [.frames_table[] | select(.[0] >= $a and .[0] < $b)];

# The output outage that spans a process restart: its last packet before the start, its first
# after the die.
def restart_outage($die; $start):
  [.output_outages[] | select(.from_wall_ms <= $start and .to_wall_ms >= $die)] | first;

# The scheduled score timeline, absolute: each {index, at_ms, due_ms} (the page has an event from
# its `at`, and shows it at `at` plus the session's overlay delay).
def scheduled: [$timeline.events[] | {index, at_ms: .at, due_ms: (.at + $timeline.delay_ms)}];

# Consecutive values that repeat (for "each kill's pid differs from the one before").
def repeats: . as $v | [range(1; $v | length) | select($v[.] == $v[. - 1])];
# Overlay runs whose index steps down (the score never regresses).
def index_regressions: [.overlay.runs[] | select(.index != null) | .index] as $i
  | [range(1; $i | length) | select($i[.] < $i[. - 1])];

# A destination stall (cases 5 and 12), timed on event timestamps (the PR #52 review's I4): the
# swap is the log's first `sink swap` line after the cut (docker-stamped, ms), and the anchor Owed 39
# asks for is the last beat before that line whose egressBytes grew (the kernel's send buffer keeps
# the total growing after a cut, so a bound timed from the cut is wrong by the drain). The true last
# growth is at or before that beat, so `from_growth_ms <= output_stall_after_ms +
# output_progress_poll_ms` never reds a correct swap, and a detector one stall threshold slow reds
# unless the true growth fell in the last poll before the previous beat (under 1 run in 20 by phase,
# when the threshold doubles). The beat that first carries the raised sinkSwaps must exist too.
#
# `cause` (C14 fix round 3, item 2): a Connected sink is swapped by exactly two paths
# (domain::output_link): the stall rule (`build` at once), or its bus error (`on_error`, then the
# back-off's `build`). The `sink swap` line names neither (Owed 87), but the error path always logs
# `output gen=<g> error:` first, and a sink the stall rule retires never logs one (gst_output.rs's
# `Watch` returns once retired). So a swap to generation g+1 is the stall rule's ("stall") only when
# generation g logged no error before it; else it is the error path's ("output_error"), and
# `error_to_swap_ms` shows the back-off between them. Mutant 9 (the stall threshold at 20 s) swaps
# just as fast on dest-trickle, through rtmp2sink's own 5 s socket timeout: only the cause tells
# them apart. A report with no `output_errors` field (one checked before fix round 3, re-judged with
# CASE_REJUDGE) reads cause "unknown", never "stall": the cause is read, not assumed (the
# re-review's m-3).
def stall_swap($cut):
  ([beats[] | select(.received_ms <= $cut)] | last | .body.sinkSwaps) as $s0
  | ([beats[] | select(.received_ms > $cut and (.body.sinkSwaps // -1) > $s0)] | first) as $swap_beat
  | ([.log.sink_swaps[] | select(.at_ms > $cut)] | first) as $sw
  | ($sw.at_ms) as $swap
  | (.log.output_errors) as $errors
  | (if $sw == null or $errors == null then [] else [$errors[] | select(.gen == $sw.gen - 1 and .at_ms < $swap)] end) as $err
  | (if $swap == null then [] else [beats[] | select(.received_ms < $swap)] end) as $b
  | ([range(1; $b | length) | select(($b[.].body.egressBytes // 0) > ($b[. - 1].body.egressBytes // 0))
      | $b[.].received_ms] | last) as $grow
  | {swaps_before: $s0, swap_ms: $swap, swap_gen: ($sw.gen // null), swap_beat_ms: ($swap_beat.received_ms // null),
     last_growth_ms: $grow,
     cause: (if $sw == null then null elif $errors == null then "unknown" elif ($err | length) > 0 then "output_error"
       else "stall" end),
     error_to_swap_ms: (if ($err | length) > 0 then $swap - ($err | first | .at_ms) else null end),
     from_cut_ms: (if $swap == null then null else $swap - $cut end),
     from_growth_ms: (if $swap == null or $grow == null then null else $swap - $grow end),
     bound_ms: ($t.output_stall_after_ms + $t.output_progress_poll_ms)};

# The output's return after a fault cleared at $clear (cases 4, 5, 12): the first outage that ends
# after it.
def outage_across($clear): [.output_outages[] | select(.to_wall_ms > $clear)] | first;
