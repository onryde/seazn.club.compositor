# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 6, cpu-starved (C14 brief, amended 2026-10-18 for Owed 20): `stress-ng --cpu 4` from 20 s for
# 40 s; 80 s live.
#
# The stress runs as its own container from the harness image (stress-ng is in the harness stage
# only; the relay image is never changed for the harness), unlimited, on the same 4-CPU host as the
# compositor (`cpus: 4`): it competes for the host's CPUs. The brief's "in the compositor's
# cgroup" is not possible from compose (a container cannot join another's cgroup), a deviation.
#
# The programme clock holds (the window's frame rate, no PTS gap); the beats show the stress
# (droppedFrames rise or the rung steps); the encoder is back at realtime within
# BUDGET_LADDER_REALTIME_MS of the end; the score stays on air (no overlay-less frame, and every
# change the timeline schedules in the window shows); every rung is full or overlay_half_rate and
# never back to full after a step (C05). The last beat's rung is printed: a step is the sizing
# alarm in production (C17), not a red here.

CASE_LIVE_S=80
CASE_KEY=window_fps
STRESS_S=40
stress_cid=""

stop_stress() {
  if [ -n "$stress_cid" ]; then
    docker rm -f "$stress_cid" >/dev/null 2>&1 || true
    stress_cid=""
  fi
}
CASE_TEARDOWN=stop_stress

case_drive() {
  local code ops
  at_s 20
  mark stress_start
  stress_cid="$(docker run -d "$HARNESS_IMAGE" stress-ng --cpu 4 --timeout "${STRESS_S}s" --metrics-brief)"
  code="$(docker wait "$stress_cid")"
  mark stress_end_seen
  docker logs "$stress_cid" >"$OUT/stress.log" 2>&1 || true
  ops="$(awk '{ for (i = 1; i < NF; i++) if ($i == "cpu" && $(i + 1) ~ /^[0-9]+$/) { print $(i + 1); exit } }' "$OUT/stress.log")"
  MARK_AT_MS="$(docker inspect --format '{{.State.FinishedAt}}' "$stress_cid" | python3 -I -c 'import sys,datetime; s=sys.stdin.read().strip(); d,_,f=s.partition("."); f=(f.rstrip("Z")+"000000")[:6]; t=datetime.datetime.strptime(d.rstrip("Z"),"%Y-%m-%dT%H:%M:%S").replace(tzinfo=datetime.timezone.utc); print(int(t.timestamp()*1000)+int(f)//1000)')" \
    mark stress_end "{\"exit_code\":$code,\"bogo_ops\":${ops:-0}}"
  stop_stress
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=14
case_verdict() {
  # Anti-vacuity: the stress ran its whole time and did work.
  require stress_ran 'mark_value("stress_end") | .exit_code == 0 and .bogo_ops > 0' \
    '{stress: mark_value("stress_end"), ran_ms: (mark_at("stress_end") - mark_at("stress_start"))}'
  require window_fps '(mark_at("stress_end") - mark_at("stress_start")) as $w
    | (frames_between(mark_at("stress_start"); mark_at("stress_end")) | length) as $n
    | $w > 0 and ($n * 1000 / $w) >= $t.budget_fps_mean_min' \
    '(mark_at("stress_end") - mark_at("stress_start")) as $w | {fps: ((frames_between(mark_at("stress_start"); mark_at("stress_end")) | length) * 1000 / $w), floor: $t.budget_fps_mean_min}'
  require pts_gaps '.pts_gaps == []' '.pts_gaps | length'
  require stress_seen_in_beats '([beats[] | select(.received_ms < mark_at("stress_start"))] | last | .body.droppedFrames // 0) as $d0
    | beats_between(mark_at("stress_start"); mark_at("stress_end") + $t.heartbeat_interval_ms)
    | length >= 1 and any(.body.droppedFrames > $d0 or .body.rung != "full")' \
    'beats_between(mark_at("stress_start"); mark_at("stress_end") + $t.heartbeat_interval_ms) | map({droppedFrames: .body.droppedFrames, rung: .body.rung})'
  require realtime_after 'beats_between(mark_at("stress_end"); mark_at("stress_end") + $t.budget_ladder_realtime_ms + $t.heartbeat_interval_ms)
    | any(.body.encoderSpeed >= $t.encoder_speed_realtime_min)' \
    '{speeds: (beats_between(mark_at("stress_end"); mark_at("stress_end") + $t.budget_ladder_realtime_ms + $t.heartbeat_interval_ms) | map(.body.encoderSpeed)), min: $t.encoder_speed_realtime_min}'
  require score_on_air 'frames_between(mark_at("stress_start"); $run.end_request_ms) as $f
    | ($f | length) >= 1 and ($f | map(select(.[2] == null)) | length) == 0' \
    'frames_between(mark_at("stress_start"); $run.end_request_ms) | {frames: length, no_overlay: (map(select(.[2] == null)) | length)}'
  require scheduled_changes_shown '. as $r | ($r.overlay.score_index_changes | map(.index)) as $shown
    | ($r.frames_table | map(select(.[2] != null)) | last | .[0]) as $last
    | [scheduled[] | select(.due_ms >= mark_at("stress_start") and .due_ms <= $last - $t.budget_frame_to_encoder_ms) | .index] as $due
    | ($due | length) >= 1 and ($due - $shown) == []' \
    '. as $r | ($r.frames_table | map(select(.[2] != null)) | last | .[0]) as $last
    | [scheduled[] | select(.due_ms >= mark_at("stress_start") and .due_ms <= $last - $t.budget_frame_to_encoder_ms) | .index] as $due
    | {due: ($due | length), missing: ($due - ($r.overlay.score_index_changes | map(.index)))}'
  require rungs_known '[beats[] | .body.rung | select(. != null)] | length >= 1 and all(. == "full" or . == "overlay_half_rate")' \
    '.heartbeats.rungs'
  require rung_holds '.heartbeats.rung_sequence | (to_entries | map(select(.value == "overlay_half_rate")) | first | .key) as $i
    | $i == null or (.[$i:] | all(. == "overlay_half_rate"))' '.heartbeats.rung_sequence'
  measure last_rung '[beats[] | .body.rung | select(. != null)] | last'
  require_no_false_restart
}
