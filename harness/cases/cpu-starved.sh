# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 6, cpu-starved (C14 brief, amended 2026-10-18 for Owed 20, 2026-10-24 for the re-review's
# I-4, and 2026-10-25 for Owed 86's honest beat): the compositor's own cgroup starved of CPU from
# 20 s for 40 s; 80 s live.
#
# The starvation is a quota, not a burn (fix round 4): the same 16-worker burn in the compositor's
# cgroup gave 11.5, 24.7 and 28.7 fps in three runs, because a burn's share depends on how many of
# the compositor's threads are runnable at each instant, so its severity was a coin. Now the drive
# reads the container's cgroup CPU time at 15 s and at 20 s (`in_compositor cpu-usage`: its
# demand, in CPUs, over those 5 s), then sets the container's quota to STARVE_SHARE of that demand
# (`docker update --cpus`; the relay image is never changed) for STRESS_S, and puts the quota back
# (`cpus` in compose.yml, read from docker). Every process in the container (the compositor and
# CEF's) then shares STARVE_SHARE of the CPU it was using, whatever the runner: the brief's "in the
# compositor cgroup", the same every run. The marks: `stress` once the quota is set (demand, quota,
# the original cpus), `stress_done` once it is back. A drive that could not read the demand, or set
# or restore the quota, marks `stress_refused` and leaves `stress_ran` red.
#
# STARVE_SHARE 0.75 is a drive parameter, not a budget: a quarter less CPU than the chain was using
# (about 0.62 of F-001's 0.83 cores at 720p30), which no chain absorbs without shedding work. Spec
# §9.4's "another process at 100 %" gives no size. Whether the ladder's one rung below full (half
# the overlay's frame rate; CEF's tree is 0.10 to 0.50 cores of the chain, F-001) can hold the clock
# against a quarter is unmeasured: Owed 86's fix is judged at this share, and the share is the
# owner's to set (Owed 86, amended in fix round 4).
#
# Anti-vacuity, two witnesses: the quota was requested below the demand and held the whole window
# (`stress_ran`, read from the drive's own marks, so it trusts `docker update`'s exit status: the
# quota requested, not the quota applied; the confirm pass's c-2), and the compositor felt it
# (`starvation_applied`: the window's recorded frame rate under the beat's floor, output_fps_floor, a
# beat's encoderSpeed under ENCODER_SPEED_REALTIME_MIN, or an overrun counted; the re-review's I-4).
# A starvation nothing felt is the harness's own red, never the product's. The witness never reads
# the beat's fps: a beat that lies about the starvation is the product's red (below), not an unfelt
# drive.
#
# The honest beat (Owed 86; the owner, 2026-10-24, "Ok", relayed by the coordinator: the beat's fps
# is the encoder's real output rate, with a starvation alarm, the ladder unchanged and performance-4x
# kept). Read from the log (`output fps beat=`, `output starvation alarm raised|cleared`; logfacts),
# against the profile's own levels (output_fps_floor 0.95 × fps, output_fps_recovered 29.5/30 × fps,
# dumped from OutputProfile::P720): (a) the run's first raise is inside the quota's window;
# (b) every beat whose window lies inside the quota's reads under the floor, and at least one does
# (`fps_beats_read`, the anti-vacuity, names why none did); (c) the first clear after that raise
# comes after the quota is lifted, within realtime_after's window (BUDGET_LADDER_REALTIME_MS plus a
# beat), and a beat in that window reads the recovered level; (d) the ladder never moved: every beat
# that carries a rung reads `full` (the ruling; a step is now a red, whatever the beat read).
#
# The programme clock holds (the window's recorded frame rate, BUDGET_FPS_MEAN_MIN, the recording's
# mean bar; no PTS gap; every change the timeline schedules in the window shows): spec §9.4's "clock
# holds", still judged. With the ladder ruled out nothing in the product holds it under this quota,
# so these three red in CI with Owed 86's signature (the honest beat held, the ladder still) until the
# owner rules whether §9.4's clause stands for this case (docs/_INDEX.md Owed 86). The encoder is back
# at realtime within BUDGET_LADDER_REALTIME_MS of the end; the score stays on air (no overlay-less
# frame); every rung is full or overlay_half_rate and never back to full after a step (C05). The last
# beat's rung is printed. 720p only, as every chaos leg: the harness session is the 720p fixture (a
# 1080p leg is owed with a 1080p fixture; the PR #52 review's m11).

CASE_LIVE_S=80
CASE_KEY=window_fps
STRESS_S=40
STARVE_SHARE=0.75
DEMAND_FROM_S=15
STRESS_FROM_S=20

case_drive() {
  local nano cpus a b demand quota start refused=""
  # shellcheck disable=SC2154 # cid: run-case.sh's compositor container
  nano="$(docker inspect --format '{{.HostConfig.NanoCpus}}' "$cid")"
  if [ "${nano:-0}" -gt 0 ]; then
    cpus="$(awk -v n="$nano" 'BEGIN { printf "%.2f", n / 1e9 }')"
  else
    cpus="$(nproc)"
  fi
  at_s "$DEMAND_FROM_S"
  a="$(in_compositor cpu-usage)" || a=""
  at_s "$STRESS_FROM_S"
  b="$(in_compositor cpu-usage)" || b=""
  # The demand in CPUs: µs of CPU per ms of wall is thousandths of a CPU.
  demand="$(jq -rn --argjson a "${a:-null}" --argjson b "${b:-null}" '[$a.usage_usec, $b.usage_usec, $a.sample_ms, $b.sample_ms] as $v
    | if ($v | all(type == "number")) and $v[3] > $v[2] and $v[1] > $v[0]
      then (($v[1] - $v[0]) / ($v[3] - $v[2]) | round) / 1000 else empty end' 2>/dev/null)" || demand=""
  if [ -z "$demand" ]; then
    refused="the cgroup CPU time did not read twice ($a; $b)"
  else
    quota="$(awk -v d="$demand" -v s="$STARVE_SHARE" 'BEGIN { q = d * s; if (q < 0.01) q = 0.01; printf "%.2f", q }')"
    if docker update --cpus "$quota" "$cid" >/dev/null; then
      start=$(now_ms)
      mark stress "{\"demand_cpus\":$demand,\"quota_cpus\":$quota,\"cpus\":$cpus,\"share\":$STARVE_SHARE,\"seconds\":$STRESS_S}"
      until_ms $((start + STRESS_S * 1000))
      if docker update --cpus "$cpus" "$cid" >/dev/null; then
        mark stress_done "{\"cpus\":$cpus}"
      else
        refused="the quota was not put back to $cpus"
      fi
    else
      refused="docker update --cpus $quota failed"
    fi
  fi
  if [ -n "$refused" ]; then
    echo "cpu-starved: $refused" >&2
    mark stress_refused "$(jq -cn --arg r "$refused" '{reason: $r}')"
  fi
  at_s "$CASE_LIVE_S"
}

# The window [stress, stress_done]: whether the compositor felt it. Any of three: the window's
# recorded frame rate under the beat's floor (output_fps_floor, OutputProfile::P720.fps_floor(); the
# PR #69 review's m1: this read budget_fps_mean_min, the recording's 29.5 mean bar, before), a beat's
# encoderSpeed (in the window or the beat after it) under ENCODER_SPEED_REALTIME_MIN, or droppedFrames
# up over the last beat before it (an overrun is the encoder's queue overflowing: starvation the
# product saw). The third is beyond the re-review's two (I-4). In CI's healthy legs the slowest beat
# read encoderSpeed 0.967 (the re-review's artifacts, 7 legs), so 0.95 is no noise.
STARVED='less(mark_at("stress_done"); mark_at("stress")) as $w
  | (if $w == null or $w <= 0 then null else (frames_between(mark_at("stress"); mark_at("stress_done")) | length) * 1000 / $w end) as $fps
  | beats_between(mark_at("stress"); mark_at("stress_done") + $t.heartbeat_interval_ms) as $b
  | ([$b[] | .body.encoderSpeed | numbers] | min) as $speed
  | ([beats[] | select(.received_ms < mark_at("stress"))] | last | .body.droppedFrames // 0) as $d0
  | (if ($b | length) == 0 then null else ($b | map(.body.droppedFrames // 0) | max) - $d0 end) as $rise
  | {applied: ($w != null and $w > 0 and (($fps != null and $fps < $t.output_fps_floor)
       or ($speed != null and $speed < $t.encoder_speed_realtime_min) or ($rise != null and $rise > 0))),
     window_fps: $fps, fps_floor: $t.output_fps_floor, min_encoder_speed: $speed,
     speed_min: $t.encoder_speed_realtime_min, dropped_rise: $rise}'
# The honest beat's evidence (Owed 86), from the log's lines (logfacts' `output_fps`,
# `starvation_raised`, `starvation_cleared`), on the docker clock the marks share:
# - the judged beats: those whose window lies inside the quota's, [stress + heartbeat_interval_ms,
#   stress_done] (a beat reads the frames since the beat before it, so the first beat after `stress`
#   straddles it; a judged beat is starved for its whole window). `reason` says why none is.
# - the run's first raise, from `stress`, against the window's span (`window_ms`);
# - the first clear after that raise, from `stress_done`, against `bound_ms`, realtime_after's window
#   (BUDGET_LADDER_REALTIME_MS, spec §6's "Real time within 10 s", plus one beat); and `after`, the
#   beats in that window.
# A report with no `output_fps` (a checker older than the honest beat) reads no beat, never one.
HONEST='mark_at("stress") as $s | mark_at("stress_done") as $e
  | $t.heartbeat_interval_ms as $hb
  | ($t.budget_ladder_realtime_ms + $t.heartbeat_interval_ms) as $bound
  | .log.output_fps as $all | ($all // []) as $b
  | (if $s == null or $e == null then [] else [$b[] | select(.at_ms >= $s + $hb and .at_ms <= $e)] end) as $in
  | ((.log.starvation_raised // []) | first) as $r
  | (if $r == null then null else [(.log.starvation_cleared // [])[] | select(.at_ms > $r.at_ms)] | first end) as $c
  | (if $e == null then [] else [$b[] | select(.at_ms >= $e and .at_ms < $e + $bound)] end) as $after
  | {reason: (if $all == null then "the report has no output_fps (a checker older than the honest beat)"
       elif ($b | length) == 0 then "no `output fps beat=` line in the log"
       elif ($in | length) == 0 then "no beat inside the starvation window" else null end),
     parsed: ($b | length), checked: ($in | length), fps: ($in | map(.fps)),
     above: ($in | map(select(.fps >= $t.output_fps_floor)) | length),
     floor: $t.output_fps_floor, recovered: $t.output_fps_recovered,
     raises: ((.log.starvation_raised // []) | length), raise_fps: ($r.fps // null),
     raise_from_stress_ms: less($r.at_ms // null; $s), window_ms: less($e; $s),
     clear_fps: ($c.fps // null), clear_from_done_ms: less($c.at_ms // null; $e), bound_ms: $bound,
     after: ($after | map(.fps))}'
# The ladder in the run: every beat that carries a rung, the ones off `full`, and droppedFrames' rise
# over the window (its input; shown, not judged). Owed 86's signature reads `stepped` (chaos.json).
LADDER='([beats[] | select(.received_ms < mark_at("stress"))] | last | .body.droppedFrames // 0) as $d0
  | beats_between(mark_at("stress"); mark_at("stress_done") + $t.heartbeat_interval_ms) as $w
  | [beats[] | select(.body.rung != null)] as $r
  | {beats: ($r | length), stepped: ($r | map(select(.body.rung != "full")) | length),
     rungs: ($r | map(.body.rung) | unique), dropped_before: $d0,
     dropped_rise: (if ($w | length) == 0 then null else (($w | map(.body.droppedFrames // 0) | max) - $d0) end)}'

CASE_CHECKS=19
case_verdict() {
  # Anti-vacuity: the quota was requested under the demand, held for STRESS_S, and put back (the
  # drive's marks; `starvation_applied` is the witness that it was applied).
  require stress_ran 'mark_value("stress") as $s | mark_value("stress_done") as $e
    | $s != null and $s.demand_cpus > 0 and $s.quota_cpus > 0 and $s.quota_cpus < $s.demand_cpus
    and $e != null and $e.cpus == $s.cpus and (mark_at("stress_done") - mark_at("stress")) >= $s.seconds * 1000' \
    '{quota: mark_value("stress"), restored: mark_value("stress_done"), refused: mark_value("stress_refused"),
      window_ms: less(mark_at("stress_done"); mark_at("stress"))}'
  require starvation_applied "$STARVED | .applied" "$STARVED"
  # The honest beat (Owed 86). Anti-vacuity first: a beat judged in the window, or the reason none is.
  require fps_beats_read "$HONEST | .checked >= 1" "$HONEST | {reason, parsed, checked}"
  # (a) The alarm is raised by the starvation: the run's first raise inside the quota's window (a
  # raise on the healthy clock before it, or only after it, is not the starvation's).
  require alarm_raised "$HONEST | .raise_from_stress_ms >= 0 and .raise_from_stress_ms <= .window_ms" \
    "$HONEST | {raises, raise_fps, raise_from_stress_ms, window_ms}"
  # (b) The beat reads the starvation: every judged beat under the floor.
  require beats_below_floor "$HONEST | .fps | length >= 1 and all(. < \$t.output_fps_floor)" \
    "$HONEST | {checked, above, floor, fps}"
  # (c) The alarm clears once the quota is lifted, within realtime_after's window, and a beat there
  # reads the recovered level.
  require alarm_cleared "$HONEST | .clear_from_done_ms >= 0 and .clear_from_done_ms <= .bound_ms
    and (.after | any(. >= \$t.output_fps_recovered))" \
    "$HONEST | {clear_fps, clear_from_done_ms, bound_ms, recovered, after}"
  # (d) The ladder never moved (the owner's ruling: no ladder change). A step is a red here.
  require ladder_unmoved '[beats[] | .body.rung | select(. != null)] | length >= 1 and all(. == "full")' "$LADDER"
  # The clock (spec §9.4's "clock holds"; Owed 86 while the owner has not ruled on it for this case).
  require window_fps '(mark_at("stress_done") - mark_at("stress")) as $w
    | (frames_between(mark_at("stress"); mark_at("stress_done")) | length) as $n
    | $w > 0 and ($n * 1000 / $w) >= $t.budget_fps_mean_min' \
    '(mark_at("stress_done") - mark_at("stress")) as $w | {fps: ((frames_between(mark_at("stress"); mark_at("stress_done")) | length) * 1000 / $w), floor: $t.budget_fps_mean_min}'
  require pts_gaps '.pts_gaps == []' '.pts_gaps | length'
  require realtime_after 'beats_between(mark_at("stress_done"); mark_at("stress_done") + $t.budget_ladder_realtime_ms + $t.heartbeat_interval_ms)
    | any(.body.encoderSpeed >= $t.encoder_speed_realtime_min)' \
    '{speeds: (beats_between(mark_at("stress_done"); mark_at("stress_done") + $t.budget_ladder_realtime_ms + $t.heartbeat_interval_ms) | map(.body.encoderSpeed)), min: $t.encoder_speed_realtime_min}'
  require score_on_air 'frames_between(mark_at("stress"); $run.end_request_ms) as $f
    | ($f | length) >= 1 and ($f | map(select(.[2] == null)) | length) == 0' \
    'frames_between(mark_at("stress"); $run.end_request_ms) | {frames: length, no_overlay: (map(select(.[2] == null)) | length)}'
  require scheduled_changes_shown '. as $r | ($r.overlay.score_index_changes | map(.index)) as $shown
    | ($r.frames_table | map(select(.[2] != null)) | last | .[0]) as $last
    | [scheduled[] | select(.due_ms >= mark_at("stress") and .due_ms <= $last - $t.budget_frame_to_encoder_ms) | .index] as $due
    | ($due | length) >= 1 and ($due - $shown) == []' \
    '. as $r | ($r.frames_table | map(select(.[2] != null)) | last | .[0]) as $last
    | [scheduled[] | select(.due_ms >= mark_at("stress") and .due_ms <= $last - $t.budget_frame_to_encoder_ms) | .index] as $due
    | {due: ($due | length), missing: ($due - ($r.overlay.score_index_changes | map(.index)))}'
  require rungs_known '[beats[] | .body.rung | select(. != null)] | length >= 1 and all(. == "full" or . == "overlay_half_rate")' \
    '.heartbeats.rungs'
  require rung_holds '.heartbeats.rung_sequence | (to_entries | map(select(.value == "overlay_half_rate")) | first | .key) as $i
    | $i == null or (.[$i:] | all(. == "overlay_half_rate"))' '.heartbeats.rung_sequence'
  measure last_rung '[beats[] | .body.rung | select(. != null)] | last'
  require_no_false_restart
}
