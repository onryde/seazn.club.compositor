#!/usr/bin/env bash
# The page source for page=real (C15): the standalone web build on this harness Machine, with its own
# Postgres, seeded through the app's own API. driver.sh runs it before fake-control starts. In order:
#
# 1. Postgres: the cluster web.Dockerfile restored at build time (db:apply and sync:sports, run on the
#    web job's runner), started on 127.0.0.1 only, trust auth (nothing else can reach it).
# 2. The web server (serve.sh: an allowlisted environment, a per-run signing key, telemetry off).
# 3. The seed (seed.mjs): one e2e account, a public competition, a cricket fixture and a badminton
#    one, both started.
# 4. The composed stream session the relay door checks (session.sql), with id SESSION_ID, on the
#    WEB_SPORT fixture (cricket; badminton for the cost legs' corner bug).
# 5. The relay door's URL (overlay-url.mjs), signed with this run's key, valid until half an hour
#    past the hard stop, at this Machine's private name (the compositor's CEF loads it from there).
# 6. A pre-flight: the page, one of its scripts and its overlay poll each answer 200.
#
# Writes $OUT/seed.json (ids only), $OUT/web.log, $OUT/postgres.log; and, under WEB_STATE (mode 700,
# outside /out, never fetched): auth-secret, cookies.json, overlay-url, server.pid.
#
#   SESSION_ID, SOAK_HARD_STOP_MS, OUT, WEB_STATE, HOST (this Machine's private name)
#   WEB_ROOT (/web), WEB_PORT (3000), WEB_PG_PORT (5432), WEB_PG_DATA (/var/lib/soak-pg)
#   WEB_TEMPLATE (bar), WEB_SPORT (cricket or badminton: the seeded fixture the session and the URL
#   name), WEB_DELAY_MS (the session's overlayDelayMs)
set -euo pipefail
: "${SESSION_ID:?}" "${SOAK_HARD_STOP_MS:?}" "${OUT:?}" "${WEB_STATE:?}" "${HOST:?}" "${WEB_DELAY_MS:?}"
HERE="$(cd "$(dirname "$0")" && pwd)"
WEB_ROOT="${WEB_ROOT:-/web}"
WEB_PORT="${WEB_PORT:-3000}"
WEB_PG_PORT="${WEB_PG_PORT:-5432}"
WEB_PG_DATA="${WEB_PG_DATA:-/var/lib/soak-pg}"
WEB_TEMPLATE="${WEB_TEMPLATE:-bar}"
WEB_SPORT="${WEB_SPORT:-cricket}"
case "$WEB_SPORT" in cricket | badminton) ;; *) echo "soak-web: WEB_SPORT must be cricket or badminton" >&2; exit 64 ;; esac
PG_BIN="${PG_BIN:-/usr/lib/postgresql/16/bin}"
log() { echo "soak-web $(date -u +%H:%M:%S) $*"; }
mkdir -p "$WEB_STATE"
chmod 700 "$WEB_STATE"

# 1. Postgres, as its own user (initdb and the server refuse root).
install -o postgres -g postgres -m 644 /dev/null "$OUT/postgres.log"
runuser -u postgres -- "$PG_BIN/pg_ctl" -D "$WEB_PG_DATA" -l "$OUT/postgres.log" -w -t 60 \
  -o "-c listen_addresses=127.0.0.1 -p $WEB_PG_PORT -k /tmp" start >/dev/null
[ "$(psql -h 127.0.0.1 -p "$WEB_PG_PORT" -U postgres -d seazn -XAtc 'show data_directory')" = "$WEB_PG_DATA" ] \
  || { log "FAIL the Postgres on $WEB_PG_PORT is not this one"; exit 1; }
sports="$(psql -h 127.0.0.1 -p "$WEB_PG_PORT" -U postgres -d seazn -XAtc 'select count(*) from seazn_club.sports')"
log "postgres: up on $WEB_PG_PORT, $sports sports"
[ "$sports" -gt 0 ] || { log "FAIL the sport catalog is empty (sync:sports did not reach the dump)"; exit 1; }

# 2. The web server.
WEB_APP="$WEB_ROOT/app/apps/web" WEB_DB="postgresql://postgres@127.0.0.1:$WEB_PG_PORT/seazn" WEB_LOG="$OUT/web.log" \
  WEB_PORT="$WEB_PORT" WEB_STATE="$WEB_STATE" "$HERE/serve.sh"

# 3. The seed, as an e2e account named for the run (nothing is ever mailed: Resend is unset).
email="delivered+soak-${SESSION_ID:0:8}@resend.dev"
node "$HERE/seed.mjs" --base "http://127.0.0.1:$WEB_PORT" --email "$email" --out "$OUT/seed.json" \
  --cookies "$WEB_STATE/cookies.json"
fixture="$(jq -er --arg s "$WEB_SPORT" '.[$s].fixture_id' "$OUT/seed.json")"

# 4. The composed stream session.
psql -h 127.0.0.1 -p "$WEB_PG_PORT" -U postgres -d seazn -XAt -v ON_ERROR_STOP=1 -v sid="$SESSION_ID" \
  -v fixture="$fixture" -v sport="$WEB_SPORT" -v delay="$WEB_DELAY_MS" -v email="$email" -f "$HERE/session.sql" \
  | grep -q "^$SESSION_ID|$fixture|composed|live$" || { log "FAIL the stream session was not written"; exit 1; }
log "session: $SESSION_ID on the $WEB_SPORT fixture $fixture (composed, live)"

# 5. The relay door's URL, signed with this run's key (the key through the environment only).
AUTH_SECRET="$(cat "$WEB_STATE/auth-secret")" node "$HERE/overlay-url.mjs" --base "http://$HOST:$WEB_PORT" \
  --sid "$SESSION_ID" --fixture "$fixture" --exp-ms $((SOAK_HARD_STOP_MS + 1800000)) --delay "$WEB_DELAY_MS" \
  --template "$WEB_TEMPLATE" --out "$WEB_STATE/overlay-url"

# 6. The pre-flight, from here: what the compositor's CEF will ask for, as it will ask (its Host),
# connected to this Machine's loopback (the private name may not resolve this early in boot). The
# signed URLs go through curl's config on stdin, never argv. A page whose HTML answers but whose
# chunks were never emitted, or whose live-data route fails, is still a dead overlay, so all three:
# the page, one of the /_next/static scripts its HTML names, and the overlay poll the page makes
# (the relay's, with sid and st). The HTML may carry the token: it stays under WEB_STATE.
fetch() { # <url> <out>: the HTTP status, the signed URL through stdin
  printf 'url = "%s"\n' "$1" \
    | curl -sS -m 20 -K - --connect-to "$HOST:$WEB_PORT:127.0.0.1:$WEB_PORT" -o "$2" -w '%{http_code}' 2>/dev/null || true
}
base="http://$HOST:$WEB_PORT"
code="$(fetch "$(cat "$WEB_STATE/overlay-url")" "$WEB_STATE/preflight-page.html")"
[ "$code" = 200 ] || { log "FAIL the overlay page answered ${code:-nothing}"; exit 1; }
chunk="$(grep -aoE '/_next/static/[A-Za-z0-9/_.~%-]+\.js' "$WEB_STATE/preflight-page.html" | head -n 1 || true)"
[ -n "$chunk" ] || { log "FAIL the overlay page's HTML names no /_next/static script"; exit 1; }
code="$(fetch "$base$chunk" "$WEB_STATE/preflight-chunk.js")"
[ "$code" = 200 ] && [ -s "$WEB_STATE/preflight-chunk.js" ] || { log "FAIL the page's script $chunk answered ${code:-nothing}"; exit 1; }
st="$(sed -nE 's/.*[?&]st=([^&]+).*/\1/p' "$WEB_STATE/overlay-url")"
code="$(fetch "$base/api/v1/public/fixtures/$fixture/overlay?sid=$SESSION_ID&st=$st" "$WEB_STATE/preflight-overlay.json")"
[ "$code" = 200 ] && jq -e 'type == "object"' "$WEB_STATE/preflight-overlay.json" >/dev/null 2>&1 \
  || { log "FAIL the overlay poll answered ${code:-nothing}"; exit 1; }
log "the overlay page answers 200, its script $chunk 200, the overlay poll 200 ($(jq -r 'keys | length' "$WEB_STATE/preflight-overlay.json") keys)"
