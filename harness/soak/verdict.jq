# The soak's verdict (C15 brief: "Pass", criterion 6, and the real-template checks (a) to (d)).
# Run through harness/soak/verdict.sh, which binds:
#   $report      the checker's report.json (the harness Machine's; [] when the file is missing)
#   $actions     driver.sh's actions.jsonl: start, dropout, disconnect and end, each with its wall time
#   $control     fake-control's /control/state at the end (control-state.json)
#   $deliveries  the score driver's deliveries.jsonl (check (c); empty on the test page)
#   $samples     the overlay sampler's overlay-samples.jsonl (check (d); empty on the test page)
#   $logscan     logscan.json: the compositor log's raw `overlay hidden` line count (check (b))
#   $t           harness/thresholds.json        $soak  harness/soak/soak.json
#   $drive_min   the run's drive, in minutes (the duration env: SOAK_DRIVE_MIN)
#   $page        "real" (the production overlay page) or "test" (C14's overlay-test.html: plumbing)
# Every number comes from $t, $soak or $drive_min: none is typed here. Each check answers
# {id, name, status: pass|fail|pending, reason, measured}; a check whose data is missing fails,
# never passes on nothing. The status is `fail` if any check fails, else `plumbing` while the
# real-page checks wait on the page source (the test page), else `pass`.
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

if ($page != "real" and $page != "test") then error("verdict: page must be real or test, not \($page)") else . end
| ($t[0]) as $t | ($soak[0]) as $soak | ($report[0] // null) as $r | ($control[0] // null) as $c
| ($logscan[0] // null) as $ls
| ($drive_min * 60) as $drive_s | ($drive_s * 1000) as $drive_ms
| soak_plan($soak; $drive_s) as $plan
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
      + (if $r != null and ($stopped.body.exitReason // null) != "ended" then ["the compositor did not end as asked (its stopped beat: \($stopped.body // null | tojson))"] else [] end);
      {frames: ($r.frames // null), starts: ($starts | length), ends: ($ends | length), drive_s: $drive_s,
       exit_reason: ($stopped.body.exitReason // null)}),

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
      (if $done != $want then ["disconnect actions: \($done | length) of \($want | length) done (missing slots \($want - $done))"] else [] end)
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

  # (c) The score driver recorded at least deliveries_per_min deliveries in every minute of the drive.
  ($drive_min | floor) as $minutes
  | [range(0; $minutes) as $k
     | select([$deliveries[] | select(.ok == true and $s != null and .at_ms >= $s + $k * 60000 and .at_ms < $s + ($k + 1) * 60000)]
         | length < $soak.deliveries_per_min) | $k] as $short
  | {deliveries_ok: ([$deliveries[] | select(.ok == true)] | length), deliveries_refused: ([$deliveries[] | select(.ok != true)] | length),
     minutes: $minutes, minutes_short: ($short | length)} as $m
  | if $page == "test" then pending("5c"; "score deliveries"; $m)
    else check("5c"; "score deliveries";
      $no_start
      + (if ($short | length) > 0 then ["deliveries: \($short | length) of \($minutes) minutes had fewer than \($soak.deliveries_per_min) (first: minute \($short[0]))"] else [] end);
      $m) end,

  # (d) Once a minute, a sample of the programme's overlay region differs from the camera-only
  #     reference: the overlay is on air.
  [range(0; $minutes) as $k | select([$samples[] | select(.minute == $k and .differs == true)] | length == 0) | $k] as $off
  | {samples: ($samples | length), minutes: $minutes, minutes_off_air: ($off | length)} as $m
  | if $page == "test" then pending("5d"; "overlay on air"; $m)
    else check("5d"; "overlay on air";
      (if ($off | length) > 0 then ["overlay off air: \($off | length) of \($minutes) minutes had no sample that differs from the camera-only reference (first: minute \($off[0]))"] else [] end);
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
| {status: (if any($checks[]; .status == "fail") then "fail" elif any($checks[]; .status == "pending") then "plumbing" else "pass" end),
   page: $page, drive_min: $drive_min,
   plan: {dropouts: ($plan.dropouts | length), disconnects: $plan.disconnects, part_s: $plan.part_s},
   checked: ($checks | length), checks: $checks}
