#!/usr/bin/env bash
# shellcheck disable=SC2016  # the mutants and jq programs hold literal $ text, never meant to expand
# The deploy checks' own test (C16, docs/tasks/C16-deploy.md). check.yml runs it after
# scripts/check-workflows.sh. Never touches the repo's files: every mutant edits a scratch copy.
#
# 1. scripts/check-workflows.sh: green on the repo's workflows; red on an empty set; red on each
#    mutant, by its own reason (each red prints the line it matched, after `::`). The brief's six:
#    (1) the ::warning:: guard removed from deploy-stg.yml, (2) environment: removed from
#    promote.yml, (4) the CEF bump gate step deleted from promote.yml, (5) a seazn-relay-cache
#    cache-from in deploy-stg.yml, (6) target: relay-test-hooks in deploy-stg.yml; e1 to e17 for the
#    first round's rules; and one per rule or refusal the PR #85 review added (m1*, m2*, m3*, i1*,
#    i2*).
# 2. scripts/check-cef-bump.sh: green on the repo; red on the brief's mutant (3), GSTCEF_COMMIT
#    changed without the record, on each malformed record, and on a record whose finding is no
#    Step 0 run on its commit (m-8); green again once the record and its finding follow the bump.
# 3. The workflows' own scripts, extracted as written and run under fake flyctl, docker and gh, in a
#    scratch git repository (main: z, a, b, c, h; side: d): promote.yml's "Promote RELAY_IMAGE" step
#    (inputs, the environment's rules, provenance, the registry, staged secrets, the previous ref,
#    the deploy and its proof, and a failed promote followed by its rollback) and its token guard;
#    deploy-stg.yml's and deploy-prod.yml's guards, provenance steps and "Already pushed?".
#
# Needs bash, ruby (YAML), jq, python3, git, and the pinned actionlint and shellcheck on PATH
# (scripts/check-workflows.sh --install DIR). Prints `checked: N` and fails on 0.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
W=$(mktemp -d)
# A run that stops before its tally is a red: bash 3.2 exits 0 after a `set -u` abort under an EXIT
# trap, which would pass a half-run test.
trap 'rm -rf "$W"; if [ "${finished:-0}" != 1 ]; then echo "deploy_checks_test: FAIL: stopped before its tally" >&2; exit 1; fi' EXIT
STG=deploy-stg.yml PROD=deploy-prod.yml PROMOTE=promote.yml

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
# edit_all FILE OLD NEW: replace every OLD, of which there must be at least one.
edit_all() {
  python3 -I -c '
import sys
p, old, new = sys.argv[1:4]
s = open(p).read()
if old not in s:
    sys.exit(f"edit_all: {p}: no match for {old!r}")
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
    ok "check-workflows red: $1 :: $(grep -m 1 -F -- "$2" "$W/check.out" | sed 's/^check-workflows: FAIL: //' | cut -c 1-240)"
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
if [ "$rc" -eq 0 ] && grep -qF "checked $n_repo workflows" "$W/check.out" && grep -qF 'deploy workflows: 3 ' "$W/check.out" \
  && grep -qF 'promote refusals before the first flyctl: 5 of 5' "$W/check.out" \
  && grep -qF 'provenance steps before a push guard: 2' "$W/check.out"; then
  ok "check-workflows green on the repo ($n_repo workflows, 3 deploy, 5 promote refusals, 2 provenance steps)"
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

STG_GATE='      - name: CEF bump gate (Owed 22)
        run: scripts/check-cef-bump.sh
      # CEF'
PUSH_INSPECT='          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
WEB_FLY='          web_fly() { FLY_API_TOKEN=$web_token flyctl "$@"; }
'

# (1) The ::warning:: guard removed from deploy-stg.yml.
fresh; edit "$W/wf/$STG" '            echo "::warning::FLY_API_TOKEN_RELAY_STG_PUSH unset — push skipped"'$'\n' ''
expect_red "(1) deploy-stg's ::warning:: guard removed" 'deploy-stg.yml FLY_API_TOKEN_RELAY_STG_PUSH: no step warns'

# (2) environment: removed from promote.yml.
fresh; edit "$W/wf/$PROMOTE" '    environment: relay-${{ inputs.env }}'$'\n' ''
expect_red "(2) promote's environment removed" 'job promote holds a Fly token and declares environment '"''"', not exactly relay-${{ inputs.env }}'

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
expect_red "e1 an ungated token step" 'push.Already pushed? holds FLY_API_TOKEN_RELAY_STG_PUSH but runs whether or not the guard found it'

# e2. The token echoed to the log.
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          flyctl auth docker
          echo "token: $FLY_API_TOKEN"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "e2 the token echoed" 'prints a secret: echo "token: $FLY_API_TOKEN"'

# e3. The token written to $GITHUB_OUTPUT.
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          flyctl auth docker
          printf "t=%s\n" "$FLY_API_TOKEN" >> "$GITHUB_OUTPUT"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "e3 the token into GITHUB_OUTPUT" 'prints a secret: printf'

# e3+ The rule's one exception, held to its shape: into a file under RUNNER_TEMP is not an echo.
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          flyctl auth docker
          ( umask 077; printf "%s\n" "$FLY_API_TOKEN" > "$RUNNER_TEMP/scan-patterns" )
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_green "e3+ the token into a RUNNER_TEMP pattern file (soak.yml's leak gate shape)"

# e4. A secret interpolated into a script.
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          flyctl auth docker
          test -n "${{ secrets.FLY_API_TOKEN_RELAY_STG_PUSH }}"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "e4 a secret interpolated into run" 'interpolates a secret into its script'

# e5. xtrace in a step that holds the token.
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          set -x
          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "e5 xtrace with the token" 'holds a secret under xtrace'

# e6. A target other than relay.
fresh; edit "$W/wf/$STG" '          target: relay' '          target: cef-test'
expect_red "e6 deploy-stg builds cef-test" "deploy-stg.yml builds target 'cef-test'"

# e7. The registry hardcoded.
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          flyctl auth docker
          ref="registry.fly.io/$RELAY_APP:$TAG"
          # Only'
expect_red "e7 the registry hardcoded" 'deploy-stg.yml hardcodes the registry'

# e8. Another token as a fallback (`a && X || Y`).
fresh; edit "$W/wf/$PROMOTE" "          FLY_API_TOKEN_WEB_STG: \${{ inputs.env == 'stg' && secrets.FLY_API_TOKEN_WEB_STG || '' }}
          FLY_API_TOKEN_WEB_PROD: \${{ inputs.env == 'prod' && secrets.FLY_API_TOKEN_WEB_PROD || '' }}
          GH_TOKEN" "          FLY_API_TOKEN_WEB_STG: \${{ inputs.env == 'prod' && secrets.FLY_API_TOKEN_WEB_PROD || secrets.FLY_API_TOKEN_WEB_STG }}
          GH_TOKEN"
expect_red "e8 a token fallback" 'names two Fly tokens in one expression'

# e9. A guard that reads the wrong env's gate: prod's web token behind inputs.env == 'stg'.
fresh; edit "$W/wf/$PROMOTE" "          FLY_API_TOKEN_WEB_PROD: \${{ inputs.env == 'prod' && secrets.FLY_API_TOKEN_WEB_PROD || '' }}
          RELAY_ENV: \${{ inputs.env }}
        run: |
          skipped" "          FLY_API_TOKEN_WEB_PROD: \${{ inputs.env == 'stg' && secrets.FLY_API_TOKEN_WEB_PROD || '' }}
          RELAY_ENV: \${{ inputs.env }}
        run: |
          skipped"
expect_red "e9 a guard on the wrong env's gate" 'promote.yml FLY_API_TOKEN_WEB_PROD: no step warns'

# e10. A job-level env holding the token.
fresh; edit "$W/wf/$STG" '      TAG: ${{ github.sha }}' '      TAG: ${{ github.sha }}
      FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG_PUSH }}'
expect_red "e10 a job env holds the token" 'job push holds FLY_API_TOKEN_RELAY_STG_PUSH in its env'

# e11. A workflow-level env holding a secret.
fresh; edit "$W/wf/$STG" '  RELAY_APP: seazn-relay-stg' '  RELAY_APP: seazn-relay-stg
  ANY_SECRET: ${{ secrets.SOMETHING }}'
expect_red "e11 a workflow env holds a secret" 'the workflow-level env holds a secret'

# e12. The token used before the checkout and its gate.
fresh; edit "$W/wf/$STG" '      # Actions pinned by commit: this job holds a Fly token (image.yml'"'"'s cache job, the PR #47 review'"'"'s m7).
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0
        with: { persist-credentials: false }
'"$STG_GATE" '      - name: Early
        env:
          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG_PUSH }}
        run: flyctl version
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0
        with: { persist-credentials: false }
'"$STG_GATE"
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
expect_red "e17 a job gate without needs" 'holds FLY_API_TOKEN_RELAY_STG_PUSH but runs whether or not the guard found it'

# m1a. The gate step under continue-on-error (promote).
fresh; edit "$W/wf/$PROMOTE" '        run: scripts/check-cef-bump.sh
      # Each env' '        run: scripts/check-cef-bump.sh
        continue-on-error: true
      # Each env'
expect_red "m1a promote's gate under continue-on-error" 'job promote: the CEF bump gate step (step 1) carries continue-on-error'

# m1b. The gate step behind a dead `if` (deploy-stg's push job).
fresh; edit "$W/wf/$STG" "$STG_GATE" "      - name: CEF bump gate (Owed 22)
        if: github.event_name == 'never'
        run: scripts/check-cef-bump.sh
      # CEF"
expect_red "m1b deploy-stg's gate behind a dead if" 'job push: the CEF bump gate step (step 1) carries if'

# m1c. The gate neutered by `|| true` (deploy-prod's push job).
fresh; edit "$W/wf/$PROD" '        run: scripts/check-cef-bump.sh
      # CEF' '        run: scripts/check-cef-bump.sh || true
      # CEF'
expect_red "m1c deploy-prod's gate || true" 'deploy-prod.yml (CEF bump gate, Owed 22): job push: the step after its checkout (step 1) is not exactly'

# m1d. deploy-prod's tag check under continue-on-error.
fresh; edit "$W/wf/$PROD" '        id: provenance
' '        id: provenance
        continue-on-error: true
'
expect_red "m1d deploy-prod's tag check under continue-on-error" 'deploy-prod.yml (provenance): job gate: the provenance step carries continue-on-error'

# m1e. deploy-stg's main check deleted (I-2).
fresh; edit "$W/wf/$STG" '      - name: The ref is main
        id: provenance
        run: |
          if [ "$GITHUB_REF" != refs/heads/main ]; then
            echo "::error::deploy-stg pushes main only, not $GITHUB_REF: nothing pushed"
            exit 1
          fi
          echo "$GITHUB_REF ($GITHUB_SHA)"
' ''
expect_red "m1e deploy-stg's main check deleted" 'deploy-stg.yml (provenance): job gate has no step with id provenance'

# m1f. deploy-stg's main check that refuses nothing (exit 0).
fresh; edit "$W/wf/$STG" '            echo "::error::deploy-stg pushes main only, not $GITHUB_REF: nothing pushed"
            exit 1' '            echo "::error::deploy-stg pushes main only, not $GITHUB_REF: nothing pushed"
            exit 0'
expect_red "m1f deploy-stg's main check exits 0" "deploy-stg.yml (provenance): job gate: the provenance step does not check 'refs/heads/main' and exit 1"

# m1g. A token step that runs past a failed gate (always()).
fresh; edit "$W/wf/$PROMOTE" "        id: promote
        if: steps.token.outputs.enabled == 'true'" "        id: promote
        if: always() && steps.token.outputs.enabled == 'true'"
expect_red "m1g promote's step under always()" 'job promote: Promote RELAY_IMAGE runs past a failed step'

# m1h. A token job under continue-on-error.
fresh; edit "$W/wf/$STG" "    needs: gate
    if: needs.gate.outputs.enabled == 'true'" "    needs: gate
    if: needs.gate.outputs.enabled == 'true'
    continue-on-error: true"
expect_red "m1h deploy-stg's push job under continue-on-error" 'job push holds a Fly token and sets continue-on-error'

# m1i. deploy-stg's main check after the token guard.
fresh; python3 -I -c '
import sys
p = sys.argv[1]
s = open(p).read()
a = s.index("      - name: The ref is main\n")
b = s.index("      - name: The Fly push token\n")
c = s.index("\n  push:\n")
open(p, "w").write(s[:a] + s[b:c] + "\n" + s[a:b].rstrip("\n") + s[c:])' "$W/wf/$STG"
expect_red "m1i deploy-stg's main check after the guard" 'deploy-stg.yml (provenance): job gate: the provenance step comes after the token guard'

# m1j. A token step whose `if` contains the guard's condition but is not it (rule 2's exact
# comparison; m1g's always() is rule 5's, so each rule has a mutant only it catches).
fresh; edit "$W/wf/$PROMOTE" "        id: promote
        if: steps.token.outputs.enabled == 'true'" "        id: promote
        if: steps.token.outputs.enabled == 'true' || github.event_name == 'workflow_dispatch'"
expect_red "m1j promote's step behind a widened if" 'promote.Promote RELAY_IMAGE holds FLY_API_TOKEN_RELAY_PROD_PUSH but runs whether or not the guard found it'

# m1k. continue-on-error on a token step that is neither a gate nor a provenance step.
fresh; edit "$W/wf/$STG" '      - name: Already pushed?
        id: exists
' '      - name: Already pushed?
        id: exists
        continue-on-error: true
'
expect_red "m1k deploy-stg's Already pushed? under continue-on-error" 'job push: Already pushed? sets continue-on-error, so its failure would not stop the job'

# m2a. An environment nobody protects.
fresh; edit "$W/wf/$PROMOTE" '    environment: relay-${{ inputs.env }}' '    environment: relay-scratch'
expect_red "m2a promote behind relay-scratch" "declares environment 'relay-scratch', not exactly relay-\${{ inputs.env }}"

# m2b. inputs.env grows a third choice (relay-qa would be unprotected).
fresh; edit "$W/wf/$PROMOTE" '        options: [stg, prod]' '        options: [stg, prod, qa]'
expect_red "m2b inputs.env takes qa" 'inputs.env is not the choice [stg, prod]'

# m3a. The alias FLY_API_TOKEN echoed (the review's MA).
fresh; edit "$W/wf/$PROMOTE" "$WEB_FLY" "$WEB_FLY"'          echo "t=$FLY_API_TOKEN"
'
expect_red "m3a FLY_API_TOKEN echoed" 'prints a secret: echo "t=$FLY_API_TOKEN"'

# m3h. FLY_API_TOKEN echoed in a step that holds no secret by env (an earlier step could have
# exported it): the name alone is a secret.
fresh; edit "$W/wf/$STG" '          docker tag "$LOCAL_IMAGE" "$ref"' '          echo "t=$FLY_API_TOKEN"
          docker tag "$LOCAL_IMAGE" "$ref"'
expect_red "m3h FLY_API_TOKEN echoed by a step with no secret env" 'push.Push prints a secret: echo "t=$FLY_API_TOKEN"'

# m3b. A laundered alias echoed.
fresh; edit "$W/wf/$PROMOTE" "$WEB_FLY" "$WEB_FLY"'          echo "w=$web_token"
'
expect_red "m3b an alias of the web token echoed" 'prints a secret: echo "w=$web_token"'

# m3c. The environment dumped (env | sort, the review's MF).
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          env | sort
          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "m3c env | sort with the token" 'dumps the environment while it holds a secret: env | sort'

# m3d. printenv.
fresh; edit "$W/wf/$PROMOTE" "$WEB_FLY" "$WEB_FLY"'          printenv | wc -l
'
expect_red "m3d printenv with the tokens" 'dumps the environment while it holds a secret: printenv | wc -l'

# m3e. declare -p.
fresh; edit "$W/wf/$PROMOTE" "$WEB_FLY" "$WEB_FLY"'          declare -p > "$RUNNER_TEMP/vars"
'
expect_red "m3e declare -p with the tokens" 'dumps the environment while it holds a secret: declare -p'

# m3f. The token in argv: -t (the review's ME).
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          flyctl auth docker -t "$FLY_API_TOKEN"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "m3f flyctl -t" 'hands flyctl a token in argv: flyctl auth docker -t'

# m3g. The token in argv: --access-token.
fresh; edit "$W/wf/$PROMOTE" '          FLY_API_TOKEN=$push_token flyctl auth docker' '          flyctl auth docker --access-token "$push_token"'
expect_red "m3g flyctl --access-token" 'hands flyctl a token in argv: flyctl auth docker --access-token'

# i1a. A web token outside promote.yml (deploy-stg reads seazn-club-stg's token).
fresh; edit "$W/wf/$STG" '          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG_PUSH }}
        run: |
          flyctl auth docker' '          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG_PUSH }}
          WEB: ${{ secrets.FLY_API_TOKEN_WEB_STG }}
        run: |
          flyctl auth docker'
expect_red "i1a deploy-stg names a web token" 'deploy-stg.yml names a web token'

# i1b. A token of unknown reach (the first round's FLY_API_TOKEN_STG) in a deploy workflow.
fresh; edit_all "$W/wf/$STG" 'FLY_API_TOKEN_RELAY_STG_PUSH' 'FLY_API_TOKEN_STG'
expect_red "i1b deploy-stg names FLY_API_TOKEN_STG" 'deploy-stg.yml names FLY_API_TOKEN_STG: a deploy workflow names only'

# i1c. A promote job that holds a web token outside the environment.
fresh; cat >> "$W/wf/$PROMOTE" << 'YML'
  peek:
    runs-on: ubuntu-24.04
    steps:
      - name: Peek
        env:
          WEB: ${{ inputs.env == 'stg' && secrets.FLY_API_TOKEN_WEB_STG || '' }}
        run: test -n "$WEB"
YML
expect_red "i1c a promote job without the environment holds a web token" 'job peek holds a web token outside relay-${{ inputs.env }}'

# i1d. A secret named by expression, which no rule can read.
fresh; edit "$W/wf/$STG" '          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG_PUSH }}
        run: |
          flyctl auth docker' "          FLY_API_TOKEN: \${{ secrets['FLY_API_TOKEN_RELAY_STG_PUSH'] }}
        run: |
          flyctl auth docker"
expect_red "i1d secrets[...] in deploy-stg" 'deploy-stg.yml names a secret by expression'

# i1e. promote's environment check deleted (the approval would rest on a comment).
fresh; python3 -I -c '
import sys
p = sys.argv[1]
s = open(p).read()
end = "not main alone: the owner sets it to main alone\"\n"
a = s.index("          envname=")
b = s.index(end) + len(end)
open(p, "w").write(s[:a] + s[b:])' "$W/wf/$PROMOTE"
expect_red "i1e promote's environment check deleted" "the promote step does not read 'environments/' before its first flyctl"

# i2a. promote's main ancestry check deleted (I-2).
fresh; edit "$W/wf/$PROMOTE" '          git merge-base --is-ancestor "$commit" origin/main || fail "$tag ($commit) is not on main: images are promoted from main only"
' ''
expect_red "i2a promote's ancestry check deleted" "the promote step does not read 'merge-base --is-ancestor' before its first flyctl"

# i2b. promote's CEF bump gate on the promoted tree deleted (I-2).
fresh; edit "$W/wf/$PROMOTE" '          scripts/check-cef-bump.sh "$tree" || fail "$tag ($commit) fails the CEF bump gate (Owed 22) in its own tree"
' ''
expect_red "i2b promote's CEF gate on the ref's tree deleted" "the promote step does not read 'scripts/check-cef-bump.sh' before its first flyctl"

# i2c. promote's ancestry check moved after its first flyctl (a refusal after a Fly call).
fresh; edit "$W/wf/$PROMOTE" '          git merge-base --is-ancestor "$commit" origin/main || fail "$tag ($commit) is not on main: images are promoted from main only"
' ''
edit "$W/wf/$PROMOTE" '          FLY_API_TOKEN=$push_token flyctl auth docker
' '          FLY_API_TOKEN=$push_token flyctl auth docker
          git merge-base --is-ancestor "$commit" origin/main || fail "$tag ($commit) is not on main: images are promoted from main only"
'
expect_red "i2c promote's ancestry check after its first flyctl" "the promote step does not read 'merge-base --is-ancestor' before its first flyctl"

# ---------------------------------------------------------------- 2. check-cef-bump.sh
pin=$(tr -d '[:space:]' < "$root/docker/GSTCEF_COMMIT")
cef_root() {
  rm -rf "$W/cef"; mkdir -p "$W/cef/docker"
  cp "$root/docker/GSTCEF_COMMIT" "$root/docker/CEF_STEP0_PASS" "$W/cef/docker/"
  cp "$root/_FINDINGS.md" "$W/cef/"
}
# add_finding F-ID SHA: a Step 0 finding that names SHA, appended to the scratch _FINDINGS.md.
add_finding() {
  printf '\n## %s: CEF loopback phase channel on the bump (C11 Step 0, 2026-11-01). OUTCOME (a)\n\n- BUILDINFO: `gstcefsrc_commit=%s`\n' "$1" "$2" >> "$W/cef/_FINDINGS.md"
}
gate() {
  if "$root/scripts/check-cef-bump.sh" "$W/cef" > "$W/gate.out" 2>&1; then rc=0; else rc=$?; fi
}
gate_red() {
  gate
  if [ "$rc" -ne 0 ] && grep -F 'cef bump gate: FAIL: ' "$W/gate.out" | grep -qF -- "$2" \
    && grep -qxF '::error::CEF bump without a passing C11 Step 0 re-run (compositor docs/_INDEX.md Owed 22)' "$W/gate.out"; then
    ok "cef bump gate red: $1 :: $(grep -m 1 -F -- "$2" "$W/gate.out" | sed 's/^cef bump gate: FAIL: //' | cut -c 1-200)"
  else
    bad "cef bump gate: $1: rc $rc, expected a red saying: $2"; sed 's/^/    /' "$W/gate.out" >&2
  fi
}
gate_green() { # gate_green NAME SHA F-ID
  gate
  if [ "$rc" -eq 0 ] && grep -qxF "cef bump gate: GSTCEF_COMMIT=$2 step0=$2 $3 (a C11 Step 0 finding naming $2)" "$W/gate.out"; then
    ok "cef bump gate green: $1"
  else
    bad "cef bump gate: $1: rc $rc, expected green at $2 $3"; sed 's/^/    /' "$W/gate.out" >&2
  fi
}
bump=0123456789abcdef0123456789abcdef01234567

# The repo's own record: the amendment's seed (the pinned commit, F-009).
if "$root/scripts/check-cef-bump.sh" > "$W/gate.out" 2>&1 \
  && grep -qxF "cef bump gate: GSTCEF_COMMIT=$pin step0=$pin F-009 (a C11 Step 0 finding naming $pin)" "$W/gate.out"; then
  ok "cef bump gate green on the repo (seeded $pin F-009)"
else
  bad "cef bump gate on the repo"; sed 's/^/    /' "$W/gate.out" >&2
fi

cef_root; gate_green "a scratch copy" "$pin" F-009
# (3) GSTCEF_COMMIT changed without touching the record.
cef_root; echo "$bump" > "$W/cef/docker/GSTCEF_COMMIT"
gate_red "(3) a bump with no Step 0 record" "docker/GSTCEF_COMMIT=$bump, but C11 Step 0 last passed on $pin (F-009)"
cef_root; add_finding F-099 "$bump"; printf '%s\nF-099\n' "$bump" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "the record moved without the pin" "docker/GSTCEF_COMMIT=$pin, but C11 Step 0 last passed on $bump (F-099)"
cef_root; printf '%s\nF-999\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "an F-id with no finding" "_FINDINGS.md has no '## F-999' heading"
# F-006 exists only as a `###` heading: a `## F-0xx` heading is the bar.
cef_root; printf '%s\nF-006\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "an F-id with only a ### heading" "_FINDINGS.md has no '## F-006' heading"
# m-8: F-004 (the YouTube audio finding) is no Step 0 run.
cef_root; printf '%s\nF-004\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "(m-8) a finding that is no Step 0 run" "_FINDINGS.md's F-004 heading does not say C11 Step 0"
# m-8: a finding that names the commit but is no Step 0 run (the heading rule alone catches it).
cef_root; printf '\n## F-098: CEF element facts on the pin (C12, 2026-11-01)\n\n- `gstcefsrc_commit=%s`\n' "$pin" >> "$W/cef/_FINDINGS.md"
printf '%s\nF-098\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "(m-8) a finding naming the commit that is no Step 0 run" "_FINDINGS.md's F-098 heading does not say C11 Step 0"
# m-8: a bump recorded with the old run's F-id: F-009 ran on the old commit.
cef_root; echo "$bump" > "$W/cef/docker/GSTCEF_COMMIT"; printf '%s\nF-009\n' "$bump" > "$W/cef/docker/CEF_STEP0_PASS"
gate_red "(m-8) a bump recorded with an old run's F-id" "_FINDINGS.md's F-009 does not name $bump"
# m-8: the commit only in the NEXT finding's section does not count.
cef_root; echo "$bump" > "$W/cef/docker/GSTCEF_COMMIT"; printf '%s\nF-099\n' "$bump" > "$W/cef/docker/CEF_STEP0_PASS"
add_finding F-099 "$pin"; add_finding F-100 "$bump"
gate_red "(m-8) the commit named only in the next section" "_FINDINGS.md's F-099 does not name $bump"
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
# The procedure's positive pair: the bump, re-run, found and recorded, passes.
cef_root; echo "$bump" > "$W/cef/docker/GSTCEF_COMMIT"; printf '%s\nF-099\n' "$bump" > "$W/cef/docker/CEF_STEP0_PASS"
add_finding F-099 "$bump"
gate_green "a bump with its record and its finding" "$bump" F-099
cef_root; printf '%s\r\nF-009\r\n' "$pin" > "$W/cef/docker/CEF_STEP0_PASS"
gate_green "a CRLF record" "$pin" F-009

# ---------------------------------------------------------------- 3. the workflows' scripts
to_json() { ruby -ryaml -rjson -e 'puts JSON.generate(YAML.load(File.read(ARGV[0])))' "$1"; }
# extract FILE JOB STEP_ID OUT: the step's run block, as the runner writes it.
extract() {
  to_json "$root/.github/workflows/$1" | jq -er --arg j "$2" --arg s "$3" '.jobs[$j].steps[] | select(.id == $s) | .run' > "$4"
  if grep -qF '${{' "$4"; then bad "$1 $2.$3 holds an expression: inputs must reach it by env"; fi
}
mkdir -p "$W/bin" "$W/rt"

# The scratch repository the promote and tag scripts run in. main: z (no CEF_STEP0_PASS yet), a,
# b (GSTCEF_COMMIT bumped with no record: fails the gate), c, h (HEAD); side: d off h.
# Tags: v1.0.0 = a, v1.1.0 = b, v1.2.3 = c, v1.2.4 = d.
g="$W/git"
mkdir -p "$g/docker" "$g/scripts"
git -C "$g" init -q -b main
gc() { git -C "$g" add -A && git -C "$g" -c user.email=t@example -c user.name=t commit -q --allow-empty -m "$1" && git -C "$g" rev-parse HEAD; }
cp "$root/scripts/check-cef-bump.sh" "$g/scripts/"; cp "$root/_FINDINGS.md" "$g/"; cp "$root/docker/GSTCEF_COMMIT" "$g/docker/"
Z=$(gc z)
cp "$root/docker/CEF_STEP0_PASS" "$g/docker/"; A=$(gc a)
echo "$bump" > "$g/docker/GSTCEF_COMMIT"; B=$(gc b)
cp "$root/docker/GSTCEF_COMMIT" "$g/docker/"; C=$(gc c)
H=$(gc h)
git -C "$g" checkout -q -b side; D=$(gc d); git -C "$g" checkout -q main
git -C "$g" tag v1.0.0 "$A"; git -C "$g" tag v1.1.0 "$B"; git -C "$g" tag v1.2.3 "$C"; git -C "$g" tag v1.2.4 "$D"
git -C "$g" update-ref refs/remotes/origin/main "$H"

# Fakes. State lives in $STATE; every call is logged to $STATE/log. flyctl's secret list is live:
# `secrets set --stage` stages a value (its digest: the value's cksum), `secrets deploy` deploys
# every staged one, unless a flag file says otherwise.
cat > "$W/bin/flyctl" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${FLY_API_TOKEN:-}" in
  "${EXPECT_PUSH:-unset}") tok=push ;;
  "${EXPECT_WEB:-unset}") tok=web ;;
  *) tok=WRONG ;;
esac
echo "flyctl $* (token $tok)" >> "$STATE/log"
s="$STATE/secrets.json"
put() { jq "$@" "$s" > "$s.tmp" && mv "$s.tmp" "$s"; }
case "$1 $2" in
  "auth docker") ;;
  "config show") cat "$STATE/config.json" ;;
  "secrets list") cat "$s" ;;
  "secrets set")
    name=${3%%=*} value=${3#*=}
    d=$(printf '%s' "$value" | cksum | cut -d' ' -f1)
    put --arg n "$name" --arg d "$d" 'if any(.[]; .name == $n) then map(if .name == $n then .digest = $d | .status = "Staged" else . end)
      else . + [{name: $n, digest: $d, status: "Staged"}] end' ;;
  "secrets deploy")
    if [ -f "$STATE/deploy-fails" ]; then rm "$STATE/deploy-fails"; echo "Error: release failed (fake)" >&2; exit 1; fi
    if [ -f "$STATE/deploy-leaves-staged" ]; then exit 0; fi
    if [ -f "$STATE/deploy-drops-image" ]; then put 'map(select(.name != "RELAY_IMAGE"))'; exit 0; fi
    if [ -f "$STATE/deploy-moves-digest" ]; then put 'map(if .name == "RELAY_IMAGE" then .digest = "999" | .status = "Deployed" else . end)'; exit 0; fi
    put 'map(.status = "Deployed")' ;;
  *) echo "fake flyctl: unexpected: $*" >&2; exit 2 ;;
esac
SH
cat > "$W/bin/docker" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "docker $*" >> "$STATE/log"
[ "$1 $2 $3" = "buildx imagetools inspect" ] || { echo "fake docker: unexpected: $*" >&2; exit 2; }
if [ -f "$STATE/registry-down" ]; then echo "ERROR: failed to authorize: 401 Unauthorized (fake)" >&2; exit 1; fi
d=$(awk -v r="$4" '$1 == r {print $2}' "$STATE/registry")
[ -n "$d" ] || { echo "ERROR: $4: not found" >&2; exit 1; }
echo "$d"
SH
cat > "$W/bin/gh" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "gh $*" >> "$STATE/log"
[ "$1" = api ] || { echo "fake gh: unexpected: $*" >&2; exit 2; }
shift
path='' expr=''
while [ $# -gt 0 ]; do
  case "$1" in
    --paginate) ;;
    --jq) expr=$2; shift ;;
    -*) echo "fake gh: unexpected flag $1" >&2; exit 2 ;;
    *) path=$1 ;;
  esac
  shift
done
case "$path" in
  */actions/workflows/promote.yml/runs\?*) f="$STATE/runs.json" ;;
  */actions/runs/*/jobs) id=${path%/jobs}; f="$STATE/jobs-${id##*/}.json" ;;
  */environments/*/deployment-branch-policies) e=${path%/deployment-branch-policies}; f="$STATE/policies-${e##*/}.json" ;;
  */environments/*) f="$STATE/env-${path##*/}.json" ;;
  *) echo "fake gh: no route $path" >&2; exit 2 ;;
esac
[ -f "$f" ] || { echo "gh: Not Found (HTTP 404)" >&2; exit 1; }
if [ -n "$expr" ]; then jq -r "$expr" "$f"; else cat "$f"; fi
SH
chmod +x "$W/bin/flyctl" "$W/bin/docker" "$W/bin/gh"

SHA=$C OLD=$A UNPUSHED=$H SIDE=$D BAD=$B PRE=$Z
SKIPPED=3333333333333333333333333333333333333333 SPOOF=5555555555555555555555555555555555555555
D_NEW=sha256:$(printf 'a%.0s' $(seq 64))
D_OLD=sha256:$(printf 'b%.0s' $(seq 64))
D_PROD=sha256:$(printf 'c%.0s' $(seq 64))
D_ANY=sha256:$(printf 'e%.0s' $(seq 64))
REG=registry.example
digest_of() { printf '%s' "$1" | cksum | cut -d' ' -f1; }
state() {
  STATE="$W/state"; rm -rf "$STATE"; mkdir -p "$STATE"; : > "$STATE/log"; : > "$W/summary"; : > "$W/output"
  export STATE
  printf '%s\n' "$REG/seazn-relay-stg:$SHA $D_NEW" "$REG/seazn-relay-stg:$OLD $D_OLD" "$REG/seazn-relay-stg:$SIDE $D_ANY" \
    "$REG/seazn-relay-stg:$BAD $D_ANY" "$REG/seazn-relay-stg:$PRE $D_ANY" \
    "$REG/seazn-relay-prod:v1.2.3 $D_PROD" "$REG/seazn-relay-prod:v1.2.4 $D_ANY" "$REG/seazn-relay-prod:v1.1.0 $D_ANY" > "$STATE/registry"
  echo '{"app":"seazn-club-stg","env":{"OAUTH_BASE_URL":"https://stg.example"}}' > "$STATE/config.json"
  echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"},{"name":"RELAY_IMAGE","digest":"d0","status":"Deployed"}]' > "$STATE/secrets.json"
  # Both environments as the owner sets them: a required reviewer, deployments from main alone.
  for e in stg prod; do
    jq -n --arg e "relay-$e" '{name: $e, protection_rules: [{type: "required_reviewers", prevent_self_review: false,
      reviewers: [{type: "User", reviewer: {login: "owner"}}]}, {type: "branch_policy"}],
      deployment_branch_policy: {protected_branches: false, custom_branch_policies: true}}' > "$STATE/env-relay-$e.json"
    echo '{"total_count":1,"branch_policies":[{"name":"main","type":"branch"}]}' > "$STATE/policies-relay-$e.json"
  done
  # Newest first: a branch run spoofing a promote (the review's R6: the lookup must skip it), a stg
  # run whose promote step was skipped (no token then), a prod run, the stg run that did promote.
  jq -n --arg sp "$SPOOF" --arg s "$SKIPPED" --arg o "$OLD" '{workflow_runs: [
    {id: 8, display_title: "promote stg \($sp)", head_branch: "feat/x", event: "workflow_dispatch"},
    {id: 7, display_title: "promote stg \($s)", head_branch: "main", event: "workflow_dispatch"},
    {id: 6, display_title: "promote prod v1.0.0", head_branch: "main", event: "workflow_dispatch"},
    {id: 5, display_title: "promote stg \($o)", head_branch: "main", event: "workflow_dispatch"}]}' > "$STATE/runs.json"
  echo '{"jobs":[{"steps":[{"name":"Promote RELAY_IMAGE","conclusion":"success"}]}]}' > "$STATE/jobs-8.json"
  echo '{"jobs":[{"steps":[{"name":"Promote RELAY_IMAGE","conclusion":"skipped"}]}]}' > "$STATE/jobs-7.json"
  echo '{"jobs":[{"steps":[{"name":"Promote RELAY_IMAGE","conclusion":"success"}]}]}' > "$STATE/jobs-6.json"
  echo '{"jobs":[{"steps":[{"name":"Promote RELAY_IMAGE","conclusion":"success"}]}]}' > "$STATE/jobs-5.json"
}
# run_step SCRIPT VAR=VALUE...: the block under the runner's shell (bash -eo pipefail), in the
# scratch repository, fakes first on PATH.
run_step() {
  local script=$1; shift
  if (cd "$g" && env -i PATH="$W/bin:$PATH" HOME="$W" RUNNER_TEMP="$W/rt" GITHUB_STEP_SUMMARY="$W/summary" \
    GITHUB_OUTPUT="$W/output" GITHUB_REPOSITORY=onryde/seazn.club.compositor GITHUB_RUN_ID=999 \
    GITHUB_REF=refs/heads/main GITHUB_SHA="$H" STATE="$STATE" RELAY_REGISTRY="$REG" GH_TOKEN=fake-gh \
    FLY_API_TOKEN_RELAY_STG_PUSH=fake-stg-push FLY_API_TOKEN_WEB_STG=fake-stg-web \
    FLY_API_TOKEN_RELAY_PROD_PUSH=fake-prod-push FLY_API_TOKEN_WEB_PROD=fake-prod-web \
    EXPECT_PUSH=fake-stg-push EXPECT_WEB=fake-stg-web "$@" \
    bash --noprofile --norc -eo pipefail "$script") > "$W/step.out" 2>&1; then rc=0; else rc=$?; fi
}
PROD_TOKENS=(EXPECT_PUSH=fake-prod-push EXPECT_WEB=fake-prod-web)
has() { grep -qF -- "$2" "$1"; }
no_set() { ! grep -qE 'flyctl secrets (set|deploy)' "$STATE/log"; }
no_fly() { ! grep -qE '^(flyctl|docker) ' "$STATE/log"; }
show() { sed 's/^/    /' "$W/step.out" "$STATE/log" "$W/summary" >&2; }
# refused NAME TEXT VAR=VALUE...: the promote step stops, says TEXT, and made no Fly or registry call.
refused() {
  local name=$1 text=$2; shift 2
  state; run_step "$W/promote.sh" "$@"
  if [ "$rc" -ne 0 ] && grep -F '::error::' "$W/step.out" | grep -qF -- "$text" && no_fly; then
    ok "promote $name :: $(grep -m 1 -F -- '::error::' "$W/step.out" | cut -c 1-220)"
  else
    bad "promote $name: rc $rc, expected a refusal before any flyctl or docker call saying: $text"; show
  fi
}

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
new_value="$REG/seazn-relay-stg:$SHA@$D_NEW"
set_line="flyctl secrets set RELAY_IMAGE=$new_value -a seazn-club-stg --stage (token web)"
if [ "$rc" -eq 0 ] && has "$STATE/log" "$set_line" && has "$STATE/log" 'flyctl auth docker (token push)' \
  && has "$STATE/log" 'flyctl config show -a seazn-club-stg (token web)' \
  && [ "$(grep -nF 'flyctl secrets set' "$STATE/log" | cut -d: -f1)" -lt "$(grep -nF 'flyctl secrets deploy -a seazn-club-stg (token web)' "$STATE/log" | cut -d: -f1)" ] \
  && ! has "$STATE/log" 'token WRONG' \
  && has "$STATE/log" 'runs?branch=main&event=workflow_dispatch&status=success&per_page=100' && has "$STATE/log" 'gh api --paginate' \
  && has "$W/summary" "| commit | $SHA (on main; CEF bump gate passed in its tree) |" \
  && has "$W/summary" "| new RELAY_IMAGE | \`$new_value\` |" \
  && has "$W/summary" "| previous RELAY_IMAGE | \`$REG/seazn-relay-stg:$OLD@$D_OLD\` (promote run 5; the digest as read now) |" \
  && has "$W/summary" "| RELAY_IMAGE's Fly digest | d0 → $(digest_of "$new_value") (Deployed) |" \
  && has "$W/summary" "-f env=stg -f ref=$OLD@$D_OLD\`"; then
  ok "promote P1: stg reads the registry with the push token and sets RELAY_IMAGE by digest with the web token, then deploys; previous = run 5 (run 8 is a branch's, run 7 skipped its promote); the rollback pins its digest"
else
  bad "promote P1: rc $rc"; show
fi

# P2: the ref was never pushed; P2b: the registry cannot be read (m-5: an error, never "not pushed").
state; run_step "$W/promote.sh" RELAY_ENV=stg REF="$UNPUSHED"
if [ "$rc" -ne 0 ] && has "$W/step.out" "::error::not pushed: $REG/seazn-relay-stg:$UNPUSHED" && no_set; then
  ok "promote P2: an unpushed main commit is refused before anything is set"
else
  bad "promote P2: rc $rc"; show
fi
state; touch "$STATE/registry-down"; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -ne 0 ] && has "$W/step.out" "::error::the registry could not be read for $REG/seazn-relay-stg:$SHA" \
  && has "$W/step.out" '401 Unauthorized' && ! has "$W/step.out" '::error::not pushed' && no_set; then
  ok "promote P2b (m-5): a registry error is a refusal of its own, not \"not pushed\""
else
  bad "promote P2b: rc $rc"; show
fi

# P3: RELAY_IMAGE is an [env] entry: refuse and print the seazn.club diff.
state; echo '{"env":{"RELAY_IMAGE":"registry.example/seazn-relay-stg:old"}}' > "$STATE/config.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -ne 0 ] && has "$W/step.out" "is an [env] entry in seazn-club-stg's config" && no_set \
  && has "$W/summary" '`fly.stg.toml`' && has "$W/summary" "+  RELAY_IMAGE = \"$new_value\"" \
  && has "$W/summary" '-  RELAY_IMAGE = "registry.example/seazn-relay-stg:old"'; then
  ok "promote P3: an [env] RELAY_IMAGE is refused, with the fly.stg.toml diff in the summary"
else
  bad "promote P3: rc $rc"; show
fi
state; echo '{"env":{"RELAY_IMAGE":"x"}}' > "$STATE/config.json"
run_step "$W/promote.sh" RELAY_ENV=prod REF=v1.2.3 "${PROD_TOKENS[@]}"
if [ "$rc" -ne 0 ] && has "$W/summary" '`fly.toml`' && no_set; then
  ok "promote P3b: prod's [env] diff names fly.toml"
else
  bad "promote P3b: rc $rc"; show
fi

# P4: a secret that is not Deployed would ship with RELAY_IMAGE (I-3): Staged, Unknown, Partial, or
# no status at all. The refusal names the app and no flyctl verb.
for status in '"status":"Staged"' '"status":"Unknown"' '"status":"partial"' ''; do
  state
  echo "[{\"name\":\"OTHER_KEY\",\"digest\":\"o1\"${status:+,$status}},{\"name\":\"RELAY_IMAGE\",\"digest\":\"d0\",\"status\":\"Deployed\"}]" > "$STATE/secrets.json"
  run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
  err=$(grep -m 1 -F '::error::' "$W/step.out" || true)
  label=${status:-no status}
  if [ "$rc" -ne 0 ] && [[ "$err" == *"seazn-club-stg has secrets that are not Deployed: OTHER_KEY ("* ]] \
    && [[ "$err" == *"the owner must resolve staged secrets on seazn-club-stg first"* ]] \
    && ! grep -qiE 'flyctl|unset|unstage|secrets (deploy|set|import|sync)' <<< "$err" && no_set; then
    ok "promote P4 (I-3): another secret at $label refuses :: $err"
  else
    bad "promote P4 ($label): rc $rc"; show
  fi
done

# P4e: RELAY_IMAGE the only secret staged (a promote whose deploy failed) does not refuse.
state; echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"},{"name":"RELAY_IMAGE","digest":"d9","status":"Staged"}]' > "$STATE/secrets.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -eq 0 ] && has "$STATE/log" "$set_line" && has "$STATE/log" 'flyctl secrets deploy -a seazn-club-stg (token web)'; then
  ok "promote P4e (I-3): RELAY_IMAGE staged alone does not refuse"
else
  bad "promote P4e: rc $rc"; show
fi

# P4f: the sequence: a promote whose deploy fails after --stage, then the rollback to the previous
# ref (pinned by digest) on the same state, which deploys and proves itself.
state; touch "$STATE/deploy-fails"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
r1=$rc; staged1=$(jq -r '.[] | select(.name == "RELAY_IMAGE") | "\(.digest) \(.status)"' "$STATE/secrets.json")
: > "$STATE/log"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$OLD@$D_OLD"
old_value="$REG/seazn-relay-stg:$OLD@$D_OLD"
after2=$(jq -r '.[] | select(.name == "RELAY_IMAGE") | "\(.digest) \(.status)"' "$STATE/secrets.json")
if [ "$r1" -ne 0 ] && [ "$staged1" = "$(digest_of "$new_value") Staged" ] && [ "$rc" -eq 0 ] \
  && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$old_value -a seazn-club-stg --stage (token web)" \
  && [ "$after2" = "$(digest_of "$old_value") Deployed" ]; then
  ok "promote P4f (I-3): a failed promote leaves RELAY_IMAGE Staged, and the rollback after it deploys ($after2)"
else
  bad "promote P4f: rc $r1/$rc, staged '$staged1', after '$after2'"; show
fi

# P5: prod, with prod's tokens; its previous ref (run 6) is gone from the registry.
state; run_step "$W/promote.sh" RELAY_ENV=prod REF=v1.2.3 "${PROD_TOKENS[@]}"
if [ "$rc" -eq 0 ] && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$REG/seazn-relay-prod:v1.2.3@$D_PROD -a seazn-club-prod --stage (token web)" \
  && has "$STATE/log" 'flyctl auth docker (token push)' \
  && has "$STATE/log" 'flyctl secrets deploy -a seazn-club-prod (token web)' && ! has "$STATE/log" 'seazn-club-stg' \
  && has "$W/summary" "| commit | $C (on main" \
  && has "$W/summary" "\`$REG/seazn-relay-prod:v1.0.0\` (promote run 6; no longer in the registry)" \
  && has "$W/summary" 'Rollback: the previous ref cannot be verified now'; then
  ok "promote P5: prod promotes v1.2.3 (its commit on main) on seazn-club-prod with prod's tokens; a vanished previous ref is said"
else
  bad "promote P5: rc $rc"; show
fi

# P6: inputs refused before any call; each env takes only its own ref shape (I-2).
refused "P6 a bad env" "env must be stg or prod: 'qa'" RELAY_ENV=qa REF="$SHA"
refused "P6 an injected ref" "a stg ref is a 40-hex main sha" RELAY_ENV=stg REF='main; touch /tmp/x'
refused "P6 a tag for stg" "a stg ref is a 40-hex main sha (deploy-stg's tag), optionally @sha256:<digest>: 'v1.2.3'" RELAY_ENV=stg REF=v1.2.3
refused "P6 a sha for prod" "a prod ref is a vX.Y.Z release tag (deploy-prod's tag), optionally @sha256:<digest>: '$SHA'" RELAY_ENV=prod REF="$SHA" "${PROD_TOKENS[@]}"
refused "P6 a malformed digest" "the part after @ must be sha256:<64 hex>: '$SHA@sha256:xyz'" RELAY_ENV=stg REF="$SHA@sha256:xyz"
refused "P7 dispatched off main" 'promote runs from main only, not refs/heads/feat/x' RELAY_ENV=stg REF="$SHA" GITHUB_REF=refs/heads/feat/x

# E: the environment's rules (I-1), each refused before any Fly call.
env_case() { # env_case NAME TEXT JQ_EDIT_OF_ENV_STG [POLICIES_JSON | none]
  state
  jq "$3" "$STATE/env-relay-stg.json" > "$STATE/e.tmp" && mv "$STATE/e.tmp" "$STATE/env-relay-stg.json"
  if [ "${4:-}" = none ]; then rm "$STATE/policies-relay-stg.json"; elif [ -n "${4:-}" ]; then echo "$4" > "$STATE/policies-relay-stg.json"; fi
  run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
  if [ "$rc" -ne 0 ] && grep -F '::error::' "$W/step.out" | grep -qF -- "$2" && no_fly; then
    ok "promote $1 :: $(grep -m 1 -F -- '::error::' "$W/step.out" | cut -c 1-220)"
  else
    bad "promote $1: rc $rc, expected: $2"; show
  fi
}
state; rm "$STATE/env-relay-stg.json"; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -ne 0 ] && has "$W/step.out" '::error::the relay-stg environment could not be read, so nobody'"'"'s approval is proven' && no_fly; then
  ok "promote E1 (I-1): an environment GitHub will not return refuses :: $(grep -m 1 -F '::error::' "$W/step.out" | cut -c 1-200)"
else
  bad "promote E1: rc $rc"; show
fi
env_case "E2 (I-1): no required-reviewers rule" 'relay-stg has no required reviewer' '.protection_rules = [{type: "wait_timer", wait_timer: 5}]'
env_case "E3 (I-1): a reviewers rule naming nobody" 'relay-stg has no required reviewer' '.protection_rules[0].reviewers = []'
env_case "E4 (I-1): any branch may deploy" 'relay-stg does not limit its deployment branches to a chosen list' '.deployment_branch_policy = null'
env_case "E5 (I-1): protected branches, not a list" 'relay-stg does not limit its deployment branches to a chosen list' \
  '.deployment_branch_policy = {protected_branches: true, custom_branch_policies: false}'
env_case "E6 (I-1): main and another branch" "relay-stg deploys from 'branch:main,branch:release/*', not main alone" '.' \
  '{"total_count":2,"branch_policies":[{"name":"release/*","type":"branch"},{"name":"main","type":"branch"}]}'
env_case "E7 (I-1): a tag named main" "relay-stg deploys from 'tag:main', not main alone" '.' \
  '{"total_count":1,"branch_policies":[{"name":"main","type":"tag"}]}'
env_case "E8 (I-1): the branch policies unreadable" "relay-stg's deployment branch policies could not be read" '.' none
env_case "E9 (I-1): not GitHub's environment object" 'the relay-stg environment did not read as' '"text"'
# The other env: prod's environment without a reviewer refuses a prod promote while stg's is fine.
state; jq '.protection_rules = []' "$STATE/env-relay-prod.json" > "$STATE/e.tmp" && mv "$STATE/e.tmp" "$STATE/env-relay-prod.json"
run_step "$W/promote.sh" RELAY_ENV=prod REF=v1.2.3 "${PROD_TOKENS[@]}"
if [ "$rc" -ne 0 ] && has "$W/step.out" 'relay-prod has no required reviewer' && no_fly && has "$STATE/log" 'environments/relay-prod'; then
  ok "promote E10 (I-1): prod reads relay-prod's rules, not stg's"
else
  bad "promote E10: rc $rc"; show
fi

# G: where the image came from (I-2), each refused before any Fly call. For stg the tag is the
# commit, so the message reads "<sha> (<sha>) is not on main".
refused "G1 (I-2): a stg sha off main" "$SIDE ($SIDE) is not on main: images are promoted from main only" RELAY_ENV=stg REF="$SIDE"
refused "G2 (I-2): a sha that is no commit here" "no commit 4444444444444444444444444444444444444444 in main's history" \
  RELAY_ENV=stg REF=4444444444444444444444444444444444444444
refused "G3 (I-2): a prod tag off main" "v1.2.4 ($SIDE) is not on main: images are promoted from main only" RELAY_ENV=prod REF=v1.2.4 "${PROD_TOKENS[@]}"
refused "G4 (I-2): a prod tag that does not exist" "no tag v9.9.9 in this checkout" RELAY_ENV=prod REF=v9.9.9 "${PROD_TOKENS[@]}"
refused "G5 (I-2): a main commit whose own tree fails the CEF bump gate" "$BAD ($BAD) fails the CEF bump gate (Owed 22) in its own tree" RELAY_ENV=stg REF="$BAD"
refused "G6 (I-2): a main commit from before the record" "$PRE ($PRE) has no docker/CEF_STEP0_PASS" RELAY_ENV=stg REF="$PRE"
refused "G7 (I-2): a prod tag whose tree fails the CEF bump gate" "v1.1.0 ($BAD) fails the CEF bump gate (Owed 22) in its own tree" RELAY_ENV=prod REF=v1.1.0 "${PROD_TOKENS[@]}"
# G5's red is the gate's own reason, read from the promoted tree, not main's (main's tree passes).
state; run_step "$W/promote.sh" RELAY_ENV=stg REF="$BAD"
if has "$W/step.out" "cef bump gate: FAIL: docker/GSTCEF_COMMIT=$bump, but C11 Step 0 last passed on $pin (F-009)"; then
  ok "promote G5: the gate ran on the promoted commit's tree (GSTCEF_COMMIT=$bump there)"
else
  bad "promote G5's gate output"; show
fi

# P8: the run history cannot be read: the promote goes ahead and says the previous ref is unknown.
state; rm "$STATE/runs.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -eq 0 ] && has "$W/summary" "| previous RELAY_IMAGE | unknown: this workflow's runs could not be read" && has "$STATE/log" 'flyctl secrets deploy'; then
  ok "promote P8: an unreadable history is said, not guessed"
else
  bad "promote P8: rc $rc"; show
fi

# P9: the first promote: no earlier run, no RELAY_IMAGE before.
state; echo '{"workflow_runs":[]}' > "$STATE/runs.json"; echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"}]' > "$STATE/secrets.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -eq 0 ] && has "$W/summary" "unset (this is seazn-club-stg's first RELAY_IMAGE)" \
  && has "$W/summary" "| RELAY_IMAGE's Fly digest | unset → $(digest_of "$new_value") (Deployed) |" && has "$W/summary" 'no earlier promote of stg'; then
  ok "promote P9: the first promote says the value was unset"
else
  bad "promote P9: rc $rc"; show
fi

# P10: no tokens for the env past the guard (never the other env's).
state; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA" FLY_API_TOKEN_WEB_STG=
if [ "$rc" -ne 0 ] && has "$W/step.out" 'no Fly tokens for stg past the guard' && no_fly; then
  ok "promote P10: stg without stg's web token stops (prod's is never used)"
else
  bad "promote P10: rc $rc"; show
fi

# P11 to P13 (m-7): the deploy's proof. RELAY_IMAGE gone, still Staged, or another digest: each red.
state; touch "$STATE/deploy-drops-image"; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -ne 0 ] && has "$W/step.out" 'RELAY_IMAGE is not set on seazn-club-stg after the deploy'; then
  ok "promote P11: a deploy that leaves RELAY_IMAGE unset is a red"
else
  bad "promote P11: rc $rc"; show
fi
state; touch "$STATE/deploy-leaves-staged"; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -ne 0 ] && has "$W/step.out" "RELAY_IMAGE on seazn-club-stg reads 'Staged' after the deploy, not Deployed: not proven live"; then
  ok "promote P12 (m-7): RELAY_IMAGE still Staged after the deploy is a red"
else
  bad "promote P12: rc $rc"; show
fi
state; touch "$STATE/deploy-moves-digest"; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -ne 0 ] && has "$W/step.out" "RELAY_IMAGE's Fly digest on seazn-club-stg is 999 after the deploy, not the $(digest_of "$new_value") this promote staged"; then
  ok "promote P13 (m-7): another digest after the deploy is a red"
else
  bad "promote P13: rc $rc"; show
fi

# P14 (m-5): a ref pinned to its digest promotes; pinned to another digest (the tag moved) it refuses.
state; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA@$D_NEW"
r1=$rc; l1=$(grep -c "flyctl secrets set RELAY_IMAGE=$new_value " "$STATE/log" || true)
state; run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA@$D_OLD"
if [ "$r1" -eq 0 ] && [ "$l1" -eq 1 ] && [ "$rc" -ne 0 ] \
  && has "$W/step.out" "$REG/seazn-relay-stg:$SHA resolves to $D_NEW, not the $D_OLD this promote names: the tag moved, nothing promoted" && no_set; then
  ok "promote P14 (m-5): ref@digest promotes when the tag still resolves to it, and refuses when it moved"
else
  bad "promote P14: rc $r1/$rc"; show
fi

# P15 (m-5): the previous run promoted ref@digest and the tag has moved since: said, and no rollback offered.
state
jq --arg o "$OLD" --arg d "$D_PROD" '.workflow_runs[3].display_title = "promote stg \($o)@\($d)"' "$STATE/runs.json" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/runs.json"
run_step "$W/promote.sh" RELAY_ENV=stg REF="$SHA"
if [ "$rc" -eq 0 ] && has "$W/summary" "\`$REG/seazn-relay-stg:$OLD@$D_PROD\` (promote run 5; the tag has moved since, to $D_OLD)" \
  && has "$W/summary" 'Rollback: the previous ref cannot be verified now' && ! has "$W/summary" '-f ref='; then
  ok "promote P15 (m-5): a previous pinned ref whose tag moved is said, with no rollback offered"
else
  bad "promote P15: rc $rc"; show
fi

# The token guards: promote's (per env, both tokens, no fallback) and the push workflows'.
extract "$PROMOTE" promote token "$W/promote-token.sh"
guard() { # guard SCRIPT EXPECT_ENABLED EXPECT_WARNING VAR=VALUE...
  local script=$1 want=$2 warn=$3; shift 3
  state; run_step "$script" "$@"
  if [ "$rc" -eq 0 ] && has "$W/output" "enabled=$want" \
    && { { [ -z "$warn" ] && ! has "$W/step.out" '::warning::'; } || { [ -n "$warn" ] && has "$W/step.out" "$warn"; }; }; then
    ok "guard $(basename "$script") $*: enabled=$want"
  else
    bad "guard $(basename "$script") $*: rc $rc"; sed 's/^/    /' "$W/step.out" "$W/output" >&2
  fi
}
guard "$W/promote-token.sh" true '' RELAY_ENV=stg
guard "$W/promote-token.sh" false '::warning::FLY_API_TOKEN_RELAY_STG_PUSH unset — promote skipped' RELAY_ENV=stg FLY_API_TOKEN_RELAY_STG_PUSH=
guard "$W/promote-token.sh" false '::warning::FLY_API_TOKEN_WEB_STG unset (an environment secret of relay-stg) — promote skipped' RELAY_ENV=stg FLY_API_TOKEN_WEB_STG=
guard "$W/promote-token.sh" false '::warning::FLY_API_TOKEN_RELAY_PROD_PUSH unset — promote skipped' RELAY_ENV=prod FLY_API_TOKEN_RELAY_PROD_PUSH=
guard "$W/promote-token.sh" false '::warning::FLY_API_TOKEN_WEB_PROD unset (an environment secret of relay-prod) — promote skipped' RELAY_ENV=prod FLY_API_TOKEN_WEB_PROD=
guard "$W/promote-token.sh" true '' RELAY_ENV=prod FLY_API_TOKEN_RELAY_STG_PUSH= FLY_API_TOKEN_WEB_STG=
extract "$STG" gate token "$W/stg-token.sh"
guard "$W/stg-token.sh" false '::warning::FLY_API_TOKEN_RELAY_STG_PUSH unset — push skipped' FLY_API_TOKEN=
guard "$W/stg-token.sh" true '' FLY_API_TOKEN=fake-stg-push
extract "$PROD" gate token "$W/prod-token.sh"
guard "$W/prod-token.sh" false '::warning::FLY_API_TOKEN_RELAY_PROD_PUSH unset — push skipped' FLY_API_TOKEN=

# deploy-stg's main check (I-2): main passes; a branch or a tag is refused.
extract "$STG" gate provenance "$W/stg-provenance.sh"
state; run_step "$W/stg-provenance.sh"
r1=$rc
state; run_step "$W/stg-provenance.sh" GITHUB_REF=refs/heads/feat/x
r2=$rc; o2=$(cat "$W/step.out")
state; run_step "$W/stg-provenance.sh" GITHUB_REF=refs/tags/v1.2.3
if [ "$r1" -eq 0 ] && [ "$r2" -ne 0 ] && [[ "$o2" == *"::error::deploy-stg pushes main only, not refs/heads/feat/x"* ]] && [ "$rc" -ne 0 ]; then
  ok "deploy-stg: main pushes; a branch (dispatch) and a tag are refused"
else
  bad "deploy-stg provenance: rc $r1/$r2/$rc"; echo "$o2" >&2
fi

# "Already pushed?", stg and prod (m-5): a pushed tag is never pushed again; only "not found" is
# absent; any other registry error is a red with no exists= written.
for wf in "$STG:seazn-relay-stg:$SHA:fake-stg-push" "$PROD:seazn-relay-prod:v1.2.3:fake-prod-push"; do
  IFS=: read -r file app pushed token <<< "$wf"
  extract "$file" push exists "$W/exists.sh"
  state; run_step "$W/exists.sh" FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG="$pushed"
  r1=$rc; out1=$(cat "$W/output")
  state; run_step "$W/exists.sh" FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG=v7.7.7
  r2=$rc; out2=$(cat "$W/output")
  state; touch "$STATE/registry-down"; run_step "$W/exists.sh" FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG=v7.7.7
  if [ "$r1" -eq 0 ] && [[ "$out1" == *"exists=true"* && "$out1" == *"digest=sha256:"* ]] \
    && [ "$r2" -eq 0 ] && [[ "$out2" == *"exists=false"* ]] \
    && [ "$rc" -ne 0 ] && has "$W/step.out" "::error::the registry could not be read for $REG/$app:v7.7.7" && [ ! -s "$W/output" ]; then
    ok "$file Already pushed?: pushed reads exists=true; not found exists=false; a registry error is a red"
  else
    bad "$file Already pushed?: rc $r1/$r2/$rc"; echo "$out1 | $out2" >&2; sed 's/^/    /' "$W/step.out" >&2
  fi
done

# deploy-prod's tag check, on the scratch repository (main z-a-b-c-h, side d).
extract "$PROD" gate provenance "$W/tag.sh"
tag_case() { # tag_case TAG SHA WANT_RC TEXT
  state
  if (cd "$g" && env -i PATH="$PATH" HOME="$W" TAG="$1" GITHUB_SHA="$2" bash --noprofile --norc -eo pipefail "$W/tag.sh") > "$W/step.out" 2>&1; then rc=0; else rc=$?; fi
  if { { [ "$3" = 0 ] && [ "$rc" -eq 0 ]; } || { [ "$3" != 0 ] && [ "$rc" -ne 0 ]; }; } && has "$W/step.out" "$4"; then
    ok "deploy-prod tag check: $1 at ${2:0:7}: $4"
  else
    bad "deploy-prod tag check: $1 at ${2:0:7}: rc $rc"; sed 's/^/    /' "$W/step.out" >&2
  fi
}
tag_case v1.2.3 "$H" 0 "v1.2.3 ($H) is on main"
tag_case v1.2.3 "$A" 0 "is on main"
tag_case v1.2.3 "$D" 1 "::error::v1.2.3 ($D) is not on main: prod images come from main only"
tag_case v1.2.3-rc1 "$H" 1 "::error::'v1.2.3-rc1' is not a release tag vX.Y.Z"

echo "checked: $checked, failed: $failures"
finished=1
if [ "$checked" -eq 0 ]; then
  echo "deploy_checks_test: FAIL: checked nothing" >&2
  exit 1
fi
[ "$failures" -eq 0 ] || exit 1
exit 0
