# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 6, cpu-starved (C14 brief, amended 2026-10-18 for Owed 20 and 2026-10-24 for the re-review's
# I-4): the compositor's own cgroup starved of CPU from 20 s for 40 s; 80 s live.
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
# quota requested, not the quota applied; the confirm pass's c-2), and the compositor felt it (`starvation_applied`: the window's frame rate under
# BUDGET_FPS_MEAN_MIN, a beat's encoderSpeed under ENCODER_SPEED_REALTIME_MIN, or an overrun counted;
# the re-review's I-4). A starvation nothing felt is the harness's own red, never the product's, and
# `ladder_moved` passes only on a window that was starved.
#
# The programme clock holds (the window's frame rate, no PTS gap); the ladder moves: a beat in the
# window reads overlay_half_rate, and droppedFrames rose by at least ladder_overruns_to_step (the
# domain's rule for the step); the encoder is back at realtime within BUDGET_LADDER_REALTIME_MS of
# the end; the score stays on air (no overlay-less frame, and every change the timeline schedules in
# the window shows); every rung is full or overlay_half_rate and never back to full after a step
# (C05). The last beat's rung is printed: a step is the sizing alarm in production (C17). 720p only,
# as every chaos leg: the harness session is the 720p fixture (a 1080p leg is owed with a 1080p
# fixture; the PR #52 review's m11).

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

# The window [stress, stress_done]: whether the compositor felt it. Any of three: the window's frame
# rate under BUDGET_FPS_MEAN_MIN, a beat's encoderSpeed (in the window or the beat after it) under
# ENCODER_SPEED_REALTIME_MIN, or droppedFrames up over the last beat before it (an overrun is the
# encoder's queue overflowing: starvation the product saw). The third is beyond the re-review's
# two (I-4): a ladder that steps within its 1 s window can keep every 5 s beat at realtime and the
# clock whole, and that correct product must not red the drive's witness. In CI's healthy legs the
# slowest beat read encoderSpeed 0.967 (the re-review's artifacts, 7 legs), so 0.95 is no noise.
STARVED='less(mark_at("stress_done"); mark_at("stress")) as $w
  | (if $w == null or $w <= 0 then null else (frames_between(mark_at("stress"); mark_at("stress_done")) | length) * 1000 / $w end) as $fps
  | beats_between(mark_at("stress"); mark_at("stress_done") + $t.heartbeat_interval_ms) as $b
  | ([$b[] | .body.encoderSpeed | numbers] | min) as $speed
  | ([beats[] | select(.received_ms < mark_at("stress"))] | last | .body.droppedFrames // 0) as $d0
  | (if ($b | length) == 0 then null else ($b | map(.body.droppedFrames // 0) | max) - $d0 end) as $rise
  | {applied: ($w != null and $w > 0 and (($fps != null and $fps < $t.budget_fps_mean_min)
       or ($speed != null and $speed < $t.encoder_speed_realtime_min) or ($rise != null and $rise > 0))),
     window_fps: $fps, fps_floor: $t.budget_fps_mean_min, min_encoder_speed: $speed,
     speed_min: $t.encoder_speed_realtime_min, dropped_rise: $rise}'
# The ladder's evidence in the window: droppedFrames' rise over the last beat before it, and the
# beats on overlay_half_rate. Owed 86's signature reads both (chaos.json).
LADDER='([beats[] | select(.received_ms < mark_at("stress"))] | last | .body.droppedFrames // 0) as $d0
  | beats_between(mark_at("stress"); mark_at("stress_done") + $t.heartbeat_interval_ms) as $w
  | {beats: ($w | length), dropped_before: $d0,
     dropped_rise: (if ($w | length) == 0 then null else ($w | map(.body.droppedFrames // 0) | max) - $d0 end),
     half_rate_beats: ($w | map(select(.body.rung == "overlay_half_rate")) | length),
     step_needs: $t.ladder_overruns_to_step,
     series: ($w | map({droppedFrames: .body.droppedFrames, rung: .body.rung, encoderSpeed: .body.encoderSpeed}))}'

CASE_CHECKS=15
case_verdict() {
  # Anti-vacuity: the quota was requested under the demand, held for STRESS_S, and put back (the
  # drive's marks; `starvation_applied` is the witness that it was applied).
  require stress_ran 'mark_value("stress") as $s | mark_value("stress_done") as $e
    | $s != null and $s.demand_cpus > 0 and $s.quota_cpus > 0 and $s.quota_cpus < $s.demand_cpus
    and $e != null and $e.cpus == $s.cpus and (mark_at("stress_done") - mark_at("stress")) >= $s.seconds * 1000' \
    '{quota: mark_value("stress"), restored: mark_value("stress_done"), refused: mark_value("stress_refused"),
      window_ms: less(mark_at("stress_done"); mark_at("stress"))}'
  require starvation_applied "$STARVED | .applied" "$STARVED"
  require window_fps '(mark_at("stress_done") - mark_at("stress")) as $w
    | (frames_between(mark_at("stress"); mark_at("stress_done")) | length) as $n
    | $w > 0 and ($n * 1000 / $w) >= $t.budget_fps_mean_min' \
    '(mark_at("stress_done") - mark_at("stress")) as $w | {fps: ((frames_between(mark_at("stress"); mark_at("stress_done")) | length) * 1000 / $w), floor: $t.budget_fps_mean_min}'
  require pts_gaps '.pts_gaps == []' '.pts_gaps | length'
  # Judged on a starved window only: its own clauses (droppedFrames up) are starvation the product
  # saw, so a pass here is a starved window by `starvation_applied`'s third clause, and a window
  # nothing starved reds both, the drive's witness first. `starved` is shown beside it.
  require ladder_moved "$LADDER | .beats >= 1 and .half_rate_beats >= 1 and .dropped_rise >= .step_needs" \
    "($STARVED | .applied) as \$starved | $LADDER + {starved: \$starved}"
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
