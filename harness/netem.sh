#!/usr/bin/env bash
# Network impairment for the harness (C13 brief, Step 4). tc acts on egress only, so each leg is
# impaired at its sender, from a sidecar that shares the sender's network namespace:
#   in   netem-in (MediaMTX's namespace): MediaMTX's SRT media TO THE COMPOSITOR only
#        (u32 match ip sport 8890, match ip dst 172.28.0.10), never to the camera. A filter on the
#        compositor's dport 8890 would only delay SRT ACKs.
#   out  netem-out (the compositor's namespace): the compositor's RTMP media to the destination
#        (u32 match ip dport 1935).
#
# Usage (from a case, while the stack is up):
#   netem.sh in|out <netem args...>   a prio qdisc (4 bands), the filter to band 4, netem on it
#   netem.sh clear in|out             removes the leg's qdiscs
#   netem.sh check in|out             FAILS unless the leg's netem qdisc has carried packets
#   netem.sh stats in|out             one JSON line: the netem qdisc's Sent bytes and packets and
#                                     its dropped count (C14: a case's witness that its fault ran;
#                                     netem counts the packets its `loss` drops there), or
#                                     "present": false once the leg is cleared
#
# `check` is what lets a case claim its fault was applied: the netem qdisc's own packet count is
# the witness that the filter matched (prints the filter's `success` count too when the kernel
# keeps one: CONFIG_CLS_U32_PERF).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
COMPOSITOR_IP=172.28.0.10

usage() {
  echo "usage: netem.sh in|out <netem args...> | clear in|out | check in|out | stats in|out" >&2
  exit 64
}

compose() { docker compose -f "$HERE/compose.yml" "$@"; }

service_of() {
  case "$1" in
    in) echo netem-in ;;
    out) echo netem-out ;;
    *) usage ;;
  esac
}

filter_of() {
  case "$1" in
    in) echo "match ip sport 8890 0xffff match ip dst $COMPOSITOR_IP/32" ;;
    out) echo "match ip dport 1935 0xffff" ;;
  esac
}

tc_on() {
  local leg="$1"
  shift
  compose exec -T "$(service_of "$leg")" tc "$@"
}

[ "$#" -ge 1 ] || usage
case "$1" in
  clear)
    [ "$#" -eq 2 ] || usage
    tc_on "$2" qdisc del dev eth0 root 2>/dev/null || true
    echo "netem $2: cleared"
    ;;
  check)
    [ "$#" -eq 2 ] || usage
    leg="$2"
    stats="$(tc_on "$leg" -s qdisc show dev eth0)"
    pkts="$(printf '%s\n' "$stats" | awk '/qdisc netem 40:/ { f = 1 } f && /Sent/ { print $4; exit }')"
    filters="$(tc_on "$leg" -s filter show dev eth0 parent 1:)"
    success="$(printf '%s\n' "$filters" | sed -n 's/.*success \([0-9][0-9]*\).*/\1/p' | head -n 1)"
    echo "netem $leg: ${pkts:-0} packets through the netem qdisc; filter success: ${success:-not reported by this kernel}"
    if ! [ "${pkts:-0}" -gt 0 ] 2>/dev/null; then
      echo "netem $leg: FAIL the filter matched nothing, so no fault was applied" >&2
      exit 1
    fi
    ;;
  stats)
    [ "$#" -eq 2 ] || usage
    leg="$2"
    stats="$(tc_on "$leg" -s qdisc show dev eth0)"
    printf '%s\n' "$stats" | awk -v leg="$leg" '
      /qdisc netem 40:/ { f = 1; next }
      f && /Sent/ {
        bytes = $2; pkts = $4; d = $0
        sub(/.*dropped /, "", d); sub(/,.*/, "", d)
        printf "{\"leg\":\"%s\",\"present\":true,\"sent_bytes\":%.0f,\"sent_pkts\":%.0f,\"dropped\":%.0f}\n", leg, bytes, pkts, d
        done = 1; exit
      }
      END { if (!done) printf "{\"leg\":\"%s\",\"present\":false,\"sent_bytes\":0,\"sent_pkts\":0,\"dropped\":0}\n", leg }'
    ;;
  in | out)
    leg="$1"
    shift
    [ "$#" -ge 1 ] || usage
    # shellcheck disable=SC2046 # the filter is several words by design
    {
      tc_on "$leg" qdisc del dev eth0 root 2>/dev/null || true
      tc_on "$leg" qdisc add dev eth0 root handle 1: prio bands 4
      tc_on "$leg" qdisc add dev eth0 parent 1:4 handle 40: netem "$@"
      tc_on "$leg" filter add dev eth0 parent 1: protocol ip prio 1 u32 $(filter_of "$leg") flowid 1:4
    }
    echo "netem $leg: netem $* on [$(filter_of "$leg")]"
    ;;
  *) usage ;;
esac
