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
#   $memory_mb   the compositor Machine's memory, in MB (soak.yml's C_MEM): check 1.projection's budget
#   $page       "real" (the production overlay page) or "test" (C14's overlay-test.html: plumbing)
#   $schedule    "soak", "dropouts" or "none" (schedule.jq's schedule_plan: what the driver was to do)
# Every number comes from $t, $soak or $drive_min: none is typed here. Each check answers
# {id, name, status: pass|fail|pending, reason, measured}; a check whose data is missing fails,
# never passes on nothing. The status is `fail` if any check fails, else `plumbing` while the
# real-page checks wait on the page source (the test page), else `pass`.
#
# `measures.memory_split` (soak 40139563554's drift, 2026-10-25) is evidence, not a gate. It reads the
# compositor's `memory split` lines (once a minute: its own RSS, and CEF's children's PSS by kind with
# how many were listed and read) and the beats' memMb, and reports:
# - the compositor-only and the CEF-only growth: the drive's last part against its first, medians,
#   as check 1 reads memMb; and CEF's children by RSS (tree_mb less the compositor's RSS), which
#   needs no PSS read and cross-checks the PSS;
# - each instant's step from the lines: the first line at least after_s[0] after the instant less the
#   last line at most before_s[1] before it (soak.json's memory_step), each inside the gap to the
#   neighbouring faults. An instant is a dropout that was done (ok), or on schedule=none (the
#   experiment's control) each instant dropout_plan would have had, where nothing happened;
# - the control: in each gap between two instants with two distinct lines, the later line less the
#   earlier, so the growth over time alone can be scaled to the steps' own span and taken off them
#   (`excess_mb`). A run with steady growth and no dropout effect reads excess about 0;
# - the diagnosis's own windowed step on the beats' memMb (scripts/epoch.py): the median over
#   [before_s] around the instant against the median over [after_s]. On schedule=dropouts its windows
#   meet the next dropout's, so its control is a schedule=none run's (the same windows, at the same
#   instants, with no dropout).
# Check 1.split judges that evidence on the experiment's schedules (dropouts, none) only, never the
# soak's: lines in the drive carrying the RSS, a CEF poll that advances, a CEF PSS read, no kind
# listed and unread, and a step measured both ways. Zero read FAILS; the memory gate (check 1) is
# unchanged.
#
# Two clocks meet here. A memory line's ts= is the compositor Machine's clock; the actions' at_ms and
# end_ms and the beats' received_ms are the harness Machine's. The skew between the two is not
# measured; the line windows' margins (10 s before, 30 s after) assume it is far smaller.
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
def round1: if . == null then null else (. * 10 | round) / 10 + 0 end;
def kb_mb: if . == null then null else . / 1024 | round1 end;
def num: if type == "string" and test("^[0-9]+$") then tonumber else null end;
def mean_of: if length == 0 then null else add / length end;
# The compositor log's `memory split` lines (driver::memory_line), each with its own ts=, as
# {at_ms, gen, tree_mb, rss_kb, anon_kb, file_kb, shmem_kb, threads, cef_poll,
#  cef: {kind: {kb, n, listed}}, cef_kb: {kind: kb|0|null}, cef_total_kb, unread: [kind], children_kb}.
# An absent field (`-`) reads null. Per kind, cef_kb is the PSS when the poll read one child of it or
# more, 0 when it listed none of that kind, and null when it listed some and read none: unknown,
# never 0 (PR #82 review I2). cef_total_kb sums the kinds read: null without a poll, and null when
# the poll read no PSS at all. unread names the kinds listed and not read. children_kb is tree_mb
# less the compositor's RSS: CEF's children by RSS.
def memory_lines($log):
  [$log | split("\n")[] | select(test("(^|\\s)msg=memory split ")) | . as $l
   | ($l | capture("(^|\\s)ts=(?<ts>\\S+)") | .ts | try stamp_ms catch null) as $at
   | select($at != null)
   | ($l | split(" msg=memory split ")[1] | split(" ") | map(select(contains("=")) | split("=")
       | {key: .[0], value: (.[1:] | join("="))}) | from_entries) as $f
   | ($f.cef_poll | num) as $poll
   | (["renderer", "gpu_process", "utility", "zygote", "other"]
      | map(. as $k | {key: $k, value: {kb: ($f["cef_\($k)_kb"] | num), n: ($f["cef_\($k)_n"] | num),
          listed: ($f["cef_\($k)_listed"] | num)}}) | from_entries) as $cef
   | ($cef | map_values(if $poll == null then null elif (.n // 0) > 0 and .kb != null then .kb
        elif .listed == 0 then 0 else null end)) as $kb
   | [$cef[] | select($poll != null and (.n // 0) > 0 and .kb != null) | .kb] as $read
   | ($f.tree_mb | num) as $tree | ($f.rss_kb | num) as $rss
   | {at_ms: $at, gen: ($f.input_gen | num), tree_mb: $tree, rss_kb: $rss, anon_kb: ($f.anon_kb | num),
      file_kb: ($f.file_kb | num), shmem_kb: ($f.shmem_kb | num), threads: ($f.threads | num),
      cef_poll: $poll, cef: $cef, cef_kb: $kb,
      cef_total_kb: (if ($read | length) == 0 then null else ($read | add) end),
      unread: [$cef | to_entries[] | select($poll != null and (.value.listed // 0) > 0 and (.value.n // 0) == 0) | .key],
      children_kb: (if $tree == null or $rss == null then null else $tree * 1024 - $rss end)}];

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
# The beats the drive must carry (check 5.beats), and the memMb beats check 1.projection must fit.
| ($drive_ms / $t.heartbeat_interval_ms * $soak.heartbeat_min_ratio) as $want_beats
| [$actions[] | select(.kind == "disconnect")] as $disc
| ($r.output_outages // []) as $outages
| ($r.log // {}) as $log
| memory_lines($complog) as $mem
| (if $s == null then [] else [$mem[] | select(.at_ms >= $s and .at_ms < $s + $drive_ms)] end) as $drive_mem
# The memory split's measures (see the header): evidence for the owner's ruling on check 1.
| ($schedule == "dropouts" or $schedule == "none") as $experiment
| ($soak.memory_step) as $w
| ($w.before_s[0] * 1000) as $b0 | ($w.before_s[1] * 1000) as $b1
| ($w.after_s[0] * 1000) as $a0 | ($w.after_s[1] * 1000) as $a1
| (if $schedule == "none" then "phantom" else "dropout" end) as $ikind
| (if $schedule == "none" and $s != null
   then [dropout_plan($soak; $drive_s).dropouts | to_entries[]
         | {kind: "phantom", slot: .key, at_ms: ($s + .value * 1000), end_ms: ($s + (.value + $soak.dropout_pause_s) * 1000), ok: true}]
   else [] end) as $phantoms
| (([$actions[] | select((.kind == "dropout" or .kind == "disconnect") and (.at_ms | type) == "number" and (.end_ms | type) == "number")]
    + $phantoms) | sort_by(.at_ms)) as $fences
| [range(0; $fences | length) as $i | $fences[$i] | select(.kind == $ikind and .ok == true) | . as $d
   | (if $i > 0 then ([$fences[$i - 1].end_ms, $fences[$i - 1].at_ms + $a0] | max) else null end) as $from
   | (if $i + 1 < ($fences | length) then $fences[$i + 1].at_ms + $b1 else ($s // 0) + $drive_ms end) as $to
   | ([$mem[] | select(.at_ms <= $d.at_ms + $b1 and ($from == null or .at_ms >= $from))] | last) as $bl
   | ([$mem[] | select(.at_ms >= $d.at_ms + $a0 and .at_ms < $to)] | first) as $al
   | def d($k): if $bl == null or $al == null or $bl[$k] == null or $al[$k] == null then null else $al[$k] - $bl[$k] end;
   ([$beats[] | select(.received_ms >= $d.at_ms + $b0 and .received_ms < $d.at_ms + $b1) | .body.memMb | numbers]) as $wb
   | ([$beats[] | select(.received_ms >= $d.at_ms + $a0 and .received_ms < $d.at_ms + $a1) | .body.memMb | numbers]) as $wa
   | {fi: $i, slot: $d.slot, at_ms: $d.at_ms, bl: $bl, al: $al,
      span_s: (if $bl == null or $al == null then null else ($al.at_ms - $bl.at_ms) / 1000 end),
      rss_kb: d("rss_kb"), anon_kb: d("anon_kb"), file_kb: d("file_kb"), cef_kb: d("cef_total_kb"),
      children_kb: d("children_kb"), gen_before: ($bl.gen // null), gen_after: ($al.gen // null),
      beats_before: ($wb | length), beats_after: ($wa | length),
      tree_window_mb: (if ($wb | length) == 0 or ($wa | length) == 0 then null else ($wa | median) - ($wb | median) end)}] as $rows
| ($rows | map({key: (.fi | tostring), value: .}) | from_entries) as $by_fence
| [$rows[] | . as $r | $by_fence[($r.fi + 1) | tostring] as $n | select($n != null) | {after_slot: $r.slot, from: $r.al, to: $n.bl}] as $gaps
| [$gaps[] | select(.from != null and .to != null and .from.at_ms < .to.at_ms) | . as $g
   | def d($k): if $g.from[$k] == null or $g.to[$k] == null then null else $g.to[$k] - $g.from[$k] end;
   {after_slot, from_ms: .from.at_ms, to_ms: .to.at_ms, span_s: ((.to.at_ms - .from.at_ms) / 1000),
    rss_kb: d("rss_kb"), anon_kb: d("anon_kb"), cef_kb: d("cef_total_kb")}] as $ctl
| [$rows[] | select(.rss_kb != null)] as $measured
| ($measured | length) as $nm
| [$rows[] | select(.tree_window_mb != null)] as $wmeasured
| ($drive_mem | map(select(.cef_poll != null))) as $polled
| [range(1; $polled | length) as $i | select($polled[$i].cef_poll <= $polled[$i - 1].cef_poll)
   | {at_ms: $polled[$i].at_ms, from: $polled[$i - 1].cef_poll, to: $polled[$i].cef_poll}] as $stale
| [$drive_mem[] | select((.unread | length) > 0)] as $unread
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
  #    against its first: the hours of a 3-hour soak). The bound is 40 % (owner, 2026-10-25: "Up to
  #    20% grew is fine", then "Machine is solely for relay so 40% is fine"; it was 10 %), and a
  #    drift exactly at it passes (`>`).
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

  # 1.projection. Memory at the session cap (owner ruling A, 2026-10-25): the drive's memMb fitted
  #    against time by least squares (x: hours since the start action), and the line projected to
  #    memory_projection.session_cap_min (a livestream's 5 h), within memory_budget_fraction of the
  #    Machine's $memory_mb. The fit needs check 5.beats's floor of memMb beats, and a spread in time
  #    to fit; fewer, none, or all at one instant fail (never a projection of nothing).
  ($soak.memory_projection) as $p
  | [$drive_beats[] | select((.body.memMb | type) == "number") | {x: ((.received_ms - $s) / 3600000), y: .body.memMb}] as $pts
  | ($pts | length) as $n
  | ($memory_mb * $p.memory_budget_fraction) as $budget
  | (if $n == 0 then null else ([$pts[] | .x] | add) / $n end) as $xm
  | (if $n == 0 then null else ([$pts[] | .y] | add) / $n end) as $ym
  | (if $n == 0 then 0 else [$pts[] | (.x - $xm) * (.x - $xm)] | add end) as $sxx
  | (if $sxx == 0 then null else ([$pts[] | (.x - $xm) * (.y - $ym)] | add) / $sxx end) as $slope
  | (if $slope == null then null else $ym - $slope * $xm end) as $intercept
  | (if $slope == null then null else $intercept + $slope * $p.session_cap_min / 60 end) as $projected
  | check("1.projection"; "memory projection";
      $no_report + $no_start
      + (if $n < $want_beats then ["memory projection: \($n) drive beats carry memMb, want at least \($want_beats) (check 5.beats's floor)"] else [] end)
      + (if $n > 0 and $slope == null then ["memory projection: \($n) memMb beat(s) with no spread in time to fit"] else [] end)
      + (if $projected != null and $projected > $budget
         then ["memory projection: the drive's fit (\($slope | round1) MB/h from \($intercept | round1) MB, \($n) beats) reaches \($projected | round1) MB at the session cap of \($p.session_cap_min) min, over the budget of \($budget) MB (\($p.memory_budget_fraction * 100) % of the Machine's \($memory_mb) MB)"] else [] end);
      {beats_fitted: $n, want_beats: $want_beats, slope_mb_per_h: ($slope | round1), intercept_mb: ($intercept | round1),
       session_cap_min: $p.session_cap_min, projected_mb: ($projected | round1), budget_mb: $budget, machine_memory_mb: $memory_mb}),

  # 1.split. The memory experiment's evidence (schedule dropouts or none; never the soak's, whose
  #    page=real log the runner may withhold): in the drive, lines carrying the compositor's RSS and
  #    a CEF poll; the poll advancing from line to line (a stuck poll thread would read CEF as flat);
  #    a CEF child's PSS read, and no kind listed with none of its children read (an unreadable
  #    rollup reads unknown, never 0); and at least one instant's step measured from the lines and
  #    one from the beats' windows. Zero read is not evidence: each fails.
  (if $experiment then
    check("1.split"; "memory split evidence";
      $no_start
      + (if $s != null and ($drive_mem | length) == 0 then ["no memory split line in the drive (\($mem | length) in the whole compositor log\(if $complog == "" then "; the log is empty or missing (on page=real the runner withholds one the harness did not scan)" else "" end))"] else [] end)
      + (if ($drive_mem | length) > 0 and ([$drive_mem[] | select(.rss_kb != null)] | length) == 0 then ["no memory split line in the drive carries the compositor's RSS"] else [] end)
      + (if ($drive_mem | length) > 0 and ($polled | length) == 0 then ["no memory split line in the drive carries a CEF poll (the CEF reader never reached the sampler)"] else [] end)
      + (if ($polled | length) > 0 and ([$drive_mem[] | select(.cef_total_kb != null)] | length) == 0 then ["no memory split line in the drive carries a CEF child's PSS (\($polled | length) polled, none read: CEF would read as flat)"] else [] end)
      + (if ($unread | length) > 0 then ["CEF listed children it read no PSS of in \($unread | length) of \($drive_mem | length) drive lines (first at \($unread[0].at_ms): \($unread[0] as $u | $u.unread | map("\(.) listed \($u.cef[.].listed), read 0") | join(", ")))"] else [] end)
      + (if ($stale | length) > 0 then ["the CEF poll did not advance between \($stale | length) pair(s) of drive lines (first at \($stale[0].at_ms): poll \($stale[0].from), then \($stale[0].to))"] else [] end)
      + (if $s != null and $nm == 0 then ["no \($ikind) step measured: 0 of \($rows | length) \($ikind)s have a memory split line on both sides"] else [] end)
      + (if $s != null and ($wmeasured | length) == 0 then ["no windowed step measured: 0 of \($rows | length) \($ikind)s have memMb beats in both windows"] else [] end);
      {lines: ($mem | length), in_drive: ($drive_mem | length),
       with_rss: ([$drive_mem[] | select(.rss_kb != null)] | length), with_cef_poll: ($polled | length),
       with_cef_pss: ([$drive_mem[] | select(.cef_total_kb != null)] | length), unread_lines: ($unread | length),
       stale_pairs: ($stale | length), instants: ($rows | length), steps_measured: $nm, windows_measured: ($wmeasured | length)})
   else empty end),

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
  check("5.beats"; "heartbeats";
      $no_report + $no_start
      + (if ($drive_beats | length) < $want_beats then ["heartbeats: \($drive_beats | length) in the drive, want at least \($want_beats)"] else [] end);
      {beats_in_drive: ($drive_beats | length), want: $want_beats, max_gap_ms: ($r.heartbeats.max_gap_ms // null)}),

  ([$actions[] | select(.kind == "dropout" and .ok == true) | .slot] | unique) as $done
  | [range(0; $plan.dropouts | length)] as $want
  | check("5.dropouts"; "camera dropouts";
      (if ($done - $want | length) > 0 then ["dropout actions the plan does not have: slots \($done - $want) (the \($schedule) schedule plans \($want | length))"]
       elif $done != $want then ["dropout actions: \($done | length) of \($want | length) done (missing slots \($want - $done))"] else [] end);
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
| ($drive_mem | map(select(.at_ms < $s + $part_ms))) as $mem_first
| ($drive_mem | map(select(.at_ms >= $s + 2 * $part_ms))) as $mem_last
| def part($ls): {rss_mb: ([$ls[] | .rss_kb | numbers] | median | kb_mb), anon_mb: ([$ls[] | .anon_kb | numbers] | median | kb_mb),
    file_mb: ([$ls[] | .file_kb | numbers] | median | kb_mb), shmem_mb: ([$ls[] | .shmem_kb | numbers] | median | kb_mb),
    threads: ([$ls[] | .threads | numbers] | median), cef_mb: ([$ls[] | .cef_total_kb | numbers] | median | kb_mb),
    cef_renderer_mb: ([$ls[] | .cef_kb.renderer | numbers] | median | kb_mb),
    cef_gpu_process_mb: ([$ls[] | .cef_kb.gpu_process | numbers] | median | kb_mb),
    cef_utility_mb: ([$ls[] | .cef_kb.utility | numbers] | median | kb_mb),
    cef_zygote_mb: ([$ls[] | .cef_kb.zygote | numbers] | median | kb_mb),
    cef_other_mb: ([$ls[] | .cef_kb.other | numbers] | median | kb_mb),
    cef_children_rss_mb: ([$ls[] | .children_kb | numbers] | median | kb_mb)};
  part($mem_first) as $p1 | part($mem_last) as $p3
| ($p1 | with_entries(.value = (if .value == null or $p3[.key] == null then null else ($p3[.key] - .value) | round1 end))) as $growth
| ([$measured[] | .span_s] | mean_of) as $span
# A control's rate (kB a second, pooled over the gaps with two lines), the growth it predicts over the
# steps' mean span, and the steps' mean less that prediction.
| def control($k): ([$ctl[] | select(.[$k] != null)]) as $c
    | (if ($c | length) == 0 then null else ([$c[] | .[$k]] | add) / ([$c[] | .span_s] | add) end) as $r
    | ([$measured[] | .[$k] | numbers] | mean_of) as $m
    | {gaps: ($c | length), rate_mb_per_min: (if $r == null then null else $r * 60 / 1024 | round1 end),
       step_mean_mb: ($m | kb_mb),
       at_step_span_mb: (if $r == null or $span == null then null else $r * $span / 1024 | round1 end),
       excess_mb: (if $r == null or $span == null or $m == null then null else ($m - $r * $span) / 1024 | round1 end)};
  def half($xs; $first): ($xs | length) as $n | if $first then $xs[:($n / 2 | floor)] else $xs[($n / 2 | floor):] end;
  {lines: ($mem | length), lines_in_drive: ($drive_mem | length),
   compositor: {first_part: ($p1 | {rss_mb, anon_mb, file_mb, shmem_mb, threads}), last_part: ($p3 | {rss_mb, anon_mb, file_mb, shmem_mb, threads}),
                growth: ($growth | {rss_mb, anon_mb, file_mb, shmem_mb, threads})},
   cef: {first_part: ($p1 | with_entries(select(.key | startswith("cef_")))), last_part: ($p3 | with_entries(select(.key | startswith("cef_")))),
         growth: ($growth | with_entries(select(.key | startswith("cef_"))))},
   steps: {of: "\($ikind)s", instants: ($rows | length), measured: $nm, unmeasured: (($rows | length) - $nm),
           span_mean_s: ($span | round1),
           rss_mean_mb: ([$measured[] | .rss_kb] | mean_of | kb_mb), rss_median_mb: ([$measured[] | .rss_kb] | median | kb_mb),
           rss_sum_mb: ([$measured[] | .rss_kb] | add | kb_mb),
           rss_first_half_mean_mb: ([half($measured; true)[] | .rss_kb] | mean_of | kb_mb),
           rss_second_half_mean_mb: ([half($measured; false)[] | .rss_kb] | mean_of | kb_mb),
           anon_mean_mb: ([$measured[] | .anon_kb | numbers] | mean_of | kb_mb),
           cef_mean_mb: ([$measured[] | .cef_kb | numbers] | mean_of | kb_mb), cef_sum_mb: ([$measured[] | .cef_kb | numbers] | add | kb_mb),
           children_mean_mb: ([$measured[] | .children_kb | numbers] | mean_of | kb_mb),
           rebuilds: ([$measured[] | select(.gen_before != null and .gen_after != null) | .gen_after - .gen_before] | add)},
   control: {gaps: ($gaps | length), measured: ($ctl | length), span_mean_s: ([$ctl[] | .span_s] | mean_of | round1),
             rss: control("rss_kb"), anon: control("anon_kb"), cef: control("cef_kb")},
   window: {before_s: $w.before_s, after_s: $w.after_s, measured: ($wmeasured | length),
            mean_mb: ([$wmeasured[] | .tree_window_mb] | mean_of | round1), median_mb: ([$wmeasured[] | .tree_window_mb] | median | round1),
            sum_mb: ([$wmeasured[] | .tree_window_mb] | add | round1),
            first_half_mean_mb: ([half($wmeasured; true)[] | .tree_window_mb] | mean_of | round1),
            second_half_mean_mb: ([half($wmeasured; false)[] | .tree_window_mb] | mean_of | round1)},
   gaps: [$ctl[] | {after_slot, from_ms, to_ms, span_s, rss_mb: (.rss_kb | kb_mb), anon_mb: (.anon_kb | kb_mb), cef_mb: (.cef_kb | kb_mb)}],
   dropouts: [$rows[] | {slot, at_ms, before_ms: (.bl.at_ms // null), after_ms: (.al.at_ms // null), span_s,
                         rss_step_mb: (.rss_kb | kb_mb), anon_step_mb: (.anon_kb | kb_mb), file_step_mb: (.file_kb | kb_mb),
                         cef_step_mb: (.cef_kb | kb_mb), children_step_mb: (.children_kb | kb_mb), gen_before, gen_after,
                         window_step_mb: (.tree_window_mb | round1), beats_before, beats_after}]} as $memory_split
| {status: (if any($checks[]; .status == "fail") then "fail" elif any($checks[]; .status == "pending") then "plumbing" else "pass" end),
   page: $page, drive_min: $drive_min, schedule: $schedule,
   plan: {dropouts: ($plan.dropouts | length), disconnects: $plan.disconnects, part_s: $plan.part_s},
   checked: ($checks | length), checks: $checks, measures: {memory_split: $memory_split}}
