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

# A destination stall (cases 5 and 12): the first beat after the cut whose sinkSwaps rose, and the
# anchor Owed 39 asks for: the last beat before it whose egressBytes grew (the kernel's send buffer
# keeps the total growing after a cut, so a bound timed from the cut is wrong by the drain). The
# true last growth is at or before that beat, so the bound from it never reds a correct swap.
def stall_swap($cut):
  ([beats[] | select(.received_ms <= $cut)] | last | .body.sinkSwaps) as $s0
  | ([beats[] | select(.received_ms > $cut and (.body.sinkSwaps // -1) > $s0)] | first) as $swap
  | (if $swap == null then [] else [beats[] | select(.received_ms < $swap.received_ms)] end) as $b
  | ([range(1; $b | length) | select(($b[.].body.egressBytes // 0) > ($b[. - 1].body.egressBytes // 0))
      | $b[.].received_ms] | last) as $grow
  | {swaps_before: $s0, swap_ms: ($swap.received_ms // null), last_growth_ms: $grow,
     from_cut_ms: (if $swap == null then null else $swap.received_ms - $cut end),
     from_growth_ms: (if $swap == null or $grow == null then null else $swap.received_ms - $grow end),
     bound_ms: ($t.output_stall_after_ms + $t.output_progress_poll_ms + $t.heartbeat_interval_ms),
     egress_between: (if $swap == null then [] else
       [beats[] | select(.received_ms >= $cut and .received_ms <= $swap.received_ms) | .body.egressBytes] end)};

# The output's return after a fault cleared at $clear (cases 4, 5, 12): the first outage that ends
# after it.
def outage_across($clear): [.output_outages[] | select(.to_wall_ms > $clear)] | first;
