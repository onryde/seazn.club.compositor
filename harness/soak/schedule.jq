# The soak's schedule (C15): one implementation, read by driver.sh (what to do, and when) and by
# verdict.jq (what must have been done). `soak_plan($soak; $drive_s)` takes harness/soak/soak.json
# and the drive in seconds, and answers {drive_s, part_s, dropouts: [s...], disconnects: [s...]},
# each an offset from the drive's start. `schedule_plan($soak; $schedule; $drive_s)` picks the
# schedule by name: `soak` (this one), `dropouts` (`dropout_plan`, the memory experiment's) or
# `none` (the experiment's control: the same drive with nothing in it).
#
# - The drive is `parts` (3) equal parts: the hours of a 3-hour soak. A shorter rehearsal is the
#   same run scaled, so it has every action kind (the brief's "scaled anti-vacuity counts").
# - Camera dropouts every `dropout_every_s` (5 min), from `dropout_phase_s` (2.5 min), each
#   `dropout_pause_s` long: 36 in 3 hours. The phase is half a period, so no dropout shares the
#   minute a disconnect starts on (the brief's "every 5 min" and "at minute 30" coincide at 0).
# - One destination disconnect per part, `disconnect_hold_s` long, at the part's middle snapped to
#   the dropout grid's whole periods: minutes 30, 90 and 150 of a 3-hour soak (the brief's "once
#   per hour, at minute 30").
#
# A drive the schedule cannot fit (two disconnects on one slot, one that would not end inside the
# drive, a dropout inside a disconnect's window) is an error here, never a quieter run.
def soak_plan($soak; $drive_s):
  ($soak.dropout_every_s) as $p
  | ($drive_s / $soak.parts) as $part
  | (if $drive_s >= $soak.dropout_phase_s + $soak.dropout_pause_s
     then (($drive_s - $soak.dropout_phase_s - $soak.dropout_pause_s) / $p | floor) + 1 else 0 end) as $n
  | [range(0; $n) | $soak.dropout_phase_s + . * $p] as $dropouts
  | [range(0; $soak.parts) | ((. + 0.5) * $part / $p | round) * $p] as $disconnects
  | if ($drive_s | type) != "number" or $drive_s < $soak.min_drive_min * 60 then
      error("soak_plan: a drive of \($drive_s) s is under min_drive_min (\($soak.min_drive_min) min)")
    elif ($disconnects | unique | length) != ($disconnects | length) then
      error("soak_plan: two disconnects share a slot in a \($drive_s) s drive")
    elif any($disconnects[]; . <= 0 or . + $soak.disconnect_hold_s >= $drive_s) then
      error("soak_plan: a disconnect does not fit inside a \($drive_s) s drive")
    elif any($dropouts[] as $x | $disconnects[] as $d
             | ($x + $soak.dropout_pause_s > $d and $x < $d + $soak.disconnect_hold_s); .) then
      error("soak_plan: a dropout falls inside a disconnect in a \($drive_s) s drive")
    else
      {drive_s: $drive_s, part_s: $part, dropouts: $dropouts, disconnects: $disconnects}
    end;

# The dropout-only experiment (soak 40139563554's memory drift, 2026-10-25): camera dropouts and
# nothing else, so each step in the compositor's memory can be tied to one. A dropout every
# `dropout_only.every_s` (125 s: 28.8 an hour, against the soak's 12) from `dropout_only.phase_s`
# (1 min), each `dropout_pause_s` long, and no destination disconnect. Each dropout owns a whole
# period (the last one's ends a period before the drive does): 35 dropouts in a 75-minute drive.
# The period is deliberately not a whole number of minutes. The memory split's lines come a minute
# apart, on the compositor's own beat grid, so a 120 s period would pin every dropout to the same
# place between two lines for the whole run: either every gap between dropouts would hold two lines
# (a control) or none would. At 125 s the lines walk 5 s a period through the gap. A drive under
# `min_drive_min` is an error, as for the soak.
def dropout_plan($soak; $drive_s):
  ($soak.dropout_only.every_s) as $p
  | ($soak.dropout_only.phase_s) as $phase
  | if ($drive_s | type) != "number" or $drive_s < $soak.min_drive_min * 60 then
      error("dropout_plan: a drive of \($drive_s) s is under min_drive_min (\($soak.min_drive_min) min)")
    elif ($p | type) != "number" or ($phase | type) != "number" or $p <= $soak.dropout_pause_s or $phase < 0 then
      error("dropout_plan: soak.json's dropout_only needs every_s over dropout_pause_s and a phase_s of 0 or more")
    else
      {drive_s: $drive_s, part_s: ($drive_s / $soak.parts),
       dropouts: [range(0; (($drive_s - $phase) / $p | floor)) | $phase + . * $p], disconnects: []}
    end;

# The experiment's control (`none`): the dropout schedule's drive with no dropout and no disconnect,
# so growth with time alone can be told from growth per dropout. verdict.jq measures the same steps
# at dropout_plan's instants, where nothing happened.
def schedule_plan($soak; $schedule; $drive_s):
  if $schedule == "soak" then soak_plan($soak; $drive_s)
  elif $schedule == "dropouts" then dropout_plan($soak; $drive_s)
  elif $schedule == "none" then dropout_plan($soak; $drive_s) | .dropouts = []
  else error("schedule_plan: no schedule \($schedule | tojson) (soak, dropouts or none)")
  end;
