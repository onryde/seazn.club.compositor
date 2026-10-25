# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 3, reader-kicked (C14 brief): MediaMTX kicks the compositor's SRT reader every 20 s, 6 times
# (`POST /v3/srt/conns/kick/{id}`, F-021); 130 s live.
#
# After each kick the camera's content returns (the first content gap that ends after the kick's
# mark ends; the mark is stamped before the kick lands, so the gap may start just after it) within
# BUDGET_RETURN_TO_LIVE_MS + input_backoff_cap_ms, and the output never drops. The reader-connection
# count is MediaMTX's, not fake-control's (a false premise): each kick hits a NEW connection, and a
# new one follows the last.
#
# The kick alert (heartbeats' inputKickAlert after kick_alert_count + 1 kicks in kick_window_ms) is
# reported, never judged: Owed 46, a peer close is an `Error`, never the `Eos` the domain counts, so
# a MediaMTX kick cannot move the count as built. What is judged is that it never comes early.

CASE_LIVE_S=130
CASE_KEY=return_after_kicks
KICKS=6

kick() {
  local id
  id="$(mtx_readers | jq -r 'first | .id // empty')"
  if [ -z "$id" ]; then
    log "no compositor reader to kick"
    mark kick_missed
    return 0
  fi
  mark kick "\"$id\""
  mtx POST "/v3/srt/conns/kick/$id" >/dev/null
}

case_drive() {
  local k
  for ((k = 1; k <= KICKS; k++)); do
    at_s $((k * 20))
    kick
  done
  at_s "$CASE_LIVE_S"
  mark readers_after "$(mtx_readers | jq -c 'map(.id)')"
}

CASE_CHECKS=14
case_verdict() {
  # Anti-vacuity: six kicks, each of a distinct connection, and a reader again after the last.
  require kicks_applied "(mark(\"kick\") | length) == $KICKS and (mark(\"kick\") | map(.value) | unique | length) == $KICKS
    and (mark(\"kick_missed\") | length) == 0" \
    '{kicks: (mark("kick") | length), distinct: (mark("kick") | map(.value) | unique | length), missed: (mark("kick_missed") | length)}'
  require reader_reconnected '(mark_value("readers_after") // []) - (mark("kick") | map(.value)) | length >= 1' \
    '{after: (mark_value("readers_after") | length)}'
  require return_after_kicks '. as $r | [mark("kick")[] | .at_ms as $k
      | ([$r.camera.content_gaps[] | select(.to_emit_ms > $k)] | first) as $g
      | if $g == null then null else $g.to_shown_ms - $k end]
    | length >= 1 and all(. != null and . <= ($t.budget_return_to_live_ms + $t.input_backoff_cap_ms))' \
    '. as $r | {return_ms: [mark("kick")[] | .at_ms as $k
      | ([$r.camera.content_gaps[] | select(.to_emit_ms > $k)] | first) as $g
      | if $g == null then null else $g.to_shown_ms - $k end], bound: ($t.budget_return_to_live_ms + $t.input_backoff_cap_ms)}'
  measure input_kicks_beats '[beats[] | .body.inputKicks] | max'
  owed kick_alert 'Owed 46' '[beats[] | select(.body.inputKickAlert == true)] | length >= 1'
  require kick_alert_not_early '(mark("kick")[$t.kick_alert_count].at_ms) as $k
    | [beats[] | select(.body.inputKickAlert == true) | .received_ms] | all(. > $k)' \
    '{first_alert_ms: ([beats[] | select(.body.inputKickAlert == true) | .received_ms] | first),
      kick_ms: (mark("kick")[$t.kick_alert_count].at_ms)}'
  require no_output_outages '.output_outages == []' '.output_outages | length'
  require counter_regressions '.camera.counter_regressions == 0'
  require_no_false_restart
}
