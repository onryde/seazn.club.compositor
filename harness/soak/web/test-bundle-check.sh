#!/usr/bin/env bash
# bundle-check.sh on synthetic builds: a clean build passes; a build carrying a PostHog project key,
# a Sentry DSN or a Stripe key fails and names the file but never the value; PostHog's own default
# host (a library constant, not a key) passes; a file argument is scanned like a directory's files;
# an empty build and a missing directory fail. The
# planted values are made at run time, so no key-shaped literal sits in the repo.
#
#   harness/soak/web/test-bundle-check.sh        (prints `checked: N`)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
checked=0
failures=0
pass() { checked=$((checked + 1)); echo "PASS $1"; }
fail() { checked=$((checked + 1)); failures=$((failures + 1)); echo "FAIL $1${2:+: $2}"; }

run() { # <name> <want exit> <dir>...
  local name="$1" want="$2"; shift 2
  local rc=0
  "$HERE/bundle-check.sh" "$@" >"$W/$name.out" 2>&1 || rc=$?
  [ "$rc" -eq "$want" ] && pass "$name: exit $want" || fail "$name" "exit $rc, want $want: $(cat "$W/$name.out")"
}
build() { # <dir>: a small clean build
  mkdir -p "$1/static/chunks" "$1/server/app"
  printf 'self.__next_f.push([1,"hello"]);var h="https://eu.i.posthog.com";\n' >"$1/static/chunks/a.js"
  printf '{"pages":{}}\n' >"$1/server/app/b.json"
}
rep() { local s=""; for _ in $(seq "$2"); do s="$s$1"; done; printf '%s' "$s"; }

build "$W/clean"
run clean 0 "$W/clean/static" "$W/clean/server"
grep -q '2 files scanned' "$W/clean.out" && pass "clean: reports the files it scanned (2)" || fail "clean: count" "$(cat "$W/clean.out")"

key="phc_$(rep Ab3 12)"
build "$W/posthog"
printf 'var k="%s";\n' "$key" >>"$W/posthog/static/chunks/a.js"
run posthog 1 "$W/posthog/static" "$W/posthog/server"
grep -q 'PostHog project key (phc_) in .*/static/chunks/a.js' "$W/posthog.out" && pass "posthog: names the file and the kind" || fail "posthog: message" "$(cat "$W/posthog.out")"
grep -qF "$key" "$W/posthog.out" && fail "posthog: the value is never printed" "it is" || pass "posthog: the value is never printed"

dsn="https://$(rep 0f 16)@o1.ingest.sentry.io/42"
build "$W/sentry"
printf '{"dsn":"%s"}\n' "$dsn" >>"$W/sentry/server/app/b.json"
run sentry 1 "$W/sentry/static" "$W/sentry/server"
grep -qF "$dsn" "$W/sentry.out" && fail "sentry: the value is never printed" "it is" || pass "sentry: the value is never printed"

build "$W/stripe"
printf 'var s="pk_%s_%s";\n' test "$(rep Zz9 8)" >>"$W/stripe/static/chunks/a.js"
run stripe 1 "$W/stripe/static" "$W/stripe/server"

# A file named on its own is scanned too (soak.yml passes server.js and the dump beside the
# directories): clean passes and counts it, a key in it fails.
build "$W/file"
printf 'const nextConfig = {"env":{}};\n' >"$W/file/server.js"
run file-clean 0 "$W/file/server.js" "$W/file/static"
grep -q '2 files scanned' "$W/file-clean.out" && pass "file: a file argument counts (1 file and 1 under static)" || fail "file: count" "$(cat "$W/file-clean.out")"
printf 'var k="phc_%s";\n' "$(rep Qq7 8)" >>"$W/file/server.js"
run file-key 1 "$W/file/server.js" "$W/file/static"
grep -q 'PostHog project key (phc_) in .*/file/server.js' "$W/file-key.out" && pass "file: a key in a file argument is named" || fail "file: message" "$(cat "$W/file-key.out")"

mkdir -p "$W/empty/static"
run empty 1 "$W/empty/static"
run missing 1 "$W/nope"
run usage 64

echo "checked: $checked, $failures failures"
[ "$checked" -gt 0 ] && [ "$failures" -eq 0 ]
