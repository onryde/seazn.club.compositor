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

# A destination stall (cases 5, 12 and 17), timed on event timestamps (the PR #52 review's I4): the
# swap is the log's first `sink swap` line after the cut (docker-stamped, ms), and the anchor Owed 39
# asks for is the last beat before that line whose egressBytes grew (the kernel's send buffer keeps
# the total growing after a cut, so a bound timed from the cut is wrong by the drain). The true last
# growth is at or before that beat, so a bound from it never reds a correct swap. The beat that first
# carries the raised sinkSwaps must exist too.
#
# Owed 87 (the owner, 2026-10-24, "A"): the output has no stall rule. A Connected sink is swapped
# only after its own bus error (domain::output_link's `on_error`, then the back-off's `build`), and
# for a stalled destination that error is rtmp2sink's socket timeout (`output_sink_timeout_ms`,
# which C09 sets on every sink). The error path always logs `output gen=<g> error:` first:
# `error_ms` is that line for the swap's outgoing generation, `error_to_swap_ms` the back-off from it
# to the swap, and `error_from_cut_ms` the wait from the cut to it. A report with no `output_errors`
# field reads no error, never one: the error is read, not assumed.
#
# `reason` (Owed 87): the driver's `sink swap reason=<r> gen=<g>` line for the swap's generation,
# logged just before the swap line (PR #57). `reasons_paired` checks the reason lines number the
# swap lines: as many, and the same generation at each place (their order against the swap lines is
# compositor/tests/wiring.rs's to pin). A report with no `sink_swap_reasons` (one checked before this
# field) reads reason "unknown", never a reason: the reason is read, not assumed.
#
# `detect_ms` is the error from the last growth, `from_growth_ms` the swap from it. `bound_ms` is the
# stall's budget from the last growth: one sink timeout, the domain's back-off from Connected
# (failure 1 waits the base), and one frame period (the driver acts on its next tick).
def stall_swap($cut):
  ([beats[] | select(.received_ms <= $cut)] | last | .body.sinkSwaps) as $s0
  | ([beats[] | select(.received_ms > $cut and (.body.sinkSwaps // -1) > $s0)] | first) as $swap_beat
  | ([.log.sink_swaps[] | select(.at_ms > $cut)] | first) as $sw
  | ($sw.at_ms) as $swap
  | (.log.output_errors) as $errors
  | (.log.sink_swap_reasons) as $reasons
  | (.log.sink_swaps) as $swaps
  | (if $sw == null or $errors == null then [] else [$errors[] | select(.gen == $sw.gen - 1 and .at_ms < $swap)] end) as $err
  | ($err | first | .at_ms) as $seen
  | (if $swap == null then [] else [beats[] | select(.received_ms < $swap)] end) as $b
  | ([range(1; $b | length) | select(($b[.].body.egressBytes // 0) > ($b[. - 1].body.egressBytes // 0))
      | $b[.].received_ms] | last) as $grow
  | {swaps_before: $s0, swap_ms: $swap, swap_gen: ($sw.gen // null), swap_beat_ms: ($swap_beat.received_ms // null),
     last_growth_ms: $grow,
     reason: (if $sw == null then null elif $reasons == null then "unknown"
       else ([$reasons[] | select(.gen == $sw.gen and .at_ms <= $swap)] | last | .reason) end),
     reasons_paired: ($reasons != null and ($reasons | length) == ($swaps | length)
       and all(range(0; $swaps | length); $reasons[.].gen == $swaps[.].gen)),
     error_ms: $seen,
     error_to_swap_ms: less($swap; $seen),
     error_from_cut_ms: less($seen; $cut),
     from_cut_ms: less($swap; $cut),
     from_growth_ms: less($swap; $grow),
     detect_ms: less($seen; $grow),
     bound_ms: ($t.output_sink_timeout_ms + $t.output_backoff_base_ms + $t.frame_period_ms)};

# A stall's swap after the cut at $cut, judged. Owed 87 leaves one path, the sink's own error:
# - the swap, the beat that counts it and a growth beat before it all exist, and the reason lines
#   number the swap lines;
# - the reason line reads error, and the outgoing sink's error line came before the swap (else
#   `error_to_swap_ms` is null, and jq sorts null below every number: that reds);
# - the sink was replaced after the domain's back-off from Connected (output_backoff_base_ms, a floor
#   the product cannot beat) and inside the spec's output budget (§6 "≤5 s",
#   budget_output_reconnect_ms);
# - the swap came within `bound_ms` of the last growth: a detector slower than the sink's timeout reds
#   (C14's mutant m9 sets it to 20 s);
# - the error came no sooner after the cut than the sink's timeout less one frame (the PR #65 review's
#   m-3: the bytes flow at full rate until the cut, so the last write is no earlier than one frame
#   before it): an error sooner than that is not the stall's.
def stall_swap_judged($cut):
  stall_swap($cut) as $s
  | $s.swap_ms != null and $s.swap_beat_ms != null and $s.last_growth_ms != null and $s.reasons_paired
    and $s.reason == "error"
    and $s.error_to_swap_ms >= $t.output_backoff_base_ms
    and $s.error_to_swap_ms <= $t.budget_output_reconnect_ms
    and $s.from_growth_ms <= $s.bound_ms
    and $s.error_from_cut_ms >= $t.output_sink_timeout_ms - $t.frame_period_ms;

# The clear's phase in the output's reconnect cycle (cases 4, 5 and 12; the re-review's I-5, Owed
# 84). Under the fault a sink is built every 6-7 s (a connect that times out, then the back-off), and
# the drive clears the leg in one of two phases (`clear_out_in_phase`, run-case.sh), named in the
# clear mark's value:
# - "pending": the last sink built before the clear was still connecting (built under
#   OUTPUT_CONNECT_TIMEOUT_MS before it, and no error of its own yet), so it connects after the
#   clear, the path that replayed the fault's backlog before Owed 84's fix;
# - "backoff": the last sink built had already failed (its own `output gen=<g> error:` line before
#   the clear), so the next build connects on a clear path.
# `phase` is what the log shows, null when neither holds or no sink was built in the fault (the
# boot's swap is no phase). `driven` is the drive's intent.
def clear_phase:
  mark_at("clear") as $c
  | ([.log.sink_swaps[] | select($c != null and .at_ms <= $c)] | last) as $sw
  | (if $sw == null then null else $c - $sw.at_ms end) as $ago
  | ([(.log.output_errors // [])[] | select($sw != null and .gen == $sw.gen and .at_ms <= $c)] | first) as $err
  | ([.log.sink_swaps[] | select($c != null and .at_ms > mark_at("cut") and .at_ms <= $c)] | length) as $n
  | ($ago != null and $ago < $t.output_connect_timeout_ms and $err == null) as $pending
  | {driven: (mark_value("clear") | if type == "object" then .phase else null end), swap_gen: ($sw.gen // null),
     swap_to_clear_ms: $ago, error_to_clear_ms: (if $err == null then null else $c - $err.at_ms end),
     swaps_in_fault: $n, connect_pending: $pending, connect_timeout_ms: $t.output_connect_timeout_ms,
     phase: (if $n == 0 then null elif $pending then "pending" elif $err != null then "backoff" else null end)};

# The output's return after a fault cleared at $clear (cases 4, 5, 12): the first outage that ends
# after it.
def outage_across($clear): [.output_outages[] | select(.to_wall_ms > $clear)] | first;
