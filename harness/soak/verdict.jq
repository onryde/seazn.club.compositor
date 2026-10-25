# The soak's verdict (C15 brief: "Pass", criterion 6, and the real-template checks (a) to (d)).
# Run through harness/soak/verdict.sh, which binds:
#   $report      the checker's report.json (the harness Machine's; [] when the file is missing)
#   $actions     driver.sh's actions.jsonl: start, dropout, disconnect and end, each with its wall time
#   $control     fake-control's /control/state at the end (control-state.json)
#   $deliveries  the score driver's deliveries.jsonl (check (c); empty on the test page)
#   $samples     the overlay sampler's overlay-samples.jsonl (check (d); empty on the test page)
#   $logscan     logscan.json: the compositor log's raw `overlay hidden` line count (check (b))
#   $complog     compositor.log, raw (flylog.py's; "" when missing): its `memory split` lines
#   $t           harness/thresholds.json        $soak  harness/soak/soak.json
#   $drive_min   the run's drive, in minutes (the duration env: SOAK_DRIVE_MIN)
#   $page        "real" (the production overlay page) or "test" (C14's overlay-test.html: plumbing)
#   $schedule    "soak" or "dropouts" (schedule.jq's schedule_plan: what the driver was to do)
# Every number comes from $t, $soak or $drive_min: none is typed here. Each check answers
# {id, name, status: pass|fail|pending, reason, measured}; a check whose data is missing fails,
# never passes on nothing. The status is `fail` if any check fails, else `plumbing` while the
# real-page checks wait on the page source (the test page), else `pass`.
#
# `measures.memory_split` (soak 40139563554's drift, 2026-10-25) is evidence, not a gate: from the
# compositor's `memory split` lines (once a minute: its own RSS and CEF's children's PSS by kind),
# the compositor-only and the CEF-only growth (the drive's last part against its first, as check 1
# reads memMb), and each dropout's step: the first line after it ended less the last line before it
# began, each line inside the gap between that dropout and its neighbouring faults. Check 1.split
# only asks that lines exist, in the drive, carrying the RSS and a CEF poll: zero lines read FAILS.
include "schedule";

def median: sort | if length == 0 then null elif length % 2 == 1 then .[(length - 1) / 2]
  else (.[length / 2 - 1] + .[length / 2]) / 2 end;
def check($id; $name; $problems; $measured):
  {id: $id, name: $name, status: (if ($problems | length) == 0 then "pass" else "fail" end),
   reason: (if ($problems | length) == 0 then null else ($problems | join("; ")) end), measured: $measured};
def pending($id; $name; $measured):
  {id: $id, name: $name, status: "pending",
   reason: "pending: page source (the C14 test page drives the plumbing only; the production page is the standalone web build on the harness Machine: page=real)",
   measured: $measured};
def overlaps($o; $d): $o.from_wall_ms <= $d.end_ms and $o.to_wall_ms >= $d.at_ms;
# An RFC 3339 UTC stamp with up to millisecond digits (the compositor's ts=), as epoch ms.
def stamp_ms: capture("^(?<s>[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2})(\\.(?<f>[0-9]+))?Z$")
  | ((.s + "Z") | fromdateiso8601) * 1000 + ((.f // "0") + "00" | .[0:3] | tonumber);
def kb_mb: if . == null then null else (. / 1024 * 10 | round) / 10 end;
def num: if type == "string" and test("^[0-9]+$") then tonumber else null end;
# The compositor log's `memory split` lines (driver::memory_line), each with its own ts=, as
# {at_ms, gen, rss_kb, anon_kb, file_kb, shmem_kb, threads, cef_poll, cef_kb: {kind: kb|null}, cef_total_kb}.
# An absent field (`-`) reads null; CEF's total is null without a poll, else the kinds' sum (a kind
# the poll found none of adds 0).
def memory_lines($log):
  [$log | split("\n")[] | select(test("(^|\\s)msg=memory split ")) | . as $l
   | ($l | capture("(^|\\s)ts=(?<ts>\\S+)") | .ts | try stamp_ms catch null) as $at
   | select($at != null)
   | ($l | split(" msg=memory split ")[1] | split(" ") | map(select(contains("=")) | split("=")
       | {key: .[0], value: (.[1:] | join("="))}) | from_entries) as $f
   | (["renderer", "gpu_process", "utility", "zygote", "other"]
      | map({key: ., value: ($f["cef_\(.)_kb"] | num)}) | from_entries) as $kinds
   | ($f.cef_poll | num) as $poll
   | {at_ms: $at, gen: ($f.input_gen | num), rss_kb: ($f.rss_kb | num), anon_kb: ($f.anon_kb | num),
      file_kb: ($f.file_kb | num), shmem_kb: ($f.shmem_kb | num), threads: ($f.threads | num),
      cef_poll: $poll, cef_kb: $kinds,
      cef_total_kb: (if $poll == null then null else ([$kinds[] | numbers] | add // 0) end)}];

if ($page != "real" and $page != "test") then error("verdict: page must be real or test, not \($page)") else . end
| ($t[0]) as $t | ($soak[0]) as $soak | ($report[0] // null) as $r | ($control[0] // null) as $c
| ($logscan[0] // null) as $ls
| ($drive_min * 60) as $drive_s | ($drive_s * 1000) as $drive_ms
| schedule_plan($soak; $schedule; $drive_s) as $plan
| ($plan.part_s * 1000) as $part_ms
| (1000 / $t.frame_period_ms) as $fps
| [$actions[] | select(.kind == "start")] as $starts
| [$actions[] | select(.kind == "end")] as $ends
| ($starts[0].at_ms // null) as $s
| (if $r == null then ["no report"] else [] end) as $no_report
| (if $s == null then ["no start action: the drive's windows are unknown"] else [] end) as $no_start
| ($r.heartbeats.series // []) as $beats
| (if $s == null then [] else [$beats[] | select(.received_ms >= $s and .received_ms < $s + $drive_ms)] end) as $drive_beats
| [$actions[] | select(.kind == "disconnect")] as $disc
| ($r.output_outages // []) as $outages
| ($r.log // {}) as $log
| memory_lines($complog) as $mem
| (if $s == null then [] else [$mem[] | select(.at_ms >= $s and .at_ms < $s + $drive_ms)] end) as $drive_mem
| [
  # The run itself: a report with frames and no checker failure, one start and one end, the drive
  # asked for, and the compositor's own end (its stopped beat says `ended`).
  ([$beats[] | select(.body.state == "stopped")] | last) as $stopped
  | check("inputs"; "the run";
      $no_report
      + (if $r != null and (($r.frames // 0) <= 0) then ["no frames decoded"] else [] end)
      + (if $r != null and (($r.checker_failures // []) | length) > 0 then ["the checker failed: \($r.checker_failures | join(", "))"] else [] end)
      + (if ($starts | length) != 1 or ($ends | length) != 1 then ["want one start and one end action, got \($starts | length) and \($ends | length)"] else [] end)
      + (if ($starts | length) == 1 and $starts[0].drive_s != $drive_s then ["the driver's drive was \($starts[0].drive_s) s, the verdict's \($drive_s) s"] else [] end)
      + (if ($starts | length) == 1 and ($starts[0].schedule // "soak") != $schedule then ["the driver ran the \($starts[0].schedule // "soak") schedule, the verdict judges \($schedule)"] else [] end)
      + (if $r != null and ($stopped.body.exitReason // null) != "ended" then ["the compositor did not end as asked (its stopped beat: \($stopped.body // null | tojson))"] else [] end);
      {frames: ($r.frames // null), starts: ($starts | length), ends: ($ends | length), drive_s: $drive_s,
       schedule: $schedule, exit_reason: ($stopped.body.exitReason // null)}),

  # 1. Memory: hour 3's median memMb within memory_drift_max of hour 1's (the drive's last part
  #    against its first: the hours of a 3-hour soak).
  ([$drive_beats[] | select(.received_ms < $s + $part_ms) | .body.memMb | numbers] | median) as $m1
  | ([$drive_beats[] | select(.received_ms >= $s + 2 * $part_ms) | .body.memMb | numbers] | median) as $m3
  | check("1"; "memory";
      $no_report + $no_start
      + (if $m1 == null then ["no memMb in hour 1's beats"] else [] end)
      + (if $m3 == null then ["no memMb in hour 3's beats"] else [] end)
      + (if $m1 != null and $m3 != null and (($m3 - $m1) | fabs) > $soak.memory_drift_max * $m1
         then ["memory drift: hour 3's median \($m3) MB is \((($m3 - $m1) / $m1 * 1000 | round) / 10) % from hour 1's \($m1) MB (at most \($soak.memory_drift_max * 100) %)"] else [] end);
      {hour1_median_mb: $m1, hour3_median_mb: $m3,
       drift_pct: (if $m1 == null or $m3 == null then null else (($m3 - $m1) / $m1 * 1000 | round) / 10 end),
       part_s: $plan.part_s}),

  # 1.split. The memory split's lines exist (measures.memory_split reads them): in the drive, one
  #    carrying the compositor's RSS and one carrying a CEF poll (the live CEF reader reached the
  #    sampler). A log with none fails: zero lines read is not evidence.
  check("1.split"; "memory split lines";
    $no_start
    + (if $s != null and ($drive_mem | length) == 0 then ["no memory split line in the drive (\($mem | length) in the whole compositor log)"] else [] end)
    + (if ($drive_mem | length) > 0 and ([$drive_mem[] | select(.rss_kb != null)] | length) == 0 then ["no memory split line in the drive carries the compositor's RSS"] else [] end)
    + (if ($drive_mem | length) > 0 and ([$drive_mem[] | select(.cef_poll != null)] | length) == 0 then ["no memory split line in the drive carries a CEF poll (the CEF reader never reached the sampler)"] else [] end);
    {lines: ($mem | length), in_drive: ($drive_mem | length),
     with_rss: ([$drive_mem[] | select(.rss_kb != null)] | length),
     with_cef_poll: ([$drive_mem[] | select(.cef_poll != null)] | length)}),

  # 2. Zero unexplained output gaps: every outage overlaps a dest disconnect and ends at most
  #    BUDGET_OUTPUT_RECONNECT_MS after that disconnect ended; no PTS gap.
  [$outages[] | . as $o | select([$disc[] | select(overlaps($o; .))] | length == 0)] as $unexplained
  | [$outages[] | . as $o | ([$disc[] | select(overlaps($o; .))] | first) as $d
     | select($d != null and $o.to_wall_ms > $d.end_ms + $t.budget_output_reconnect_ms)
     | {to_after_end_ms: ($o.to_wall_ms - $d.end_ms), slot: $d.slot}] as $late
  | check("2"; "output gaps";
      $no_report
      + (if ($unexplained | length) > 0 then ["unexplained output gap: \($unexplained | length) outage(s) overlap no dest disconnect (first \($unexplained[0].from_wall_ms) to \($unexplained[0].to_wall_ms))"] else [] end)
      + [$late[] | "an outage ended \(.to_after_end_ms) ms after its disconnect ended (slot \(.slot); budget \($t.budget_output_reconnect_ms) ms)"]
      + (if (($r.pts_gaps // []) | length) > 0 then ["pts gaps: \($r.pts_gaps | length)"] else [] end);
      {outages: ($outages | length), unexplained: ($unexplained | length), late: $late,
       pts_gaps: (($r.pts_gaps // []) | length), budget_ms: $t.budget_output_reconnect_ms}),

  # 3. No programme rebuild: fake-control saw exactly one session GET (one process; C09 builds the
  #    programme once per process).
  check("3"; "no programme rebuild";
    (if $c == null then ["no control state"]
     elif $c.session_fetches != 1 then ["programme rebuilt: fake-control saw \($c.session_fetches) session GETs (exactly one is one process)"]
     else [] end);
    {session_fetches: ($c.session_fetches // null), other_session_requests: ($c.other_session_requests // null),
     unauthorized_requests: ($c.unauthorized_requests // null)}),

  # 4. Dropped frames: the final droppedFrames (a counter: its largest value) under
  #    dropped_frames_max_ratio of the drive's frames at the thresholds' frame rate.
  ([$beats[] | .body.droppedFrames | numbers] | max) as $dropped
  | ($drive_s * $fps * $soak.dropped_frames_max_ratio) as $budget
  | check("4"; "dropped frames";
      $no_report
      + (if $dropped == null then ["no beat carried droppedFrames"]
         elif $dropped >= $budget then ["dropped frames: \($dropped) is at or over \($budget) (\($soak.dropped_frames_max_ratio * 100) % of \($drive_s) s at \($fps) fps)"]
         else [] end);
      {dropped_frames: $dropped, budget: $budget}),

  # 5. Anti-vacuity: the beats, every scheduled dropout and disconnect done, and each disconnect
  #    landed (packets dropped) and was felt (an output outage over it).
  ($drive_ms / $t.heartbeat_interval_ms * $soak.heartbeat_min_ratio) as $want_beats
  | check("5.beats"; "heartbeats";
      $no_report + $no_start
      + (if ($drive_beats | length) < $want_beats then ["heartbeats: \($drive_beats | length) in the drive, want at least \($want_beats)"] else [] end);
      {beats_in_drive: ($drive_beats | length), want: $want_beats, max_gap_ms: ($r.heartbeats.max_gap_ms // null)}),

  ([$actions[] | select(.kind == "dropout" and .ok == true) | .slot] | unique) as $done
  | [range(0; $plan.dropouts | length)] as $want
  | check("5.dropouts"; "camera dropouts";
      (if $done != $want then ["dropout actions: \($done | length) of \($want | length) done (missing slots \($want - $done))"] else [] end);
      {done: ($done | length), want: ($want | length)}),

  ([$disc[] | select(.ok == true)]) as $ok
  | ([$ok[] | .slot] | unique) as $done
  | [range(0; $plan.disconnects | length)] as $want
  | check("5.disconnects"; "dest disconnects";
      (if ($done - $want | length) > 0 then ["disconnect actions the plan does not have: slots \($done - $want) (the \($schedule) schedule plans \($want | length))"]
       elif $done != $want then ["disconnect actions: \($done | length) of \($want | length) done (missing slots \($want - $done))"] else [] end)
      + [$ok[] | select((.dropped_packets // 0) <= 0) | "disconnect slot \(.slot) dropped no packet (the fault never landed)"]
      + [$ok[] | . as $d | select([$outages[] | select(overlaps(.; $d))] | length == 0)
         | "disconnect slot \(.slot) caused no output outage (a fault nothing felt)"];
      {done: ($done | length), want: ($want | length), dropped_packets: [$ok[] | .dropped_packets],
       families: ([$ok[] | .families // []] | add // [] | unique)}),

  # (a) `cef alive` lines for at least cef_alive_min_ratio of the drive / cef_alive_every_ms.
  ([($log.cef_alive // [])[] | select($s != null and .at_ms >= $s and .at_ms < $s + $drive_ms)] | length) as $alive
  | ($drive_ms / $t.cef_alive_every_ms * $soak.cef_alive_min_ratio) as $want_alive
  | if $page == "test" then pending("5a"; "cef alive"; {cef_alive_in_drive: $alive, want: $want_alive})
    else check("5a"; "cef alive";
      $no_report + $no_start
      + (if $alive < $want_alive then ["cef alive: \($alive) lines in the drive, want at least \($want_alive)"] else [] end);
      {cef_alive_in_drive: $alive, want: $want_alive}) end,

  # (b) graphics never reads failed; every `overlay hidden` names hang, crash or recycle, and no
  #     line is bare (logscan counts every `overlay hidden` line; logfacts parses only those with a
  #     reason and a generation). The hides are printed by reason.
  ([$beats[] | select(.body.graphics == "failed")] | length) as $failed_beats
  | (($log.overlay_hidden // []) | group_by(.reason) | map({key: .[0].reason, value: length}) | from_entries) as $by_reason
  | [($log.overlay_hidden // [])[] | select(.reason as $x | $soak.hidden_reasons | index($x) | not)] as $bad
  | ((($ls.overlay_hidden_lines // 0) - (($log.overlay_hidden // []) | length))) as $bare
  | {failed_beats: $failed_beats, hides_by_reason: $by_reason, bare_hides: $bare,
     log_lines: ($log.lines // 0), logscan_lines: ($ls.lines // 0)} as $m
  | if $page == "test" then pending("5b"; "graphics and hides"; $m)
    else check("5b"; "graphics and hides";
      $no_report
      + (if ($log.lines // 0) <= 0 or ($ls.lines // 0) <= 0 then ["no compositor log (an empty log reads clean)"] else [] end)
      + (if $failed_beats > 0 then ["graphics read failed in \($failed_beats) beat(s)"] else [] end)
      + [$bad[] | "overlay hidden reason=\(.reason) gen=\(.gen) at \(.at_ms)"]
      + (if $bare > 0 then ["\($bare) overlay hidden line(s) without a reason (a build older than C05b's amendment)"] else [] end);
      $m) end,

  # (c) The score driver recorded at least deliveries_per_min deliveries in every minute of the drive,
  #     and the PAGE received them: its own feed-health signal (`seazn:score-stale`, which the beats
  #     carry as scoreStale) reached the beats in the drive, and never read true. The driver's 200s
  #     alone say the API took the score, not that the page polled it back.
  ($drive_min | floor) as $minutes
  | [range(0; $minutes) as $k
     | select([$deliveries[] | select(.ok == true and $s != null and .at_ms >= $s + $k * 60000 and .at_ms < $s + ($k + 1) * 60000)]
         | length < $soak.deliveries_per_min) | $k] as $short
  | [$drive_beats[] | select(.body | has("scoreStale"))] as $stale_carried
  | [$stale_carried[] | select(.body.scoreStale != false)] as $stale
  | {deliveries_ok: ([$deliveries[] | select(.ok == true)] | length), deliveries_refused: ([$deliveries[] | select(.ok != true)] | length),
     minutes: $minutes, minutes_short: ($short | length),
     stale_beats_carried: ($stale_carried | length), stale_beats_true: ($stale | length)} as $m
  | if $page == "test" then pending("5c"; "score deliveries"; $m)
    else check("5c"; "score deliveries";
      $no_start
      + (if ($short | length) > 0 then ["deliveries: \($short | length) of \($minutes) minutes had fewer than \($soak.deliveries_per_min) (first: minute \($short[0]))"] else [] end)
      + (if ($stale_carried | length) == 0 then ["no drive beat carried scoreStale: the page never reported its feed's health"] else [] end)
      + (if ($stale | length) > 0 then ["score stale: \($stale | length) drive beat(s) say the page had no good live-data update for the page's stale budget (first at \($stale[0].received_ms))"] else [] end);
      $m) end,

  # (d) Once a sample slot (sample_every_s: the sampler's own unit), a sample of the programme's
  #     overlay region differs from the camera-only reference: the overlay is on air.
  ($drive_s / $soak.sample_every_s | floor) as $slots
  | [range(0; $slots) as $k | select([$samples[] | select(.minute == $k and .differs == true)] | length == 0) | $k] as $off
  | {samples: ($samples | length), minutes: $slots, minutes_off_air: ($off | length)} as $m
  | if $page == "test" then pending("5d"; "overlay on air"; $m)
    else check("5d"; "overlay on air";
      (if ($off | length) > 0 then ["overlay off air: \($off | length) of \($slots) minutes had no sample that differs from the camera-only reference (first: minute \($off[0]))"] else [] end);
      $m) end,

  # 6. No step down: every beat's rung is rung_ok (`full`); a stepped beat is web's sizing alarm.
  [$beats[] | select(.body | has("rung"))] as $rung_beats
  | [$rung_beats[] | select(.body.rung != $soak.rung_ok)] as $stepped
  | check("6"; "no step down";
      $no_report
      + (if ($rung_beats | length) == 0 then ["no beat carried a rung"] else [] end)
      + (if ($stepped | length) > 0 then ["sizing alarm: the guest is short of CPU (C17 runbook): \($stepped | length) beat(s) off \($soak.rung_ok) (\([$stepped[] | .body.rung] | unique | join(", ")), first at \($stepped[0].received_ms))"] else [] end);
      {rung_beats: ($rung_beats | length), stepped: ($stepped | length)})
] as $checks
# The memory split's measures (see the header): evidence for the owner's ruling on check 1.
| ($drive_mem | map(select(.at_ms < $s + $part_ms))) as $mem_first
| ($drive_mem | map(select(.at_ms >= $s + 2 * $part_ms))) as $mem_last
| def part($ls): {rss_mb: ([$ls[] | .rss_kb | numbers] | median | kb_mb), anon_mb: ([$ls[] | .anon_kb | numbers] | median | kb_mb),
    file_mb: ([$ls[] | .file_kb | numbers] | median | kb_mb), shmem_mb: ([$ls[] | .shmem_kb | numbers] | median | kb_mb),
    threads: ([$ls[] | .threads | numbers] | median), cef_mb: ([$ls[] | .cef_total_kb | numbers] | median | kb_mb),
    cef_renderer_mb: ([$ls[] | select(.cef_poll != null) | .cef_kb.renderer // 0] | median | kb_mb),
    cef_gpu_process_mb: ([$ls[] | select(.cef_poll != null) | .cef_kb.gpu_process // 0] | median | kb_mb),
    cef_utility_mb: ([$ls[] | select(.cef_poll != null) | .cef_kb.utility // 0] | median | kb_mb),
    cef_zygote_mb: ([$ls[] | select(.cef_poll != null) | .cef_kb.zygote // 0] | median | kb_mb),
    cef_other_mb: ([$ls[] | select(.cef_poll != null) | .cef_kb.other // 0] | median | kb_mb)};
  part($mem_first) as $p1 | part($mem_last) as $p3
| ($p1 | with_entries(.value = (if .value == null or $p3[.key] == null then null else (($p3[.key] - .value) * 10 | round) / 10 end))) as $growth
| ([$actions[] | select((.kind == "dropout" or .kind == "disconnect") and (.at_ms | type) == "number" and (.end_ms | type) == "number")]
   | sort_by(.at_ms)) as $faults
| [range(0; $faults | length) as $i | $faults[$i] | select(.kind == "dropout") | . as $d
   | (if $i > 0 then $faults[$i - 1].end_ms else null end) as $from
   | (if $i + 1 < ($faults | length) then $faults[$i + 1].at_ms else ($s // 0) + $drive_ms end) as $to
   | ([$mem[] | select(.at_ms < $d.at_ms and ($from == null or .at_ms >= $from))] | last) as $b
   | ([$mem[] | select(.at_ms >= $d.end_ms and .at_ms < $to)] | first) as $a
   | def step($k): if $b[$k] == null or $a[$k] == null then null else (($a[$k] - $b[$k]) / 1024 * 10 | round) / 10 end;
     {slot: $d.slot, at_ms: $d.at_ms, before_ms: ($b.at_ms // null), after_ms: ($a.at_ms // null),
      rss_step_mb: step("rss_kb"), anon_step_mb: step("anon_kb"), file_step_mb: step("file_kb"),
      cef_step_mb: step("cef_total_kb"), gen_before: ($b.gen // null), gen_after: ($a.gen // null)}] as $steps
| [$steps[] | select(.rss_step_mb != null)] as $measured
| ($measured | length) as $nm
| def mean($xs): if ($xs | length) == 0 then null else (($xs | add) / ($xs | length) * 10 | round) / 10 end;
  def sum1($xs): if ($xs | length) == 0 then null else (($xs | add) * 10 | round) / 10 end;
  {lines: ($mem | length), lines_in_drive: ($drive_mem | length),
   compositor: {first_part: ($p1 | {rss_mb, anon_mb, file_mb, shmem_mb, threads}), last_part: ($p3 | {rss_mb, anon_mb, file_mb, shmem_mb, threads}),
                growth: ($growth | {rss_mb, anon_mb, file_mb, shmem_mb, threads})},
   cef: {first_part: ($p1 | with_entries(select(.key | startswith("cef_")))), last_part: ($p3 | with_entries(select(.key | startswith("cef_")))),
         growth: ($growth | with_entries(select(.key | startswith("cef_"))))},
   steps: {dropouts: ($steps | length), measured: $nm, unmeasured: (($steps | length) - $nm),
           rss_mean_mb: mean([$measured[] | .rss_step_mb]), rss_median_mb: ([$measured[] | .rss_step_mb] | median),
           rss_sum_mb: sum1([$measured[] | .rss_step_mb]),
           rss_first_half_mean_mb: mean([$measured[:($nm / 2 | floor)][] | .rss_step_mb]),
           rss_second_half_mean_mb: mean([$measured[($nm / 2 | floor):][] | .rss_step_mb]),
           anon_mean_mb: mean([$measured[] | .anon_step_mb | numbers]),
           cef_mean_mb: mean([$measured[] | .cef_step_mb | numbers]), cef_sum_mb: sum1([$measured[] | .cef_step_mb | numbers]),
           rebuilds: ([$measured[] | select(.gen_before != null and .gen_after != null) | .gen_after - .gen_before] | add)},
   dropouts: $steps} as $memory_split
| {status: (if any($checks[]; .status == "fail") then "fail" elif any($checks[]; .status == "pending") then "plumbing" else "pass" end),
   page: $page, drive_min: $drive_min, schedule: $schedule,
   plan: {dropouts: ($plan.dropouts | length), disconnects: $plan.disconnects, part_s: $plan.part_s},
   checked: ($checks | length), checks: $checks, measures: {memory_split: $memory_split}}
