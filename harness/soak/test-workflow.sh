#!/usr/bin/env bash
# soak.yml's shape, and its log converter (C15). The workflow only runs on Fly, so what can go wrong
# without a Machine is checked here:
#
# - the token gate: an empty FLY_API_TOKEN_SOAK warns and skips (the gate step run for real, both
#   ways), and the image and soak jobs run only when it said enabled;
# - the numbers: the plan step run for real (30 and 185 minutes, and a run too short to drive), the
#   soak job's timeout read from it;
# - the Machines: --restart no and --rm on the one `flyctl machine run`, restart "no" in both
#   configs, the compositor on the production guest;
# - the job token: made from 32 random bytes, masked before any other use, and only ever in a
#   variable assignment or a builtin's redirect (never argv), never in a run line's `-e`/`--env`;
#   FLY_API_TOKEN on steps only;
# - the cleanup: an `if: always()` step destroys every Machine and fails unless the list reads empty;
# - inputs reach scripts through env only (no `${{ inputs.* }}` inside a run);
# - page=real's page source: the web job runs on page=real only, holds no secret, builds seazn.club
#   at a 40-hex web_sha (refused otherwise, the args step run for real), refuses a PostHog, Sentry or
#   Stripe setting in the build's environment (that guard run for real, both ways), and checks the
#   bundle before it packs; the images job smokes the page-source image before it pushes it, and
#   runs when the web job was skipped; the harness Machine's image and guest follow the page
#   (soak.json's harness_guest); the page source's outputs are fetched; no SOAK_OVERLAY_URL is left;
# - the run list says PLUMBING ONLY on page=test (run-name, the step summary); the page source's
#   artifact survives "Re-run failed jobs"; the compositor's deadline is anchored at its launch; the
#   leak gate run for real eight ways (page=real or test with no harness scan, a leak, clean, and the
#   runner's compositor log kept only when the harness scanned its bytes); and
#   soak.json's watch, hard stop, timeout and post-check reserve in order;
# - flylog.py on a recorded-shape stream: pretty-printed entries, a replayed buffer, another
#   Machine's entry, a cut-off tail, stamps with 9 and 3 fraction digits and an offset, a bare and a
#   reasoned `overlay hidden`; and an empty stream refused.
#
#   harness/soak/test-workflow.sh                 (SOAK_YML=<file> checks another copy: mutants)
#
# Needs ruby (YAML), jq and python3. Prints `checked: N` and fails on zero.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
YML="${SOAK_YML:-$ROOT/.github/workflows/soak.yml}"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT

ruby -ryaml -rjson -e 'puts JSON.generate(YAML.load(File.read(ARGV[0])))' "$YML" >"$W/wf.json"

checked=0
failures=0
pass() { checked=$((checked + 1)); echo "PASS $1"; }
fail() { checked=$((checked + 1)); failures=$((failures + 1)); echo "FAIL $1${2:+: $2}"; }
# check <name> <jq filter over the workflow, true to pass>
check() {
  if jq -e "$2" "$W/wf.json" >/dev/null 2>&1; then pass "$1"; else fail "$1"; fi
}
step() { jq -r --arg j "$1" --arg n "$2" '.jobs[$j].steps[] | select(.name == $n) | .run' "$W/wf.json"; }

# --- trigger and gate -------------------------------------------------------------------------
check "dispatch only, inputs sha, minutes 185, page test" \
  '(.on // .["true"]) as $on | ($on | keys) == ["workflow_dispatch"]
   and $on.workflow_dispatch.inputs.minutes.default == "185" and ($on.workflow_dispatch.inputs | has("sha"))
   and $on.workflow_dispatch.inputs.page.default == "test"'
check "the gate reads FLY_API_TOKEN_SOAK" \
  '.jobs.plan.steps[] | select(.id == "gate") | .env.FLY_API_TOKEN == "${{ secrets.FLY_API_TOKEN_SOAK }}"'
check "images and soak run only when the gate said enabled, and never after a failed or cancelled job" \
  '(.jobs.images.if | contains("needs.plan.outputs.enabled == '"'"'true'"'"'") and contains("!cancelled()")
     and contains("needs.web.result == '"'"'success'"'"' || needs.web.result == '"'"'skipped'"'"'"))
   and (.jobs.soak.if | contains("needs.plan.outputs.enabled == '"'"'true'"'"'") and contains("!cancelled()")
     and contains("needs.images.result == '"'"'success'"'"'"))
   and .jobs.images.needs == ["plan", "web"] and .jobs.soak.needs == ["plan", "images"]'
check "the web job: page=real only, when enabled" \
  '.jobs.web.if == "needs.plan.outputs.enabled == '"'"'true'"'"' && inputs.page == '"'"'real'"'"'" and .jobs.web.needs == "plan"'
gate="$(jq -r '.jobs.plan.steps[] | select(.id == "gate") | .run' "$W/wf.json")"
: >"$W/gate.out"
out="$(FLY_API_TOKEN="" GITHUB_OUTPUT="$W/gate.out" bash -eo pipefail -c "$gate" 2>&1)" && rc=0 || rc=$?
if [ "$rc" -eq 0 ] && grep -qx 'enabled=false' "$W/gate.out" && grep -qF '::warning::FLY_API_TOKEN_SOAK unset — soak skipped' <<<"$out"; then
  pass "an empty token: the warning, enabled=false, exit 0"
else
  fail "an empty token" "exit $rc, output '$out', outputs '$(cat "$W/gate.out")'"
fi
: >"$W/gate.out"
out="$(FLY_API_TOKEN="x" GITHUB_OUTPUT="$W/gate.out" bash -eo pipefail -c "$gate" 2>&1)" && rc=0 || rc=$?
if [ "$rc" -eq 0 ] && grep -qx 'enabled=true' "$W/gate.out" && ! grep -q warning <<<"$out"; then
  pass "a token: enabled=true, no warning"
else
  fail "a token" "exit $rc, outputs '$(cat "$W/gate.out")'"
fi

# --- the numbers --------------------------------------------------------------------------------
numbers="$(step plan "The run's numbers")"
margin="$(jq -r .deadline_margin_min "$HERE/soak.json")"
after="$(jq -r .timeout_after_min "$HERE/soak.json")"
[ "$after" = 40 ] && pass "soak.json's timeout_after_min is the brief's 40" || fail "timeout_after_min" "$after, the brief says 40"
for m in 30 185; do
  : >"$W/plan.out"
  (cd "$ROOT" && MINUTES="$m" GITHUB_OUTPUT="$W/plan.out" bash -eo pipefail -c "$numbers" >/dev/null 2>&1) && rc=0 || rc=$?
  if [ "$rc" -eq 0 ] && grep -qx "timeout=$((m + 40))" "$W/plan.out" && grep -qx "drive=$((m - margin))" "$W/plan.out"; then
    pass "$m minutes: timeout $((m + 40)), drive $((m - margin))"
  else
    fail "$m minutes" "exit $rc, outputs '$(tr '\n' ' ' <"$W/plan.out")'"
  fi
done
short=$(($(jq -r .min_drive_min "$HERE/soak.json") + margin - 1))
(cd "$ROOT" && MINUTES="$short" GITHUB_OUTPUT="$W/plan.out" bash -eo pipefail -c "$numbers" >/dev/null 2>&1) && rc=0 || rc=$?
[ "$rc" -ne 0 ] && pass "$short minutes (a drive under min_drive_min) is refused" || fail "$short minutes" "accepted"
check "the soak job's timeout is the plan's" \
  '.jobs.soak["timeout-minutes"] == "${{ fromJSON(needs.plan.outputs.timeout) }}" and .jobs.plan.outputs.timeout == "${{ steps.plan.outputs.timeout }}"'

# --- the Machines ---------------------------------------------------------------------------------
launch="$(step soak "Launch the Machines")"
runs="$(grep -c 'flyctl machine run ' <<<"$launch" || true)"
if [ "$runs" -eq 1 ] && grep -E 'flyctl machine run .*--restart no .*--rm ' <<<"$launch" >/dev/null; then
  pass "one flyctl machine run, with --restart no and --rm"
else
  fail "flyctl machine run" "$runs run line(s), or no --restart no / --rm"
fi
if [ "$(grep -c 'restart: {policy: "no"}' <<<"$launch" || true)" -eq 2 ] && [ "$(grep -c 'auto_destroy: true' <<<"$launch" || true)" -eq 2 ]; then
  pass "both configs: restart no, auto_destroy"
else
  fail "both configs" "restart no / auto_destroy not in both"
fi
grep -qF 'guest: {cpu_kind: "performance", cpus: 4, memory_mb: 8192}' <<<"$launch" \
  && grep -qF 'COMPOSITOR_DRIVERS: "live"' <<<"$launch" \
  && pass "the compositor: performance, 4 CPUs, 8192 MB, live drivers" || fail "the compositor's guest"

# --- the job token ---------------------------------------------------------------------------------
grep -qE 'job_token="\$\(od -An -N32 -tx1 /dev/urandom' <<<"$launch" && pass "the job token: 32 random bytes" || fail "the job token's source"
first_use="$(grep -n 'job_token' <<<"$launch" | grep -v 'job_token="$(od' | grep -v '=~ \^\[0-9a-f\]{64}' | head -n 1 | cut -d: -f2-)"
if grep -qF 'echo "::add-mask::$job_token"' <<<"$first_use"; then
  pass "masked before any other use"
else
  fail "masked before any other use" "first use: $first_use"
fi
bad="$(grep 'job_token' <<<"$launch" | grep -v -E \
  'job_token="\$\(od |\[\[ "\$job_token" =~|echo "::add-mask::\$job_token"$|printf .%s\\n. "\$job_token" > "\$RUNNER_TEMP/job-token"$|JOB_TOKEN="\$job_token" ' || true)"
[ -z "$bad" ] && pass "the job token only in an assignment, the mask, or a builtin's redirect" || fail "the job token in argv" "$bad"
bad="$(jq -r '[.jobs[].steps[]? | .run // empty] | join("\n")' "$W/wf.json" | grep -E -- '(-e|--env) +"?JOB_TOKEN|--arg[a-z]* +[a-z_]+ +"?\$job_token' || true)"
[ -z "$bad" ] && pass "no JOB_TOKEN on a command line" || fail "JOB_TOKEN on a command line" "$bad"
check "FLY_API_TOKEN on steps only" \
  '((.env // {}) | has("FLY_API_TOKEN") | not) and ([.jobs[] | (.env // {}) | has("FLY_API_TOKEN")] | any | not)'

# --- the cleanup ----------------------------------------------------------------------------------
check "the destroy step is if: always(), in the job that launches" \
  '[.jobs.soak.steps[] | select(.id == "destroy")] | length == 1 and .[0].if == "always()"'
check "it comes after the launch" \
  '[.jobs.soak.steps | to_entries[] | select(.value.id == "launch" or .value.id == "destroy") | .value.id] == ["launch", "destroy"]'
destroy="$(jq -r '.jobs.soak.steps[] | select(.id == "destroy") | .run' "$W/wf.json")"
grep -qF 'flyctl machine destroy "$id" -a "$FLY_APP" --force' <<<"$destroy" \
  && grep -qF 'flyctl machine list -a "$FLY_APP"' <<<"$destroy" \
  && grep -qF '::error::Machines left in' <<<"$destroy" \
  && pass "it destroys every Machine, lists the app, and fails unless empty" || fail "the destroy step's body"

# --- inputs through env only, the verdict -------------------------------------------------------------
bad="$(jq -r '[.jobs[].steps[]? | .run // empty] | join("\n")' "$W/wf.json" | grep -n -F '${{ inputs.' || true)"
[ -z "$bad" ] && pass "no input expanded inside a run" || fail "an input inside a run" "$bad"
verdict="$(step soak "Verdict")"
grep -qF 'harness/soak/verdict.sh --dir "$OUT" --page "$PAGE" --drive-min "$DRIVE_MIN"' <<<"$verdict" \
  && grep -qF '::warning::PLUMBING ONLY' <<<"$verdict" && pass "the verdict: verdict.sh, PLUMBING ONLY on the test page" || fail "the verdict step"

# --- page=real's page source ---------------------------------------------------------------------
check "the web job holds no secret" '.jobs.web | tojson | contains("secrets.") | not'
check "it builds seazn.club at the plan's web_sha, without credentials" \
  '[.jobs.web.steps[] | select(.with.repository == "onryde/seazn.club")] | length == 1
   and .[0].with.ref == "${{ needs.plan.outputs.web_sha }}" and .[0].with["persist-credentials"] == false'
check "web_sha's default is 40 hex" '((.on // .["true"]).workflow_dispatch.inputs.web_sha.default | test("^[0-9a-f]{40}$"))'
args="$(jq -r '.jobs.plan.steps[] | select(.id == "args") | .run' "$W/wf.json")"
argrun() { : >"$W/args.out"; SHA_IN="$(printf 'a%.0s' {1..40})" MINUTES=30 PAGE="$1" WEB_SHA="$2" GITHUB_SHA=x GITHUB_OUTPUT="$W/args.out" \
  bash -eo pipefail -c "$args" >/dev/null 2>&1; }
argrun real nope && fail "a bad web_sha on page=real" "accepted" || pass "a bad web_sha on page=real is refused"
argrun test nope && pass "page=test ignores web_sha" || fail "page=test with any web_sha" "refused"
argrun real "$(printf 'b%.0s' {1..40})" && grep -qx "web_sha=$(printf 'b%.0s' {1..40})" "$W/args.out" \
  && pass "a 40-hex web_sha reaches the plan's output" || fail "a good web_sha" "$(cat "$W/args.out")"
build="$(step web "Build (no telemetry setting)")"
guard="$(sed -n '/^if env | cut/,/^fi$/p' <<<"$build")"
if [ -n "$guard" ]; then
  env -i PATH="$PATH" bash -eo pipefail -c "$guard" >/dev/null 2>&1 && pass "the build guard: a clean environment passes" || fail "the build guard" "refused a clean environment"
  rc=0; for v in NEXT_PUBLIC_POSTHOG_KEY SENTRY_DSN STRIPE_SECRET_KEY POSTHOG_KEY; do
    env -i PATH="$PATH" "$v=x" bash -eo pipefail -c "$guard" >/dev/null 2>&1 && rc=1
  done
  [ "$rc" -eq 0 ] && pass "the build guard: a PostHog, Sentry or Stripe setting is refused (4 names)" || fail "the build guard" "let one through"
else
  fail "the build guard" "not found in the build step"
fi
check "the bundle check runs after the build and before the pack and the upload" \
  '[.jobs.web.steps | to_entries[] | select(.value.name == "Build (no telemetry setting)" or .value.name == "The bundle carries no telemetry key"
     or .value.name == "Pack the page source" or (.value.uses // "" | startswith("actions/upload-artifact"))) | .key]
   | length == 4 and . == sort'
bc="$(step web "The bundle carries no telemetry key")"
grep -qF 'find "$A" -mindepth 1 -maxdepth 1 ! -name node_modules ! -name apps -print' <<<"$bc" \
  && grep -qF 'find "$A/apps" -mindepth 2 -maxdepth 2 ! -name node_modules -print' <<<"$bc" \
  && grep -qF 'harness/soak/web/bundle-check.sh "${parts[@]}" "$RUNNER_TEMP/build/db"' <<<"$bc" \
  && grep -qF 'A=web/apps/web/.next/standalone' <<<"$bc" \
  && pass "the bundle check scans what app.tgz packs (but node_modules) and the dump" || fail "the bundle check's targets"
smoke="$(step images "Build and smoke the page-source image")"
grep -qF 'docker run --rm --entrypoint /soak/web/smoke.sh "$WEB_IMAGE"' <<<"$smoke" && pass "the page-source image is smoked" || fail "the smoke"
check "the smoke comes before the push, both page=real's image steps conditional" \
  '. as $wf | [.jobs.images.steps | to_entries[] | select(.value.name == "Build and smoke the page-source image" or .value.name == "Push to Fly'"'"'s registry") | .key]
     | length == 2 and .[0] < .[1]
   and ([$wf.jobs.images.steps[] | select(.name == "Build and smoke the page-source image" or .name == "The page source (web job)") | .if]
     == ["inputs.page == '"'"'real'"'"'", "inputs.page == '"'"'real'"'"'"])'
grep -qF '[ "$PAGE" != real ] || harness="harness-web $WEB_IMAGE"' <<<"$(step images "Push to Fly's registry")" \
  && pass "page=real pushes the page-source image as harness-web" || fail "the push by page"
grep -qF '[ "$PAGE" != real ] || h_image="registry.fly.io/$FLY_APP:harness-web-$SHA"' <<<"$launch" \
  && grep -qF "jq -r --arg p \"\$PAGE\" '.harness_guest[\$p].cpus'" <<<"$launch" \
  && grep -qF 'run_machine "$name_h" "$h_image" "$RUNNER_TEMP/harness.machine.json" "$h_cpus" "$h_mem"' <<<"$launch" \
  && pass "the harness Machine's image and guest follow the page" || fail "the harness by page"
jq -e '.harness_guest.test == {cpus: 4, memory_mb: 8192} and (.harness_guest.real.cpus >= 4) and (.harness_guest.real.memory_mb >= 8192)' "$HERE/soak.json" >/dev/null \
  && pass "soak.json: the harness guest per page, page=real at least the test page's" || fail "soak.json's harness_guest"
fetch="$(step soak "Fetch the outputs")"
miss=""; for f in deliveries.jsonl score.log seed.json web-up.log web.log postgres.log overlay-samples.jsonl; do
  grep -qE "(^| )$f( |;|$)" <<<"$fetch" || miss="$miss $f"; done
[ -z "$miss" ] && pass "the fetch takes the page source's outputs (7 files)" || fail "the fetch" "missing:$miss"
grep -qF SOAK_OVERLAY_URL "$YML" && fail "no SOAK_OVERLAY_URL is left" "it is" || pass "no SOAK_OVERLAY_URL is left"

# --- the run list, re-runs, the deadline, the leak gate -------------------------------------------
check "run-name: the page and the minutes, PLUMBING ONLY on page=test" \
  '.["run-name"] | contains("page=${{ inputs.page }}") and contains("${{ inputs.minutes }} min")
   and contains("inputs.page == '"'"'test'"'"' && '"'"' (PLUMBING ONLY)'"'"'")'
grep -qF '# PLUMBING ONLY — NOT A SOAK PASS' <<<"$verdict" && pass "the step summary heads a plumbing run PLUMBING ONLY" || fail "the summary heading"
check "the page source's artifact: one name per run, replaced by a re-run (no run_attempt)" \
  '[.jobs.web.steps[], .jobs.images.steps[] | select((.uses // "") | test("actions/(up|down)load-artifact")) | .with]
   | length == 2 and all(.name == "soak-web-${{ github.run_id }}") and (.[0].overwrite == true)'
if awk '/the harness never came up/ { up = NR } /deadline_c_ms=\$\(\(\$\(date \+%s%3N\) \+ 10#\$MINUTES \* 60000\)\)/ { d = NR }
        /DEADLINE_AT="\$deadline_at"/ { c = NR } END { exit !(up && d > up && c > d) }' <<<"$launch"; then
  pass "the compositor's deadline is anchored at its launch, after the harness came up"
else
  fail "the compositor's deadline anchor"
fi
gate="$(step soak "Leak gate")"
leakgate() { # <page> <secrets.json content or ""> [compositor.log content]: runs the step on a fake
  # /out; rc, then what is left. @LOGSUM@ in the secrets is the log's own sha256.
  rm -rf "$W/lg"; mkdir -p "$W/lg/out"
  printf 'tok_%s\n' "$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')" >"$W/lg/job-token"
  for f in status.json done driver.log web.log report.json; do echo '{}' >"$W/lg/out/$f"; done
  local sec="$2" sum
  if [ -n "${3:-}" ]; then
    printf '%s\n' "$3" >"$W/lg/out/compositor.log"
    sum="$(python3 -I -c 'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$W/lg/out/compositor.log")"
    sec="${sec//@LOGSUM@/$sum}"
  fi
  [ -z "$sec" ] || printf '%s\n' "$sec" >"$W/lg/out/secrets.json"
  : >"$W/lg/gh-out"
  local rc=0
  RUNNER_TEMP="$W/lg" PAGE="$1" GITHUB_OUTPUT="$W/lg/gh-out" bash -eo pipefail -c "$gate" >"$W/lg/log" 2>&1 || rc=$?
  echo "$rc $(cd "$W/lg/out" && ls | sort | tr '\n' ' ')"
}
got="$(leakgate real "")"
[ "$got" = "0 done status.json " ] && grep -qx 'ok=true' "$W/lg/gh-out" \
  && pass "leak gate: page=real with no harness scan keeps only status.json and done" || fail "leak gate, page=real, no scan" "$got"
got="$(leakgate test "")"
[ "$got" = "0 done driver.log report.json status.json web.log " ] \
  && pass "leak gate: page=test with no harness scan uploads on the job's scan" || fail "leak gate, page=test, no scan" "$got"
got="$(leakgate real '{"scanned":24,"leaks":1,"skipped":[],"leaked":[{"kind":"WEB_COOKIE[seazn_session]","where":["web.log:3 \"cookie: seazn_session=<WEB_COOKIE[seazn_session]>\""]}]}')"
[ "${got%% *}" = 1 ] && ! grep -qx 'ok=true' "$W/lg/gh-out" \
  && grep -qF 'leaked: WEB_COOKIE[seazn_session] in web.log:3 "cookie: seazn_session=<WEB_COOKIE[seazn_session]>"' "$W/lg/log" \
  && pass "leak gate: a leak uploads nothing and prints the harness's redacted report" || fail "leak gate, a leak" "$got: $(head -c 400 "$W/lg/log")"
got="$(leakgate real '{"scanned":24,"leaks":0,"skipped":[],"leaked":[]}')"
[ "${got%% *}" = 0 ] && grep -qx 'ok=true' "$W/lg/gh-out" && pass "leak gate: a clean scan uploads" || fail "leak gate, clean" "$got"
# The runner's own compositor log (page=real): up only when the harness scanned those bytes.
clog='ts=2026-10-24T12:00:00.000Z level=info msg=cef alive gen=1'
got="$(leakgate real '{"scanned":24,"leaks":0,"skipped":[],"leaked":[],"compositor_log_sha256":"@LOGSUM@"}' "$clog")"
[ "$got" = "0 compositor.log done driver.log report.json secrets.json status.json web.log " ] \
  && pass "leak gate: page=real uploads the compositor log the harness scanned (same sha256)" || fail "leak gate, scanned log" "$got"
got="$(leakgate real '{"scanned":24,"leaks":0,"skipped":[],"leaked":[],"compositor_log_sha256":"0000"}' "$clog")"
[ "$got" = "0 done driver.log report.json secrets.json status.json web.log " ] && grep -qF 'did not scan this compositor log' "$W/lg/log" \
  && pass "leak gate: page=real drops a compositor log the harness never scanned (its upload failed)" || fail "leak gate, unscanned log" "$got"
got="$(leakgate real '{"scanned":24,"leaks":0,"skipped":[],"leaked":[]}' "$clog")"
[ "$got" = "0 done driver.log report.json secrets.json status.json web.log " ] \
  && pass "leak gate: page=real drops the compositor log when the scan names none" || fail "leak gate, no log sum" "$got"
got="$(leakgate test '{"scanned":24,"leaks":0,"skipped":[],"leaked":[]}' "$clog")"
[ "$got" = "0 compositor.log done driver.log report.json secrets.json status.json web.log " ] \
  && pass "leak gate: page=test keeps its compositor log (no page-source credential exists)" || fail "leak gate, test log" "$got"
jq -e '.poll_after_min >= 20 and .poll_after_min < .hard_stop_after_min and .hard_stop_after_min < .timeout_after_min
       and .post_check_reserve_s > 60 and .post_check_reserve_s < (.hard_stop_after_min - .poll_after_min) * 60' "$HERE/soak.json" >/dev/null \
  && pass "soak.json: watch 20+ min past the run, then the hard stop, then the job's timeout; the post-check reserve fits" \
  || fail "soak.json's watch, stop and reserve" "$(jq -c '{poll_after_min, hard_stop_after_min, timeout_after_min, post_check_reserve_s}' "$HERE/soak.json")"

# --- flylog.py ---------------------------------------------------------------------------------
cat >"$W/stream.json" <<'EOF'
{
  "instance": "m1",
  "level": "info",
  "message": "ts=2026-10-24T12:00:00.100Z level=info msg=boot",
  "region": "lhr",
  "timestamp": "2026-10-24T12:00:00.123456789Z"
}
{"instance":"m2","message":"another Machine","timestamp":"2026-10-24T12:00:00.200Z"}
{"instance":"m1","message":"ts=2026-10-24T12:00:01.000Z level=warn msg=overlay hidden reason=hang gen=1","timestamp":"2026-10-24T12:00:01.001Z"}
not json at all
{"instance":"m1","message":"ts=2026-10-24T12:00:02.000Z level=warn msg=overlay hidden","timestamp":"2026-10-24T13:00:02+01:00"}
{"instance":"m1","message":"no stamp here","timestamp":""}
{
  "instance": "m1",
  "level": "info",
  "message": "ts=2026-10-24T12:00:00.100Z level=info msg=boot",
  "region": "lhr",
  "timestamp": "2026-10-24T12:00:00.123456789Z"
}
{"instance":"m1","message":"ts=2026-10-24T12:00:03.000Z msg=overlay hiddenness is not a hide","timestamp":"2026-10-24T12:00:03Z"}
{"instance":"m1","message":"cut off","timest
EOF
cat >"$W/want.log" <<'EOF'
2026-10-24T12:00:00.123456Z ts=2026-10-24T12:00:00.100Z level=info msg=boot
2026-10-24T12:00:01.001000Z ts=2026-10-24T12:00:01.000Z level=warn msg=overlay hidden reason=hang gen=1
2026-10-24T12:00:02.000000Z ts=2026-10-24T12:00:02.000Z level=warn msg=overlay hidden
no stamp here
2026-10-24T12:00:03.000000Z ts=2026-10-24T12:00:03.000Z msg=overlay hiddenness is not a hide
EOF
rc=0
python3 -I "$HERE/flylog.py" --in "$W/stream.json" --machine m1 --log "$W/got.log" --scan "$W/scan.json" >/dev/null || rc=$?
if [ "$rc" -eq 0 ] && diff -u "$W/want.log" "$W/got.log" >"$W/log.diff"; then
  pass "flylog: the lines, in order, once each, of this Machine, stamps in UTC"
else
  fail "flylog's lines" "exit $rc; $(head -c 600 "$W/log.diff")"
fi
jq -e '. == {lines: 5, overlay_hidden_lines: 2, other_machine_entries: 1}' "$W/scan.json" >/dev/null 2>&1 \
  && pass "flylog: logscan counts both hides, the bare one too, and not the lookalike" || fail "flylog's logscan" "$(cat "$W/scan.json" 2>/dev/null)"
printf '{"instance":"m2","message":"x","timestamp":"2026-10-24T12:00:00Z"}\n' >"$W/empty.json"
rc=0
python3 -I "$HERE/flylog.py" --in "$W/empty.json" --machine m1 --log "$W/e.log" --scan "$W/e.json" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] && pass "flylog: a stream with nothing of the Machine is refused" || fail "flylog's empty stream" "exit $rc"

echo "checked: $checked, $failures failures"
[ "$checked" -gt 0 ] && [ "$failures" -eq 0 ]
