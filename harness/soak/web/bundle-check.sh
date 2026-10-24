#!/usr/bin/env bash
# The standalone web build's telemetry gate (C15, page=real). A local web server sends to LIVE
# PostHog when its build carries a key, so the soak's build leaves every NEXT_PUBLIC_POSTHOG*,
# Sentry DSN and Stripe key unset, and this proves it on the build itself: no file under the given
# directories carries a PostHog project key, a Sentry DSN or a Stripe key. It names the file and
# the kind of key, never the value. Zero files scanned is a failure, never a pass.
#
#   harness/soak/web/bundle-check.sh <dir>...      (e.g. .next/static .next/server)
set -euo pipefail
[ "$#" -gt 0 ] || { echo "bundle-check: usage: bundle-check.sh <dir>..." >&2; exit 64; }

files=0
for d in "$@"; do
  [ -d "$d" ] || { echo "bundle-check: FAIL $d is not a directory"; exit 1; }
  n=$(find "$d" -type f | wc -l | tr -d ' ')
  files=$((files + n))
done
[ "$files" -gt 0 ] || { echo "bundle-check: FAIL 0 files scanned in $*"; exit 1; }

found=0
scan_all() { local kind="$1" re="$2"; shift 2; local hits
  hits=$(grep -rlaE -- "$re" "$@" 2>/dev/null | head -20 || true)
  if [ -n "$hits" ]; then
    found=$((found + 1))
    while IFS= read -r f; do echo "bundle-check: FAIL $kind in $f"; done <<<"$hits"
  fi
}
scan_all "a PostHog project key (phc_)" 'phc_[A-Za-z0-9]{16,}' "$@"
scan_all "a Sentry DSN" 'https://[0-9a-f]{16,}@[A-Za-z0-9.-]*sentry\.io' "$@"
scan_all "a Stripe key" '(sk|pk|rk)_(live|test)_[A-Za-z0-9]{16,}' "$@"

if [ "$found" -gt 0 ]; then
  echo "bundle-check: FAIL $files files scanned, $found kind(s) of key found"
  exit 1
fi
echo "bundle-check: $files files scanned, no PostHog key, Sentry DSN or Stripe key"
