#!/usr/bin/env bash
# driver.sh under stubs (C15): one 1-minute drive on a compressed schedule (dropouts every 6 s,
# disconnects 2 s long), with a stub compositor that beats once a second, stub fakes, a stub curl
# answering as fake-control and fake-camera do, stub iptables that count the packets a DROP rule
# "dropped", a stub checker, and a stub job that uploads the compositor's log once /out/ended
# exists. What it proves: the driver's own sequence (live, every scheduled action with its wall
# time and witness, the end request, the log hand-off, the checker, the secrets scan, done), and
# that the actions it writes are the plan verdict.jq judges against. The media is soak.yml's run.
# Run 1 also serves soak.json's soak_output, the soak's output (1080p: production's output since the
# owner's ruling of 2026-10-27, ADR 0005's amendment), as the session, unchanged, with the camera at
# its size; run 3 serves it on page=real. Runs 3 and 4 (in parallel) take page=real with a stub
# page source (web/up.sh) and a stub score driver: the session fake-control serves carries the
# page's URL and token and stays out of /out,
# the deliveries are written, and the secrets scan covers the page source's credentials (run 4
# plants the page token in /out, and must read as a leak). Run 5 (in parallel too) hangs the checker:
# the driver stops it at the hard stop's reserve, skips the sampler, and still exits by the hard
# stop. Run 6 (page=real) fails in the page source after its signing key reached /out: the error
# path's scan names it before anything is fetched. Run 7 (page=real) fails the same way with a jar
# that also holds a cookie the scan was never told about (planted in /out) and a short one, and the
# session cookie planted: three leaks by name, the inert cookies named as not scanned. Run 8
# (page=real) finishes with a jar of inert cookies only: the strict scan has no cookie to scan with
# and fails by name (WEB_COOKIES), never passes on nothing. Runs 9 and 10 are cost legs B and C
# (SOAK_LEG): the leg's session fixture (720p, 1080p), sport and template reach the page source and
# fake-control, the camera's size follows the output, no dropout or disconnect is scheduled, the
# leg's score driver runs at its pace (rally.mjs every 3 s; score.mjs --boundaries every 6 s), and
# done names the leg and its stills. Runs 11 and 12 refuse an unknown leg and a leg on page=test.
# Run 13 (page=test, in parallel) takes SOAK_SCHEDULE=dropouts (the memory experiment's schedule,
# compressed to a dropout every 4 s from 2 s): every dropout dropout_plan holds, on its planned time,
# no disconnect and no firewall rule, and the start action names the schedule (run 1's names soak, a
# leg's names leg). Runs 14 and 15 refuse an unknown schedule and the dropouts schedule on a leg.
# Run 16 (page=test, in parallel) takes SOAK_SCHEDULE=none (the experiment's control): the start
# names it, and no dropout, disconnect, camera pause or firewall rule happens in the whole drive.
# Run 17 refuses the none schedule on a leg. Runs 18, 19 and 20 refuse a soak_output with no session
# fixture, an empty one and an absent one (64, by name, before any action). Run 21 is leg C with
# soak_output absent: a leg serves its own output, never reads soak_output, and reaches the next
# refusal (no page source); leg B serving 720p beside a 1080p soak_output (run 9) is the same rule.
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
cp "$ROOT/contracts/fixtures/relay-job.v1/session-composed-720p.json" "$ROOT/contracts/fixtures/relay-job.v1/session-composed-1080p.json" "$W/soak/"
cp "$ROOT/harness/pages/overlay-test.html" "$W/soak/pages/"
jq '. + {min_drive_min: 1, dropout_every_s: 6, dropout_phase_s: 3, dropout_pause_s: 1, disconnect_hold_s: 2,
         dropout_only: {every_s: 4, phase_s: 2},
         control_wait_s: 3, live_wait_s: 20, end_wait_s: 10, log_wait_s: 20, lost_beats_end_s: 5,
         sample_offsets_s: [6], sample_margin_s: 0, post_check_reserve_s: 5}' "$HERE/soak.json" >"$W/soak/soak.json"

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
# The checker: a report whose frame count says it ran on the driver's inputs; it hangs (its pid
# recorded) when the run's state says so.
cat >"$W/bin/checker" <<'EOF'
#!/usr/bin/env bash
echo "checker $*" >>"$STATE/calls"
if [ -e "$STATE/hang" ]; then echo "$$" >"$STATE/checker.pid"; exec sleep 300; fi
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
PATH_STUB="$W/bin:/usr/bin:/bin:/usr/sbin:/sbin:$(dirname "$(command -v jq)"):$(dirname "$(command -v node)")"

# Runs 3 and 4, page=real, started first and in the background (they take as long as run 1): each
# with its own stub compositor, stub job, state and outputs.
mkdir -p "$W/soak/web"
cat >"$W/soak/web/up.sh" <<'STUB'
#!/usr/bin/env bash
# The stub page source: the files the real up.sh leaves, with random credentials.
set -euo pipefail
echo "up.sh $(env | cut -d= -f1 | sort | tr '\n' ' ')" >>"$STATE/calls"
echo "up.sh-leg sport=${WEB_SPORT:-} template=${WEB_TEMPLATE:-} delay=${WEB_DELAY_MS:-}" >>"$STATE/calls"
mkdir -p "$WEB_STATE"; chmod 700 "$WEB_STATE"
r() { od -An -N16 -tx1 /dev/urandom | tr -d ' \n'; }
r >"$WEB_STATE/auth-secret"
# The jar as the web leaves it: the session, the org, and the signed-in hint, whose value is "1"
# (rehearsal 40131252539 read that "1" as a leak in every file).
if [ -e "$STATE/jar-inert" ]; then
  # Only the inert cookies (soak.json's web_cookies_inert): nothing in the jar to scan with.
  printf '{"seazn_org":"%s","seazn_signed_in":"1"}' "org$(r)" >"$WEB_STATE/cookies.json"
elif [ -e "$STATE/jar-extra" ]; then
  # A cookie no list names (a credential the web might add) and a short one.
  printf '{"seazn_session":"%s","seazn_org":"%s","seazn_signed_in":"1","seazn_future":"fut%s","seazn_lang":"en"}' \
    "ses$(r)" "org$(r)" "$(r)" >"$WEB_STATE/cookies.json"
else
  printf '{"seazn_session":"%s","seazn_org":"%s","seazn_signed_in":"1"}' "ses$(r)" "org$(r)" >"$WEB_STATE/cookies.json"
fi
# Plants in /out, by the run's state: the signing key, the session cookie, the unnamed cookie.
[ ! -e "$STATE/plant-key" ] || echo "AUTH_SECRET=$(cat "$WEB_STATE/auth-secret")" >>"$OUT/web.log"
[ ! -e "$STATE/plant-session" ] || echo "cookie: seazn_session=$(jq -r .seazn_session "$WEB_STATE/cookies.json")" >>"$OUT/web.log"
[ ! -e "$STATE/plant-future" ] || echo "set-cookie: seazn_future=$(jq -r .seazn_future "$WEB_STATE/cookies.json")" >>"$OUT/score.log"
# A page source that fails before the signed URL.
if [ -e "$STATE/web-fail" ]; then echo "soak-web: FAIL (stub)"; exit 1; fi
st="eyJ$(r).eyJ$(r).$(r)"
printf 'http://%s:%s/overlay/fixtures/aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee?sid=%s&st=%s&host=compositor&delay=%s&template=%s\n' \
  "$HOST" "$WEB_PORT" "$SESSION_ID" "$st" "$WEB_DELAY_MS" "${WEB_TEMPLATE:-bar}" >"$WEB_STATE/overlay-url"
echo '{"cricket":{"fixture_id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"},"badminton":{"fixture_id":"bbbbbbbb-cccc-4ddd-8eee-ffffffffffff"}}' >"$OUT/seed.json"
sleep 300 &
echo "$!" >"$WEB_STATE/server.pid"
[ ! -e "$STATE/leak" ] || echo "GET /overlay?st=$st" >>"$OUT/web.log"
echo "soak-web: up (stub)"
STUB
cat >"$W/soak/web/score.mjs" <<'STUB'
// The stub score driver: a delivery every 0.5 s until --until-ms or TERM; its argv recorded.
import { appendFileSync } from "node:fs";
const a = (k) => process.argv[process.argv.indexOf(`--${k}`) + 1];
appendFileSync(`${process.env.STATE}/calls`, `score.mjs ${process.argv.slice(2).join(" ")}\n`);
let stop = false;
process.on("SIGTERM", () => { stop = true; });
let seq = 1;
while (!stop && Date.now() < Number(a("until-ms"))) {
  appendFileSync(a("out"), JSON.stringify({ at_ms: Date.now(), ok: true, status: 201, seq: ++seq }) + "\n");
  await new Promise((r) => setTimeout(r, 500));
}
console.log(`score: ${seq - 1} attempts`);
STUB
sed 's/score\.mjs /rally.mjs /; s/The stub score driver/The stub rally driver/' "$W/soak/web/score.mjs" >"$W/soak/web/rally.mjs"
chmod +x "$W/soak/web/up.sh"
stub_run() { # <out> <state>: the stub compositor and the stub job, for one run
  (
    while [ ! -e "$2/ending" ]; do
      printf '{"received_ms":%s,"body":{"state":"playing","programmePhase":"live","rung":"full"}}\n' \
        "$(python3 -I -c 'import time; print(int(time.time()*1000))')" >>"$1/heartbeats.jsonl"
      sleep 1
    done
    printf '{"received_ms":%s,"body":{"state":"stopped","exitReason":"ended"}}\n' \
      "$(python3 -I -c 'import time; print(int(time.time()*1000))')" >>"$1/heartbeats.jsonl"
  ) &
  stub_pids="$stub_pids $!"
  (
    until [ -e "$1/ended" ]; do sleep 0.5; done
    printf '2026-10-24T12:00:00.000000000Z ts=2026-10-24T12:00:00.000Z level=info msg=cef alive gen=1\n' >"$1/compositor.log"
    touch "$1/log-ready"
  ) &
  stub_pids="$stub_pids $!"
}
real_run() { # <n> [VAR=value...]: page=real, under the stub page source, with the extra environment
  local n="$1" t rc=0
  t="$(now_ms)"
  env -i PATH="$PATH_STUB" HOME="$W" STATE="$W/state$n" SOAK_ROOT="$W/soak" SOAK_OUT="$W/out$n" SOAK_WEB_STATE="$W/web$n" \
    SESSION_ID="11111111-2222-4333-8444-5555555555$(printf '%02d' "$n")" JOB_TOKEN="$JOB_TOKEN" SOAK_DRIVE_MIN=1 \
    SOAK_DEADLINE_MS=$((t + 600000)) SOAK_HARD_STOP_MS=$((t + 100000)) SOAK_PAGE=real \
    FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test "${@:2}" "$BASH5" "$W/soak/driver.sh" >"$W/driver$n.out" 2>&1 || rc=$?
  echo "$rc" >"$W/rc$n"
}
for n in 3 4; do mkdir -p "$W/out$n" "$W/state$n"; stub_run "$W/out$n" "$W/state$n"; done
touch "$W/state4/leak"
real_run 3 &
real_pids="$!"
real_run 4 &
real_pids="$real_pids $!"
# Run 8: the inert-only jar. Runs 9 and 10: cost legs B (noise) and C (snow).
for n in 8 9 10; do mkdir -p "$W/out$n" "$W/state$n"; stub_run "$W/out$n" "$W/state$n"; done
touch "$W/state8/jar-inert"
real_run 8 &
real_pids="$real_pids $!"
real_run 9 SOAK_LEG=B CAMERA_PATTERN=noise &
real_pids="$real_pids $!"
real_run 10 SOAK_LEG=C CAMERA_PATTERN=snow &
real_pids="$real_pids $!"
# Run 5: page=test, the checker hangs. Its start and end times recorded, against its hard stop.
mkdir -p "$W/out5" "$W/state5"
touch "$W/state5/hang"
stub_run "$W/out5" "$W/state5"
(
  t="$(now_ms)"
  rc=0
  env -i PATH="$PATH_STUB" HOME="$W" STATE="$W/state5" SOAK_ROOT="$W/soak" SOAK_OUT="$W/out5" \
    SESSION_ID="11111111-2222-4333-8444-555555555556" JOB_TOKEN="$JOB_TOKEN" SOAK_DRIVE_MIN=1 \
    SOAK_DEADLINE_MS=$((t + 600000)) SOAK_HARD_STOP_MS=$((t + 100000)) SOAK_PAGE=test \
    FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test "$BASH5" "$W/soak/driver.sh" >"$W/driver5.out" 2>&1 || rc=$?
  echo "$rc $t $(now_ms)" >"$W/rc5"
) &
real_pids="$real_pids $!"
# Run 13: page=test on the dropout-only schedule.
mkdir -p "$W/out13" "$W/state13"
stub_run "$W/out13" "$W/state13"
(
  t="$(now_ms)"
  rc=0
  env -i PATH="$PATH_STUB" HOME="$W" STATE="$W/state13" SOAK_ROOT="$W/soak" SOAK_OUT="$W/out13" \
    SESSION_ID="11111111-2222-4333-8444-555555555513" JOB_TOKEN="$JOB_TOKEN" SOAK_DRIVE_MIN=1 \
    SOAK_DEADLINE_MS=$((t + 600000)) SOAK_HARD_STOP_MS=$((t + 100000)) SOAK_PAGE=test SOAK_SCHEDULE=dropouts \
    FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test "$BASH5" "$W/soak/driver.sh" >"$W/driver13.out" 2>&1 || rc=$?
  echo "$rc" >"$W/rc13"
) &
real_pids="$real_pids $!"
# Run 16: page=test on the control schedule (nothing in the drive).
mkdir -p "$W/out16" "$W/state16"
stub_run "$W/out16" "$W/state16"
(
  t="$(now_ms)"
  rc=0
  env -i PATH="$PATH_STUB" HOME="$W" STATE="$W/state16" SOAK_ROOT="$W/soak" SOAK_OUT="$W/out16" \
    SESSION_ID="11111111-2222-4333-8444-555555555516" JOB_TOKEN="$JOB_TOKEN" SOAK_DRIVE_MIN=1 \
    SOAK_DEADLINE_MS=$((t + 600000)) SOAK_HARD_STOP_MS=$((t + 100000)) SOAK_PAGE=test SOAK_SCHEDULE=none \
    FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test "$BASH5" "$W/soak/driver.sh" >"$W/driver16.out" 2>&1 || rc=$?
  echo "$rc" >"$W/rc16"
) &
real_pids="$real_pids $!"
# Run 6: page=real, the page source fails after leaking its key into /out; the hard stop is near.
mkdir -p "$W/out6" "$W/state6"
touch "$W/state6/web-fail" "$W/state6/plant-key"
(
  t="$(now_ms)"
  rc=0
  env -i PATH="$PATH_STUB" HOME="$W" STATE="$W/state6" SOAK_ROOT="$W/soak" SOAK_OUT="$W/out6" SOAK_WEB_STATE="$W/web6" \
    SESSION_ID="11111111-2222-4333-8444-555555555557" JOB_TOKEN="$JOB_TOKEN" SOAK_DRIVE_MIN=1 \
    SOAK_DEADLINE_MS=$((t + 600000)) SOAK_HARD_STOP_MS=$((t + 8000)) SOAK_PAGE=real \
    FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test "$BASH5" "$W/soak/driver.sh" >"$W/driver6.out" 2>&1 || rc=$?
  echo "$rc" >"$W/rc6"
) &
real_pids="$real_pids $!"
# Run 7: page=real, the page source fails with the extra jar, the session and the unnamed cookie in /out.
mkdir -p "$W/out7" "$W/state7"
touch "$W/state7/web-fail" "$W/state7/jar-extra" "$W/state7/plant-session" "$W/state7/plant-future"
(
  t="$(now_ms)"
  rc=0
  env -i PATH="$PATH_STUB" HOME="$W" STATE="$W/state7" SOAK_ROOT="$W/soak" SOAK_OUT="$W/out7" SOAK_WEB_STATE="$W/web7" \
    SESSION_ID="11111111-2222-4333-8444-555555555558" JOB_TOKEN="$JOB_TOKEN" SOAK_DRIVE_MIN=1 \
    SOAK_DEADLINE_MS=$((t + 600000)) SOAK_HARD_STOP_MS=$((t + 8000)) SOAK_PAGE=real \
    FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test "$BASH5" "$W/soak/driver.sh" >"$W/driver7.out" 2>&1 || rc=$?
  echo "$rc" >"$W/rc7"
) &
real_pids="$real_pids $!"

# The hard stop from the launch itself (the stubs above took a moment), with room for a slow boot.
t_run="$(now_ms)"
env -i PATH="$PATH_STUB" HOME="$W" STATE="$STATE" \
  SOAK_ROOT="$W/soak" SOAK_OUT="$W/out" SESSION_ID="11111111-2222-4333-8444-555555555555" JOB_TOKEN="$JOB_TOKEN" \
  SOAK_DRIVE_MIN=1 SOAK_DEADLINE_MS=$((t_run + 600000)) SOAK_HARD_STOP_MS=$((t_run + 100000)) SOAK_PAGE=test \
  FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test "$BASH5" "$W/soak/driver.sh" >"$W/driver.out" 2>&1 || rc=$?

checked=0
failures=0
check() {
  checked=$((checked + 1))
  if jq -e -s "$2" "$3" >/dev/null 2>&1; then echo "PASS $1"; else echo "FAIL $1 ($2): $(jq -c -s . "$3" 2>/dev/null | head -c 400)"; failures=$((failures + 1)); fi
}
# check, with jq options before the file: <name> <expr> <option>... <file>
checkx() {
  checked=$((checked + 1))
  if jq -e -s "$2" "${@:3}" >/dev/null 2>&1; then echo "PASS $1"; else echo "FAIL $1 ($2): $(jq -c -s . "${@: -1}" 2>/dev/null | head -c 400)"; failures=$((failures + 1)); fi
}
plan="$(jq -c -n -L "$W/soak" --slurpfile soak "$W/soak/soak.json" 'include "schedule"; soak_plan($soak[0]; 60)')"
echo "driver exit $rc; plan $plan"
[ "$rc" -eq 0 ] || { echo "FAIL the driver exited $rc"; tail -n 30 "$W/driver.out"; failures=$((failures + 1)); }
A="$W/out/actions.jsonl"
check "one start, drive 60 s, the soak schedule named" '[.[] | select(.kind == "start")] | length == 1 and .[0].drive_s == 60 and .[0].schedule == "soak"' "$A"
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
checkx "secrets.json names the compositor log it scanned by its sha256" '.[0].compositor_log_sha256 == $want' \
  --arg want "$(python3 -I -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$W/out/compositor.log")" "$W/out/secrets.json"
# No recording under stubs: the sampler ran, and its one minute says it had no segment.
check "the sampler ran: one minute, unsampled for want of a segment" \
  'length == 1 and (.[0] | .minute == 0 and .differs == false and (.error | test("no programme segment")))' "$W/out/overlay-samples.jsonl"
check "done carries the sampler's exit, 0" '.[0].sampler_exit == 0' "$W/out/done"
check "status done" '.[0].phase == "done"' "$W/out/status.json"
check "fake-control on every address, the private name in the session's origins" \
  '.[0] | test("--listen \\[::\\]:7000") and test("--input-origin srt://m0.vm.soak-test.internal:8890") and test("--overlay-origin http://m0.vm.soak-test.internal:7000")' \
  <(grep '^fake-control ' "$STATE/calls" | jq -R .)
# The soak's session: soak.json's soak_output, 1080p (production's output, ADR 0005's 2026-10-27
# amendment), the vendored fixture served unchanged on page=test, and the camera at its size.
checkx "the soak serves the 1080p fixture, unchanged (soak.json soak_output)" \
  '.[0] == $want[0] and .[0].output.width == 1920 and .[0].output.height == 1080' \
  --slurpfile want "$ROOT/contracts/fixtures/relay-job.v1/session-composed-1080p.json" "$W/out/session.json"
check "the soak's camera at 1920x1080, ball" '.[0] | test("--size 1920x1080 --pattern ball( |$)")' \
  <(grep '^fake-camera ' "$STATE/calls" | jq -R .)

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
check "error: done not ok, with the driver's exit" '.[0] | .ok == false and .driver_exit == 1 and .leaks == 0' "$W/out2/done"
check "error: status error" '.[0].phase == "error"' "$W/out2/status.json"
check "error: the secrets scan ran before the fetch, clean, nothing skipped" '.[0] | .scanned > 0 and .leaks == 0 and .skipped == []' "$W/out2/secrets.json"
checked=$((checked + 1))
if [ "$rc2" -eq 1 ] && [ "$held_ms" -ge 7500 ] && [ "$held_ms" -lt 14000 ]; then
  echo "PASS error: exit 1, /out held to the hard stop and no longer (${held_ms} ms)"
else
  echo "FAIL error: exit $rc2 after ${held_ms} ms (want 1, held from 7500 ms to under 14000 ms)"
  tail -n 20 "$W/driver2.out"
  failures=$((failures + 1))
fi

# Runs 3 and 4 (page=real).
wait $real_pids
R="$W/out3"
checked=$((checked + 1))
if [ "$(cat "$W/rc3")" -eq 0 ]; then echo "PASS real: the driver exited 0"; else
  echo "FAIL real: the driver exited $(cat "$W/rc3")"; tail -n 30 "$W/driver3.out"; failures=$((failures + 1)); fi
check "real: the page source ran without the run's credentials in its environment" \
  '.[0] | test("^up.sh ") and (test(" JOB_TOKEN ") | not) and (test(" HARNESS_SRT_PASSPHRASE ") | not) and test(" SESSION_ID ")' \
  <(grep '^up.sh ' "$W/state3/calls" | jq -R .)
check "real: fake-control serves the session under the web state, the page's origin as the overlay's" \
  ".[0] | test(\"--fixture $W/web3/session.json \") and test(\"--overlay-origin http://m0.vm.soak-test.internal:3000 \")" \
  <(grep '^fake-control ' "$W/state3/calls" | jq -R .)
checkx "real: the session carries the page's URL, its token as pageToken, the seeded fixture, soak_output's (1080p) output and delay" \
  "(.[0]) as \$s | (\$s.overlayUrl == \"$(tr -d '\n' <"$W/web3/overlay-url")\")
     and (\$s.overlayUrl | contains(\"st=\" + \$s.pageToken + \"&\")) and (\$s.pageToken | length > 40)
     and \$s.fixtureId == \"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee\"
     and \$s.output == \$want[0].output and \$s.output.height == 1080 and \$s.overlayDelayMs == \$want[0].overlayDelayMs" \
  --slurpfile want "$ROOT/contracts/fixtures/relay-job.v1/session-composed-1080p.json" "$W/web3/session.json"
checked=$((checked + 1))
if [ ! -e "$R/session.json" ] && ! grep -rqF "st=" "$R" 2>/dev/null; then echo "PASS real: no session or signed URL in /out"; else
  echo "FAIL real: the session or a signed URL is in /out"; failures=$((failures + 1)); fi
check "real: the score driver ran on soak.json's cadence to the drive's end, its argv clean" \
  "(.[0]) as \$a | (\$a | test(\"--every-s 30 \") and test(\"--until-ms [0-9]{13} \") and test(\"--out $R/deliveries.jsonl\"))
     and (\$a | test(\"st=|eyJ\") | not)" <(grep '^score.mjs ' "$W/state3/calls" | jq -R .)
checkx "real: deliveries written, every one after the start" \
  "(\$start[0]) as \$s | length > 10 and all(.ok and .at_ms >= \$s)" --slurpfile start <(jq -c 'select(.kind == "start") | .at_ms' "$R/actions.jsonl") "$R/deliveries.jsonl"
checkx "real: the score driver stopped before the end request" \
  "(\$end[0]) as \$e | all(.at_ms <= \$e)" --slurpfile end <(jq -c '.end_request_ms' "$R/ended") "$R/deliveries.jsonl"
check "real: done ok, no leak" '.[0] | .ok and .leaks == 0 and .checker_exit == 0' "$R/done"
checked=$((checked + 1))
if grep -q "secrets checked" "$W/driver3.out" && ! grep -q "is empty: nothing to scan with" "$W/driver3.out"; then echo "PASS real: every credential had a value to scan with"; else
  echo "FAIL real: a credential was empty"; failures=$((failures + 1)); fi
checked=$((checked + 1))
if grep -q "secrets: inert cookies not scanned (soak.json web_cookies_inert): seazn_org, seazn_signed_in" "$W/driver3.out"; then
  echo "PASS real: the jar scanned but its inert cookies, named (the org, the hint)"; else
  echo "FAIL real: no cookies-not-scanned line"; grep 'secrets' "$W/driver3.out"; failures=$((failures + 1)); fi
check "real leak: the page token in /out reads as a leak, done not ok" '.[0] | .leaks == 1 and .ok == false' "$W/out4/done"
checked=$((checked + 1))
if grep -q "FAIL WEB_PAGE_TOKEN appears in /out: web.log:1 \"GET /overlay?st=<WEB_PAGE_TOKEN>\"" "$W/driver4.out"; then
  echo "PASS real leak: named WEB_PAGE_TOKEN, its file and line, the line redacted"; else
  echo "FAIL real leak: no redacted WEB_PAGE_TOKEN line"; grep 'appears in' "$W/driver4.out"; failures=$((failures + 1)); fi
check "real leak: secrets.json carries the redacted report (kind, file:line, the line)" \
  '.[0] | .leaks == 1 and (.leaked | length == 1) and .leaked[0].kind == "WEB_PAGE_TOKEN"
   and .leaked[0].where == ["web.log:1 \"GET /overlay?st=<WEB_PAGE_TOKEN>\""]' "$W/out4/secrets.json"
checked=$((checked + 1))
if sed -nE 's/.*[?&]st=([^&]+).*/\1/p' "$W/web4/overlay-url" | grep -qF -f - "$W/driver4.out" "$W/out4/secrets.json"; then
  echo "FAIL real leak: the report printed the token it found"; failures=$((failures + 1)); else
  echo "PASS real leak: the report (the log, secrets.json) never prints the token"; fi

# Run 5: the hung checker stopped at the reserve, the sampler skipped, the driver out by the hard stop.
read -r rc5 t5 e5 <"$W/rc5"
checked=$((checked + 1))
if [ "$rc5" -eq 0 ] && [ $((e5 - t5)) -ge 99000 ] && [ $((e5 - t5)) -lt 104000 ]; then
  echo "PASS hung checker: the driver exited 0 by the hard stop ($((e5 - t5)) ms of 100000)"
else
  echo "FAIL hung checker: the driver exited $rc5 after $((e5 - t5)) ms (want 0, from 99000 to under 104000)"
  tail -n 20 "$W/driver5.out"; failures=$((failures + 1))
fi
check "hung checker: done carries 124 for the checker and the sampler" '.[0] | .checker_exit == 124 and .sampler_exit == 124 and .leaks == 0' "$W/out5/done"
checked=$((checked + 1))
if grep -q "FAIL checker: still running at the hard stop's reserve: stopped" "$W/driver5.out" \
  && grep -q "FAIL sampler: no time left before the hard stop's reserve: skipped" "$W/driver5.out"; then
  echo "PASS hung checker: stopped by name, the sampler skipped by name"
else
  echo "FAIL hung checker: no stop or skip line"; tail -n 12 "$W/driver5.out"; failures=$((failures + 1)); fi
checked=$((checked + 1))
if [ -s "$W/state5/checker.pid" ] && ! kill -0 "$(cat "$W/state5/checker.pid")" 2>/dev/null; then
  echo "PASS hung checker: its process is gone"
else
  echo "FAIL hung checker: $( [ -s "$W/state5/checker.pid" ] && echo "pid $(cat "$W/state5/checker.pid") still runs" || echo "it never started")"
  failures=$((failures + 1)); fi

# Run 6: the page source's key in /out, found by the error path's scan; the token never made, skipped.
checked=$((checked + 1))
if [ "$(cat "$W/rc6")" -eq 1 ] && grep -q "FAIL WEB_SIGNING_KEY appears in /out" "$W/driver6.out"; then
  echo "PASS web error: exit 1, the error path's scan names WEB_SIGNING_KEY"
else
  echo "FAIL web error: exit $(cat "$W/rc6"), no WEB_SIGNING_KEY line"; tail -n 12 "$W/driver6.out"; failures=$((failures + 1)); fi
check "web error: secrets.json has the leak and skips the token it never made" \
  '.[0] | .scanned > 0 and .leaks == 1 and .skipped == ["WEB_PAGE_TOKEN"]' "$W/out6/secrets.json"
check "web error: done not ok, with the leak" '.[0] | .ok == false and .leaks == 1' "$W/out6/done"

# Run 7: every cookie but the inert ones is scanned; the short one fails; the planted session reds.
check "jar: three leaks by kind, the session and the unnamed cookie in /out, the short one refused" \
  '.[0] | .leaks == 3 and ([.leaked[].kind] | sort) == ["WEB_COOKIE[seazn_future]", "WEB_COOKIE[seazn_lang]", "WEB_COOKIE[seazn_session]"]
   and .skipped == ["WEB_PAGE_TOKEN"]' "$W/out7/secrets.json"
check "jar: the planted session cookie found in web.log, the line redacted" \
  '.[0] | [.leaked[] | select(.kind == "WEB_COOKIE[seazn_session]") | .where] == [["web.log:1 \"cookie: seazn_session=<WEB_COOKIE[seazn_session]>\""]]' "$W/out7/secrets.json"
checked=$((checked + 1))
if [ "$(cat "$W/rc7")" -eq 1 ] && grep -q 'FAIL WEB_COOKIE\[seazn_lang\] is too short to scan (2 characters, at least 16)' "$W/driver7.out" \
  && grep -q 'FAIL WEB_COOKIE\[seazn_future\] appears in /out: score.log:1' "$W/driver7.out" \
  && grep -q 'inert cookies not scanned (soak.json web_cookies_inert): seazn_org, seazn_signed_in' "$W/driver7.out"; then
  echo "PASS jar: the short cookie fails by name, the unnamed one is scanned, the inert ones named"
else
  echo "FAIL jar: the scan's lines"; grep -E 'secrets|FAIL' "$W/driver7.out"; failures=$((failures + 1)); fi
check "jar: done not ok, with the three leaks" '.[0] | .ok == false and .leaks == 3' "$W/out7/done"
checked=$((checked + 1))
if jq -r '.seazn_session, .seazn_future' "$W/web7/cookies.json" | grep -qF -f - "$W/driver7.out" "$W/out7/secrets.json"; then
  echo "FAIL jar: the report printed a cookie it found"; failures=$((failures + 1)); else
  echo "PASS jar: the report (the log, secrets.json) never prints a cookie"; fi

# Run 8: a jar of inert cookies only. The strict scan has nothing to scan the jar with: it fails by
# name, the inert cookies named as not scanned, and done is not ok.
checked=$((checked + 1))
if grep -q "FAIL WEB_COOKIES is empty: nothing to scan with" "$W/driver8.out" \
  && grep -q "secrets: inert cookies not scanned (soak.json web_cookies_inert): seazn_org, seazn_signed_in" "$W/driver8.out"; then
  echo "PASS inert jar: the strict scan fails WEB_COOKIES by name (nothing to scan with), the inert cookies named"
else
  echo "FAIL inert jar: no WEB_COOKIES line"; grep -E 'secrets|FAIL' "$W/driver8.out"; failures=$((failures + 1)); fi
check "inert jar: secrets.json counts the leak, skips nothing" '.[0] | .scanned > 0 and .leaks == 1 and .skipped == []' "$W/out8/secrets.json"
check "inert jar: done not ok, with the leak" '.[0] | .ok == false and .leaks == 1 and .checker_exit == 0' "$W/out8/done"

# Runs 9 and 10: cost legs B (720p, bug on badminton, camera noise) and C (1080p, bar on cricket, snow).
for n in 9 10; do
  checked=$((checked + 1))
  if [ "$(cat "$W/rc$n")" -eq 0 ]; then echo "PASS leg run $n: the driver exited 0"; else
    echo "FAIL leg run $n: the driver exited $(cat "$W/rc$n")"; tail -n 20 "$W/driver$n.out"; failures=$((failures + 1)); fi
done
check "leg B: the page source gets badminton, bug, the 720p session's delay" \
  ".[0] == \"up.sh-leg sport=badminton template=bug delay=$(jq -r .overlayDelayMs "$ROOT/contracts/fixtures/relay-job.v1/session-composed-720p.json")\"" \
  <(grep '^up.sh-leg ' "$W/state9/calls" | jq -R .)
check "leg C: the page source gets cricket, bar, the 1080p session's delay" \
  ".[0] == \"up.sh-leg sport=cricket template=bar delay=$(jq -r .overlayDelayMs "$ROOT/contracts/fixtures/relay-job.v1/session-composed-1080p.json")\"" \
  <(grep '^up.sh-leg ' "$W/state10/calls" | jq -R .)
checkx "leg B: fake-control serves the 720p fixture's output on the badminton fixture" \
  '(.[0]) as $s | $s.output == $want[0].output and $s.fixtureId == "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff"
   and ($s.overlayUrl | contains("template=bug"))' \
  --slurpfile want "$ROOT/contracts/fixtures/relay-job.v1/session-composed-720p.json" "$W/web9/session.json"
checkx "leg C: fake-control serves the 1080p fixture's output on the cricket fixture" \
  '(.[0]) as $s | $s.output == $want[0].output and $s.output.height == 1080 and $s.fixtureId == "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"' \
  --slurpfile want "$ROOT/contracts/fixtures/relay-job.v1/session-composed-1080p.json" "$W/web10/session.json"
check "leg B: the camera at the output's size, its picture as asked" '.[0] | test("--size 1280x720 --pattern noise( |$)")' \
  <(grep '^fake-camera ' "$W/state9/calls" | jq -R .)
check "leg C: the camera at 1920x1080, snow" '.[0] | test("--size 1920x1080 --pattern snow( |$)")' \
  <(grep '^fake-camera ' "$W/state10/calls" | jq -R .)
check "leg B: no dropout or disconnect on a leg (the soak's schedule would hold some in this drive), one start and one end" \
  '[.[] | .kind] == ["start", "end"]' "$W/out9/actions.jsonl"
checked=$((checked + 1))
if grep -q '^rally.mjs .*--every-s 3 --until-ms [0-9]\{13\} ' "$W/state9/calls" && ! grep -q '^score.mjs ' "$W/state9/calls" \
  && grep -q '^score.mjs .*--every-s 6 --boundaries --until-ms [0-9]\{13\} ' "$W/state10/calls" && ! grep -q '^rally.mjs ' "$W/state10/calls"; then
  echo "PASS legs: badminton's rally driver every cost_rally_every_s (3), cricket's boundaries every cost_ball_every_s (6)"
else
  echo "FAIL legs: the score drivers"; grep -hE '^(score|rally)\.mjs' "$W/state9/calls" "$W/state10/calls"; failures=$((failures + 1)); fi
check "leg B: done ok, naming the leg and its stills' exit (no recordings here: none written, 1)" \
  '.[0] | .ok and .leg == "B" and .stills_exit == 1 and .checker_exit == 0' "$W/out9/done"
checked=$((checked + 1))
if grep -q "cost leg B (bug on badminton, session-composed-720p.json)" "$W/driver9.out" && grep -q "stills: 0 written" "$W/out9/stills.log"; then
  echo "PASS leg B: the boot line names the leg; the stills were tried at the leg's instants"
else
  echo "FAIL leg B: the boot or stills line"; grep -E 'boot|stills' "$W/driver9.out"; failures=$((failures + 1)); fi
check "the soak names no leg in done" '.[0] | has("leg") | not' "$W/out3/done"
check "leg B: the start action names no schedule but the leg" '[.[] | select(.kind == "start") | .schedule] == ["leg"]' "$W/out9/actions.jsonl"
# Run 13: the dropout-only schedule, against dropout_plan on the same compressed soak.json.
dplan="$(jq -c -n -L "$W/soak" --slurpfile soak "$W/soak/soak.json" 'include "schedule"; dropout_plan($soak[0]; 60)')"
checked=$((checked + 1))
if [ "$(cat "$W/rc13")" -eq 0 ] && grep -q "schedule dropouts" "$W/driver13.out"; then echo "PASS dropouts run 13: exit 0, the boot line names the schedule"; else
  echo "FAIL dropouts run 13: exit $(cat "$W/rc13")"; tail -n 20 "$W/driver13.out"; failures=$((failures + 1)); fi
check "dropouts: the plan holds dropouts and no disconnect (anti-vacuity: at least 10)" \
  "($dplan) as \$p | (\$p.dropouts | length) >= 10 and \$p.disconnects == []" <(echo null)
check "dropouts: one start naming the schedule, every planned dropout ok on its planned time, no disconnect" \
  "(.[0]) as \$st | ($dplan) as \$p
   | \$st.kind == \"start\" and \$st.schedule == \"dropouts\" and \$st.drive_s == 60
     and ([.[] | select(.kind == \"dropout\")] | map(.slot) == [range(0; \$p.dropouts | length)])
     and ([.[] | select(.kind == \"dropout\") | .planned_ms - \$st.at_ms] == (\$p.dropouts | map(. * 1000)))
     and all(.[] | select(.kind == \"dropout\"); .ok and (.at_ms - .planned_ms) >= 0 and (.at_ms - .planned_ms) < 1500)
     and ([.[] | select(.kind == \"disconnect\")] == [])
     and (last | .kind == \"end\" and (has(\"ended_early\") | not))" "$W/out13/actions.jsonl"
checked=$((checked + 1))
if grep -q ' -I INPUT ' "$W/state13/calls"; then
  echo "FAIL dropouts: a firewall rule was inserted on the dropout-only schedule"; grep -- '-I INPUT' "$W/state13/calls"; failures=$((failures + 1))
elif grep -q '^curl POST http://127.0.0.1:7001/pause' "$W/state13/calls"; then
  echo "PASS dropouts: the camera paused, no firewall rule inserted"
else
  echo "FAIL dropouts: no camera pause reached fake-camera"; failures=$((failures + 1))
fi
# Run 16: the control schedule: the whole drive with nothing in it.
checked=$((checked + 1))
if [ "$(cat "$W/rc16")" -eq 0 ] && grep -q "schedule none" "$W/driver16.out"; then echo "PASS none run 16: exit 0, the boot line names the schedule"; else
  echo "FAIL none run 16: exit $(cat "$W/rc16")"; tail -n 20 "$W/driver16.out"; failures=$((failures + 1)); fi
check "none: one start naming the schedule and the drive, then only the end: no dropout, no disconnect" \
  '(.[0]) as $st | $st.kind == "start" and $st.schedule == "none" and $st.drive_s == 60
   and ([.[] | .kind] == ["start", "end"]) and (last | has("ended_early") | not)' "$W/out16/actions.jsonl"
checked=$((checked + 1))
if grep -q ' -I INPUT ' "$W/state16/calls" || grep -q '^curl POST http://127.0.0.1:7001/pause' "$W/state16/calls"; then
  echo "FAIL none: a fault reached the control run"; grep -E -- '-I INPUT|7001/pause' "$W/state16/calls"; failures=$((failures + 1))
elif [ -s "$W/state16/calls" ]; then
  echo "PASS none: no camera pause and no firewall rule ($(wc -l <"$W/state16/calls" | tr -d ' ') stub calls read)"
else
  echo "FAIL none: no stub call was recorded (nothing read)"; failures=$((failures + 1))
fi
# Runs 11 and 12: an unknown leg, and a leg on page=test, refused before anything starts.
for case in "11 E real" "12 A test"; do
  set -- $case
  rc=0
  env -i PATH="$PATH_STUB" HOME="$W" STATE="$W/state$1" SOAK_ROOT="$W/soak" SOAK_OUT="$W/out$1" SOAK_WEB_STATE="$W/web$1" \
    SESSION_ID="11111111-2222-4333-8444-5555555555$1" JOB_TOKEN="$JOB_TOKEN" SOAK_DRIVE_MIN=1 SOAK_DEADLINE_MS=1 \
    SOAK_HARD_STOP_MS=1 SOAK_PAGE="$3" SOAK_LEG="$2" FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test \
    "$BASH5" "$W/soak/driver.sh" >"$W/driver$1.out" 2>&1 || rc=$?
  echo "$rc" >"$W/rc$1"
done
checked=$((checked + 1))
if [ "$(cat "$W/rc11")" -eq 64 ] && grep -q "FAIL SOAK_LEG 'E' is not one of soak.json's legs" "$W/driver11.out" \
  && [ "$(cat "$W/rc12")" -eq 64 ] && grep -q "FAIL a cost leg runs on the production page (SOAK_PAGE=real)" "$W/driver12.out"; then
  echo "PASS legs: an unknown leg and a leg on page=test are refused (64), by name"
else
  echo "FAIL legs: the refusals"; tail -n 3 "$W/driver11.out" "$W/driver12.out"; failures=$((failures + 1)); fi
# Runs 14 and 15: an unknown schedule, and the dropouts schedule on a cost leg, refused before
# anything starts.
for case in "14 weekly -" "15 dropouts A" "17 none A"; do
  set -- $case
  leg="$3"
  [ "$leg" != - ] || leg=""
  rc=0
  env -i PATH="$PATH_STUB" HOME="$W" STATE="$W/state$1" SOAK_ROOT="$W/soak" SOAK_OUT="$W/out$1" SOAK_WEB_STATE="$W/web$1" \
    SESSION_ID="11111111-2222-4333-8444-5555555555$1" JOB_TOKEN="$JOB_TOKEN" SOAK_DRIVE_MIN=1 SOAK_DEADLINE_MS=1 \
    SOAK_HARD_STOP_MS=1 SOAK_PAGE=real SOAK_SCHEDULE="$2" SOAK_LEG="$leg" FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test \
    "$BASH5" "$W/soak/driver.sh" >"$W/driver$1.out" 2>&1 || rc=$?
  echo "$rc" >"$W/rc$1"
done
checked=$((checked + 1))
if [ "$(cat "$W/rc14")" -eq 64 ] && grep -q "FAIL SOAK_SCHEDULE must be soak, dropouts or none" "$W/driver14.out" \
  && [ "$(cat "$W/rc15")" -eq 64 ] && grep -q "FAIL a cost leg runs no schedule: SOAK_SCHEDULE=dropouts with SOAK_LEG" "$W/driver15.out" \
  && [ "$(cat "$W/rc17")" -eq 64 ] && grep -q "FAIL a cost leg runs no schedule: SOAK_SCHEDULE=none with SOAK_LEG" "$W/driver17.out" \
  && [ ! -s "$W/out14/actions.jsonl" ] && [ ! -s "$W/out15/actions.jsonl" ] && [ ! -s "$W/out17/actions.jsonl" ]; then
  echo "PASS schedules: an unknown schedule, and the dropouts and none schedules on a leg, are refused (64), by name, before any action"
else
  echo "FAIL schedules: the refusals"; tail -n 3 "$W/driver14.out" "$W/driver15.out" "$W/driver17.out"; failures=$((failures + 1)); fi
# Runs 18 to 21: soak.json's soak_output, each on its own copy of the driver's files with soak.json
# changed. 18: an output with no session fixture; 19: empty; 20: absent (both 64, by name, before
# any action: there is no default output). 21: leg C with soak_output absent, on page=real with no
# page source: a leg never reads soak_output, so it passes the session's choice and is refused at
# the page source instead.
for case in '18|.soak_output = "4k"' '19|.soak_output = ""' '20|del(.soak_output)' '21|del(.soak_output)'; do
  n="${case%%|*}"
  mkdir -p "$W/soak$n"
  cp "$W/soak/driver.sh" "$W/soak/schedule.jq" "$W/soak/session-composed-720p.json" "$W/soak/session-composed-1080p.json" "$W/soak$n/"
  jq "${case#*|}" "$W/soak/soak.json" >"$W/soak$n/soak.json"
done
for case in "18 test -" "19 test -" "20 test -" "21 real C"; do
  set -- $case
  leg="$3"
  [ "$leg" != - ] || leg=""
  rc=0
  env -i PATH="$PATH_STUB" HOME="$W" STATE="$W/state$1" SOAK_ROOT="$W/soak$1" SOAK_OUT="$W/out$1" SOAK_WEB_STATE="$W/web$1" \
    SESSION_ID="11111111-2222-4333-8444-5555555555$1" JOB_TOKEN="$JOB_TOKEN" SOAK_DRIVE_MIN=1 SOAK_DEADLINE_MS=1 \
    SOAK_HARD_STOP_MS=1 SOAK_PAGE="$2" SOAK_LEG="$leg" FLY_MACHINE_ID=m0 FLY_APP_NAME=soak-test \
    "$BASH5" "$W/soak$1/driver.sh" >"$W/driver$1.out" 2>&1 || rc=$?
  echo "$rc" >"$W/rc$1"
done
for case in "18|FAIL soak_output '4k': no session fixture $W/soak18/session-composed-4k.json" \
  "19|FAIL soak.json has no soak_output" "20|FAIL soak.json has no soak_output"; do
  n="${case%%|*}"
  checked=$((checked + 1))
  if [ "$(cat "$W/rc$n")" -eq 64 ] && grep -qF "${case#*|}" "$W/driver$n.out" && [ ! -s "$W/out$n/actions.jsonl" ]; then
    echo "PASS soak_output run $n ($(jq -c '.soak_output' "$W/soak$n/soak.json")): refused (64), by name, before any action"
  else
    echo "FAIL soak_output run $n: exit $(cat "$W/rc$n"), want 64 and \"${case#*|}\""; tail -n 3 "$W/driver$n.out"; failures=$((failures + 1)); fi
done
checked=$((checked + 1))
if [ "$(cat "$W/rc21")" -eq 64 ] && grep -qF "FAIL SOAK_PAGE=real needs the page source" "$W/driver21.out" \
  && ! grep -q soak_output "$W/driver21.out"; then
  echo "PASS soak_output run 21: leg C with soak_output absent passes the session's choice (refused later, at the page source)"
else
  echo "FAIL soak_output run 21: exit $(cat "$W/rc21")"; tail -n 3 "$W/driver21.out"; failures=$((failures + 1)); fi

checked=$((checked + 1))
if printf '%s\n' "$JOB_TOKEN" | grep -rqF -f - "$W/out" "$W/out2" "$W/out3" "$W/out4" "$W/out5" "$W/out6" "$W/driver.out" "$W/driver2.out" \
  "$W/driver3.out" "$W/driver4.out" "$W/driver5.out" "$W/driver6.out" "$W/driver7.out" "$W/out7" "$STATE/calls" "$W/state3/calls" \
  "$W/state4/calls" "$W/state5/calls" "$W/state6/calls" "$W/state7/calls" "$W/out8" "$W/out9" "$W/out10" "$W/driver8.out" \
  "$W/driver9.out" "$W/driver10.out" "$W/driver11.out" "$W/driver12.out" "$W/state8/calls" "$W/state9/calls" "$W/state10/calls" \
  "$W/out13" "$W/driver13.out" "$W/state13/calls" "$W/driver14.out" "$W/driver15.out" "$W/out16" "$W/driver16.out" \
  "$W/state16/calls" "$W/driver17.out" "$W/driver18.out" "$W/driver19.out" "$W/driver20.out" "$W/driver21.out"; then
  echo "FAIL the job token reached a file"
  failures=$((failures + 1))
else
  echo "PASS the job token reached no file"
fi
echo "checked: $checked, $failures failures ($(( ($(now_ms) - t0) / 1000 )) s)"
[ "$checked" -gt 0 ] && [ "$failures" -eq 0 ] || { tail -n 40 "$W/driver.out"; exit 1; }
