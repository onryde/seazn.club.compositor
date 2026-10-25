#!/usr/bin/env bash
# shellcheck disable=SC2016  # the mutants and jq programs hold literal $ text, never meant to expand
# The deploy checks' own test (C16, docs/tasks/C16-deploy.md). check.yml runs it after
# scripts/check-workflows.sh. Never touches the repo's files: every mutant edits a scratch copy.
#
# 1. scripts/check-workflows.sh: green on the repo's workflows; red on an empty set; red on each
#    mutant, by its own reason. The brief's six: (1) the ::warning:: guard removed from
#    deploy-stg.yml, (2) environment: removed from promote.yml, (4) the CEF bump gate step deleted
#    from promote.yml, (5) a seazn-relay-cache cache-from in deploy-stg.yml, (6) target:
#    relay-test-hooks in deploy-stg.yml; plus one for each further rule (e1 to e17).
# 2. scripts/check-cef-bump.sh: green on the repo; red on the brief's mutant (3), GSTCEF_COMMIT
#    changed without the record, and on each malformed record; green again once the record follows
#    the bump (the procedure's positive pair).
# 3. The workflows' own scripts, extracted as written and run under fake flyctl, docker and gh:
#    promote.yml's "Promote RELAY_IMAGE" step and its token guard, deploy-stg.yml's token guard and
#    "Already pushed?", and deploy-prod.yml's "The tag is a release on main" (on a scratch git repo).
#
# Needs bash, ruby (YAML), jq, python3, git, and the pinned actionlint and shellcheck on PATH
# (scripts/check-workflows.sh --install DIR). Prints `checked: N` and fails on 0.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT

checked=0 failures=0
ok() { checked=$((checked + 1)); echo "ok: $1"; }
bad() { checked=$((checked + 1)); failures=$((failures + 1)); echo "FAIL: $1" >&2; }

# edit FILE OLD NEW: replace OLD, which must occur exactly once (a mutant that no longer applies is a
# red here, never a silent pass).
edit() {
  python3 -I -c '
import sys
p, old, new = sys.argv[1:4]
s = open(p).read()
n = s.count(old)
if n != 1:
    sys.exit(f"edit: {p}: {n} matches for {old!r}")
open(p, "w").write(s.replace(old, new))' "$@"
}

# ---------------------------------------------------------------- 1. check-workflows.sh
fresh() { rm -rf "$W/wf"; mkdir -p "$W/wf"; cp "$root"/.github/workflows/*.yml "$W/wf/"; }
n_repo=$(find "$root/.github/workflows" -maxdepth 1 -type f \( -name '*.yml' -o -name '*.yaml' \) | wc -l | tr -d ' ')

check() {
  if "$root/scripts/check-workflows.sh" "$1" > "$W/check.out" 2>&1; then rc=0; else rc=$?; fi
}
# expect_red NAME REASON: the check fails, says REASON, and read the whole scratch set first.
expect_red() {
  local n
  n=$(find "$W/wf" -maxdepth 1 -type f -name '*.yml' | wc -l | tr -d ' ')
  check "$W/wf"
  if [ "$rc" -ne 0 ] && grep -qF -- "$2" "$W/check.out" && grep -qF "checked $n workflows" "$W/check.out"; then
    ok "check-workflows red: $1"
  else
    bad "check-workflows: $1: rc $rc, expected a red saying: $2"
    sed 's/^/    /' "$W/check.out" >&2
  fi
}
expect_green() {
  check "$W/wf"
  if [ "$rc" -eq 0 ] && grep -qF 'check-workflows: OK' "$W/check.out"; then
    ok "check-workflows green: $1"
  else
    bad "check-workflows: $1: rc $rc, expected green"
    sed 's/^/    /' "$W/check.out" >&2
  fi
}

# The repo as it is.
check "$root/.github/workflows"
if [ "$rc" -eq 0 ] && grep -qF "checked $n_repo workflows" "$W/check.out" && grep -qF 'deploy workflows: 3 ' "$W/check.out"; then
  ok "check-workflows green on the repo ($n_repo workflows, 3 deploy)"
else
  bad "check-workflows on the repo: rc $rc"; sed 's/^/    /' "$W/check.out" >&2
fi

# Step 1's red: an empty set.
mkdir -p "$W/empty"
check "$W/empty"
# Its own reason, not the missing deploy workflows' (which would red an empty set too).
if [ "$rc" -ne 0 ] && grep -qF 'checked 0 workflows' "$W/check.out" \
  && grep -qF '0 workflows found: a check of nothing proves nothing' "$W/check.out"; then
  ok "check-workflows red on an empty set (checked 0 workflows)"
else
  bad "check-workflows on an empty set: rc $rc"; sed 's/^/    /' "$W/check.out" >&2
fi

STG=deploy-stg.yml PROD=deploy-prod.yml PROMOTE=promote.yml

# (1) The ::warning:: guard removed from deploy-stg.yml.
fresh; edit "$W/wf/$STG" '            echo "::warning::FLY_API_TOKEN_STG unset — push skipped"'$'\n' ''
expect_red "(1) deploy-stg's ::warning:: guard removed" 'deploy-stg.yml FLY_API_TOKEN_STG: no step warns'

# (2) environment: removed from promote.yml.
fresh; edit "$W/wf/$PROMOTE" '    environment: relay-${{ inputs.env }}'$'\n' ''
expect_red "(2) promote's environment removed" 'job promote holds a Fly token and declares no relay-* environment'

# (4) The gate step deleted from promote.yml.
fresh; edit "$W/wf/$PROMOTE" '      - name: CEF bump gate (Owed 22)
        run: scripts/check-cef-bump.sh
      # Each env' '      # Each env'
expect_red "(4) promote's CEF bump gate deleted" 'promote.yml (CEF bump gate, Owed 22): job promote: the step after its checkout'

# (5) A seazn-relay-cache cache-from in deploy-stg.yml.
fresh; edit "$W/wf/$STG" '          load: true
          push: false' '          load: true
          push: false
          cache-from: type=registry,ref=ghcr.io/onryde/seazn-relay-cache:cef-x'
expect_red "(5) deploy-stg reads seazn-relay-cache" 'deploy-stg.yml names the CI build cache'

# (6) target: relay-test-hooks in deploy-stg.yml.
fresh; edit "$W/wf/$STG" '          target: relay' '          target: relay-test-hooks'
expect_red "(6) deploy-stg builds relay-test-hooks" 'deploy-stg.yml names a test-hooks target'

# e1. A token step no guard gates: deploy-stg's push job without its `if`.
fresh; edit "$W/wf/$STG" "    needs: gate
    if: needs.gate.outputs.enabled == 'true'" '    needs: gate'
expect_red "e1 an ungated token step" 'push.Already pushed? holds FLY_API_TOKEN_STG but runs whether or not the guard found it'

# e2. The token echoed to the log.
fresh; edit "$W/wf/$STG" '          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"' '          flyctl auth docker
          echo "token: $FLY_API_TOKEN"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"'
expect_red "e2 the token echoed" 'prints FLY_API_TOKEN: echo "token: $FLY_API_TOKEN"'

# e3. The token written to $GITHUB_OUTPUT.
fresh; edit "$W/wf/$STG" '          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"' '          flyctl auth docker
          printf "t=%s\n" "$FLY_API_TOKEN" >> "$GITHUB_OUTPUT"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"'
expect_red "e3 the token into GITHUB_OUTPUT" 'prints FLY_API_TOKEN: printf'

# e3+ The rule's one exception, held to its shape: into a file under RUNNER_TEMP is not an echo.
fresh; edit "$W/wf/$STG" '          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"' '          flyctl auth docker
          ( umask 077; printf "%s\n" "$FLY_API_TOKEN" > "$RUNNER_TEMP/scan-patterns" )
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"'
expect_green "e3+ the token into a RUNNER_TEMP pattern file (soak.yml's leak gate shape)"

# e4. A secret interpolated into a script.
fresh; edit "$W/wf/$STG" '          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"' '          flyctl auth docker
          test -n "${{ secrets.FLY_API_TOKEN_STG }}"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"'
expect_red "e4 a secret interpolated into run" 'interpolates a secret into its script'

# e5. xtrace in a step that holds the token.
fresh; edit "$W/wf/$STG" '          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"' '          set -x
          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"'
expect_red "e5 xtrace with the token" 'holds a secret under xtrace'

# e6. A target other than relay.
fresh; edit "$W/wf/$STG" '          target: relay' '          target: cef-test'
expect_red "e6 deploy-stg builds cef-test" "deploy-stg.yml builds target 'cef-test'"

# e7. The registry hardcoded.
fresh; edit "$W/wf/$STG" '          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"' '          flyctl auth docker
          ref="registry.fly.io/$RELAY_APP:$TAG"'
expect_red "e7 the registry hardcoded" 'deploy-stg.yml hardcodes the registry'

# e8. The other env's token as a fallback (`a && X || Y`).
fresh; edit "$W/wf/$PROMOTE" "          FLY_API_TOKEN_STG: \${{ inputs.env == 'stg' && secrets.FLY_API_TOKEN_STG || '' }}
          FLY_API_TOKEN_PROD: \${{ inputs.env == 'prod' && secrets.FLY_API_TOKEN_PROD || '' }}
          GH_TOKEN" "          FLY_API_TOKEN_STG: \${{ inputs.env == 'prod' && secrets.FLY_API_TOKEN_PROD || secrets.FLY_API_TOKEN_STG }}
          GH_TOKEN"
expect_red "e8 a token fallback" 'names two Fly tokens in one expression'

# e9. A guard that reads the wrong env's gate: prod's token behind inputs.env == 'stg'.
fresh; edit "$W/wf/$PROMOTE" "          FLY_API_TOKEN_PROD: \${{ inputs.env == 'prod' && secrets.FLY_API_TOKEN_PROD || '' }}
          RELAY_ENV" "          FLY_API_TOKEN_PROD: \${{ inputs.env == 'stg' && secrets.FLY_API_TOKEN_PROD || '' }}
          RELAY_ENV"
expect_red "e9 a guard on the wrong env's gate" 'promote.yml FLY_API_TOKEN_PROD: no step warns'

# e10. A job-level env holding the token.
fresh; edit "$W/wf/$STG" '      TAG: ${{ github.sha }}' '      TAG: ${{ github.sha }}
      FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_STG }}'
expect_red "e10 a job env holds the token" 'job push holds FLY_API_TOKEN_STG in its env'

# e11. A workflow-level env holding a secret.
fresh; edit "$W/wf/$STG" '  RELAY_APP: seazn-relay-stg' '  RELAY_APP: seazn-relay-stg
  ANY_SECRET: ${{ secrets.SOMETHING }}'
expect_red "e11 a workflow env holds a secret" 'the workflow-level env holds a secret'

# e12. The token used before the checkout and its gate.
fresh; edit "$W/wf/$STG" '      # Actions pinned by commit: this job holds a Fly token (image.yml'"'"'s cache job, the PR #47 review'"'"'s m7).
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0
        with: { persist-credentials: false }
      - name: CEF bump gate (Owed 22)
        run: scripts/check-cef-bump.sh
      # CEF' '      - name: Early
        env:
          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_STG }}
        run: flyctl version
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0
        with: { persist-credentials: false }
      - name: CEF bump gate (Owed 22)
        run: scripts/check-cef-bump.sh
      # CEF'
expect_red "e12 the token before the checkout" 'job push uses its Fly token before it checks out'

# e13. A deploy workflow by what it does, not its name: a copy of deploy-stg with no CEF bump gate.
fresh; cp "$W/wf/$STG" "$W/wf/ship.yml"
python3 -I -c '
import sys
p = sys.argv[1]
s = open(p).read()
gate = "      - name: CEF bump gate (Owed 22)\n        run: scripts/check-cef-bump.sh\n"
if s.count(gate) != 2: sys.exit("ship.yml: expected 2 gate steps")
open(p, "w").write(s.replace(gate, "").replace("name: deploy-stg", "name: ship"))' "$W/wf/ship.yml"
expect_red "e13 an unnamed deploy workflow with no gate" 'ship.yml (CEF bump gate, Owed 22)'

# e14. A required deploy workflow missing.
fresh; rm "$W/wf/$PROD"
expect_red "e14 deploy-prod.yml missing" 'deploy workflow missing: deploy-prod.yml'

# e15. actionlint (shellcheck off) on every workflow: a bad value in check.yml.
fresh; edit "$W/wf/check.yml" '    timeout-minutes: 25' '    timeout-minutes: soon'
expect_red "e15 actionlint on a non-deploy workflow" 'check-workflows: FAIL: actionlint:'

# e16. shellcheck on a deploy workflow: an unquoted expansion.
fresh; edit "$W/wf/$STG" '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          {' '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          cd $RUNNER_TEMP
          {'
expect_red "e16 shellcheck on a deploy workflow" 'actionlint with shellcheck on the deploy workflows'

# e17. A token step whose guard is in another job that the step's job does not need.
fresh; edit "$W/wf/$STG" "    needs: gate
    if: needs.gate.outputs.enabled == 'true'" "    if: needs.gate.outputs.enabled == 'true'"
expect_red "e17 a job gate without needs" 'holds FLY_API_TOKEN_STG but runs whether or not the guard found it'

# ---------------------------------------------------------------- 2. check-cef-bump.sh
pin=$(tr -d '[:space:]' < "$root/docker/GSTCEF_COMMIT")
cef_root() {
  rm -rf "$W/cef"; mkdir -p "$W/cef/docker"
  cp "$root/docker/GSTCEF_COMMIT" "$root/docker/CEF_STEP0_PASS" "$W/cef/docker/"
  cp "$root/_FINDINGS.md" "$W/cef/"
}
gate() {
  if "$root/scripts/check-cef-bump.sh" "$W/cef" > "$W/gate.out" 2>&1; then rc=0; else rc=$?; fi
}
gate_red() {
  gate
  if [ "$rc" -ne 0 ] && grep -qF -- "$2" "$W/gate.out" \
    && grep -qxF '::error::CEF bump without a passing C11 Step 0 re-run (compositor docs/_INDEX.md Owed 22)' "$W/gate.out"; then
    ok "cef bump gate red: $1"
  else
    bad "cef bump gate: $1: rc $rc, expected a red saying: $2"; sed 's/^/    /' "$W/gate.out" >&2
  fi
}
gate_green() {
  gate
  if [ "$rc" -eq 0 ] && grep -qxF "cef bump gate: GSTCEF_COMMIT=$2 step0=$2 F-009" "$W/gate.out"; then
    ok "cef bump gate green: $1"
  else
    bad "cef bump gate: $1: rc $rc, expected green at $2 F-009"; sed 's/^/    /' "$W/gate.out" >&2
  fi
}
bump=0123456789abcdef0123456789abcdef01234567

# The repo's own record: the amendment's seed (the pinned commit, F-009).
if "$root/scripts/check-cef-bump.sh" > "$W/gate.out" 2>&1 \
  && grep -qxF "cef bump gate: GSTCEF_COMMIT=$pin step0=$pin F-009" "$W/gate.out"; then
  ok "cef bump gate green on the repo (seeded $pin F-009)"
else
  bad "cef bump gate on the repo"; sed 's/^/    /' "$W/gate.out" >&2
fi

cef_root; gate_green "a scratch copy" "$pin"
# (3) GSTCEF_COMMIT changed without touching the record.
cef_root; echo "$bump" > "$W/cef/docker/GSTCEF_COMMIT"
gate_red "(3) a bump with no Step 0 record" "docker/GSTCEF_COMMIT=$bump, but C11 Step 0 last passed on $pin (F-009)"
cef_root; printf '%s\nF-009\n' "$bump" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "the record moved without the pin" "but C11 Step 0 last passed on $bump"
cef_root; printf '%s\nF-999\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "an F-id with no finding" "_FINDINGS.md has no '## F-999' heading"
# F-006 exists only as a `###` heading: a `## F-0xx` heading is the bar.
cef_root; printf '%s\nF-006\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "an F-id with only a ### heading" "_FINDINGS.md has no '## F-006' heading"
cef_root; printf '%s\nF-09\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "a malformed F-id" "line 2 is not an F-id: 'F-09'"
cef_root; printf '%s\nF-009\nF-010\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "a three-line record" "has 3 lines, not 2"
cef_root; printf '%s\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "a one-line record" "has 1 lines, not 2"
cef_root; rm "$W/cef/docker/CEF_STEP0_PASS"
gate_red "no record" "no docker/CEF_STEP0_PASS"
cef_root; echo "v139" > "$W/cef/docker/GSTCEF_COMMIT"
gate_red "a pin that is not a commit" "docker/GSTCEF_COMMIT is not a 40-hex commit: 'v139'"
# The procedure's positive pair: the bump, re-run and recorded, passes.
cef_root; echo "$bump" > "$W/cef/docker/GSTCEF_COMMIT"; printf '%s\nF-009\n' "$bump" > "$W/cef/docker/CEF_STEP0_PASS"
gate_green "a bump with its record" "$bump"
cef_root; printf '%s\r\nF-009\r\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_green "a CRLF record" "$pin"

# ---------------------------------------------------------------- 3. the workflows' scripts
to_json() { ruby -ryaml -rjson -e 'puts JSON.generate(YAML.load(File.read(ARGV[0])))' "$1"; }
# extract FILE JOB STEP_ID OUT: the step's run block, as the runner writes it.
extract() {
  to_json "$root/.github/workflows/$1" | jq -er --arg j "$2" --arg s "$3" '.jobs[$j].steps[] | select(.id == $s) | .run' > "$4"
  if grep -qF '${{' "$4"; then bad "$1 $2.$3 holds an expression: inputs must reach it by env"; fi
}
mkdir -p "$W/bin" "$W/rt"

# Fakes. State lives in $STATE; every call is logged to $STATE/log.
cat > "$W/bin/flyctl" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${FLY_API_TOKEN:-}" = "${EXPECT_TOKEN:-unset}" ]; then tok=ok; else tok=WRONG; fi
echo "flyctl $* (token $tok)" >> "$STATE/log"
case "$1 $2" in
  "auth docker") ;;
  "config show") cat "$STATE/config.json" ;;
  "secrets list")
    if [ -f "$STATE/deployed" ] && [ -f "$STATE/secrets-after.json" ]; then cat "$STATE/secrets-after.json"; else cat "$STATE/secrets.json"; fi ;;
  "secrets set") ;;
  "secrets deploy") touch "$STATE/deployed" ;;
  *) echo "fake flyctl: unexpected: $*" >&2; exit 2 ;;
esac
SH
cat > "$W/bin/docker" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "docker $*" >> "$STATE/log"
[ "$1 $2 $3" = "buildx imagetools inspect" ] || { echo "fake docker: unexpected: $*" >&2; exit 2; }
d=$(awk -v r="$4" '$1 == r {print $2}' "$STATE/registry")
[ -n "$d" ] || { echo "ERROR: $4: not found" >&2; exit 1; }
echo "$d"
SH
cat > "$W/bin/gh" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "gh $*" >> "$STATE/log"
[ "$1" = api ] && [ "$3" = --jq ] || { echo "fake gh: unexpected: $*" >&2; exit 2; }
case "$2" in
  */actions/workflows/promote.yml/runs\?*) f="$STATE/runs.json" ;;
  */actions/runs/*/jobs) id=${2%/jobs}; f="$STATE/jobs-${id##*/}.json" ;;
  *) echo "fake gh: no route $2" >&2; exit 2 ;;
esac
[ -f "$f" ] || { echo "HTTP 403: fake" >&2; exit 1; }
jq -r "$4" "$f"
SH
chmod +x "$W/bin/flyctl" "$W/bin/docker" "$W/bin/gh"

SHA=1111111111111111111111111111111111111111
OLD=2222222222222222222222222222222222222222
SKIPPED=3333333333333333333333333333333333333333
D_NEW=sha256:$(printf 'a%.0s' $(seq 64))
D_OLD=sha256:$(printf 'b%.0s' $(seq 64))
D_PROD=sha256:$(printf 'c%.0s' $(seq 64))
REG=registry.example
state() {
  STATE="$W/state"; rm -rf "$STATE"; mkdir -p "$STATE"; : > "$STATE/log"; : > "$W/summary"; : > "$W/output"
  export STATE
  printf '%s\n' "$REG/seazn-relay-stg:$SHA $D_NEW" "$REG/seazn-relay-stg:$OLD $D_OLD" "$REG/seazn-relay-prod:v1.2.3 $D_PROD" > "$STATE/registry"
  echo '{"app":"seazn-club-stg","env":{"OAUTH_BASE_URL":"https://stg.example"}}' > "$STATE/config.json"
  echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"},{"name":"RELAY_IMAGE","digest":"d0","status":"Deployed"}]' > "$STATE/secrets.json"
  echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"},{"name":"RELAY_IMAGE","digest":"d1","status":"Deployed"}]' > "$STATE/secrets-after.json"
  # Newest first: a stg run whose promote step was skipped (no token then), a prod run, the stg run
  # that did promote.
  jq -n --arg s "$SKIPPED" --arg o "$OLD" '{workflow_runs: [
    {id: 7, display_title: "promote stg \($s)"}, {id: 6, display_title: "promote prod v1.0.0"},
    {id: 5, display_title: "promote stg \($o)"}]}' > "$STATE/runs.json"
  echo '{"jobs":[{"steps":[{"name":"Promote RELAY_IMAGE","conclusion":"skipped"}]}]}' > "$STATE/jobs-7.json"
  echo '{"jobs":[{"steps":[{"name":"Promote RELAY_IMAGE","conclusion":"success"}]}]}' > "$STATE/jobs-6.json"
  echo '{"jobs":[{"steps":[{"name":"Promote RELAY_IMAGE","conclusion":"success"}]}]}' > "$STATE/jobs-5.json"
}
# run_step SCRIPT VAR=VALUE...: the block under the runner's shell (bash -eo pipefail), fakes first on PATH.
run_step() {
  local script=$1; shift
  if env -i PATH="$W/bin:$PATH" HOME="$W" RUNNER_TEMP="$W/rt" GITHUB_STEP_SUMMARY="$W/summary" \
    GITHUB_OUTPUT="$W/output" GITHUB_REPOSITORY=onryde/seazn.club.compositor GITHUB_RUN_ID=999 \
    GITHUB_REF=refs/heads/main STATE="$STATE" RELAY_REGISTRY="$REG" GH_TOKEN=fake-gh \
    FLY_API_TOKEN_STG=fake-stg FLY_API_TOKEN_PROD=fake-prod EXPECT_TOKEN=fake-stg "$@" \
    bash --noprofile --norc -eo pipefail "$script" > "$W/step.out" 2>&1; then rc=0; else rc=$?; fi
}
has() { grep -qF -- "$2" "$1"; }
no_set() { ! grep -qE 'flyctl secrets (set|deploy)' "$STATE/log"; }

extract "$PROMOTE" promote promote "$W/promote.sh"
step_name=$(to_json "$root/.github/workflows/$PROMOTE" | jq -r '.jobs.promote.steps[] | select(.id == "promote") | .name')
# The previous-ref lookup matches this step by name in earlier runs: the name and the filter agree.
if [ "$step_name" = "Promote RELAY_IMAGE" ] && has "$W/promote.sh" 'select(.name == "Promote RELAY_IMAGE"'; then
  ok "promote: the step is named as the previous-ref lookup reads it"
else
  bad "promote: step name '$step_name' and the lookup's filter disagree"
fi

# P1: stg, the happy path.
state; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
set_line="flyctl secrets set RELAY_IMAGE=$REG/seazn-relay-stg:$SHA@$D_NEW -a seazn-club-stg --stage (token ok)"
if [ "$rc" -eq 0 ] && has "$STATE/log" "$set_line" \
  && [ "$(grep -nF 'flyctl secrets set' "$STATE/log" | cut -d: -f1)" -lt "$(grep -nF 'flyctl secrets deploy -a seazn-club-stg (token ok)' "$STATE/log" | cut -d: -f1)" ] \
  && ! has "$STATE/log" 'token WRONG' \
  && has "$W/summary" "| new RELAY_IMAGE | \`$REG/seazn-relay-stg:$SHA@$D_NEW\` |" \
  && has "$W/summary" "| previous RELAY_IMAGE | \`$REG/seazn-relay-stg:$OLD@$D_OLD\` (promote run 5) |" \
  && has "$W/summary" "| RELAY_IMAGE's Fly digest | d0 → d1 |" \
  && has "$W/summary" "-f env=stg -f ref=$OLD"; then
  ok "promote P1: stg sets RELAY_IMAGE by digest with stg's token, then deploys; previous = run 5 (run 7 skipped its promote)"
else
  bad "promote P1: rc $rc"; sed 's/^/    /' "$W/step.out" "$STATE/log" "$W/summary" >&2
fi

# P2: the ref was never pushed.
state; run_step "$W/promote.sh" RELAY_ENV=stg REF=4444444444444444444444444444444444444444
if [ "$rc" -ne 0 ] && has "$W/step.out" "::error::not pushed: $REG/seazn-relay-stg:4444444444444444444444444444444444444444" && no_set; then
  ok "promote P2: an unpushed ref is refused before anything is set"
else
  bad "promote P2: rc $rc"; sed 's/^/    /' "$W/step.out" "$STATE/log" >&2
fi

# P3: RELAY_IMAGE is an [env] entry: refuse and print the seazn.club diff.
state; echo '{"env":{"RELAY_IMAGE":"registry.example/seazn-relay-stg:old"}}' > "$STATE/config.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -ne 0 ] && has "$W/step.out" "is an [env] entry in seazn-club-stg's config" && no_set \
  && has "$W/summary" '`fly.stg.toml`' && has "$W/summary" "+  RELAY_IMAGE = \"$REG/seazn-relay-stg:$SHA@$D_NEW\"" \
  && has "$W/summary" '-  RELAY_IMAGE = "registry.example/seazn-relay-stg:old"'; then
  ok "promote P3: an [env] RELAY_IMAGE is refused, with the fly.stg.toml diff in the summary"
else
  bad "promote P3: rc $rc"; sed 's/^/    /' "$W/step.out" "$STATE/log" "$W/summary" >&2
fi
state; echo '{"env":{"RELAY_IMAGE":"x"}}' > "$STATE/config.json"
run_step "$W/promote.sh" RELAY_ENV=prod REF=v1.2.3 EXPECT_TOKEN=fake-prod
if [ "$rc" -ne 0 ] && has "$W/summary" '`fly.toml`' && no_set; then
  ok "promote P3b: prod's [env] diff names fly.toml"
else
  bad "promote P3b: rc $rc"; sed 's/^/    /' "$W/step.out" "$W/summary" >&2
fi

# P4: another secret already staged: deploying would ship it too.
state; echo '[{"name":"OTHER_KEY","digest":"o1","status":"Staged"},{"name":"RELAY_IMAGE","digest":"d0","status":"Deployed"}]' > "$STATE/secrets.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -ne 0 ] && has "$W/step.out" "seazn-club-stg has staged secrets (OTHER_KEY)" && no_set; then
  ok "promote P4: staged secrets refuse the promote"
else
  bad "promote P4: rc $rc"; sed 's/^/    /' "$W/step.out" "$STATE/log" >&2
fi

# P5: prod, with prod's token; its previous ref (run 6) is gone from the registry.
state; run_step "$W/promote.sh" RELAY_ENV=prod REF=v1.2.3 EXPECT_TOKEN=fake-prod
if [ "$rc" -eq 0 ] && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$REG/seazn-relay-prod:v1.2.3@$D_PROD -a seazn-club-prod --stage (token ok)" \
  && has "$STATE/log" 'flyctl secrets deploy -a seazn-club-prod (token ok)' && ! has "$STATE/log" 'seazn-club-stg' \
  && has "$W/summary" "\`$REG/seazn-relay-prod:v1.0.0\` (promote run 6; no longer in the registry)"; then
  ok "promote P5: prod promotes on seazn-club-prod with prod's token; a vanished previous ref is said"
else
  bad "promote P5: rc $rc"; sed 's/^/    /' "$W/step.out" "$STATE/log" "$W/summary" >&2
fi

# P6: inputs refused before any call.
state; run_step "$W/promote.sh" RELAY_ENV=qa REF="$SHA"
r1=$rc; o1=$(cat "$W/step.out"); l1=$(wc -c < "$STATE/log" | tr -d ' ')
state; run_step "$W/promote.sh" RELAY_ENV=stg REF='main; touch /tmp/x'
if [ "$r1" -ne 0 ] && [[ "$o1" == *"env must be stg or prod: 'qa'"* ]] && [ "$l1" -eq 0 ] \
  && [ "$rc" -ne 0 ] && has "$W/step.out" 'ref must be a 40-hex sha' && [ ! -s "$STATE/log" ]; then
  ok "promote P6: a bad env and a bad ref are refused before any flyctl, docker or gh call"
else
  bad "promote P6: rc $r1/$rc"; echo "$o1" >&2; sed 's/^/    /' "$W/step.out" >&2
fi

# P7: dispatched off main.
state; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA" GITHUB_REF=refs/heads/feat/x
if [ "$rc" -ne 0 ] && has "$W/step.out" 'promote runs from main only, not refs/heads/feat/x' && [ ! -s "$STATE/log" ]; then
  ok "promote P7: off main is refused"
else
  bad "promote P7: rc $rc"; sed 's/^/    /' "$W/step.out" >&2
fi

# P8: the run history cannot be read: the promote goes ahead and says the previous ref is unknown.
state; rm "$STATE/runs.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -eq 0 ] && has "$W/summary" "| previous RELAY_IMAGE | unknown: this workflow's runs could not be read" && has "$STATE/log" 'flyctl secrets deploy'; then
  ok "promote P8: an unreadable history is said, not guessed"
else
  bad "promote P8: rc $rc"; sed 's/^/    /' "$W/step.out" "$W/summary" >&2
fi

# P9: the first promote: no earlier run, no RELAY_IMAGE before.
state; echo '{"workflow_runs":[]}' > "$STATE/runs.json"; echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"}]' > "$STATE/secrets.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -eq 0 ] && has "$W/summary" "unset (this is seazn-club-stg's first RELAY_IMAGE)" \
  && has "$W/summary" "| RELAY_IMAGE's Fly digest | unset → d1 |" && has "$W/summary" 'no earlier promote of stg'; then
  ok "promote P9: the first promote says the value was unset"
else
  bad "promote P9: rc $rc"; sed 's/^/    /' "$W/step.out" "$W/summary" >&2
fi

# P10: no token for the env past the guard (never the other env's).
state; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA" FLY_API_TOKEN_STG=
if [ "$rc" -ne 0 ] && has "$W/step.out" 'no Fly token for stg past the guard' && [ ! -s "$STATE/log" ]; then
  ok "promote P10: stg without stg's token stops (prod's is never used)"
else
  bad "promote P10: rc $rc"; sed 's/^/    /' "$W/step.out" "$STATE/log" >&2
fi

# P11: RELAY_IMAGE absent after the deploy.
state; cp "$STATE/secrets.json" "$STATE/secrets-after.json"
jq '[.[] | select(.name != "RELAY_IMAGE")]' "$STATE/secrets.json" > "$STATE/secrets-after.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -ne 0 ] && has "$W/step.out" 'RELAY_IMAGE is not set on seazn-club-stg after the deploy'; then
  ok "promote P11: a deploy that leaves RELAY_IMAGE unset is a red"
else
  bad "promote P11: rc $rc"; sed 's/^/    /' "$W/step.out" >&2
fi

# The token guards: promote's (per env, no fallback) and deploy-stg's.
extract "$PROMOTE" promote token "$W/promote-token.sh"
guard() { # guard SCRIPT EXPECT_ENABLED EXPECT_WARNING VAR=VALUE...
  local script=$1 want=$2 warn=$3; shift 3
  state; run_step "$script" "$@"
  if [ "$rc" -eq 0 ] && has "$W/output" "enabled=$want" \
    && { [ -z "$warn" ] && ! has "$W/step.out" '::warning::' || { [ -n "$warn" ] && has "$W/step.out" "$warn"; }; }; then
    ok "guard $(basename "$script") $*: enabled=$want"
  else
    bad "guard $(basename "$script") $*: rc $rc"; sed 's/^/    /' "$W/step.out" "$W/output" >&2
  fi
}
guard "$W/promote-token.sh" true '' RELAY_ENV=stg
guard "$W/promote-token.sh" false '::warning::FLY_API_TOKEN_STG unset — promote skipped' RELAY_ENV=stg FLY_API_TOKEN_STG=
guard "$W/promote-token.sh" false '::warning::FLY_API_TOKEN_PROD unset — promote skipped' RELAY_ENV=prod FLY_API_TOKEN_PROD=
guard "$W/promote-token.sh" true '' RELAY_ENV=prod FLY_API_TOKEN_STG=
extract "$STG" gate token "$W/stg-token.sh"
guard "$W/stg-token.sh" false '::warning::FLY_API_TOKEN_STG unset — push skipped' FLY_API_TOKEN=
guard "$W/stg-token.sh" true '' FLY_API_TOKEN=fake-stg
extract "$PROD" gate token "$W/prod-token.sh"
guard "$W/prod-token.sh" false '::warning::FLY_API_TOKEN_PROD unset — push skipped' FLY_API_TOKEN=

# deploy-stg's "Already pushed?": a pushed sha is never pushed again.
extract "$STG" push exists "$W/exists.sh"
state; run_step "$W/exists.sh" FLY_API_TOKEN=fake-stg RELAY_APP=seazn-relay-stg TAG="$SHA"
r1=$rc; out1=$(cat "$W/output")
state; run_step "$W/exists.sh" FLY_API_TOKEN=fake-stg RELAY_APP=seazn-relay-stg TAG=5555555555555555555555555555555555555555
if [ "$r1" -eq 0 ] && [[ "$out1" == *"exists=true"* && "$out1" == *"digest=$D_NEW"* ]] && [ "$rc" -eq 0 ] && has "$W/output" 'exists=false'; then
  ok "deploy-stg: a pushed sha reads exists=true with its digest; a new one exists=false"
else
  bad "deploy-stg Already pushed?: rc $r1/$rc"; echo "$out1" >&2; sed 's/^/    /' "$W/output" >&2
fi

# deploy-prod's tag check, on a scratch repository: main a-b, a side commit c off a.
extract "$PROD" gate tag "$W/tag.sh"
g="$W/git"; rm -rf "$g"; mkdir -p "$g"
git -C "$g" init -q -b main
git -C "$g" -c user.email=t@example -c user.name=t commit -q --allow-empty -m a
a=$(git -C "$g" rev-parse HEAD)
git -C "$g" -c user.email=t@example -c user.name=t commit -q --allow-empty -m b
b=$(git -C "$g" rev-parse HEAD)
git -C "$g" checkout -q -b side "$a"
git -C "$g" -c user.email=t@example -c user.name=t commit -q --allow-empty -m c
c=$(git -C "$g" rev-parse HEAD)
git -C "$g" update-ref refs/remotes/origin/main "$b"
tag_case() { # tag_case TAG SHA WANT_RC TEXT
  state
  if (cd "$g" && env -i PATH="$PATH" HOME="$W" TAG="$1" GITHUB_SHA="$2" bash --noprofile --norc -eo pipefail "$W/tag.sh") > "$W/step.out" 2>&1; then rc=0; else rc=$?; fi
  if { { [ "$3" = 0 ] && [ "$rc" -eq 0 ]; } || { [ "$3" != 0 ] && [ "$rc" -ne 0 ]; }; } && has "$W/step.out" "$4"; then
    ok "deploy-prod tag check: $1 at ${2:0:7}: $4"
  else
    bad "deploy-prod tag check: $1 at ${2:0:7}: rc $rc"; sed 's/^/    /' "$W/step.out" >&2
  fi
}
tag_case v1.2.3 "$b" 0 "v1.2.3 ($b) is on main"
tag_case v1.2.3 "$a" 0 "is on main"
tag_case v1.2.3 "$c" 1 "::error::v1.2.3 ($c) is not on main: prod images come from main only"
tag_case v1.2.3-rc1 "$b" 1 "::error::'v1.2.3-rc1' is not a release tag vX.Y.Z"

echo "checked: $checked, failed: $failures"
if [ "$checked" -eq 0 ]; then
  echo "deploy_checks_test: FAIL: checked nothing" >&2
  exit 1
fi
[ "$failures" -eq 0 ]
