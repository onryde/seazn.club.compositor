#!/usr/bin/env bash
# driver.sh under stubs (C15): one 1-minute drive on a compressed schedule (dropouts every 6 s,
# disconnects 2 s long), with a stub compositor that beats once a second, stub fakes, a stub curl
# answering as fake-control and fake-camera do, stub iptables that count the packets a DROP rule
# "dropped", a stub checker, and a stub job that uploads the compositor's log once /out/ended
# exists. What it proves: the driver's own sequence (live, every scheduled action with its wall
# time and witness, the end request, the log hand-off, the checker, the secrets scan, done), and
# that the actions it writes are the plan verdict.jq judges against. The media is soak.yml's run.
#
#   HARNESS_BASH=<bash 5> harness/soak/test-driver.sh     (driver.sh needs bash 5: mapfile, EPOCHREALTIME)
#
# About 100 s. Prints `checked: N` and fails on zero.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
BASH5="${HARNESS_BASH:-bash}"
"$BASH5" -c '[ "${BASH_VERSINFO[0]}" -ge 5 ]' || { echo "FAIL test-driver.sh needs bash 5 (HARNESS_BASH)"; exit 1; }
W="$(mktemp -d)"
cleanup() {
  [ -z "${stub_pids:-}" ] || kill $stub_pids 2>/dev/null || true
  rm -rf "$W"
}
trap cleanup EXIT

# The driver's files, as the image lays them out, with a compressed schedule.
mkdir -p "$W/soak/pages" "$W/out" "$W/bin" "$W/state"
cp "$HERE/driver.sh" "$HERE/schedule.jq" "$HERE/mediamtx.yml" "$HERE/sampler.py" "$W/soak/"
cp "$ROOT/contracts/fixtures/relay-job.v1/session-composed-720p.json" "$W/soak/"
cp "$ROOT/harness/pages/overlay-test.html" "$W/soak/pages/"
jq '. + {min_drive_min: 1, dropout_every_s: 6, dropout_phase_s: 3, dropout_pause_s: 1, disconnect_hold_s: 2,
         control_wait_s: 3, live_wait_s: 20, end_wait_s: 10, log_wait_s: 20, lost_beats_end_s: 5,
         sample_offsets_s: [6], sample_margin_s: 0}' "$HERE/soak.json" >"$W/soak/soak.json"

STATE="$W/state"
export STATE
# The long-running fakes: they only have to live until the driver kills them.
for b in mediamtx fake-camera fake-control; do
  printf '#!/usr/bin/env bash\necho "%s $*" >>"$STATE/calls"\nexec sleep 300\n' "$b" >"$W/bin/$b"
done
# curl: fake-control's routes on 127.0.0.1:7000 and fake-camera's on 127.0.0.1:7001.
cat >"$W/bin/curl" <<'EOF'
#!/usr/bin/env bash
method=GET url="" wfmt="" out=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -X) method="$2"; shift 2 ;;
    -w) wfmt="$2"; shift 2 ;;
    -o) out="$2"; shift 2 ;;
    -m) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
echo "curl $method $url" >>"$STATE/calls"
body=""
case "$method $url" in
  "GET http://127.0.0.1:7000/healthz") [ ! -e "$STATE/no-healthz" ] || exit 7; body="ok" ;;
  "POST http://127.0.0.1:7000/control/timeline-start") body='{"start_wall_ms":1}' ;;
  "POST http://127.0.0.1:7000/control/desired?state=ending") touch "$STATE/ending"; body='{"desiredState":"ending"}' ;;
  "GET http://127.0.0.1:7000/control/timeline") body='{"start_wall_ms":1,"delay_ms":3100,"events":[]}' ;;
  "GET http://127.0.0.1:7000/control/state") body='{"session_fetches":1,"beats":60,"other_session_requests":0,"unauthorized_requests":0}' ;;
  "POST http://127.0.0.1:7001/pause" | "POST http://127.0.0.1:7001/resume") body='{}' ;;
  *) echo "stub curl: no route $method $url" >&2; exit 22 ;;
esac
if [ -n "$out" ]; then printf '%s' "$body" >"$out"; else printf '%s' "$body"; fi
[ -n "$wfmt" ] && printf '200'
exit 0
EOF
# iptables: each backend's INPUT chain is a file; a DROP rule on dport 1935 counts 7 packets.
for fam in iptables ip6tables; do
  cat >"$W/bin/$fam-nft" <<EOF
#!/usr/bin/env bash
f="\$STATE/$fam.rules"
echo "$fam-nft \$*" >>"\$STATE/calls"
case " \$* " in
  *" -I INPUT "*) echo "DROP 1935" >>"\$f" ;;
  *" -D INPUT "*) grep -q DROP "\$f" 2>/dev/null || exit 1; : >"\$f" ;;
  *" -L INPUT -v -n -x "*)
    echo "Chain INPUT (policy ACCEPT 0 packets, 0 bytes)"
    echo "    pkts      bytes target     prot opt in     out     source               destination"
    if grep -q DROP "\$f" 2>/dev/null; then echo "       7      420 DROP       6    --  *      *       ::/0                 ::/0                 tcp dpt:1935"; fi ;;
  *" -L INPUT -n "*) echo "Chain INPUT (policy ACCEPT)" ;;
  *) exit 2 ;;
esac
EOF
  printf '#!/usr/bin/env bash\nexit 3\n' >"$W/bin/$fam-legacy"
done
# The checker: a report whose frame count says it ran on the driver's inputs.
cat >"$W/bin/checker" <<'EOF'
#!/usr/bin/env bash
echo "checker $*" >>"$STATE/calls"
o=""
while [ "$#" -gt 0 ]; do [ "$1" = --out ] && o="$2"; shift; done
echo '{"frames":1}' >"$o"
echo "checker: report written to $o"
EOF
chmod +x "$W"/bin/*

# The stub compositor: a live beat a second until the end request, then a stopped beat.
(
  while [ ! -e "$STATE/ending" ]; do
    printf '{"received_ms":%s,"body":{"state":"playing","programmePhase":"live","rung":"full"}}\n' \
      "$(python3 -I -c 'import time; print(int(time.time()*1000))')" >>"$W/out/heartbeats.jsonl"
    sleep 1
  done
  printf '{"received_ms":%s,"body":{"state":"stopped","exitReason":"ended"}}\n' \
    "$(python3 -I -c 'import time; print(int(time.time()*1000))')" >>"$W/out/heartbeats.jsonl"
) &
stub_pids="$!"
# The stub job: the compositor's log once the run has ended.
(
  until [ -e "$W/out/ended" ]; do sleep 0.5; done
  printf '2026-10-24T12:00:00.000000000Z ts=2026-10-24T12:00:00.000Z level=info msg=cef alive gen=1\n' >"$W/out/compositor.log"
  touch "$W/out/log-ready"
) &
stub_pids="$stub_pids $!"

now_ms() { python3 -I -c 'import time; print(int(time.time()*1000))'; }
t0="$(now_ms)"
JOB_TOKEN="tjt_$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')"
rc=0
# The driver truncates heartbeats.jsonl at boot: the stub compositor appends after it (>>), so the
# first second's beats may be lost, as a real compositor's beats before boot would be.
env -i PATH="$W/bin:/usr/bin:/bin:/usr/sbin:/sbin:$(dirname "$(command -v jq)")" HOME="$W" STATE="$STATE" \
  SOAK_ROOT="$W/soak" SOAK_OUT="$W/out" SESSION_ID="11111111-2222-4333-8444-555555555555" JOB_TOKEN="$JOB_TOKEN" \
  SOAK_DRIVE_MIN=1 SOAK_DEADLINE_MS=$((t0 + 600000)) SOAK_HARD_STOP_MS=$((t0 + 90000)) SOAK_PAGE=test \
  FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test "$BASH5" "$W/soak/driver.sh" >"$W/driver.out" 2>&1 || rc=$?

checked=0
failures=0
check() {
  checked=$((checked + 1))
  if jq -e -s "$2" "$3" >/dev/null 2>&1; then echo "PASS $1"; else echo "FAIL $1 ($2): $(jq -c -s . "$3" 2>/dev/null | head -c 400)"; failures=$((failures + 1)); fi
}
plan="$(jq -c -n -L "$W/soak" --slurpfile soak "$W/soak/soak.json" 'include "schedule"; soak_plan($soak[0]; 60)')"
echo "driver exit $rc; plan $plan"
[ "$rc" -eq 0 ] || { echo "FAIL the driver exited $rc"; tail -n 30 "$W/driver.out"; failures=$((failures + 1)); }
A="$W/out/actions.jsonl"
check "one start, drive 60 s" '[.[] | select(.kind == "start")] | length == 1 and .[0].drive_s == 60' "$A"
check "one end, last, not early" '(last | .kind == "end" and (has("ended_early") | not)) and ([.[] | select(.kind == "end")] | length == 1)' "$A"
check "every planned dropout, ok, in slot order" \
  "[.[] | select(.kind == \"dropout\")] | (map(.slot) == [range(0; $(jq '.dropouts | length' <<<"$plan"))]) and all(.ok and .pause == 200 and .resume == 200)" "$A"
check "every planned disconnect, ok, both families, packets dropped" \
  "[.[] | select(.kind == \"disconnect\")] | (map(.slot) == [range(0; $(jq '.disconnects | length' <<<"$plan"))])
     and all(.ok and .dropped_packets == 14 and .families == [\"ipv4\", \"ipv6\"])" "$A"
check "each action on its planned time (within 1.5 s), lasting its hold" \
  "(.[0].at_ms) as \$s | ($plan) as \$p | [.[] | select(.kind == \"dropout\" or .kind == \"disconnect\")]
     | all((.at_ms - .planned_ms) >= 0 and (.at_ms - .planned_ms) < 1500)
     and ([.[] | select(.kind == \"dropout\") | .planned_ms - \$s] == (\$p.dropouts | map(. * 1000)))
     and ([.[] | select(.kind == \"disconnect\") | .planned_ms - \$s] == (\$p.disconnects | map(. * 1000)))
     and all(.end_ms - .at_ms >= (if .kind == \"dropout\" then 1000 else 2000 end))" "$A"
check "in time order" '[.[] | .at_ms] as $t | $t == ($t | sort)' "$A"
check "every rule removed" '. == []' <(cat "$STATE/iptables.rules" "$STATE/ip6tables.rules" | jq -R . )
check "the end request, after the drive" "(.[0]) as \$e | \$e.end_request_ms >= $t0" "$W/out/ended"
check "the checker ran on the uploaded log" '.[0] | test("--compositor-log [^ ]*/out/compositor.log") and test("--end-request-ms [0-9]+")' \
  <(grep '^checker ' "$STATE/calls" | jq -R .)
check "done: ok, live, checker 0, no leak" '.[0] | .ok and .went_live and .checker_exit == 0 and .leaks == 0 and .ended_early == null' "$W/out/done"
check "secrets scanned" '.[0] | .scanned > 5 and .leaks == 0' "$W/out/secrets.json"
# No recording under stubs: the sampler ran, and its one minute says it had no segment.
check "the sampler ran: one minute, unsampled for want of a segment" \
  'length == 1 and (.[0] | .minute == 0 and .differs == false and (.error | test("no programme segment")))' "$W/out/overlay-samples.jsonl"
check "done carries the sampler's exit, 0" '.[0].sampler_exit == 0' "$W/out/done"
check "status done" '.[0].phase == "done"' "$W/out/status.json"
check "fake-control on every address, the private name in the session's origins" \
  '.[0] | test("--listen \\[::\\]:7000") and test("--input-origin srt://m0.vm.soak-test.internal:8890") and test("--overlay-origin http://m0.vm.soak-test.internal:7000")' \
  <(grep '^fake-control ' "$STATE/calls" | jq -R .)

# Run 2, the error path: fake-control never answers. The driver writes done (not ok, with its exit)
# and status error, then holds /out until the hard stop (soak.yml fetches on phase error), and exits
# with its error.
mkdir -p "$W/out2"
touch "$STATE/no-healthz"
t1="$(now_ms)"
rc2=0
env -i PATH="$W/bin:/usr/bin:/bin:/usr/sbin:/sbin:$(dirname "$(command -v jq)")" HOME="$W" STATE="$STATE" \
  SOAK_ROOT="$W/soak" SOAK_OUT="$W/out2" SESSION_ID="11111111-2222-4333-8444-555555555555" JOB_TOKEN="$JOB_TOKEN" \
  SOAK_DRIVE_MIN=1 SOAK_DEADLINE_MS=$((t1 + 600000)) SOAK_HARD_STOP_MS=$((t1 + 8000)) SOAK_PAGE=test \
  FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test "$BASH5" "$W/soak/driver.sh" >"$W/driver2.out" 2>&1 || rc2=$?
held_ms=$(($(now_ms) - t1))
check "error: done not ok, with the driver's exit" '.[0] | .ok == false and .driver_exit == 1' "$W/out2/done"
check "error: status error" '.[0].phase == "error"' "$W/out2/status.json"
checked=$((checked + 1))
if [ "$rc2" -eq 1 ] && [ "$held_ms" -ge 7500 ]; then
  echo "PASS error: exit 1, /out held to the hard stop (${held_ms} ms)"
else
  echo "FAIL error: exit $rc2 after ${held_ms} ms (want 1, held at least 7500 ms)"
  tail -n 20 "$W/driver2.out"
  failures=$((failures + 1))
fi
checked=$((checked + 1))
if printf '%s\n' "$JOB_TOKEN" | grep -rqF -f - "$W/out" "$W/out2" "$W/driver.out" "$W/driver2.out" "$STATE/calls"; then
  echo "FAIL the job token reached a file"
  failures=$((failures + 1))
else
  echo "PASS the job token reached no file"
fi
echo "checked: $checked, $failures failures ($(( ($(now_ms) - t0) / 1000 )) s)"
[ "$checked" -gt 0 ] && [ "$failures" -eq 0 ] || { tail -n 40 "$W/driver.out"; exit 1; }
