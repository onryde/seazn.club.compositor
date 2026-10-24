# The soak's schedule (C15): one implementation, read by driver.sh (what to do, and when) and by
# verdict.jq (what must have been done). `soak_plan($soak; $drive_s)` takes harness/soak/soak.json
# and the drive in seconds, and answers {drive_s, part_s, dropouts: [s...], disconnects: [s...]},
# each an offset from the drive's start.
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
