# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 6, cpu-starved (C14 brief, amended 2026-10-18 for Owed 20): CPU burned in the compositor's
# own cgroup from 20 s for 40 s; 80 s live.
#
# The burn runs INSIDE the compositor's container (`in_compositor burn`: pure-bash busy loops, each
# stopping itself at the window's end; the relay image is never changed), so it shares the
# compositor's CPU quota, as the brief's "in the compositor cgroup" asks. The first build ran
# stress-ng as a sibling container, which only took the host's fair share: every beat read
# droppedFrames 0 and rung full (the PR #52 review's I3). Workers: BURN_PER_CPU per CPU of the
# compositor's quota (`cpus` in compose.yml, read from docker), so the compositor's few busy threads
# get about 1 / (BURN_PER_CPU + 1) of it: an encoder that needs more than that overruns.
#
# The programme clock holds (the window's frame rate, no PTS gap); the ladder moves: a beat in the
# window reads overlay_half_rate, and droppedFrames rose by at least ladder_overruns_to_step (the
# domain's rule for the step; a stress that moves nothing is a FAIL); the encoder is back at
# realtime within BUDGET_LADDER_REALTIME_MS of the end; the score stays on air (no overlay-less
# frame, and every change the timeline schedules in the window shows); every rung is full or
# overlay_half_rate and never back to full after a step (C05). The last beat's rung is printed: a
# step is the sizing alarm in production (C17). 720p only, as every chaos leg: the harness session
# is the 720p fixture (a 1080p leg is owed with a 1080p fixture; the PR #52 review's m11).

CASE_LIVE_S=80
CASE_KEY=window_fps
STRESS_S=40
BURN_PER_CPU=4

case_drive() {
  local nano cpus
  # shellcheck disable=SC2154 # cid: run-case.sh's compositor container
  nano="$(docker inspect --format '{{.HostConfig.NanoCpus}}' "$cid")"
  cpus=$(((nano + 999999999) / 1000000000))
  [ "$cpus" -gt 0 ] || cpus="$(nproc)"
  at_s 20
  in_compositor burn $((cpus * BURN_PER_CPU)) "$STRESS_S" | mark_lines stress || true
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=14
case_verdict() {
  # Anti-vacuity: the burn ran every worker its whole time (its end line, stamped after the last
  # worker stopped).
  require stress_ran 'mark_value("stress") as $b | mark_value("stress_done") as $e
    | $b.workers >= 1 and $e != null and $e.workers == $b.workers and $e.elapsed_ms >= $b.seconds * 1000' \
    '{burn: mark_value("stress"), done: mark_value("stress_done")}'
  require window_fps '(mark_at("stress_done") - mark_at("stress")) as $w
    | (frames_between(mark_at("stress"); mark_at("stress_done")) | length) as $n
    | $w > 0 and ($n * 1000 / $w) >= $t.budget_fps_mean_min' \
    '(mark_at("stress_done") - mark_at("stress")) as $w | {fps: ((frames_between(mark_at("stress"); mark_at("stress_done")) | length) * 1000 / $w), floor: $t.budget_fps_mean_min}'
  require pts_gaps '.pts_gaps == []' '.pts_gaps | length'
  require ladder_moved '([beats[] | select(.received_ms < mark_at("stress"))] | last | .body.droppedFrames // 0) as $d0
    | beats_between(mark_at("stress"); mark_at("stress_done") + $t.heartbeat_interval_ms) as $w
    | ($w | length) >= 1 and ($w | any(.body.rung == "overlay_half_rate"))
    and (($w | map(.body.droppedFrames // 0) | max) - $d0) >= $t.ladder_overruns_to_step' \
    '([beats[] | select(.received_ms < mark_at("stress"))] | last | .body.droppedFrames // 0) as $d0
    | {dropped_before: $d0, step_needs: $t.ladder_overruns_to_step,
       beats: (beats_between(mark_at("stress"); mark_at("stress_done") + $t.heartbeat_interval_ms)
         | map({droppedFrames: .body.droppedFrames, rung: .body.rung, encoderSpeed: .body.encoderSpeed}))}'
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
