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
# field reads no error, never one: the error is read, not assumed. `error_to_swap_ms` is an interval
# the product counts on its own clock, so it is read from the two lines' own `ts=` (logfacts'
# `ts_ms`; the PR #73 confirm pass's b-1); every interval that crosses into the harness (the cut,
# the beats) stays on docker's stamps.
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
  | ($err | first) as $e
  | ($e.at_ms) as $seen
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
     error_to_swap_ms: less($sw.ts_ms; $e.ts_ms),
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
# - the sink was replaced after the domain's back-off from Connected (output_backoff_base_ms, on the
#   lines' own `ts=`: the error line is written before the domain's `now` for it is read, and the
#   swap line after the tick that builds, both on the process's ms clock, so a correct swap is never
#   under the base; a swap that skipped the back-off lands within a tick of the error, far below it)
#   and inside the spec's output budget (§6 "≤5 s", budget_output_reconnect_ms). Docker's stamps
#   cannot judge this: `docker logs --timestamps` stamps each line when its daemon reads it, so the
#   pair can sit closer than the process wrote them (PR #71's CI run 40137725801: 499.38 ms at ns
#   precision for a swap whose `ts=` were 500 apart, the error line read about 1.4 ms late);
# - the swap came within `bound_ms` of the last growth: a detector slower than the sink's timeout reds
#   (C14's mutant m9 sets it to 20 s);
# - the error came no sooner after the cut than the sink's timeout less one frame (the PR #65 review's
#   m-3: the bytes flow at full rate until the cut, so the last write is no earlier than one frame
#   before it): an error sooner than that is not the stall's;
# - and no later than the sink's timeout plus `budget_output_lost_max_ms` after the cut (the PR #73
#   review's m-1): what the sink writes after the cut goes into a socket nothing drains, and spec §6
#   lets a stall lose at most that much programme, so its last write is no later than that and its
#   error one sink timeout after. The growth bound above is anchored on a beat, up to one beat after
#   the last write, so on its own it passes a timeout up to about 8-9 s; this ceiling is anchored on
#   the cut, and reds one past about 6 s (CI's error_from_cut_ms read 5 450-5 898).
def stall_swap_judged($cut):
  stall_swap($cut) as $s
  | $s.swap_ms != null and $s.swap_beat_ms != null and $s.last_growth_ms != null and $s.reasons_paired
    and $s.reason == "error"
    and $s.error_to_swap_ms >= $t.output_backoff_base_ms
    and $s.error_to_swap_ms <= $t.budget_output_reconnect_ms
    and $s.from_growth_ms <= $s.bound_ms
    and $s.error_from_cut_ms >= $t.output_sink_timeout_ms - $t.frame_period_ms
    and $s.error_from_cut_ms <= $t.output_sink_timeout_ms + $t.budget_output_lost_max_ms;

# Owed 94's egress backstop (case 18, output-hung; the owner, 2026-10-24, "B"): a sink whose egress
# stays flat while both encoders deliver is swapped, its reason line `egress_stalled`. The fault is
# the hook's plant (`.log.sink_stall_planted`, Owed 94's `test hook` line: sink 1's writes held, or
# its streaming thread hung), and `swap` is the first sink swap after it. Both are the process's own
# lines, so `planted_to_swap_ms` is read on their `ts=` (as `error_to_swap_ms` is, b-1).
# The window is the domain's, every term from $t (domain::output_link's backstop):
# - the last reading that saw growth is no more than one poll before the plant (the poll before it
#   saw the bytes of the frames before it), and the plant line is written within a tick of the hold,
#   so the swap is no sooner than output_egress_flat_ms + output_backoff_base_ms - one poll - one
#   tick after the plant line;
# - and no later than output_egress_flat_ms + output_backoff_base_ms + two polls (the reading after
#   the plant can still see what the sink wrote before it, and one more for that reading's way to
#   the driver) + two ticks (the tick the backstop fires on, and the one that builds after the
#   back-off).
# `errors_before` lists the outgoing sink's own error lines before the swap: none, or the swap was
# the error path's (a destination that dropped the idle publisher first, MediaMTX's readTimeout).
def hung_swap:
  ((.log.sink_stall_planted // []) | first) as $p
  | [.log.sink_swaps[] | select($p != null and .at_ms >= $p.at_ms)] as $after
  | ($after | first) as $sw
  | (.log.sink_swap_reasons) as $reasons
  | (.log.sink_swaps) as $swaps
  | {planted_ms: ($p.at_ms // null), swap_ms: ($sw.at_ms // null), swap_gen: ($sw.gen // null),
     swaps_after: ($after | length),
     reason: (if $sw == null then null elif $reasons == null then "unknown"
       else ([$reasons[] | select(.gen == $sw.gen and .at_ms <= $sw.at_ms)] | last | .reason) end),
     reasons_paired: ($reasons != null and ($reasons | length) == ($swaps | length)
       and all(range(0; $swaps | length); $reasons[.].gen == $swaps[.].gen)),
     errors_before: [(.log.output_errors // [])[] | select($sw != null and .gen == $sw.gen - 1 and .at_ms < $sw.at_ms) | .at_ms],
     planted_to_swap_ms: less($sw.ts_ms; $p.ts_ms),
     floor_ms: ($t.output_egress_flat_ms + $t.output_backoff_base_ms - $t.output_progress_poll_ms - $t.tick_widest_ms),
     ceiling_ms: ($t.output_egress_flat_ms + $t.output_backoff_base_ms + 2 * $t.output_progress_poll_ms + 2 * $t.tick_widest_ms)};

# The backstop's swap, judged: the reason `egress_stalled` with the reason lines pairing the swap
# lines, no error line of the outgoing sink before it, and inside the window. (No swap reads reason
# null, a red; and the swap is sink 2: the plant went in on sink 1, `planted` checks that.)
def hung_swap_judged:
  hung_swap as $s
  | $s.reasons_paired and $s.reason == "egress_stalled"
    and $s.errors_before == []
    and $s.planted_to_swap_ms >= $s.floor_ms and $s.planted_to_swap_ms <= $s.ceiling_ms;

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
