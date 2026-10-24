# A cost leg's verdict (C15 brief, "Cost legs": the size rule, the frequent moments and the one page
# reload). Run through harness/soak/cost/verdict.sh, which binds:
#   $samples   leg.sh's cost-samples.jsonl: in-compositor.sh's one line a second on the compositor
#              Machine (the whole Machine's cores, the tree's, the renderer's, the busiest thread)
#   $beats     fake-control's heartbeats.jsonl: {received_ms, body} (rung, droppedFrames, encoderSpeed,
#              graphics)
#   $actions   driver.sh's actions.jsonl: the start (with drive_s) and the end
#   $kills     leg.sh's cost-kill.jsonl: the renderer kill's targets (at_ms, killed, planned_ms) and rc
#   $deliveries the leg's score driver's deliveries.jsonl, each accepted event with the moments the
#              page's feed makes of it
#   $soak      harness/soak/soak.json      $leg, $camera, $guest   the leg, its picture, its guest
# Every number comes from $soak: none is typed here. Each check answers {id, name, status: pass|fail,
# reason, measured}; a check whose data is missing fails, never passes on nothing. `row` is F-005's
# table shape for the leg.
def r3: if . == null then null else (. * 1000 | round) / 1000 end;
def avg: if length == 0 then null else (add / length) end;
def median: sort | if length == 0 then null elif length % 2 == 1 then .[(length - 1) / 2]
  else (.[length / 2 - 1] + .[length / 2]) / 2 end;
# Nearest rank: the smallest value with at least 95 % of the values at or under it.
def p95: sort | if length == 0 then null else .[((length * 0.95) | ceil) - 1] end;
def check($id; $name; $problems; $measured):
  {id: $id, name: $name, status: (if ($problems | length) == 0 then "pass" else "fail" end),
   reason: (if ($problems | length) == 0 then null else ($problems | join("; ")) end), measured: $measured};

($soak[0]) as $soak
| ($soak.legs[$leg] // error("cost: no leg \($leg) in soak.json")) as $spec
| ($soak.compositor_guest[$guest] // error("cost: no guest \($guest) in soak.json")) as $g
| ($g.cpus * $soak.cost_cpu_p95_max_share) as $cpu_max
| [$actions[] | select(.kind == "start")] as $starts
| [$actions[] | select(.kind == "end")] as $ends
| ($starts[0].at_ms // null) as $s
| ($starts[0].drive_s // null) as $drive_s
| ($ends[0].at_ms // (if $s == null then null else $s + $drive_s * 1000 end)) as $e
| (if $s == null then null else $s + $soak.cost_sample_from_s * 1000 end) as $from
| (if $s == null then [] else
    [$samples[] | select(type == "object" and (.t_ms | type) == "number" and (.machine | type) == "number")]
    | unique_by(.t_ms) | map(select(.t_ms > $from and .t_ms <= $e)) end) as $win
| (if $s == null then 0 else (($e - $from) / 1000 | floor) end) as $expected
| [$beats[] | select(type == "object" and (.body | type) == "object")] as $all_beats
| (if $s == null then [] else [$all_beats[] | select(.received_ms > $from and .received_ms <= $e)] end) as $win_beats
| [$kills[] | select(has("at_ms"))] as $kill_lines
| [$kills[] | select(has("rc"))] as $rc_lines
| ($kill_lines[0] // null) as $k
| (if $s == null then [] else [$deliveries[] | select(.ok == true and .at_ms >= $s and .at_ms <= $e)] end) as $events
| [$events[] | .moments | select(type == "array") | .[]] as $moments
| (if $drive_s == null then null else (($e - $s) / 60000) end) as $drive_min
| ($win | map(.machine)) as $m
| ($win | map(.cats.renderer // empty)) as $rend
| [
  check("run"; "the leg ran";
      (if ($starts | length) != 1 or ($ends | length) != 1 then ["want one start and one end action, got \($starts | length) and \($ends | length)"] else [] end)
      + (if $drive_s != null and $drive_s < $soak.leg_min_drive_min * 60 then ["a \($drive_s) s drive is under leg_min_drive_min (\($soak.leg_min_drive_min) min)"] else [] end);
      {start_ms: $s, end_ms: $e, drive_s: $drive_s}),

  # Anti-vacuity: a sample a second through the window, on the guest asked for.
  ([$win[] | .ncpu] | unique) as $ncpus
  | check("samples"; "a sample a second, from +\($soak.cost_sample_from_s) s to the end";
      (if ($win | length) == 0 then ["no sample in the window"]
       elif ($win | length) < $expected * $soak.cost_samples_min_ratio then ["\($win | length) samples of \($expected) (under \($soak.cost_samples_min_ratio))"] else [] end)
      + (if ($win | length) > 0 and $ncpus != [$g.cpus] then ["the Machine read \($ncpus) CPUs, the guest \($guest) has \($g.cpus)"] else [] end);
      {samples: ($win | length), expected: $expected, ncpu: $ncpus}),

  # The size rule's 1: Machine CPU p95 at most cost_cpu_p95_max_share of the guest's cores.
  check("cpu"; "Machine CPU p95 at most \($soak.cost_cpu_p95_max_share * 100 | floor) % of \($g.cpus) cores (\($cpu_max))";
      (if ($m | length) == 0 then ["no sample"]
       elif ($m | p95) > $cpu_max then ["p95 \($m | p95 | r3) cores, over \($cpu_max): a sizing finding (move to the next performance size)"] else [] end);
      {p95: ($m | p95 | r3), max: $cpu_max}),

  # The size rule's 2: every beat's rung is rung_ok, the reload included.
  [$all_beats[] | select(.body | has("rung"))] as $rb
  | [$rb[] | select(.body.rung != $soak.rung_ok)] as $stepped
  | check("rung"; "no step down: every beat's rung is \($soak.rung_ok)";
      (if ($rb | length) == 0 then ["no beat carried a rung"] else [] end)
      + (if ($stepped | length) > 0 then ["\($stepped | length) beat(s) off \($soak.rung_ok) (\([$stepped[] | .body.rung] | unique | join(", ")), first at \($stepped[0].received_ms))"] else [] end);
      {rung_beats: ($rb | length), stepped: ($stepped | length)}),

  # The size rule's 3: no pre-encoder drops (droppedFrames is a counter: its largest value).
  ([$all_beats[] | .body.droppedFrames | numbers] | max) as $dropped
  | check("drops"; "no pre-encoder drops: the final droppedFrames is 0";
      (if $dropped == null then ["no beat carried droppedFrames"] elif $dropped > 0 then ["droppedFrames reached \($dropped)"] else [] end);
      {dropped: $dropped}),

  # Frequent moments: at least cost_moments_min_per_min, by the page's own feed.
  ([$events[] | select(.moments == null)] | length) as $unread
  | check("moments"; "at least \($soak.cost_moments_min_per_min) moment a minute";
      (if ($events | length) == 0 then ["no accepted event in the drive"]
       elif $unread == ($events | length) then ["the page's feed carried none of the \($events | length) events"] else [] end)
      + (if $drive_min != null and $drive_min > 0 and (($moments | length) / $drive_min) < $soak.cost_moments_min_per_min
         then ["\($moments | length) moments in \($drive_min | r3) min (\(($moments | length) / $drive_min | r3) a minute)"] else [] end);
      {moments: ($moments | length), per_min: (if $drive_min == null or $drive_min == 0 then null else (($moments | length) / $drive_min | r3) end),
       events: ($events | length), unread: $unread}),

  # One page reload: the renderer killed once, at the midpoint (within cost_kill_late_max_s), inside
  # the drive, and a beat after it reads graphics ready (the page is back).
  ([$all_beats[] | select($k != null and .received_ms > $k.at_ms and .body.graphics == "ready")] | first) as $ready
  | check("reload"; "one renderer kill at the midpoint, and the page back";
      (if ($kill_lines | length) == 0 then ["no kill ran (leg.sh's cost-kill.jsonl)"]
       elif ($kill_lines | length) > 1 then ["\($kill_lines | length) kills: one a leg"] else [] end)
      + (if $k != null and (($k.killed // []) | length) == 0 then ["the kill found no renderer"] else [] end)
      + (if $k != null and ($rc_lines | length) > 0 and ($rc_lines | all(.rc == 0) | not) then ["the kill's status: \([$rc_lines[] | .rc])"] else [] end)
      + (if $k != null and ($k.at_ms < $k.planned_ms or $k.at_ms > $k.planned_ms + $soak.cost_kill_late_max_s * 1000)
         then ["the kill came \(($k.at_ms - $k.planned_ms) / 1000) s from the midpoint (0 to \($soak.cost_kill_late_max_s))"] else [] end)
      + (if $k != null and $s != null and ($k.at_ms <= $s or $k.at_ms >= $e) then ["the kill fell outside the drive"] else [] end)
      + (if $k != null and $ready == null then ["no beat after the kill reads graphics ready"] else [] end);
      {kill_at_s: (if $k == null or $s == null then null else (($k.at_ms - $s) / 1000 | r3) end),
       killed: (($k.killed // []) | length), ready_after_s: (if $ready == null then null else (($ready.received_ms - $k.at_ms) / 1000 | r3) end)})
]
| . as $checks
| ($win | map(.top_thread | select(. != null))) as $tops
| ([$win_beats[] | .body.encoderSpeed | numbers]) as $enc
| {
    leg: $leg, camera: $camera, guest: $guest, cpus: $g.cpus, output: $spec.output, template: $spec.template, sport: $spec.sport,
    status: (if any($checks[]; .status == "fail") then "fail" else "pass" end),
    checked: ($checks | length),
    checks: $checks,
    row: {
      machine: {avg: ($m | avg | r3), p95: ($m | p95 | r3), max: ($m | max | r3),
                p95_pct: (if ($m | length) == 0 then null else (($m | p95) / $g.cpus * 100 | r3) end)},
      tree_avg: ($win | map(.tree) | avg | r3),
      cats_avg: (if ($win | length) == 0 then null else
        ($win[0].cats | keys_unsorted) as $ks | reduce $ks[] as $c ({}; .[$c] = ($win | map(.cats[$c]) | avg | r3)) end),
      top_thread: (if ($tops | length) == 0 then null else
        {name: ($tops | group_by(.name) | max_by(length) | .[0].name), p95: ($tops | map(.cores) | p95 | r3), max: ($tops | map(.cores) | max | r3)} end),
      encoder_speed: {median: ($enc | median | r3), min: ($enc | min | r3)},
      fps_median: ([$win_beats[] | .body.fps | numbers] | median | r3),
      compositor_cpu_pct_median: ([$win_beats[] | .body.cpuPct | numbers] | median | r3),
      dropped_final: ([$all_beats[] | .body.droppedFrames | numbers] | max),
      rungs: ([$all_beats[] | .body.rung | strings] | group_by(.) | map({(.[0]): length}) | add),
      moments: {n: ($moments | length), per_min: (if $drive_min == null or $drive_min == 0 then null else (($moments | length) / $drive_min | r3) end),
                kinds: ($moments | group_by(.) | map({(.[0]): length}) | add)},
      mem_used_max_mb: ($win | map(.mem_used_mb) | max),
      samples: {n: ($win | length), expected: $expected}
    },
    # Owed 93 (a held teardown's page keeps painting at about the live renderer's CPU): what two held
    # pages would add to this leg's p95, against the size rule. Measured, never judged (owner ruling
    # 4A kept).
    owed93: (if ($rend | length) == 0 or ($m | length) == 0 then null else
      {renderer_avg: ($rend | avg | r3), p95_plus_two_held: (($m | p95) + 2 * ($rend | avg) | r3), size_rule_max: $cpu_max,
       within: ((($m | p95) + 2 * ($rend | avg)) <= $cpu_max)} end)
  }
