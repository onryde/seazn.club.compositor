#!/usr/bin/env bash
# The page source's smoke (C15, page=real), inside the built image on the images job's runner, before
# it is pushed: up.sh exactly as the Machine runs it (Postgres from the restored cluster, the web
# server, the seed, the stream session, the signed URL, the pre-flight), then three deliveries
# through score.mjs, read back. Then the cost legs' drivers: three boundaries (score.mjs
# --boundaries), each a moment by the page's own feed, and 41 badminton rallies (rally.mjs) on the
# best-of-5 division, whose feed reads a fresh game point at 20-19 and at 21-20 and nothing before.
# A page source or a driver that does not work fails here, not on Fly.
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
node /soak/web/score.mjs --base http://127.0.0.1:3000 --cookies "$WEB_STATE/cookies.json" --seed "$OUT/seed.json" \
  --every-s 0.2 --until-ms $(( $(date +%s) * 1000 + 60000 )) --max 3 --out "$OUT/boundaries.jsonl" --boundaries
# Balls 4 to 6 of the first over: 6, 4, 6 in turn.
jq -e -s 'length == 3 and all(.ok) and map(.moments) == [["six"], ["four"], ["six"]]' "$OUT/boundaries.jsonl" >/dev/null \
  || { echo "smoke: FAIL the boundaries' moments: $(jq -c -s 'map({ok, runs, moments})' "$OUT/boundaries.jsonl")"; exit 1; }
fx="$(jq -r .badminton.fixture_id "$OUT/seed.json")"
best="$(node -e '
const [base, fx, jar] = process.argv.slice(1);
const cookie = Object.entries(JSON.parse(require("fs").readFileSync(jar, "utf8"))).map(([k, v]) => `${k}=${v}`).join("; ");
fetch(`${base}/api/v1/fixtures/${fx}/state`, { headers: { cookie } }).then((r) => r.json())
  .then((j) => console.log(j?.data?.state?.cfg?.bestOf ?? "none"), () => console.log("none"));
' http://127.0.0.1:3000 "$fx" "$WEB_STATE/cookies.json")"
[ "$best" = 5 ] || { echo "smoke: FAIL the badminton fixture is best of $best, not 5 (seed.mjs's division config)"; exit 1; }
node /soak/web/rally.mjs --base http://127.0.0.1:3000 --cookies "$WEB_STATE/cookies.json" --seed "$OUT/seed.json" \
  --every-s 0 --until-ms $(( $(date +%s) * 1000 + 120000 )) --max 41 --out "$OUT/rallies.jsonl"
# Rally 39 makes 20-19 and rally 41 makes 21-20: a fresh game point each; every other rally none.
jq -e -s 'length == 41 and all(.ok) and all(.moments != null)
  and ([to_entries[] | select(.value.moments != []) | {n: (.key + 1), m: .value.moments}]
       == [{n: 39, m: ["point.set"]}, {n: 41, m: ["point.set"]}])' "$OUT/rallies.jsonl" >/dev/null \
  || { echo "smoke: FAIL the rallies' moments: $(jq -c -s '[.[] | select((.ok | not) or .moments != [])]' "$OUT/rallies.jsonl" | head -c 1500)"; exit 1; }
echo "smoke: 3 boundaries, each a moment; 41 rallies to 21-20, game points at 20-19 and 21-20 (best of 5)"
