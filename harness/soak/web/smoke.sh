#!/usr/bin/env bash
# The page source's smoke (C15, page=real), inside the built image on the images job's runner, before
# it is pushed: up.sh exactly as the Machine runs it (Postgres from the restored cluster, the web
# server, the seed, the stream session, the signed URL, the pre-flight), then three deliveries
# through score.mjs, read back. A page source that does not come up fails here, not on Fly.
#
#   docker run --rm --entrypoint /soak/web/smoke.sh <image>
set -euo pipefail
export OUT=/tmp/smoke-out WEB_STATE=/tmp/smoke-state HOST=soak-smoke.invalid WEB_DELAY_MS=3100
export SESSION_ID="$(cat /proc/sys/kernel/random/uuid)"
export SOAK_HARD_STOP_MS=$(( $(date +%s) * 1000 + 3600000 ))
mkdir -p "$OUT"
rc=0
/soak/web/up.sh || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "smoke: FAIL up.sh exited $rc"
  tail -n 30 "$OUT/web.log" "$OUT/postgres.log" 2>/dev/null || true
  exit 1
fi
node /soak/web/score.mjs --base http://127.0.0.1:3000 --cookies "$WEB_STATE/cookies.json" --seed "$OUT/seed.json" \
  --every-s 0.2 --until-ms $(( $(date +%s) * 1000 + 60000 )) --max 3 --out "$OUT/deliveries.jsonl"
ok="$(jq -s '[.[] | select(.ok == true)] | length' "$OUT/deliveries.jsonl")"
[ "$ok" -eq 3 ] || { echo "smoke: FAIL $ok of 3 deliveries recorded: $(jq -c -s . "$OUT/deliveries.jsonl")"; exit 1; }
echo "smoke: the page source came up; 3 of 3 deliveries recorded ($(jq -c . /web/web.json))"
