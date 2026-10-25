#!/usr/bin/env bash
# The soak driver (C15): the harness Machine's main process, under tini (harness/soak/Dockerfile).
# It runs MediaMTX, fake-camera and fake-control on this Machine, waits for the compositor Machine's
# first live beat, drives the schedule (schedule.jq: a camera dropout every 5 minutes, a destination
# disconnect at the middle of each third of the drive), ends the run, takes the compositor's log
# from the job, runs the checker and the overlay sampler (sampler.py), and writes /out/done.
# soak.yml polls /out/status.json, uploads /out/compositor.log (then /out/log-ready) once
# /out/ended exists, and fetches the outputs after /out/done.
#
# Environment (the Machine's; set by soak.yml):
#   SESSION_ID, JOB_TOKEN   the run's (JOB_TOKEN is random per run and never printed or written)
#   SOAK_DRIVE_MIN          the drive, in minutes (the verdict's duration)
#   SOAK_DEADLINE_MS        a lower bound on the compositor's RELAY_DEADLINE_AT, epoch ms: the launch
#                           step's start plus the run (the compositor's own is anchored at its launch,
#                           after this Machine's boot, so later). A guard: the drive should end before
#                           it; the WARN below is conservative, and the compositor's `deadline=` line
#                           is the real one
#   SOAK_HARD_STOP_MS       epoch ms: this process exits by then whatever happened, so the Machine
#                           stops even if the job's cleanup never runs
#   SOAK_PAGE               test (C14's overlay-test.html, served by fake-control: plumbing) or real
#                           (the production overlay page from the standalone web build on this
#                           Machine, web/up.sh; its URL carries a signed token and stays under
#                           SOAK_WEB_STATE, never /out)
#   CAMERA_PATTERN          fake-camera's picture (ball, snow or noise; default ball)
#   SOAK_LEG                empty for the soak; A, B, C or D for one of C15's cost legs (soak.json's
#                           legs: the output, the template and the sport). A leg runs on page=real
#                           with no dropout and no disconnect: its only fault is the renderer kill
#                           at its midpoint, which soak.yml's cost/leg.sh does on the compositor
#                           Machine. Its score driver records a moment-making event at the page's
#                           pace (cost_ball_every_s for cricket, cost_rally_every_s for badminton),
#                           and the run ends with programme stills for a person to look at
#   SOAK_SCHEDULE           soak (the default: schedule.jq's soak_plan) or dropouts (dropout_plan:
#                           the memory experiment's camera dropouts every 2 minutes and no
#                           disconnect; soak 40139563554's drift). A cost leg runs no schedule, so
#                           dropouts with SOAK_LEG is refused
#   FLY_MACHINE_ID, FLY_APP_NAME   set by the platform: this Machine's private name
#
# Every scheduled action is a line of /out/actions.jsonl with its wall time (ms, this Machine's
# clock: the clock the heartbeats' received_ms and the recordings' segment names use too).
set -euo pipefail

# SOAK_ROOT and SOAK_OUT move the driver's files and its outputs (test-driver.sh, under stubs);
# the Machine uses the defaults.
SOAK="${SOAK_ROOT:-/soak}"
OUT="${SOAK_OUT:-/out}"
mkdir -p "$OUT/rec" "$OUT/camrec"
exec > >(tee -a "$OUT/driver.log") 2>&1

now_ms() {
  local t=$EPOCHREALTIME
  t=${t//[.,]/}
  echo $((10#$t / 1000))
}
log() { echo "soak-driver $(date -u +%H:%M:%S) $*"; }
status() {
  jq -cn --arg phase "$1" --arg note "${2:-}" --argjson at "$(now_ms)" \
    --argjson actions "$(wc -l <"$OUT/actions.jsonl" 2>/dev/null || echo 0)" \
    --argjson beats "$(wc -l <"$OUT/heartbeats.jsonl" 2>/dev/null || echo 0)" \
    '{phase: $phase, note: $note, at_ms: $at, actions: $actions, beats: $beats}' >"$OUT/status.json.tmp"
  mv "$OUT/status.json.tmp" "$OUT/status.json"
}
action() { jq -cn "$@" >>"$OUT/actions.jsonl"; }

: "${SESSION_ID:?}" "${JOB_TOKEN:?}" "${SOAK_DRIVE_MIN:?}" "${SOAK_DEADLINE_MS:?}" "${SOAK_HARD_STOP_MS:?}"
: "${FLY_MACHINE_ID:?}" "${FLY_APP_NAME:?}"
SOAK_PAGE="${SOAK_PAGE:-test}"
CAMERA_PATTERN="${CAMERA_PATTERN:-ball}"
SOAK_LEG="${SOAK_LEG:-}"
SOAK_SCHEDULE="${SOAK_SCHEDULE:-soak}"
case "$SOAK_SCHEDULE" in
  soak | dropouts) ;;
  *) log "FAIL SOAK_SCHEDULE must be soak or dropouts"; exit 64 ;;
esac
if [ -n "$SOAK_LEG" ] && [ "$SOAK_SCHEDULE" != soak ]; then
  log "FAIL a cost leg runs no schedule: SOAK_SCHEDULE=$SOAK_SCHEDULE with SOAK_LEG"
  exit 64
fi
# The session fake-control hands out (a vendored relay-job fixture: the output profile), and the
# overlay's sport and template. The soak: 720p, bar on cricket.
SESSION_SRC="$SOAK/session-composed-720p.json"
LEG_SPORT=cricket
LEG_TEMPLATE=bar
if [ -n "$SOAK_LEG" ]; then
  leg_spec="$(jq -c --arg l "$SOAK_LEG" '.legs[$l] // empty' "$SOAK/soak.json")"
  [ -n "$leg_spec" ] || { log "FAIL SOAK_LEG '$SOAK_LEG' is not one of soak.json's legs"; exit 64; }
  [ "$SOAK_PAGE" = real ] || { log "FAIL a cost leg runs on the production page (SOAK_PAGE=real)"; exit 64; }
  SESSION_SRC="$SOAK/session-composed-$(jq -r .output <<<"$leg_spec").json"
  LEG_SPORT="$(jq -r .sport <<<"$leg_spec")"
  LEG_TEMPLATE="$(jq -r .template <<<"$leg_spec")"
  [ -f "$SESSION_SRC" ] || { log "FAIL leg $SOAK_LEG: no session fixture $SESSION_SRC"; exit 64; }
fi
case "$SOAK_PAGE" in
  test) ;;
  real) [ -x "$SOAK/web/up.sh" ] || { log "FAIL SOAK_PAGE=real needs the page source ($SOAK/web/up.sh: the web image)"; exit 64; } ;;
  *) log "FAIL SOAK_PAGE must be test or real"; exit 64 ;;
esac
# The page source's credentials (the signing key, the session cookie, the signed URL) live here,
# mode 700, outside /out: nothing fetches it.
WEB_STATE="${SOAK_WEB_STATE:-/run/soak-web}"
[[ "$SOAK_DRIVE_MIN" =~ ^[0-9]+$ ]] || { log "FAIL SOAK_DRIVE_MIN must be whole minutes"; exit 64; }
HOST="$FLY_MACHINE_ID.vm.$FLY_APP_NAME.internal"
DRIVE_S=$((SOAK_DRIVE_MIN * 60))
soak() { jq -r --arg k "$1" '.[$k]' "$SOAK/soak.json"; }
if [ -n "$SOAK_LEG" ]; then
  PLAN='{"dropouts":[],"disconnects":[]}'
  RAN=leg
else
  PLAN="$(jq -c -n -L "$SOAK" --slurpfile soak "$SOAK/soak.json" --argjson d "$DRIVE_S" --arg s "$SOAK_SCHEDULE" \
    'include "schedule"; schedule_plan($soak[0]; $s; $d)')"
  RAN="$SOAK_SCHEDULE"
fi
CAMERA_SIZE="$(jq -r '.output | "\(.width)x\(.height)"' "$SESSION_SRC")"
: >"$OUT/actions.jsonl"
: >"$OUT/heartbeats.jsonl"
: >"$OUT/emit.jsonl"
status booting "plan $(jq -c '{dropouts: (.dropouts | length), disconnects}' <<<"$PLAN")"
log "boot: drive $SOAK_DRIVE_MIN min, schedule $RAN, page $SOAK_PAGE, camera $CAMERA_PATTERN $CAMERA_SIZE, ${SOAK_LEG:+cost leg $SOAK_LEG ($LEG_TEMPLATE on $LEG_SPORT, $(basename "$SESSION_SRC")), }host $HOST, plan $(jq -c '{dropouts: (.dropouts | length), disconnects}' <<<"$PLAN")"
log "disk: $(df -h "$OUT" | tail -n 1 | awk '{print $2" total, "$4" free"}')"

# The SRT passphrase: made here, handed to MediaMTX, fake-camera and fake-control (which puts it in
# the session the compositor fetches) through the environment only.
HARNESS_SRT_PASSPHRASE="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
export HARNESS_SRT_PASSPHRASE

# The secrets scan: no credential in any file this run wrote (the recordings are binary and hold
# none: skipped). The values go to grep on a pipe, never in argv. On page=real, the page source's
# too: its signing key, the page token in the signed URL, and every cookie in the seed account's jar
# but soak.json's web_cookies_inert (the org id and the signed-in hint, whose value is "1": no
# authority), named as not scanned. So a credential cookie the web adds later is scanned, never
# missed. A value under scan_min_chars (16) cannot be scanned without matching everything: it FAILS,
# by name, and is never skipped.
# `scan_secrets strict` (the success path, where every credential was made): an empty one counts as
# a leak, since there is nothing to scan with. `scan_secrets made` (the error path, which may have
# stopped before the page source made its credentials): an empty one is skipped, by name. A leak is
# reported by kind, file and line, with the line redacted (leak_where). Writes secrets.json; sets
# scanned and leaks.
scanned=0
leaks=0
declare -A vals=()
scan_names=()
files=()
# `leak_where <name>`: where vals[name] is, never what: each file:line (five at most) and the line
# around it with every credential this scan holds replaced by its name and any other long token-like
# run masked. python reads the values from its environment (export, a builtin), never argv.
leak_where() {
  local f n
  while IFS=: read -r f n; do
    [[ "$n" =~ ^[0-9]+$ ]] || continue
    (
      i=0
      for k in "${scan_names[@]}"; do
        [ -n "${vals[$k]:-}" ] || continue
        export "SCAN_N_$i=$k" "SCAN_V_$i=${vals[$k]}"
        i=$((i + 1))
      done
      exec python3 -I -c '
import os, re, sys
path, want = sys.argv[1], int(sys.argv[2])
pairs, i = [], 0
while "SCAN_V_%d" % i in os.environ:
    pairs.append((os.environ["SCAN_N_%d" % i], os.environ["SCAN_V_%d" % i])); i += 1
line = b""
with open(path, "rb") as fh:
    for k, line in enumerate(fh, 1):
        if k == want:
            break
text = line.decode("utf-8", "replace").rstrip("\n")
for name, v in sorted(pairs, key=lambda p: -len(p[1])):
    text = text.replace(v, "<" + name + ">")
text = re.sub(r"[A-Za-z0-9+/=_.~%-]{24,}", "<long>", text)
at = max(text.find("<" + sys.argv[3] + ">"), 0)
print("%s:%d \"%s\"" % (os.path.basename(path), want, text[max(at - 60, 0):at + 100]))
' "$f" "$n" "$1"
    ) || echo "$(basename "$f"):$n (the line could not be read)"
  done < <(printf '%s\n' "${vals[$1]}" | grep -nF -f - "${files[@]}" 2>/dev/null | cut -d: -f1,2 | head -n 5)
}
scan_secrets() {
  local mode="$1" name skipped=() leaked=() inert=() where k min
  leaks=0
  min="$(soak scan_min_chars)"
  mapfile -t files < <(find "$OUT" \( -path "$OUT/rec" -o -path "$OUT/camrec" \) -prune -o -type f -print)
  scanned="${#files[@]}"
  vals=([JOB_TOKEN]="$JOB_TOKEN" [HARNESS_SRT_PASSPHRASE]="${HARNESS_SRT_PASSPHRASE:-}")
  scan_names=(JOB_TOKEN HARNESS_SRT_PASSPHRASE)
  if [ "$SOAK_PAGE" = real ]; then
    vals[WEB_SIGNING_KEY]="$(cat "$WEB_STATE/auth-secret" 2>/dev/null || true)"
    vals[WEB_PAGE_TOKEN]="$(sed -nE 's/.*[?&]st=([^&]+).*/\1/p' "$WEB_STATE/overlay-url" 2>/dev/null || true)"
    scan_names+=(WEB_SIGNING_KEY WEB_PAGE_TOKEN)
    while IFS= read -r k; do
      [ -n "$k" ] || continue
      if jq -e --arg k "$k" '.web_cookies_inert | index($k) != null' "$SOAK/soak.json" >/dev/null; then
        inert+=("$k")
      else
        vals["WEB_COOKIE[$k]"]="$(jq -r --arg k "$k" '.[$k] | tostring' "$WEB_STATE/cookies.json")"
        scan_names+=("WEB_COOKIE[$k]")
      fi
    done < <(jq -r 'keys[]' "$WEB_STATE/cookies.json" 2>/dev/null || true)
    [ "${#inert[@]}" -eq 0 ] || log "secrets: inert cookies not scanned (soak.json web_cookies_inert): $(printf '%s\n' "${inert[@]}" | paste -sd ',' - | sed 's/,/, /g')"
    # No cookie to scan: the jar was never made (the error path skips it) or holds no session.
    k=0
    for name in "${scan_names[@]}"; do [[ "$name" != WEB_COOKIE\[* ]] || k=1; done
    [ "$k" = 1 ] || scan_names+=(WEB_COOKIES)
  fi
  if [ "$scanned" -eq 0 ]; then
    log "FAIL no file in /out to scan"
    leaks=$((leaks + 1))
  fi
  for name in "${scan_names[@]}"; do
    if [ -z "${vals[$name]:-}" ]; then
      if [ "$mode" = strict ]; then
        log "FAIL $name is empty: nothing to scan with"
        leaks=$((leaks + 1))
      else
        skipped+=("$name")
      fi
    elif [ "${#vals[$name]}" -lt "$min" ]; then
      log "FAIL $name is too short to scan (${#vals[$name]} characters, at least $min): it would match anything"
      leaks=$((leaks + 1))
      leaked+=("$(jq -cn --arg n "$name" --arg w "${#vals[$name]} characters: too short to scan" '{kind: $n, where: [$w]}')")
    elif [ "$scanned" -gt 0 ] && printf '%s\n' "${vals[$name]}" | grep -qF -f - "${files[@]}"; then
      where="$(leak_where "$name")"
      log "FAIL $name appears in /out: $(paste -sd ';' - <<<"$where" | sed 's/;/; /g')"
      leaks=$((leaks + 1))
      # The redacted report, for the job to print when it refuses the upload.
      leaked+=("$(jq -cn --arg n "$name" --arg w "$where" '{kind: $n, where: ($w | split("\n") | map(select(. != "")))}')")
    fi
  done
  # The compositor's log, by its bytes: the job uploads its own copy only when it is this one (the
  # job's upload to here can fail, and then its copy was never scanned).
  local logsum=""
  [ ! -f "$OUT/compositor.log" ] || logsum="$(python3 -I -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$OUT/compositor.log")"
  printf '%s\n' "${leaked[@]}" | jq -cs --argjson s "$scanned" --argjson l "$leaks" --arg skipped "${skipped[*]:-}" --arg logsum "$logsum" \
    '{scanned: $s, leaks: $l, skipped: ($skipped | split(" ") | map(select(. != ""))), leaked: .,
      compositor_log_sha256: (if $logsum == "" then null else $logsum end)}' >"$OUT/secrets.json"
  log "secrets checked: $scanned files, $leaks leaks${skipped[*]:+ (not made yet, skipped: ${skipped[*]})}"
}

pids=()
done_written=0
stopping=0
finish() {
  local rc=$?
  set +e
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done
  if [ "$done_written" -eq 0 ]; then
    status error "the driver exited ($rc) before done"
    # The job uploads what it fetches here too: the same scan first (secrets.json), over what was
    # made, unless the Machine is being stopped (nothing is fetched then).
    [ "$stopping" -eq 1 ] || scan_secrets made
    jq -cn --argjson rc "$rc" --argjson l "$leaks" '{ok: false, driver_exit: $rc, leaks: $l}' >"$OUT/done"
    # Keep /out for the job's fetch (it reads phase error and fetches at once), unless the Machine is
    # being stopped: the hard stop still bounds it.
    if [ "$stopping" -eq 0 ]; then
      log "error: holding /out for the job until the hard stop"
      while [ "$(now_ms)" -lt "$SOAK_HARD_STOP_MS" ]; do sleep 2; done
    fi
  fi
}
trap finish EXIT
trap 'stopping=1; log "TERM: stopping"; exit 143' TERM INT

# The dest disconnect's firewall: whichever iptables backend this kernel takes, for each family.
FW4=""
FW6=""
for b in iptables-nft iptables-legacy; do
  if "$b" -w -L INPUT -n >/dev/null 2>&1; then FW4="$b"; break; fi
done
for b in ip6tables-nft ip6tables-legacy; do
  if "$b" -w -L INPUT -n >/dev/null 2>&1; then FW6="$b"; break; fi
done
log "firewall: ipv4 ${FW4:-none}, ipv6 ${FW6:-none}"

# The page source (page=real): the standalone web app, seeded, with the stream session and the signed
# URL (web/up.sh). The run's own credentials stay out of its environment.
if [ "$SOAK_PAGE" = real ]; then
  status booting "page source"
  web_rc=0
  env -u JOB_TOKEN -u HARNESS_SRT_PASSPHRASE OUT="$OUT" WEB_STATE="$WEB_STATE" HOST="$HOST" \
    WEB_PORT="$(soak web_port)" WEB_DELAY_MS="$(jq -r .overlayDelayMs "$SESSION_SRC")" \
    WEB_SPORT="$LEG_SPORT" WEB_TEMPLATE="$LEG_TEMPLATE" \
    "$SOAK/web/up.sh" >"$OUT/web-up.log" 2>&1 || web_rc=$?
  cat "$OUT/web-up.log"
  [ -f "$WEB_STATE/server.pid" ] && pids+=("$(cat "$WEB_STATE/server.pid")")
  [ "$web_rc" -eq 0 ] && [ -s "$WEB_STATE/overlay-url" ] || { log "FAIL the page source did not come up ($web_rc)"; exit 1; }
fi

# The scripted score timeline the test page shows (fake-control's --score): an event every 3 s, for
# the drive and the end, so the overlay repaints through the whole run (harness/pages/score.json's
# pace; its 50 events would end at 150 s).
jq -n --argjson n $(((DRIVE_S + 900) / 3)) '{events: [range(1; $n + 1) | {index: ., offset_ms: (. * 3000)}]}' >"$OUT/score.json"
# The session fake-control hands out: the vendored composed one, with the production page's URL when
# there is one (fake-control rewrites its origin to itself, so it is passed as the overlay origin
# and stays unchanged).
if [ "$SOAK_PAGE" = real ]; then
  # The URL and its token go through files (jq --rawfile), never argv; the session carries the
  # token, so it stays under WEB_STATE.
  SESSION_FILE="$WEB_STATE/session.json"
  jq --rawfile u "$WEB_STATE/overlay-url" --slurpfile seed "$OUT/seed.json" --arg sport "$LEG_SPORT" \
    '($u | rtrimstr("\n")) as $url | .overlayUrl = $url | .fixtureId = $seed[0][$sport].fixture_id
     | .pageToken = ($url | capture("[?&]st=(?<t>[^&]+)").t)' \
    "$SESSION_SRC" >"$SESSION_FILE"
  OVERLAY_ORIGIN="http://$HOST:$(soak web_port)"
else
  SESSION_FILE="$OUT/session.json"
  cp "$SESSION_SRC" "$SESSION_FILE"
  OVERLAY_ORIGIN="http://$HOST:7000"
fi

TZ=UTC MTX_PATHS_CAM_SRTREADPASSPHRASE="$HARNESS_SRT_PASSPHRASE" MTX_PATHS_CAM_SRTPUBLISHPASSPHRASE="$HARNESS_SRT_PASSPHRASE" \
  env -u JOB_TOKEN -u SESSION_ID mediamtx "$SOAK/mediamtx.yml" >"$OUT/mediamtx.log" 2>&1 &
pids+=($!)
mtx_pid=$!
XDG_CACHE_HOME=/tmp env -u JOB_TOKEN -u SESSION_ID fake-camera --emit "$OUT/emit.jsonl" --listen 127.0.0.1:7001 --srt-host 127.0.0.1 \
  --srt-port 8890 --stream-id publish:cam --size "$CAMERA_SIZE" --pattern "$CAMERA_PATTERN" \
  >"$OUT/fake-camera.log" 2>&1 &
pids+=($!)
fake-control --fixture "$SESSION_FILE" --page "$SOAK/pages/overlay-test.html" --score "$OUT/score.json" \
  --beats "$OUT/heartbeats.jsonl" --listen "[::]:7000" --input-origin "srt://$HOST:8890" \
  --target-origin "rtmp://$HOST:1935" --overlay-origin "$OVERLAY_ORIGIN" --stream-id read:cam --stream-key dest \
  >"$OUT/fake-control.log" 2>&1 &
pids+=($!)
control() { curl -fsS -m 10 -X "$1" "http://127.0.0.1:7000$2"; }
# fake-camera's control's HTTP status, as a number (0 when nothing answered).
camera() {
  local code
  code="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:7001$1" 2>/dev/null)" || true
  [[ "$code" =~ ^[0-9]{3}$ ]] || code=0
  echo $((10#$code))
}
for _ in $(seq 1 "$(soak control_wait_s)"); do control GET /healthz >/dev/null 2>&1 && break; sleep 1; done
control GET /healthz >/dev/null 2>&1 || { log "FAIL fake-control never answered"; tail -n 20 "$OUT/fake-control.log"; exit 1; }

# The last beat's receipt time (LAST_BEAT_MS), kept when the file's last line is still being written.
LAST_BEAT_MS=0
read_last_beat() {
  local v
  v="$(tail -n 1 "$OUT/heartbeats.jsonl" 2>/dev/null | jq -er '.received_ms' 2>/dev/null)" || return 0
  [[ "$v" =~ ^[0-9]+$ ]] && LAST_BEAT_MS="$v"
  return 0
}

# Live: a heartbeat whose programmePhase is live.
status waiting_live
live_deadline=$(($(now_ms) + $(soak live_wait_s) * 1000))
until grep -q '"programmePhase":"live"' "$OUT/heartbeats.jsonl"; do
  if [ "$(now_ms)" -ge "$live_deadline" ]; then
    log "FAIL no live beat within $(soak live_wait_s) s ($(wc -l <"$OUT/heartbeats.jsonl") beats)"
    break
  fi
  sleep 1
done
went_live=0
grep -q '"programmePhase":"live"' "$OUT/heartbeats.jsonl" && went_live=1
control POST /control/timeline-start >/dev/null || log "timeline-start refused"
start_ms="$(now_ms)"
end_ms=$((start_ms + DRIVE_S * 1000))
ending_budget_ms=$(($(soak end_wait_s) * 1000))
if [ $((end_ms + ending_budget_ms)) -gt "$SOAK_DEADLINE_MS" ]; then
  log "WARN the drive ends $(((end_ms + ending_budget_ms - SOAK_DEADLINE_MS) / 1000)) s after the launch step's deadline bound allows (the compositor's own is later by its boot: its \`deadline=\` line)"
fi

# The schedule, in time order: [{kind, slot, at_s}], dropouts and disconnects (never overlapping:
# schedule.jq refuses a plan where they would).
mapfile -t steps < <(jq -c '[(.dropouts | to_entries[] | {kind: "dropout", slot: .key, at_s: .value}),
  (.disconnects | to_entries[] | {kind: "disconnect", slot: .key, at_s: .value})] | sort_by(.at_s)[]' <<<"$PLAN")
lost_beats_ms=$(($(soak lost_beats_end_s) * 1000))
ended_early=""
score_pid=""
if [ "$went_live" -eq 1 ]; then
  action --argjson at "$start_ms" --argjson d "$DRIVE_S" --arg s "$RAN" '{kind: "start", at_ms: $at, drive_s: $d, schedule: $s}'
  read_last_beat
  log "live: the drive starts, $DRIVE_S s, ${#steps[@]} actions"
  # Check (c)'s deliveries, through the standalone app's API, for the whole drive. A cost leg's: a
  # moment-making event at the page's pace (cricket: a boundary every cost_ball_every_s; badminton:
  # a rally every cost_rally_every_s), each row carrying the moments the page's feed makes of it.
  if [ "$SOAK_PAGE" = real ]; then
    score_js=score.mjs
    score_args=(--every-s "$(soak score_every_s)")
    if [ -n "$SOAK_LEG" ] && [ "$LEG_SPORT" = badminton ]; then
      score_js=rally.mjs
      score_args=(--every-s "$(soak cost_rally_every_s)")
    elif [ -n "$SOAK_LEG" ]; then
      score_args=(--every-s "$(soak cost_ball_every_s)" --boundaries)
    fi
    env -u JOB_TOKEN -u SESSION_ID -u HARNESS_SRT_PASSPHRASE node "$SOAK/web/$score_js" \
      --base "http://127.0.0.1:$(soak web_port)" --cookies "$WEB_STATE/cookies.json" --seed "$OUT/seed.json" \
      "${score_args[@]}" --until-ms "$end_ms" --out "$OUT/deliveries.jsonl" >"$OUT/score.log" 2>&1 &
    score_pid=$!
    pids+=("$score_pid")
  fi
else
  ended_early="the compositor never went live"
fi

# Sleeps until <ms>, watching the beats: false (and ended_early set) when they stopped.
wait_until() {
  local target="$1" n lb
  while :; do
    n="$(now_ms)"
    [ "$n" -ge "$target" ] && return 0
    if [ "$n" -ge "$SOAK_HARD_STOP_MS" ]; then
      ended_early="the hard stop came before the drive ended"
      return 1
    fi
    read_last_beat
    lb="$LAST_BEAT_MS"
    if [ $((n - lb)) -gt "$lost_beats_ms" ]; then
      ended_early="no beat for $(((n - lb) / 1000)) s"
      return 1
    fi
    if [ $((n / 60000)) -ne "${last_status_min:-0}" ]; then
      last_status_min=$((n / 60000))
      status driving "minute $(((n - start_ms) / 60000)) of $SOAK_DRIVE_MIN"
    fi
    sleep "$(awk -v d=$((target - n)) 'BEGIN { s = d / 1000; if (s > 5) s = 5; printf "%.3f", s }')"
  done
}

# The DROP rule's packets for one family (`$1`: the backend), or 0.
dropped() {
  [ -n "$1" ] || { echo 0; return; }
  "$1" -w -L INPUT -v -n -x 2>/dev/null | awk '$3 == "DROP" && /dpt:1935/ { s += $1 } END { print s + 0 }'
}
dropout() {
  local slot="$1" planned="$2" at p r end
  at="$(now_ms)"
  p="$(camera /pause)"
  sleep "$(soak dropout_pause_s)"
  r="$(camera /resume)"
  end="$(now_ms)"
  action --argjson slot "$slot" --argjson planned "$planned" --argjson at "$at" --argjson end "$end" \
    --argjson p "$p" --argjson r "$r" \
    '{kind: "dropout", slot: $slot, planned_ms: $planned, at_ms: $at, end_ms: $end, ok: ($p == 200 and $r == 200), pause: $p, resume: $r}'
  log "dropout $slot: pause $p, resume $r"
}
disconnect() {
  local slot="$1" planned="$2" at end ok=true fams=() n4 n6 fw
  at="$(now_ms)"
  for fw in "$FW4" "$FW6"; do
    if [ -n "$fw" ] && "$fw" -w -I INPUT -p tcp --dport 1935 -j DROP; then fams+=("$fw"); else ok=false; fi
  done
  sleep "$(soak disconnect_hold_s)"
  n4="$(dropped "$FW4")"
  n6="$(dropped "$FW6")"
  for fw in "${fams[@]}"; do "$fw" -w -D INPUT -p tcp --dport 1935 -j DROP || ok=false; done
  end="$(now_ms)"
  action --argjson slot "$slot" --argjson planned "$planned" --argjson at "$at" --argjson end "$end" \
    --argjson ok "$ok" --argjson n4 "$n4" --argjson n6 "$n6" --arg fams "${fams[*]}" \
    '{kind: "disconnect", slot: $slot, planned_ms: $planned, at_ms: $at, end_ms: $end, ok: $ok,
      dropped_packets: ($n4 + $n6), dropped_ipv4: $n4, dropped_ipv6: $n6,
      families: ($fams | split(" ") | map(select(. != "") | if startswith("ip6") then "ipv6" else "ipv4" end))}'
  log "disconnect $slot: ok $ok, dropped ipv4 $n4, ipv6 $n6 (${fams[*]:-no rule})"
}

if [ -z "$ended_early" ]; then
  status driving "minute 0 of $SOAK_DRIVE_MIN"
  for s in "${steps[@]}"; do
    kind="$(jq -r .kind <<<"$s")"
    slot="$(jq -r .slot <<<"$s")"
    planned=$((start_ms + $(jq -r .at_s <<<"$s") * 1000))
    wait_until "$planned" || break
    "$kind" "$slot" "$planned"
  done
  [ -n "$ended_early" ] || wait_until "$end_ms" || true
fi

# The end: the score driver stopped (its last line written), the request, then the compositor's
# stopped beat.
if [ -n "$score_pid" ]; then
  kill -TERM "$score_pid" 2>/dev/null || true
  for _ in $(seq 1 30); do kill -0 "$score_pid" 2>/dev/null || break; sleep 1; done
  tail -n 1 "$OUT/score.log" || true
fi
end_request_ms="$(now_ms)"
[ "$went_live" -eq 1 ] && action --argjson at "$end_request_ms" --arg why "$ended_early" \
  '{kind: "end", at_ms: $at} + (if $why == "" then {} else {ended_early: $why} end)'
log "end requested${ended_early:+ early: $ended_early}"
status ending "${ended_early}"
control POST '/control/desired?state=ending' >/dev/null || log "the end request was refused"
stop_deadline=$(($(now_ms) + ending_budget_ms))
until grep -q '"state":"stopped"' "$OUT/heartbeats.jsonl"; do
  [ "$(now_ms)" -ge "$stop_deadline" ] && { log "no stopped beat within $(soak end_wait_s) s"; break; }
  sleep 1
done
control GET /control/timeline >"$OUT/timeline.json" || echo '{"start_wall_ms":0,"delay_ms":0,"events":[]}' >"$OUT/timeline.json"
control GET /control/state >"$OUT/control-state.json" || echo '{}' >"$OUT/control-state.json"
log "control state: $(jq -c '{session_fetches, beats, other_session_requests, unauthorized_requests}' "$OUT/control-state.json")"

# The compositor's log comes from the job (the platform's log stream; this Machine cannot read it).
jq -cn --argjson at "$end_request_ms" '{end_request_ms: $at}' >"$OUT/ended"
status awaiting_log
log_deadline=$(($(now_ms) + $(soak log_wait_s) * 1000))
until [ -f "$OUT/log-ready" ]; do
  if [ "$(now_ms)" -ge "$log_deadline" ]; then
    log "no compositor log within $(soak log_wait_s) s: checking without one"
    : >"$OUT/compositor.log"
    break
  fi
  sleep 2
done
[ -f "$OUT/compositor.log" ] || : >"$OUT/compositor.log"

# The recordings closed (MediaMTX's last segment ends on its own exit), the fakes stopped, then the
# checker.
for p in "${pids[@]}"; do [ "$p" = "$mtx_pid" ] || kill "$p" 2>/dev/null || true; done
# TERM, not INT: a non-interactive shell's background jobs start with SIGINT ignored. MediaMTX
# closes its recordings on TERM; a bounded wait, then KILL.
kill -TERM "$mtx_pid" 2>/dev/null || true
for _ in $(seq 1 30); do kill -0 "$mtx_pid" 2>/dev/null || break; sleep 1; done
kill -KILL "$mtx_pid" 2>/dev/null || true
wait "$mtx_pid" 2>/dev/null || true
status checking
# The checker and the sampler each run until the hard stop less post_check_reserve_s (the job's
# fetch after done), no longer: one that hangs is stopped (TERM, then KILL after 5 s) and reads exit
# 124, and one with no time left is skipped with 124. So the hard stop bounds this Machine on every
# path, a hung checker's too, even when the job's cleanup never runs.
check_until_ms=$((SOAK_HARD_STOP_MS - $(soak post_check_reserve_s) * 1000))
bounded() { # <name> <log> <command>...
  local name="$1" out="$2" p rc=0
  shift 2
  if [ "$(now_ms)" -ge "$check_until_ms" ]; then
    log "FAIL $name: no time left before the hard stop's reserve: skipped"
    return 124
  fi
  "$@" >"$out" 2>&1 &
  p=$!
  pids+=("$p")
  while kill -0 "$p" 2>/dev/null; do
    if [ "$(now_ms)" -ge "$check_until_ms" ]; then
      log "FAIL $name: still running at the hard stop's reserve: stopped"
      kill -TERM "$p" 2>/dev/null || true
      for _ in 1 2 3 4 5; do kill -0 "$p" 2>/dev/null || break; sleep 1; done
      kill -KILL "$p" 2>/dev/null || true
      wait "$p" 2>/dev/null || true
      return 124
    fi
    sleep 1
  done
  wait "$p" || rc=$?
  return "$rc"
}
checker_exit=0
sampler_exit=0
bounded checker "$OUT/checker.log" env -u JOB_TOKEN -u SESSION_ID -u HARNESS_SRT_PASSPHRASE XDG_CACHE_HOME=/tmp \
  checker --recordings "$OUT/rec" --emit "$OUT/emit.jsonl" --heartbeats "$OUT/heartbeats.jsonl" \
  --timeline "$OUT/timeline.json" --compositor-log "$OUT/compositor.log" --end-request-ms "$end_request_ms" \
  --out "$OUT/report.json" || checker_exit=$?
tail -n 5 "$OUT/checker.log" 2>/dev/null || true
# Check (d)'s samples: the programme against the camera-only recording, once a minute (on the test
# page too, where the verdict holds (d) pending: its numbers are printed all the same).
status sampling
bounded sampler "$OUT/sampler.log" python3 -I "$SOAK/sampler.py" --programme "$OUT/rec" --camera "$OUT/camrec" \
  --actions "$OUT/actions.jsonl" --soak "$SOAK/soak.json" --out "$OUT/overlay-samples.jsonl" || sampler_exit=$?
tail -n 2 "$OUT/sampler.log" 2>/dev/null || true
# A cost leg's stills, for a person to look at (never committed): six a second apart at a quarter of
# the drive (the moments running), one 45 s after the midpoint's renderer kill (the reloaded page),
# and one 30 s before the end. In /out/stills.tar, which the job fetches.
stills_exit=""
if [ -n "$SOAK_LEG" ] && [ "$went_live" -eq 1 ]; then
  q=$((start_ms + DRIVE_S * 250))
  at="$(for i in 0 1 2 3 4 5; do printf 'moment%s:%s,' "$i" $((q + i * 1000)); done)reload:$((start_ms + DRIVE_S * 500 + 45000)),end:$((end_ms - 30000))"
  stills_exit=0
  bounded stills "$OUT/stills.log" python3 -I "$SOAK/sampler.py" --programme "$OUT/rec" --soak "$SOAK/soak.json" \
    --stills "$OUT/stills" --still-at "$at" || stills_exit=$?
  tail -n 1 "$OUT/stills.log" 2>/dev/null || true
  if [ -d "$OUT/stills" ]; then tar -cf "$OUT/stills.tar" -C "$OUT" stills && rm -rf "$OUT/stills"; fi
fi

scan_secrets strict

jq -cn --argjson c "$checker_exit" --argjson sx "$sampler_exit" --argjson live "$went_live" --arg why "$ended_early" --argjson l "$leaks" \
  --arg leg "$SOAK_LEG" --arg stx "$stills_exit" \
  '{ok: ($l == 0), checker_exit: $c, sampler_exit: $sx, went_live: ($live == 1), ended_early: (if $why == "" then null else $why end), leaks: $l}
   + (if $leg == "" then {} else {leg: $leg, stills_exit: (if $stx == "" then null else ($stx | tonumber) end)} end)' \
  >"$OUT/done"
done_written=1
status done "checker exit $checker_exit"
log "done: checker exit $checker_exit; waiting for the job to fetch /out"

# Stay up for the job's fetch, then stop (the Machine stops with this process): a lost cleanup
# never leaves the harness running past the hard stop.
while [ "$(now_ms)" -lt "$SOAK_HARD_STOP_MS" ]; do
  sleep "$(awk -v d=$((SOAK_HARD_STOP_MS - $(now_ms))) 'BEGIN { s = d / 1000; if (s > 10) s = 10; if (s < 0.1) s = 0.1; printf "%.3f", s }')"
done
log "hard stop"
