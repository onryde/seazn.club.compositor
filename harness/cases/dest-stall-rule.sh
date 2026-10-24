# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 17, dest-stall-rule (C14 wave-gate-3 follow-ups, 2026-10-24; amended for Owed 87, the owner,
# 2026-10-24, "A": the output's stall rule is retired, and rtmp2sink's own socket timeout is the
# only detector of a destination stall). A destination that keeps the socket open and takes bytes,
# too slowly for one video frame to leave within output_sink_timeout_ms, and never errors, through a
# send buffer capped at 8 KiB. The case demands that the sink's own error still catches it within
# budget: the outgoing sink logs `output gen=<g> error:` before the swap, the swap's reason line
# reads `error`, the swap comes within one sink timeout, the back-off from Connected and a frame of
# the last growth, and the error no sooner than one sink timeout, less a frame, after the cut
# (`stall_swap_judged`, lib.jq). Mutant m9 (output_sink_timeout_ms at 20 s) must red `stall_swap`.
#
# Why this setting (the PR #65 review's C-1): rtmp2 counts a message's bytes only once the whole
# message is written (`rtmpconnection.c:519-628`), and the socket times out only when one wait for
# room lasts the timeout. The cap was meant to wake the writer inside one message, so that the total
# stands still while no wait lasts that long, the one path where only a stall rule could see the
# stall. On CI the socket queued a whole frame past the cap (Send-Q 16 806-16 810 B against 8 192, 3 s
# into the fault), so each wake completes a message and the socket's wait is the stall's gap. That
# is the measurement Owed 87's ruling rests on, and this case keeps it under watch: if a kernel ever
# honours the cap inside a message, the sink no longer errors here, and `stall_swap` reds.
#
# Anti-vacuity, judged on the effect, not the setting (the PR #65 review's I-1): `buffer_capped`
# needs the cap set as asked (the namespace's tcp_wmem reads what compose set) and every RTMP
# socket's buffer read and at most the cap before the cut (each socket's Send-Q in the fault is
# measured, not judged: it runs past the cap, C-1); `trickled` needs netem to have sent at least 2
# packets, at a measured rate (its bytes over the fault window) under one frame per sink timeout.

CASE_LIVE_S=80
CASE_KEY=stall_swap
CASE_COMPOSE=compose.stall-rule.yml
STALL_DRAIN_X=2
FIXTURE="$HERE/../contracts/fixtures/relay-job.v1/session-composed-720p.json"

# The compositor's established RTMP sockets, read by ss from netem-out (its network namespace):
# how many, each one's send buffer (skmem's tb, bytes) and Send-Q, as one JSON line.
rtmp_sockets() {
  compose exec -T netem-out ss -tmnH state established '( dport = :1935 )' 2>/dev/null |
    python3 -I -c 'import json, re, sys
t = sys.stdin.read().strip()
socks = re.split(r"\n(?=\S)", t) if t else []
def num(m): return int(m.group(1)) if m else None
print(json.dumps({"sockets": len(socks),
  "tb": [num(re.search(r"\btb(\d+)", s)) for s in socks],
  "send_q": [int(s.split()[1]) if len(s.split()) > 1 and s.split()[1].isdigit() else None for s in socks]}))'
}

case_drive() {
  local frame_bits rate wanted got
  frame_bits="$(jq -e '.output.videoKbps * 1000 / .output.fps | floor' "$FIXTURE")"
  rate=$((frame_bits * 1000 / (STALL_DRAIN_X * $(threshold output_sink_timeout_ms))))
  # Each null when it cannot be read: buffer_capped then reds, and the drive goes on.
  wanted="$(compose config --format json | jq -c '.services.compositor.sysctls["net.ipv4.tcp_wmem"]
    | [splits("\\s+") | select(. != "") | tonumber]' 2>/dev/null || echo null)"
  got="$(compose exec -T netem-out cat /proc/sys/net/ipv4/tcp_wmem |
    jq -Rc '[splits("\\s+") | select(. != "") | tonumber]' 2>/dev/null || echo null)"
  mark send_buffer "$(jq -cn --argjson w "$wanted" --argjson r "$got" --argjson s "$(rtmp_sockets)" \
    '{wanted: $w, read: $r, before_cut: $s}')"
  mark rate "$(jq -cn --argjson f "$frame_bits" --argjson r "$rate" --argjson x "$STALL_DRAIN_X" \
    '{frame_bits: $f, bit_per_s: $r, drain_x: $x}')"
  at_s 20
  mark cut
  "$HERE/netem.sh" out rate "${rate}bit"
  at_s 23
  mark sockets_in_fault "$(rtmp_sockets)"
  at_s 50
  mark netem_out "$("$HERE/netem.sh" stats out)"
  "$HERE/netem.sh" clear out
  mark clear
  at_s "$CASE_LIVE_S"
}

CASE_CHECKS=16
case_verdict() {
  require buffer_capped 'mark_value("send_buffer") as $b
    | $b.read == $b.wanted and $b.before_cut.sockets >= 1
    and ($b.before_cut.tb | all(. != null and . <= $b.wanted[2]))' \
    '{send_buffer: mark_value("send_buffer"), in_fault: mark_value("sockets_in_fault")}'
  # The trickle's measured rate: netem's bytes over the fault (the cut to its stats, read just before
  # the clear), as bits per sink timeout against one frame's bits. A window of 0 or less reds there
  # (the bytes are never below 0), and shows no rate.
  require trickled 'mark_value("netem_out") as $n | (mark_at("netem_out") - mark_at("cut")) as $w
    | $n.sent_pkts >= 2
    and $n.sent_bytes * 8 * $t.output_sink_timeout_ms < mark_value("rate").frame_bits * $w' \
    '(mark_at("netem_out") - mark_at("cut")) as $w | {netem: mark_value("netem_out"), rate: mark_value("rate"),
      window_ms: $w, measured_bit_per_s: (if $w > 0 then mark_value("netem_out").sent_bytes * 8000 / $w | floor else null end)}'
  require stall_swap 'stall_swap_judged(mark_at("cut"))' 'stall_swap(mark_at("cut"))'
  measure swap_log_from_cut_ms '[.log.sink_swaps[] | select(.at_ms > mark_at("cut"))] | first | .at_ms - mark_at("cut")'
  measure sink_errors_in_fault '[.log.output_errors[] | select(.at_ms > mark_at("cut") and .at_ms <= mark_at("clear"))]
    | map({gen, after_cut_ms: (.at_ms - mark_at("cut"))})'
  measure sockets_in_fault 'mark_value("sockets_in_fault")'
  # A sink that publishes under the trickle (the PR #65 review's m-4) is Owed 88's write-stall path:
  # the clear's phase, and each publish from the cut, against the clear.
  measure clear_phase 'clear_phase'
  measure publishes_from_clear_ms 'if .log.sink_publishes == null then null
    else [.log.sink_publishes[] | select(.at_ms > mark_at("cut"))] | map({gen, from_clear_ms: (.at_ms - mark_at("clear"))}) end'
  require_output_recovers
  require_no_false_restart
}
