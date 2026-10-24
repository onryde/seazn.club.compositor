# shellcheck shell=bash disable=SC2034,SC2016 # sourced by run-case.sh; jq filters keep their $ (SC2016)
# Case 17, dest-stall-rule (C14 wave-gate-3 follow-ups; the coordinator's decision on Owed 87,
# 2026-10-24): a destination that keeps the socket open and takes bytes, too slowly for one video
# frame to leave within output_stall_after_ms, and never errors. Only the stall rule can swap the
# sink here, and it must, within its budget: the swap's reason line reads `stall`, the outgoing sink
# logged no error, and the swap came within output_stall_after_ms + output_progress_poll_ms of the
# last growth (`stall_swap_judged(...; ["stall"])`, lib.jq). Cases 5 and 12 cannot show this: there
# rtmp2sink's own 5 s socket timeout fires first (Owed 87).
#
# How (harness/compose.stall-rule.yml says why): the compositor's TCP send buffer is capped at 8 KiB
# in its network namespace, and `netem out rate` from 20 s lets one video frame (the fixture's
# videoKbps / fps) leave in STALL_DRAIN_X stall thresholds. rtmp2sink counts a message's bytes only
# once it is all written, so the total stops growing, while the kernel frees room every second or
# two and the socket's 5 s wait never runs out. Cleared at 50 s; 80 s live; the output then
# recovers as in case 4.
#
# Anti-vacuity: the cap applied (the namespace's tcp_wmem reads what compose set, and every RTMP
# socket's send buffer was read and is at most its max: a buffer that compose never set reads null
# on both sides and fails there), and bytes really trickled (netem sent at least 2 packets, so this
# is no plain stall), at a rate (netem's 0 is no limit at all) under one frame per stall threshold. A red with reason `error` is the socket timing out after all:
# the cap or the rate did not hold the path the case needs, and the measures say which.
# Mutant 9 (output_stall_after_ms at 20 s) must red `stall_swap` here.

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
  rate=$((frame_bits * 1000 / (STALL_DRAIN_X * $(threshold output_stall_after_ms))))
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

CASE_CHECKS=14
case_verdict() {
  require buffer_capped 'mark_value("send_buffer") as $b | $b.read == $b.wanted
    and $b.before_cut.sockets >= 1 and ($b.before_cut.tb | all(. != null and . <= $b.wanted[2]))' \
    '{send_buffer: mark_value("send_buffer"), in_fault: mark_value("sockets_in_fault")}'
  require trickled 'mark_value("rate") as $r | mark_value("netem_out").sent_pkts >= 2 and $r.bit_per_s > 0
    and $r.bit_per_s * $t.output_stall_after_ms < $r.frame_bits * 1000' \
    '{netem: mark_value("netem_out"), rate: mark_value("rate")}'
  require stall_swap 'stall_swap_judged(mark_at("cut"); ["stall"])' 'stall_swap(mark_at("cut"))'
  measure swap_log_from_cut_ms '[.log.sink_swaps[] | select(.at_ms > mark_at("cut"))] | first | .at_ms - mark_at("cut")'
  measure sink_errors_in_fault '[.log.output_errors[] | select(.at_ms > mark_at("cut") and .at_ms <= mark_at("clear"))]
    | map({gen, after_cut_ms: (.at_ms - mark_at("cut"))})'
  measure sockets_in_fault 'mark_value("sockets_in_fault")'
  require_output_recovers
  require_no_false_restart
}
