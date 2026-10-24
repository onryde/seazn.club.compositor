#!/usr/bin/env bash
# Starts the standalone web app (C15, page=real) with an allowlisted environment, then waits for it
# to answer. Nothing of the caller's environment reaches the server but the allowlist below: no job
# token, no SRT passphrase, and no PostHog, Sentry, Stripe, Resend or Supabase setting, so the
# server has nothing to send anywhere. The signing key (AUTH_SECRET) is made here per run, kept in a
# mode-600 file under WEB_STATE (never /out), and reaches the server through its environment only,
# never argv. AUTH_DEV_LINKS=1 (the dev magic link the seed signs in with) is set here, on this
# isolated Machine only.
#
#   WEB_APP     the standalone app's directory (holds server.js)
#   WEB_PORT    the port (3000)
#   WEB_DB      the local Postgres's URL (no password: trust auth on 127.0.0.1)
#   WEB_STATE   a mode-700 directory outside /out: auth-secret, server.pid
#   WEB_LOG     the server's log
#
#   serve.sh    exit 0 once /api/health answers 200 from this process; 1 otherwise
set -euo pipefail
: "${WEB_APP:?}" "${WEB_PORT:?}" "${WEB_DB:?}" "${WEB_STATE:?}" "${WEB_LOG:?}"
[ -f "$WEB_APP/server.js" ] || { echo "serve: FAIL no server.js in $WEB_APP"; exit 1; }
[[ "$WEB_DB" =~ ^postgresql://[a-z_]+@(127\.0\.0\.1|localhost|\[::1\]):[0-9]+/[A-Za-z0-9_]+$ ]] \
  || { echo "serve: FAIL WEB_DB must be a local, passwordless Postgres URL"; exit 1; }
mkdir -p "$WEB_STATE"
chmod 700 "$WEB_STATE"
umask 077
[ -s "$WEB_STATE/auth-secret" ] || od -An -N32 -tx1 /dev/urandom | tr -d ' \n' >"$WEB_STATE/auth-secret"
[ "$(wc -c <"$WEB_STATE/auth-secret" | tr -d ' ')" -eq 64 ] || { echo "serve: FAIL no signing key"; exit 1; }

(
  # Every exported variable goes but the few the server needs; then the allowlist is exported.
  port="$WEB_PORT" db="$WEB_DB" app="$WEB_APP" state="$WEB_STATE"
  for v in $(compgen -e); do
    case "$v" in PATH | HOME | LANG | TZ) ;; *) unset "$v" 2>/dev/null || true ;; esac
  done
  export NODE_ENV=production PORT="$port" HOSTNAME="::" DATABASE_URL="$db" DATABASE_SSL=disable
  export NEXT_TELEMETRY_DISABLED=1 AUTH_DEV_LINKS=1 LOG_LEVEL=warn
  AUTH_SECRET="$(cat "$state/auth-secret")"
  export AUTH_SECRET
  cd "$app"
  exec node server.js
) >>"$WEB_LOG" 2>&1 </dev/null &
pid=$!
echo "$pid" >"$WEB_STATE/server.pid"
# The shell's own bookkeeping (_, PWD, OLDPWD, SHLVL) may ride along; it holds no value of the caller's.
ALLOW="AUTH_DEV_LINKS AUTH_SECRET DATABASE_SSL DATABASE_URL HOME HOSTNAME LANG LOG_LEVEL NEXT_TELEMETRY_DISABLED NODE_ENV PATH PORT TZ _ PWD OLDPWD SHLVL"

for _ in $(seq 1 90); do
  kill -0 "$pid" 2>/dev/null || { echo "serve: FAIL the server exited"; tail -n 20 "$WEB_LOG"; exit 1; }
  code="$(curl -sS -m 5 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$WEB_PORT/api/health" 2>/dev/null || true)"
  if [ "$code" = 200 ]; then
    # Ours, not a squatter: the pid still runs and holds the port.
    kill -0 "$pid" 2>/dev/null || { echo "serve: FAIL the server exited after answering"; exit 1; }
    # The server's environment, by name (Linux: /proc), holds nothing beyond the allowlist.
    if [ -r "/proc/$pid/environ" ]; then
      extra="$(tr '\0' '\n' <"/proc/$pid/environ" | cut -d= -f1 | grep -vxF -f <(tr ' ' '\n' <<<"$ALLOW") | tr '\n' ' ' || true)"
      [ -z "$extra" ] || { echo "serve: FAIL the server's environment holds more than the allowlist: $extra"; kill "$pid"; exit 1; }
      echo "serve: the server's environment is the allowlist ($(tr '\0' '\n' <"/proc/$pid/environ" | grep -c =) names)"
    fi
    echo "serve: up on $WEB_PORT (pid $pid)"
    exit 0
  fi
  sleep 1
done
echo "serve: FAIL /api/health never answered 200"
tail -n 20 "$WEB_LOG"
exit 1
