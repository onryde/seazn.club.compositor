#!/usr/bin/env bash
# shellcheck disable=SC2016  # the mutants and jq programs hold literal $ text, never meant to expand
# The deploy checks' own test (C16, docs/tasks/C16-deploy.md). check.yml runs it after
# scripts/check-workflows.sh. Never touches the repo's files: every mutant edits a scratch copy.
#
# 1. scripts/check-workflows.sh: green on the repo's workflows; red on an empty set; red on each
#    mutant, by its own reason (each red prints the line it matched, after `::`). The brief's six, as
#    amended 2026-10-25: (1) the ::warning:: guard removed from deploy-stg.yml, (2) an environment
#    on deploy-stg.yml's push job (promote.yml, which mutant 2 first targeted, is gone), (4) the CEF
#    bump gate deleted from deploy-prod.yml (likewise), (5) a seazn-relay-cache cache-from in
#    deploy-stg.yml, (6) target: relay-test-hooks in deploy-stg.yml; then one per rule or refusal
#    each review added (e*, m*, i*), and one per rule the 2026-10-25 shape added (n*: no GitHub
#    environment, the secrets each workflow may name, no web-app token, the steps after the guard,
#    main only, the build record's order, the write grants, no promote workflow).
# 2. scripts/check-cef-bump.sh: green on the repo; red on the brief's mutant (3), GSTCEF_COMMIT
#    changed without the record, on each malformed record, and on a record whose finding is no
#    Step 0 run on its commit (m-8); green again once the record and its finding follow the bump.
# 3. The deploy workflows' own scripts, extracted as written and run under fake flyctl, docker and gh:
#    the token guards, the provenance steps, "Already pushed?", the build record checks (pushed,
#    found, squatted) and the summaries, whose promote command the real scripts/promote.sh accepts.
# 4. scripts/promote.sh, as the operator runs it (from a clone of a scratch "GitHub" repository,
#    main: z, a, b, c, h; side: d), under the same fakes: every refusal (the arguments and the prod
#    flag, the tools, main's copy, provenance, the registry, main's build record field by field, the
#    web app's [env] and staged secrets), no token ever used or printed, the previous RELAY_IMAGE from
#    the promote log, the deploy and its proof, and a failed promote followed by its rollback.
#
# Needs bash, ruby (YAML), jq, python3, git, and the pinned actionlint and shellcheck on PATH
# (scripts/check-workflows.sh --install DIR). Prints `checked: N` and fails on 0.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
W=$(mktemp -d)
# A run that stops before its tally is a red: bash 3.2 exits 0 after a `set -u` abort under an EXIT
# trap, which would pass a half-run test.
trap 'rm -rf "$W"; if [ "${finished:-0}" != 1 ]; then echo "deploy_checks_test: FAIL: stopped before its tally" >&2; exit 1; fi' EXIT
STG=deploy-stg.yml PROD=deploy-prod.yml

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
# move FILE A B: move the text from marker A up to (not including) marker B so it sits before marker C.
move_before() {
  python3 -I -c '
import sys
p, a, b, c = sys.argv[1:5]
s = open(p).read()
i, j = s.index(a), s.index(b)
block = s[i:j]
s = s[:i] + s[j:]
k = s.index(c)
open(p, "w").write(s[:k] + block + s[k:])' "$@"
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
  if [ "$rc" -ne 0 ] && grep -F 'check-workflows: FAIL: ' "$W/check.out" | grep -qF -- "$2" && grep -qF "checked $n workflows" "$W/check.out"; then
    ok "check-workflows red: $1 :: $(grep -F 'check-workflows: FAIL: ' "$W/check.out" | grep -m 1 -F -- "$2" | sed 's/^check-workflows: FAIL: //' | cut -c 1-240)"
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
if [ "$rc" -eq 0 ] && grep -qF "checked $n_repo workflows" "$W/check.out" && grep -qF 'deploy workflows: 2 ' "$W/check.out" \
  && grep -qF "no github environment: " "$W/check.out" \
  && grep -qF 'deploy workflows naming their own relay token in their push job: 2 of 2' "$W/check.out" \
  && grep -qF "web-app tokens and web apps named: 0 in $n_repo workflows" "$W/check.out" \
  && grep -qF 'provenance steps before a push guard: 2' "$W/check.out" \
  && grep -qF 'push jobs behind their ref (hygiene, not a guard): 2 of 2' "$W/check.out" \
  && grep -qF 'build records: 2 of 2 push jobs attest and verify both paths' "$W/check.out" \
  && grep -qF "write grants: 5 of 5 allowed (id-token and attestations on the two push jobs, packages on image.yml's cache job); $n_repo workflows read" "$W/check.out" \
  && grep -qF "promote workflows: 0 lines changing Fly secrets in $n_repo workflows" "$W/check.out"; then
  ok "check-workflows green on the repo ($n_repo workflows, 2 deploy, no environment, 2 relay tokens in their push jobs, no web token, 2 provenance steps, 2 main-only ifs, 2 build records, 5 write grants, no promote workflow)"
else
  bad "check-workflows on the repo: rc $rc"; sed 's/^/    /' "$W/check.out" >&2
fi
# Each rule read something on the repo (a rule that reads nothing proves nothing).
for want in 'token steps gated by them: ' 'deploy steps after a guard that read it: ' 'no secret echoed: '; do
  n=$(grep -F "$want" "$W/check.out" | grep -oE "${want}[0-9]+" | grep -oE '[0-9]+$' | head -n 1 || true)
  if [ -n "$n" ] && [ "$n" -gt 0 ]; then ok "check-workflows on the repo: '$want$n'"; else bad "check-workflows on the repo read nothing for '$want'"; fi
done
env_read=$(grep -oE 'no github environment: [0-9]+ jobs read' "$W/check.out" | grep -oE '[0-9]+' || true)
if [ -n "$env_read" ] && [ "$env_read" -gt 0 ]; then ok "check-workflows: the environment rule read $env_read jobs"; else bad "the environment rule read no job"; fi

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
      - name: The ref is main'
PROD_GATE='      - name: CEF bump gate (Owed 22)
        run: scripts/check-cef-bump.sh
      - name: The tag is a release on main'
PUSH_INSPECT='          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
STG_EXISTS="      - name: Already pushed?
        id: exists
        if: steps.token.outputs.enabled == 'true'"

# (1) The ::warning:: guard removed from deploy-stg.yml.
fresh; edit "$W/wf/$STG" '            echo "::warning::FLY_API_TOKEN_RELAY_STG unset (a repository secret: docs/_INDEX.md, the C16 handoff) — push skipped"'$'\n' ''
expect_red "(1) deploy-stg's ::warning:: guard removed" 'deploy-stg.yml FLY_API_TOKEN_RELAY_STG: no step warns'

# (2) An environment on deploy-stg's push job (amended 2026-10-25: no GitHub environment anywhere).
fresh; edit "$W/wf/$STG" '    timeout-minutes: 90
' '    timeout-minutes: 90
    environment: relay-stg-push
'
expect_red "(2) deploy-stg's push job in an environment" "deploy-stg.yml: job push declares environment 'relay-stg-push': no GitHub environment"

# (4) The CEF bump gate deleted from deploy-prod.yml.
fresh; edit "$W/wf/$PROD" "$PROD_GATE" '      - name: The tag is a release on main'
expect_red "(4) deploy-prod's CEF bump gate deleted" 'deploy-prod.yml (CEF bump gate, Owed 22): job push: the step after its checkout'

# (5) A seazn-relay-cache cache-from in deploy-stg.yml.
fresh; edit "$W/wf/$STG" '          load: true
          push: false' '          load: true
          push: false
          cache-from: type=registry,ref=ghcr.io/onryde/seazn-relay-cache:cef-x'
expect_red "(5) deploy-stg reads seazn-relay-cache" 'deploy-stg.yml names the CI build cache'

# (6) target: relay-test-hooks in deploy-stg.yml.
fresh; edit "$W/wf/$STG" '          target: relay' '          target: relay-test-hooks'
expect_red "(6) deploy-stg builds relay-test-hooks" 'deploy-stg.yml names a test-hooks target'

# e1. A token step no guard gates: deploy-stg's "Already pushed?" without its `if`.
fresh; edit "$W/wf/$STG" "$STG_EXISTS" '      - name: Already pushed?
        id: exists'
expect_red "e1 an ungated token step" 'push.Already pushed? holds FLY_API_TOKEN_RELAY_STG but runs whether or not the guard found it'

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
          test -n "${{ secrets.FLY_API_TOKEN_RELAY_STG }}"
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

# e8. Another token as a fallback (`a || X`): prod's token when stg's is empty.
fresh; edit "$W/wf/$STG" '        env:
          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}
        run: |
          flyctl auth docker' '        env:
          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG || secrets.FLY_API_TOKEN_RELAY_PROD }}
        run: |
          flyctl auth docker'
expect_red "e8 a token fallback" 'names two Fly tokens in one expression'

# e10. A job-level env holding the token.
fresh; edit "$W/wf/$STG" '      TAG: ${{ github.sha }}' '      TAG: ${{ github.sha }}
      FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}'
expect_red "e10 a job env holds the token" 'job push holds FLY_API_TOKEN_RELAY_STG in its env'

# e11. A workflow-level env holding a secret.
fresh; edit "$W/wf/$STG" '  RELAY_APP: seazn-relay-stg' '  RELAY_APP: seazn-relay-stg
  ANY_SECRET: ${{ secrets.SOMETHING }}'
expect_red "e11 a workflow env holds a secret" 'the workflow-level env holds a secret'

# e12. The token used before the checkout and its gate.
fresh; edit "$W/wf/$STG" '      # Actions pinned by commit: this job holds a Fly token (image.yml'"'"'s cache job, the PR #47 review'"'"'s m7).
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0' '      - name: Early
        env:
          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}
        run: flyctl version
      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0'
expect_red "e12 the token before the checkout" 'job push uses its Fly token before it checks out'

# e13. A deploy workflow by what it does, not its name: a copy of deploy-stg with no CEF bump gate.
fresh; cp "$W/wf/$STG" "$W/wf/ship.yml"
python3 -I -c '
import sys
p = sys.argv[1]
s = open(p).read()
gate = "      - name: CEF bump gate (Owed 22)\n        run: scripts/check-cef-bump.sh\n"
if s.count(gate) != 1: sys.exit("ship.yml: expected 1 gate step")
open(p, "w").write(s.replace(gate, "").replace("name: deploy-stg", "name: ship"))' "$W/wf/ship.yml"
expect_red "e13 an unnamed deploy workflow with no gate" 'ship.yml (CEF bump gate, Owed 22)'

# e14. A required deploy workflow missing.
fresh; rm "$W/wf/$PROD"
expect_red "e14 deploy-prod.yml missing" 'deploy workflow missing: deploy-prod.yml'

# e15. actionlint (shellcheck off) on every workflow: a bad value in check.yml.
fresh; edit "$W/wf/check.yml" '    timeout-minutes: 30' '    timeout-minutes: soon'
expect_red "e15 actionlint on a non-deploy workflow" 'check-workflows: FAIL: actionlint:'

# e16. shellcheck on a deploy workflow: an unquoted expansion.
fresh; edit "$W/wf/$STG" '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          {' '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          cd $RUNNER_TEMP
          {'
expect_red "e16 shellcheck on a deploy workflow" 'actionlint with shellcheck on the deploy workflows'

# m1a. The gate step under continue-on-error (deploy-stg).
fresh; edit "$W/wf/$STG" "$STG_GATE" '      - name: CEF bump gate (Owed 22)
        run: scripts/check-cef-bump.sh
        continue-on-error: true
      - name: The ref is main'
expect_red "m1a deploy-stg's gate under continue-on-error" 'job push: the CEF bump gate step (step 1) carries continue-on-error'

# m1b. The gate step behind a dead `if` (deploy-stg).
fresh; edit "$W/wf/$STG" "$STG_GATE" "      - name: CEF bump gate (Owed 22)
        if: github.event_name == 'never'
        run: scripts/check-cef-bump.sh
      - name: The ref is main"
expect_red "m1b deploy-stg's gate behind a dead if" 'job push: the CEF bump gate step (step 1) carries if'

# m1c. The gate neutered by `|| true` (deploy-prod).
fresh; edit "$W/wf/$PROD" "$PROD_GATE" '      - name: CEF bump gate (Owed 22)
        run: scripts/check-cef-bump.sh || true
      - name: The tag is a release on main'
expect_red "m1c deploy-prod's gate || true" 'deploy-prod.yml (CEF bump gate, Owed 22): job push: the step after its checkout (step 1) is not exactly'

# m1d. deploy-prod's tag check under continue-on-error.
fresh; edit "$W/wf/$PROD" '        id: provenance
' '        id: provenance
        continue-on-error: true
'
expect_red "m1d deploy-prod's tag check under continue-on-error" 'deploy-prod.yml (provenance): job push: the provenance step carries continue-on-error'

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
expect_red "m1e deploy-stg's main check deleted" 'deploy-stg.yml (provenance): job push has no step with id provenance'

# m1f. deploy-stg's main check that refuses nothing (exit 0).
fresh; edit "$W/wf/$STG" '            echo "::error::deploy-stg pushes main only, not $GITHUB_REF: nothing pushed"
            exit 1' '            echo "::error::deploy-stg pushes main only, not $GITHUB_REF: nothing pushed"
            exit 0'
expect_red "m1f deploy-stg's main check exits 0" "deploy-stg.yml (provenance): job push: the provenance step does not check 'refs/heads/main' and exit 1"

# m1g. A token step that runs past a failed step (always()).
fresh; edit "$W/wf/$STG" "$STG_EXISTS" "      - name: Already pushed?
        id: exists
        if: always() && steps.token.outputs.enabled == 'true'"
expect_red "m1g deploy-stg's token step under always()" 'job push: Already pushed? runs past a failed step'

# m1h. A token job under continue-on-error.
fresh; edit "$W/wf/$STG" "    if: github.ref == 'refs/heads/main'
" "    if: github.ref == 'refs/heads/main'
    continue-on-error: true
"
expect_red "m1h deploy-stg's push job under continue-on-error" 'job push holds a Fly token and sets continue-on-error'

# m1i. deploy-stg's main check after the token guard.
fresh; move_before "$W/wf/$STG" '      - name: The ref is main' '      - name: The Fly push token' '      # Every step from here reads the guard'
expect_red "m1i deploy-stg's main check after the guard" 'deploy-stg.yml (provenance): job push: the provenance step comes after the token guard'

# m1j. A token step whose `if` contains the guard's condition but is not it (rule 2's exact
# comparison; m1g's always() is rule 7's, so each rule has a mutant only it catches).
fresh; edit "$W/wf/$STG" "$STG_EXISTS" "      - name: Already pushed?
        id: exists
        if: steps.token.outputs.enabled == 'true' || github.event_name == 'workflow_dispatch'"
expect_red "m1j deploy-stg's token step behind a widened if" 'push.Already pushed? holds FLY_API_TOKEN_RELAY_STG but runs whether or not the guard found it'

# m1k. continue-on-error on a token step that is neither a gate nor a provenance step.
fresh; edit "$W/wf/$STG" '      - name: Already pushed?
        id: exists
' '      - name: Already pushed?
        id: exists
        continue-on-error: true
'
expect_red "m1k deploy-stg's Already pushed? under continue-on-error" 'job push: Already pushed? sets continue-on-error, so its failure would not stop the job'

# m3h. FLY_API_TOKEN echoed in a step that holds no secret by env (an earlier step could have
# exported it): the name alone is a secret.
fresh; edit "$W/wf/$STG" '          docker tag "$LOCAL_IMAGE" "$ref"' '          echo "t=$FLY_API_TOKEN"
          docker tag "$LOCAL_IMAGE" "$ref"'
expect_red "m3h FLY_API_TOKEN echoed by a step with no secret env" 'push.Push prints a secret: echo "t=$FLY_API_TOKEN"'

# m3b. A laundered alias echoed.
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          flyctl auth docker
          t=$FLY_API_TOKEN
          echo "w=$t"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "m3b an alias of the token echoed" 'prints a secret: echo "w=$t"'

# m3c. The environment dumped (env | sort, the review's MF).
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          env | sort
          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "m3c env | sort with the token" 'dumps the environment while it holds a secret: env | sort'

# m3d. printenv.
fresh; edit "$W/wf/$PROD" "$PUSH_INSPECT" '          printenv | wc -l
          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "m3d printenv with the token" 'dumps the environment while it holds a secret: printenv | wc -l'

# m3e. declare -p.
fresh; edit "$W/wf/$PROD" "$PUSH_INSPECT" '          declare -p > "$RUNNER_TEMP/vars"
          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "m3e declare -p with the token" 'dumps the environment while it holds a secret: declare -p'

# m3f. The token in argv: -t (the review's ME).
fresh; edit "$W/wf/$STG" "$PUSH_INSPECT" '          flyctl auth docker -t "$FLY_API_TOKEN"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "m3f flyctl -t" 'hands flyctl a token in argv: flyctl auth docker -t'

# m3g. The token in argv: --access-token.
fresh; edit "$W/wf/$PROD" "$PUSH_INSPECT" '          flyctl auth docker --access-token "$FLY_API_TOKEN"
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "m3g flyctl --access-token" 'hands flyctl a token in argv: flyctl auth docker --access-token'

# i1a. A web token in a deploy workflow (it would set seazn-club-stg's secrets).
fresh; edit "$W/wf/$STG" '          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}
        run: |
          flyctl auth docker' '          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}
          WEB: ${{ secrets.FLY_API_TOKEN_WEB_STG }}
        run: |
          flyctl auth docker'
expect_red "i1a deploy-stg names a web token" 'deploy-stg.yml names the secret FLY_API_TOKEN_WEB_STG (push): no web-app token is ever in GitHub'

# i1b. A token of unknown reach (the first round's FLY_API_TOKEN_STG) in a deploy workflow.
fresh; edit_all "$W/wf/$STG" 'FLY_API_TOKEN_RELAY_STG' 'FLY_API_TOKEN_STG'
expect_red "i1b deploy-stg names FLY_API_TOKEN_STG" 'deploy-stg.yml names the secret FLY_API_TOKEN_STG (push): a deploy workflow names only its own relay token (FLY_API_TOKEN_RELAY_STG)'

# i1d. A secret named by expression, which no rule can read.
fresh; edit "$W/wf/$STG" '          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}
        run: |
          flyctl auth docker' "          FLY_API_TOKEN: \${{ secrets['FLY_API_TOKEN_RELAY_STG'] }}
        run: |
          flyctl auth docker"
expect_red "i1d secrets[...] in deploy-stg" 'deploy-stg.yml names a secret by expression'

# i4e. deploy-stg's attest step deleted (no build record: promote would refuse every push).
fresh; python3 -I -c '
import sys
p = sys.argv[1]
s = open(p).read()
a = s.index("      - name: Attest the push\n")
b = s.index("          push-to-registry: false\n") + len("          push-to-registry: false\n")
open(p, "w").write(s[:a] + s[b:])' "$W/wf/$STG"
expect_red "i4e deploy-stg's attest step deleted" 'deploy-stg.yml (build record): job push has no step with id attest'

# i4f. deploy-prod's record check on the build path only: a found (squatted) tag goes unchecked.
fresh; edit "$W/wf/$PROD" "      - name: The release's build record
        id: record
        if: steps.token.outputs.enabled == 'true'
" "      - name: The release's build record
        id: record
        if: steps.token.outputs.enabled == 'true' && steps.exists.outputs.exists != 'true'
"
expect_red "i4f deploy-prod's record check skipped for a found tag" "deploy-prod.yml (build record): job push: the record check runs on if 'steps.token.outputs.enabled == 'true' && steps.exists.outputs.exists != 'true''"

# i4g. deploy-stg's record check by --signer-workflow, a prefix match with no ref (gh's policy.go).
fresh; edit "$W/wf/$STG" '              --cert-identity "$identity" --source-ref refs/heads/main' '              --signer-workflow "$GITHUB_REPOSITORY/.github/workflows/deploy-stg.yml" --source-ref refs/heads/main'
expect_red "i4g deploy-stg's record check without --cert-identity" "deploy-stg.yml (build record): job push: the record check does not read '--cert-identity'"

# i4j. A new deploy workflow (named by what it does) holding a relay token: only deploy-stg.yml may.
fresh; cp "$W/wf/$STG" "$W/wf/ship.yml"; edit "$W/wf/ship.yml" 'name: deploy-stg' 'name: ship'
expect_red "i4j an unnamed deploy workflow holding a relay token" 'ship.yml names the secret FLY_API_TOKEN_RELAY_STG (push): a deploy workflow names only its own relay token'

# m-11 and the 2026-10-25 grants: a `write` appears only as id-token and attestations on the two push
# jobs (each needs both: the attest step signs with them) and as packages on image.yml's cache job.
fresh; edit "$W/wf/$STG" '
permissions: { contents: read }
' '
permissions: { contents: read, id-token: write }
'
expect_red "m11a id-token: write at deploy-stg's workflow level" "deploy-stg.yml (write grants): the workflow-level permissions grant id-token: every job in it could write or sign as this workflow"
fresh; edit "$W/wf/check.yml" '
permissions: { contents: read }
' '
permissions: { contents: read, attestations: write }
'
expect_red "m11b attestations: write at check.yml's workflow level" "check.yml (write grants): the workflow-level permissions grant attestations"
fresh; edit "$W/wf/chaos.yml" '    # Read only: this workflow never writes a cache or the registry.
    permissions: { contents: read, packages: read }' '    # Read only: this workflow never writes a cache or the registry.
    permissions: { contents: read, id-token: write }'
expect_red "m11c id-token: write on chaos.yml's images job" "chaos.yml (write grants): job images grants id-token, and only"
fresh; edit "$W/wf/soak.yml" '
permissions: { contents: read }
' '
permissions: write-all
'
expect_red "m11d write-all at soak.yml's workflow level" "soak.yml (write grants): the workflow-level permissions grant write-all"
fresh; edit "$W/wf/image.yml" 'permissions: { contents: read, packages: write }' 'permissions: { contents: read, packages: write, id-token: write }'
expect_red "m11e id-token: write on image.yml's cache job (packages alone is its exemption)" "image.yml (write grants): job cache grants id-token, and only"
fresh; edit "$W/wf/$STG" 'permissions: { contents: read, id-token: write, attestations: write }' 'permissions: { contents: read, id-token: write }'
expect_red "m11f deploy-stg's push job without attestations: write" "deploy-stg.yml (write grants): job push does not grant both id-token: write and attestations: write"
fresh; edit "$W/wf/$PROD" 'permissions: { contents: read, id-token: write, attestations: write }' 'permissions: { contents: read }'
expect_red "m11g deploy-prod's push job with no signing grant" "write grants: 3 of 5 allowed grants found"
fresh; edit "$W/wf/$STG" '    permissions: { contents: read, id-token: write, attestations: write }' '    permissions: write-all'
expect_red "m11h write-all on deploy-stg's push job" "deploy-stg.yml (write grants): job push grants write-all, and only"
fresh; edit "$W/wf/$STG" '    permissions: { contents: read, id-token: write, attestations: write }' '    permissions: { contents: read, id-token: write, attestations: write, packages: write }'
expect_red "n14 packages: write on deploy-stg's push job (a Fly push needs none)" "deploy-stg.yml (write grants): job push grants packages, and only"
fresh; edit "$W/wf/image.yml" '    permissions: { contents: read, packages: read }' '    permissions: { contents: read, packages: write }'
expect_red "n15 packages: write on image.yml's image job (the exemption is the cache job's alone)" "image.yml (write grants): job image grants packages, and only"

# n1. No GitHub environment, in any workflow: soak.yml's plan job names one.
fresh; edit "$W/wf/soak.yml" '  plan:
    runs-on: ubuntu-24.04' '  plan:
    environment: soak
    runs-on: ubuntu-24.04'
expect_red "n1 an environment on soak.yml's plan job" "soak.yml: job plan declares environment 'soak': no GitHub environment"

# n2. The relay token outside its push job.
fresh; cat >> "$W/wf/$STG" << 'YML'
  peek:
    runs-on: ubuntu-24.04
    steps:
      - name: Peek
        id: token
        env:
          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}
        run: |
          if [ -z "$FLY_API_TOKEN" ]; then
            echo "::warning::FLY_API_TOKEN_RELAY_STG unset"
            exit 0
          fi
YML
expect_red "n2 deploy-stg's relay token in a second job" 'deploy-stg.yml names FLY_API_TOKEN_RELAY_STG outside its push job (peek)'

# n3. The relay token in another spelling (secret names ignore case; the token rules read one).
fresh; edit "$W/wf/$STG" '          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}
        run: |
          flyctl auth docker' '          FLY_API_TOKEN: ${{ secrets.fly_api_token_relay_stg }}
        run: |
          flyctl auth docker'
expect_red "n3 deploy-stg's relay token in lower case" 'deploy-stg.yml names fly_api_token_relay_stg: spell it FLY_API_TOKEN_RELAY_STG'

# n4. soak.yml's exemption is soak.yml's: its token in chaos.yml.
fresh; edit "$W/wf/chaos.yml" '    # Read only: this workflow never writes a cache or the registry.
    permissions: { contents: read, packages: read }
    env:' '    # Read only: this workflow never writes a cache or the registry.
    permissions: { contents: read, packages: read }
    env:
      SOAK: ${{ secrets.FLY_API_TOKEN_SOAK }}'
expect_red "n4 FLY_API_TOKEN_SOAK in chaos.yml" "chaos.yml names the Fly secret FLY_API_TOKEN_SOAK (images): only deploy-stg.yml"

# n5. ... and the exemption is FLY_API_TOKEN_SOAK alone: another Fly secret in soak.yml.
fresh; edit "$W/wf/soak.yml" '  plan:
    runs-on: ubuntu-24.04' '  plan:
    env:
      OTHER: ${{ secrets.FLY_ORG_TOKEN }}
    runs-on: ubuntu-24.04'
expect_red "n5 another Fly secret in soak.yml" "soak.yml names the Fly secret FLY_ORG_TOKEN (plan)"

# n6. A web app handed to flyctl in a workflow (by app, whatever token).
fresh; edit "$W/wf/$STG" '          docker tag "$LOCAL_IMAGE" "$ref"' '          flyctl status -a seazn-club-stg
          docker tag "$LOCAL_IMAGE" "$ref"'
expect_red "n6 deploy-stg drives seazn-club-stg" 'deploy-stg.yml hands flyctl a web app'

# n7. A web-app secret in a non-deploy workflow (no Fly name: rule 5 is silent, rule 6 is not).
fresh; edit "$W/wf/soak.yml" '  plan:
    runs-on: ubuntu-24.04' '  plan:
    env:
      KEY: ${{ secrets.SEAZN_CLUB_DEPLOY_KEY }}
    runs-on: ubuntu-24.04'
expect_red "n7 a web secret in soak.yml" 'soak.yml names the secret SEAZN_CLUB_DEPLOY_KEY (plan): no web-app token is ever in GitHub'

# n8. A step after the guard that does not read it: an unset token would red the push, not skip it.
fresh; edit "$W/wf/$STG" "      - name: Free disk space
        if: steps.token.outputs.enabled == 'true'
" '      - name: Free disk space
'
expect_red "n8 deploy-stg's Free disk space without the guard's if" 'push.Free disk space comes after the FLY_API_TOKEN_RELAY_STG guard but does not read it'

# n9. Main only (hygiene): deploy-stg's push job without its ref `if`, and deploy-prod's widened.
fresh; edit "$W/wf/$STG" "    if: github.ref == 'refs/heads/main'
" ''
expect_red "n9 deploy-stg's push job on any ref" "deploy-stg.yml (main only): job push runs on if '', not exactly 'github.ref == 'refs/heads/main''"
fresh; edit "$W/wf/$PROD" "    if: startsWith(github.ref, 'refs/tags/v')" "    if: startsWith(github.ref, 'refs/')"
expect_red "n9b deploy-prod's push job on any ref" "deploy-prod.yml (main only): job push runs on if 'startsWith(github.ref, 'refs/')'"

# n10. The attest step before the push (it would attest no digest).
fresh; move_before "$W/wf/$STG" '      # Main'"'"'s build record: signed with' '      # Both paths: the digest this run pushed' '      - name: Push
'
expect_red "n10 deploy-stg attests before it pushes" 'deploy-stg.yml (build record): job push: the attest step comes before the push'

# n11. The record check before the attest step.
fresh; move_before "$W/wf/$PROD" '      # Both paths: the digest this run pushed' '      - name: Summary' '      # The release'"'"'s build record: signed with'
expect_red "n11 deploy-prod verifies before it attests" 'deploy-prod.yml (build record): job push: the record check comes before the attest step'

# n12. The record check without --signer-digest (a signer at another commit would pass).
fresh; edit "$W/wf/$STG" '              --signer-digest "$GITHUB_SHA" --deny-self-hosted-runners' '              --deny-self-hosted-runners'
expect_red "n12 deploy-stg's record check without --signer-digest" "deploy-stg.yml (build record): job push: the record check does not read '--signer-digest \"\$GITHUB_SHA\"'"

# n13. A promote workflow, by name, and by what it does.
fresh; cat > "$W/wf/promote.yml" << 'YML'
name: promote
on: { workflow_dispatch: {} }
permissions: { contents: read }
jobs:
  promote:
    runs-on: ubuntu-24.04
    steps:
      - run: echo promote
YML
expect_red "n13 a workflow named promote.yml" 'promote.yml: a promote workflow'
fresh; edit "$W/wf/soak.yml" '  plan:
    runs-on: ubuntu-24.04
    timeout-minutes: 5' '  plan:
    runs-on: ubuntu-24.04
    timeout-minutes: 5
    # flyctl secrets set RELAY_IMAGE=x -a seazn-relay-soak'
expect_red "n13b a workflow that changes Fly secrets" 'soak.yml changes Fly secrets'

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

# ---------------------------------------------------------------- 3 and 4: the fakes and the repository
to_json() { ruby -ryaml -rjson -e 'puts JSON.generate(YAML.load(File.read(ARGV[0])))' "$1"; }
# extract FILE JOB STEP_ID OUT: the step's run block, as the runner writes it.
extract() {
  to_json "$root/.github/workflows/$1" | jq -er --arg j "$2" --arg s "$3" '.jobs[$j].steps[] | select(.id == $s) | .run' > "$4"
  if grep -qF '${{' "$4"; then bad "$1 $2.$3 holds an expression: inputs must reach it by env"; fi
}
mkdir -p "$W/bin" "$W/rt"

# The scratch "GitHub" repository: main z (no CEF_STEP0_PASS yet), a, b (GSTCEF_COMMIT bumped with no
# record: fails the gate), c, h (HEAD); side: d off h. Tags: v1.0.0 = a, v1.1.0 = b, v1.2.3 = c,
# v1.2.4 = d. Every commit carries this tree's scripts/promote.sh and check-cef-bump.sh, so main's
# copy is the one under test. The operator's checkout ($W/op) is a clone of it.
g="$W/git"
mkdir -p "$g/docker" "$g/scripts"
git -C "$g" init -q -b main
gc() { git -C "$g" add -A && git -C "$g" -c user.email=t@example -c user.name=t commit -q --allow-empty -m "$1" && git -C "$g" rev-parse HEAD; }
cp "$root/scripts/check-cef-bump.sh" "$root/scripts/promote.sh" "$g/scripts/"; cp "$root/_FINDINGS.md" "$g/"; cp "$root/docker/GSTCEF_COMMIT" "$g/docker/"
Z=$(gc z)
cp "$root/docker/CEF_STEP0_PASS" "$g/docker/"; A=$(gc a)
echo "$bump" > "$g/docker/GSTCEF_COMMIT"; B=$(gc b)
cp "$root/docker/GSTCEF_COMMIT" "$g/docker/"; C=$(gc c)
H=$(gc h)
git -C "$g" checkout -q -b side; D=$(gc d); git -C "$g" checkout -q main
git -C "$g" tag v1.0.0 "$A"; git -C "$g" tag v1.1.0 "$B"; git -C "$g" tag v1.2.3 "$C"; git -C "$g" tag v1.2.4 "$D"
git -C "$g" update-ref refs/remotes/origin/main "$H"
git clone -q "$g" "$W/op"

# Fakes. State lives in $STATE; every call is logged to $STATE/log. flyctl's secret list is live:
# `secrets set --stage` stages a value (its digest: the value's cksum), `secrets deploy` deploys
# every staged one, unless a flag file says otherwise. flyctl logs which token it ran with: none
# (the stored login, promote's), the push token the test expects (the deploy workflows'), or WRONG.
cat > "$W/bin/flyctl" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ -z "${FLY_API_TOKEN+x}${FLY_ACCESS_TOKEN+x}" ]; then tok=none
elif [ -z "${FLY_ACCESS_TOKEN+x}" ] && [ "$FLY_API_TOKEN" = "${EXPECT_PUSH:-unset}" ]; then tok=push
else tok=WRONG; fi
echo "flyctl $* (token $tok) [cwd $PWD]" >> "$STATE/log"
s="$STATE/secrets.json"
put() { jq "$@" "$s" > "$s.tmp" && mv "$s.tmp" "$s"; }
case "$1 ${2:-}" in
  "auth docker") [ ! -f "$STATE/auth-fails" ] || { echo "Error: no access token available. Please login with 'flyctl auth login' (fake)" >&2; exit 1; } ;;
  "config show") [ ! -f "$STATE/config-fails" ] || { echo "Error: app not found (fake)" >&2; exit 1; }; cat "$STATE/config.json" ;;
  "secrets list") cat "$s" ;;
  "secrets set")
    [ ! -f "$STATE/set-fails" ] || { echo "Error: unauthorized (fake)" >&2; exit 1; }
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
if [ "$1 ${2:-}" = "buildx version" ]; then
  [ ! -f "$STATE/no-buildx" ] || { echo "docker: unknown command: docker buildx" >&2; exit 1; }
  echo "github.com/docker/buildx v0.0.0 fake"
  exit 0
fi
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
# `gh attestation verify oci://<repo>@<digest> --repo R --cert-identity I ...`, as gh 2.95.0's
# verify/policy.go enforces it: --cert-identity matches the signing certificate's SAN exactly,
# --source-ref, --source-digest and --signer-digest match those extensions when given (an omitted
# flag enforces nothing), --deny-self-hosted-runners requires a GitHub-hosted runner, and --repo
# scopes the lookup to that repository. gh reads the manifest by digest from the registry first.
# $STATE/attestations: one per line, `digest identity source_ref source_digest signer_digest runner repo`.
[ "$1 ${2:-}" = "attestation verify" ] || { echo "fake gh: unexpected: $*" >&2; exit 2; }
subject=$3; shift 3
repo='' ident='' sref='' sdig='' gdig='' deny=no
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo=$2; shift ;;
    --cert-identity) ident=$2; shift ;;
    --source-ref) sref=$2; shift ;;
    --source-digest) sdig=$2; shift ;;
    --signer-digest) gdig=$2; shift ;;
    --deny-self-hosted-runners) deny=yes ;;
    *) echo "fake gh: unexpected attestation flag $1" >&2; exit 2 ;;
  esac
  shift
done
[ -n "$repo" ] || { echo "fake gh: --repo or --owner is required" >&2; exit 2; }
[ -n "$ident" ] || { echo "fake gh: this fake models --cert-identity only" >&2; exit 2; }
if [ -f "$STATE/attest-down" ]; then echo "Error: failed to fetch attestations: HTTP 503 (fake)" >&2; exit 1; fi
name=${subject#oci://} digest=${subject##*@}; name=${name%@*}
# A manifest stays addressable by digest after its tag moves: $STATE/manifests lists those.
{ awk -v n="$name:" -v d="$digest" 'index($1, n) == 1 && $2 == d {f = 1} END {exit !f}' "$STATE/registry" \
    || { [ -f "$STATE/manifests" ] && awk -v n="$name" -v d="$digest" '$1 == n && $2 == d {f = 1} END {exit !f}' "$STATE/manifests"; }; } \
  || { echo "Error: $name@$digest: not found (fake)" >&2; exit 1; }
if awk -v d="$digest" -v i="$ident" -v r="$sref" -v s="$sdig" -v g="$gdig" -v deny="$deny" -v repo="$repo" '
    $1 == d && $2 == i && (r == "" || $3 == r) && (s == "" || $4 == s) && (g == "" || $5 == g) \
      && (deny == "no" || $6 == "github-hosted") && $7 == repo {f = 1} END {exit !f}' "$STATE/attestations"; then
  echo "Verification succeeded! (fake)"
  exit 0
fi
echo "Error: verifying with issuer \"sigstore.dev\": no attestation matched the policy (fake)" >&2
exit 1
SH
chmod +x "$W/bin/flyctl" "$W/bin/docker" "$W/bin/gh"
# A PATH with no flyctl at all (the fakes but flyctl, git and jq, then the system's).
mkdir -p "$W/bin-nofly"
cp "$W/bin/docker" "$W/bin/gh" "$W/bin-nofly/"
ln -s "$(command -v jq)" "$W/bin-nofly/jq"; ln -s "$(command -v git)" "$W/bin-nofly/git"

SHA=$C OLD=$A UNPUSHED=$H SIDE=$D BAD=$B PRE=$Z
SPOOF=5555555555555555555555555555555555555555
D_NEW=sha256:$(printf 'a%.0s' $(seq 64))
D_OLD=sha256:$(printf 'b%.0s' $(seq 64))
D_PROD=sha256:$(printf 'c%.0s' $(seq 64))
D_ANY=sha256:$(printf 'e%.0s' $(seq 64))
D_SQ=sha256:$(printf 'd%.0s' $(seq 64))
REPO=onryde/seazn.club.compositor
WFURL=https://github.com/$REPO/.github/workflows
ID_STG="$WFURL/deploy-stg.yml@refs/heads/main"
# attest_line DIGEST IDENTITY SOURCE_REF COMMIT: main's own build record, as deploy-<env>.yml signs it.
attest_line() { echo "$1 $2 $3 $4 $4 github-hosted $REPO"; }
REG=registry.example
digest_of() { printf '%s' "$1" | cksum | cut -d' ' -f1; }
PLOG="$W/home/.local/state/seazn-relay-promote"
state() {
  STATE="$W/state"; rm -rf "$STATE"; mkdir -p "$STATE"; : > "$STATE/log"; : > "$W/summary"; : > "$W/output"
  export STATE
  printf '%s\n' "$REG/seazn-relay-stg:$SHA $D_NEW" "$REG/seazn-relay-stg:$OLD $D_OLD" "$REG/seazn-relay-stg:$SIDE $D_ANY" \
    "$REG/seazn-relay-stg:$BAD $D_ANY" "$REG/seazn-relay-stg:$PRE $D_ANY" \
    "$REG/seazn-relay-prod:v1.2.3 $D_PROD" "$REG/seazn-relay-prod:v1.2.4 $D_ANY" "$REG/seazn-relay-prod:v1.1.0 $D_ANY" > "$STATE/registry"
  # Main's build records for what main pushed: deploy-stg at refs/heads/main for $SHA and $OLD,
  # deploy-prod at refs/tags/v1.2.3 for C. Nothing attests the D_ANY images.
  { attest_line "$D_NEW" "$ID_STG" refs/heads/main "$SHA"; attest_line "$D_OLD" "$ID_STG" refs/heads/main "$OLD"
    attest_line "$D_PROD" "$WFURL/deploy-prod.yml@refs/tags/v1.2.3" refs/tags/v1.2.3 "$C"; } > "$STATE/attestations"
  echo '{"app":"seazn-club-stg","env":{"OAUTH_BASE_URL":"https://stg.example"}}' > "$STATE/config.json"
  echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"},{"name":"RELAY_IMAGE","digest":"d0","status":"Deployed"}]' > "$STATE/secrets.json"
  # The operator's promote log: stg's newest entry is not what RELAY_IMAGE is now (Fly digest d7,
  # say a later promote's whose deploy failed), the one before it is (d0); prod's names v1.0.0.
  rm -rf "$W/home"; mkdir -p "$PLOG"
  printf '%s\n' "2026-10-20T10:00:00Z stg $OLD@$D_OLD $REG/seazn-relay-stg@$D_OLD d0" \
    "2026-10-22T10:00:00Z stg $SPOOF@$D_ANY $REG/seazn-relay-stg@$D_ANY d7" > "$PLOG/stg.log"
  echo "2026-10-21T10:00:00Z prod v1.0.0@$D_ANY $REG/seazn-relay-prod@$D_ANY d0" > "$PLOG/prod.log"
}
# run_step SCRIPT VAR=VALUE...: a workflow's block under the runner's shell (bash -eo pipefail), in
# the scratch repository, fakes first on PATH.
run_step() {
  local script=$1; shift
  if (cd "$g" && env -i PATH="$W/bin:$PATH" HOME="$W" RUNNER_TEMP="$W/rt" GITHUB_STEP_SUMMARY="$W/summary" \
    GITHUB_OUTPUT="$W/output" GITHUB_REPOSITORY=onryde/seazn.club.compositor GITHUB_RUN_ID=999 GITHUB_SERVER_URL=https://github.com \
    GITHUB_REF=refs/heads/main GITHUB_SHA="$H" STATE="$STATE" RELAY_REGISTRY="$REG" GH_TOKEN=fake-gh \
    EXPECT_PUSH=fake-stg-push "$@" \
    bash --noprofile --norc -eo pipefail "$script") > "$W/step.out" 2>&1; then rc=0; else rc=$?; fi
}
# promote_run [VAR=VALUE...] -- ARG...: scripts/promote.sh as the operator runs it, from their
# checkout, on their own login (no token in its environment unless a case puts one there). A
# SCRIPT=<path> pseudo-variable runs another copy.
promote_run() {
  local envs=() script="$W/op/scripts/promote.sh"
  while [ "$#" -gt 0 ] && [ "$1" != -- ]; do
    case "$1" in SCRIPT=*) script=${1#SCRIPT=} ;; *) envs+=("$1") ;; esac
    shift
  done
  [ "$#" -eq 0 ] || shift
  if (cd "$W/op" && env -i PATH="$W/bin:$PATH" HOME="$W/home" STATE="$STATE" PROMOTE_REMOTE="$g" RELAY_REGISTRY="$REG" \
      ${envs[@]+"${envs[@]}"} bash --noprofile --norc "$script" "$@") > "$W/step.out" 2>&1; then rc=0; else rc=$?; fi
}
has() { grep -qF -- "$2" "$1"; }
no_set() { ! grep -qE 'flyctl secrets (set|deploy)' "$STATE/log"; }
# No Fly or registry call (the tools check's `docker buildx version` reads nothing).
no_fly() { ! grep -E '^(flyctl|docker) ' "$STATE/log" | grep -vqF 'docker buildx version'; }
show() { sed 's/^/    /' "$W/step.out" "$STATE/log" "$W/summary" >&2; }
refusal() { grep -m 1 -F 'promote: REFUSED: ' "$W/step.out" | cut -c 1-220; }
# refused NAME TEXT [VAR=VALUE...] -- ARG...: promote stops, says TEXT, and made no Fly or registry call.
refused() {
  local name=$1 text=$2; shift 2
  state; promote_run "$@"
  if [ "$rc" -ne 0 ] && grep -F 'promote: REFUSED: ' "$W/step.out" | grep -qF -- "$text" && no_fly; then
    ok "promote $name :: $(refusal)"
  else
    bad "promote $name: rc $rc, expected a refusal before any flyctl or docker call saying: $text"; show
  fi
}
# refused_before_web NAME TEXT -- ARG...: promote stops, says TEXT, and never called the web app.
refused_before_web() {
  local name=$1 text=$2; shift 2
  promote_run "$@"
  if [ "$rc" -ne 0 ] && grep -F 'promote: REFUSED: ' "$W/step.out" | grep -qF -- "$text" && no_set \
    && ! grep -q 'seazn-club-' "$STATE/log"; then
    ok "promote $name :: $(refusal)"
  else
    bad "promote $name: rc $rc, expected a refusal before any call to the web app saying: $text"; show
  fi
}

# ---------------------------------------------------------------- 3. the deploy workflows' scripts
# The token guards.
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
extract "$STG" push token "$W/stg-token.sh"
guard "$W/stg-token.sh" false '::warning::FLY_API_TOKEN_RELAY_STG unset (a repository secret: docs/_INDEX.md, the C16 handoff) — push skipped' FLY_API_TOKEN=
guard "$W/stg-token.sh" true '' FLY_API_TOKEN=fake-stg-push
extract "$PROD" push token "$W/prod-token.sh"
guard "$W/prod-token.sh" false '::warning::FLY_API_TOKEN_RELAY_PROD unset (a repository secret, added only after C17' FLY_API_TOKEN=
guard "$W/prod-token.sh" true '' FLY_API_TOKEN=fake-prod-push

# deploy-stg's main check (I-2): main passes; a branch or a tag is refused.
extract "$STG" push provenance "$W/stg-provenance.sh"
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
# absent; any other registry error is a red with no exists= written. flyctl runs on the push token.
for wf in "$STG:seazn-relay-stg:$SHA:fake-stg-push" "$PROD:seazn-relay-prod:v1.2.3:fake-prod-push"; do
  IFS=: read -r file app pushed token <<< "$wf"
  extract "$file" push exists "$W/exists.sh"
  state; run_step "$W/exists.sh" FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG="$pushed"
  r1=$rc; out1=$(cat "$W/output"); tok1=$(grep -c 'flyctl auth docker (token push)' "$STATE/log" || true)
  state; run_step "$W/exists.sh" FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG=v7.7.7
  r2=$rc; out2=$(cat "$W/output")
  state; touch "$STATE/registry-down"; run_step "$W/exists.sh" FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG=v7.7.7
  if [ "$r1" -eq 0 ] && [ "$tok1" -eq 1 ] && [[ "$out1" == *"exists=true"* && "$out1" == *"digest=sha256:"* ]] \
    && [ "$r2" -eq 0 ] && [[ "$out2" == *"exists=false"* ]] \
    && [ "$rc" -ne 0 ] && has "$W/step.out" "::error::the registry could not be read for $REG/$app:v7.7.7" && [ ! -s "$W/output" ]; then
    ok "$file Already pushed?: pushed reads exists=true (on the push token); not found exists=false; a registry error is a red"
  else
    bad "$file Already pushed?: rc $r1/$r2/$rc"; echo "$out1 | $out2" >&2; sed 's/^/    /' "$W/step.out" >&2
  fi
done

# Main's build record (I-4), on both paths: a pushed or found digest must verify as this commit's
# build by this workflow at its own ref. A squat found under the tag is a red, never "pushed".
record_case() { # record_case FILE NAME WANT_RC TEXT VAR=VALUE...
  local file=$1 name=$2 want=$3 text=$4; shift 4
  extract "$file" push record "$W/record.sh"
  run_step "$W/record.sh" "$@"
  if { [ "$want" = 0 ] && [ "$rc" -eq 0 ] && has "$W/step.out" "$text"; } \
    || { [ "$want" != 0 ] && [ "$rc" -ne 0 ] && grep -F '::error::' "$W/step.out" | grep -qF -- "$text"; }; then
    ok "$file build record $name :: $(head -n 1 "$W/step.out" | cut -c 1-200)"
  else
    bad "$file build record $name: rc $rc, expected rc $want saying: $text"; show
  fi
}
# sq_state ROW: the squat in the registry under main's sha, the row (if any) as the only record of D_SQ.
sq_state() {
  state
  sed "s|^$REG/seazn-relay-stg:$SHA .*|$REG/seazn-relay-stg:$SHA $D_SQ|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"
  [ -z "$1" ] || echo "$1" >> "$STATE/attestations"
}
STG_REC=(RELAY_APP=seazn-relay-stg TAG="$SHA" GITHUB_SHA="$SHA")
PROD_REC=(RELAY_APP=seazn-relay-prod TAG=v1.2.3 GITHUB_SHA="$C" GITHUB_REF=refs/tags/v1.2.3)
state; record_case "$STG" "pushed and attested" 0 "$REG/seazn-relay-stg:$SHA@$D_NEW: attested by $ID_STG for $SHA" "${STG_REC[@]}" FOUND=false DIGEST="$D_NEW"
state; record_case "$STG" "found, main's own build" 0 "attested by $ID_STG for $SHA" "${STG_REC[@]}" FOUND=true DIGEST="$D_NEW"
sq_state "$D_SQ $WFURL/deploy-stg.yml@refs/heads/feat/x refs/heads/feat/x $SIDE $SIDE github-hosted $REPO"
record_case "$STG" "found, a squat a branch attested" 1 "$REG/seazn-relay-stg:$SHA is in the registry ($D_SQ) but is not main's own build: no attestation from $ID_STG for $SHA" \
  "${STG_REC[@]}" FOUND=true DIGEST="$D_SQ"
sq_state ""
record_case "$STG" "found, a squat with no record" 1 "is in the registry ($D_SQ) but is not main's own build" "${STG_REC[@]}" FOUND=true DIGEST="$D_SQ"
state; grep -vF "$D_NEW" "$STATE/attestations" > "$STATE/a.tmp" && mv "$STATE/a.tmp" "$STATE/attestations"
record_case "$STG" "pushed, its record never landed" 1 "this run's push of $REG/seazn-relay-stg:$SHA ($D_NEW) has no attestation that verifies" "${STG_REC[@]}" FOUND=false DIGEST="$D_NEW"
state; record_case "$STG" "no digest from either path" 1 "no digest for $REG/seazn-relay-stg:$SHA" "${STG_REC[@]}" FOUND=false DIGEST=
state; record_case "$PROD" "found, the release's own build" 0 "attested by $WFURL/deploy-prod.yml@refs/tags/v1.2.3 for $C" "${PROD_REC[@]}" FOUND=true DIGEST="$D_PROD"
state; grep -vF "$D_PROD" "$STATE/attestations" > "$STATE/a.tmp" && mv "$STATE/a.tmp" "$STATE/attestations"
attest_line "$D_PROD" "$ID_STG" refs/heads/main "$C" >> "$STATE/attestations"
record_case "$PROD" "found, only stg's build of the commit" 1 "is in the registry ($D_PROD) but is not this release's own build: no attestation from $WFURL/deploy-prod.yml@refs/tags/v1.2.3 for $C" \
  "${PROD_REC[@]}" FOUND=true DIGEST="$D_PROD"

# The summary: a found tag is never "Pushed"; both paths print the pinned promote command, and that
# exact command, run as printed, is one scripts/promote.sh accepts and promotes (the seam).
summary_cmd() { awk '/^```$/ {f = !f; next} f && /^scripts\/promote\.sh / {print; exit}' "$W/summary"; }
promotes_as_printed() { # promotes_as_printed NAME: run the summary's command through promote.sh
  local cmd words
  cmd=$(summary_cmd)
  read -r -a words <<< "$cmd"
  state; promote_run -- "${words[@]:1}"
  if [ -n "$cmd" ] && [ "${words[0]}" = scripts/promote.sh ] && [ "$rc" -eq 0 ] && has "$W/step.out" 'promoted '; then
    ok "$1: the summary's command, as printed, promotes ($cmd)"
  else
    bad "$1: the summary's command '$cmd' did not promote: rc $rc"; show
  fi
}
for found in true false; do
  extract "$STG" push summary "$W/summary.sh"
  state; run_step "$W/summary.sh" RELAY_APP=seazn-relay-stg TAG="$SHA" FOUND="$found" DIGEST="$D_NEW"
  common=false
  if [ "$rc" -eq 0 ] && [ "$(summary_cmd)" = "scripts/promote.sh stg $SHA@$D_NEW" ] \
    && has "$W/summary" "It stages \`RELAY_IMAGE=$REG/seazn-relay-stg@$D_NEW\` (the digest alone) on seazn-club-stg, then deploys it."; then
    common=true
  fi
  if [ "$found" = true ]; then
    if [ "$common" = true ] && has "$W/summary" "Found in the registry, not built by this run: \`$REG/seazn-relay-stg:$SHA\` (\`$D_NEW\`)" && ! has "$W/summary" 'Pushed'; then
      ok "deploy-stg summary, found: says found, not built by this run, and never Pushed; prints scripts/promote.sh stg <sha>@<digest>"
    else
      bad "deploy-stg summary, found: rc $rc"; sed 's/^/    /' "$W/summary" >&2
    fi
  else
    if [ "$common" = true ] && has "$W/summary" "Pushed and attested as main's build of $SHA" && ! has "$W/summary" 'Found in the registry'; then
      ok "deploy-stg summary, pushed: says pushed and attested, and RELAY_IMAGE by its digest alone"
    else
      bad "deploy-stg summary, pushed: rc $rc"; sed 's/^/    /' "$W/summary" >&2
    fi
    promotes_as_printed "deploy-stg summary"
  fi
done
extract "$PROD" push summary "$W/summary.sh"
state; run_step "$W/summary.sh" RELAY_APP=seazn-relay-prod TAG=v1.2.3 FOUND=true DIGEST="$D_PROD"
if [ "$rc" -eq 0 ] && has "$W/summary" 'Found in the registry, not built by this run' && ! has "$W/summary" 'Pushed' \
  && [ "$(summary_cmd)" = "scripts/promote.sh prod v1.2.3@$D_PROD --confirm-prod" ] \
  && has "$W/summary" "It stages \`RELAY_IMAGE=$REG/seazn-relay-prod@$D_PROD\` (the digest alone) on seazn-club-prod"; then
  ok "deploy-prod summary, found: says found, not built by this run, and never Pushed; prints the prod command with --confirm-prod"
else
  bad "deploy-prod summary, found: rc $rc"; sed 's/^/    /' "$W/summary" >&2
fi
promotes_as_printed "deploy-prod summary"

# deploy-prod's tag check, on the scratch repository (main z-a-b-c-h, side d).
extract "$PROD" push provenance "$W/tag.sh"
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

# ---------------------------------------------------------------- 4. scripts/promote.sh
if shellcheck --version 2> /dev/null | grep -qx 'version: 0.11.0' && shellcheck "$root/scripts/promote.sh" > "$W/sc.out" 2>&1; then
  ok "promote.sh: shellcheck 0.11.0 clean"
else
  bad "promote.sh: shellcheck 0.11.0 (pinned) not clean or not on PATH"; sed 's/^/    /' "$W/sc.out" >&2 2> /dev/null || true
fi

# P1: stg, the happy path.
state; promote_run -- stg "$SHA@$D_NEW"
new_value="$REG/seazn-relay-stg@$D_NEW"  # m-10: the attested digest alone, no tag a squat could move
set_line="flyctl secrets set RELAY_IMAGE=$new_value -a seazn-club-stg --stage (token none)"
if [ "$rc" -eq 0 ] && has "$STATE/log" "$set_line" && has "$STATE/log" 'flyctl auth docker (token none)' \
  && has "$STATE/log" 'flyctl config show -a seazn-club-stg (token none)' \
  && [ "$(grep -nF 'flyctl secrets set' "$STATE/log" | cut -d: -f1)" -lt "$(grep -nF 'flyctl secrets deploy -a seazn-club-stg (token none)' "$STATE/log" | cut -d: -f1)" ] \
  && ! grep -qE 'token (WRONG|push)' "$STATE/log" && ! has "$STATE/log" "[cwd $W/op" \
  && has "$STATE/log" "gh attestation verify oci://$REG/seazn-relay-stg@$D_NEW --repo $REPO --cert-identity $ID_STG --source-ref refs/heads/main --source-digest $SHA --signer-digest $SHA --deny-self-hosted-runners" \
  && [ "$(grep -nF 'gh attestation verify' "$STATE/log" | cut -d: -f1)" -lt "$(grep -nF 'flyctl config show' "$STATE/log" | cut -d: -f1)" ] \
  && has "$W/step.out" "  commit:                $SHA (on main; the CEF bump gate passed in its tree)" \
  && has "$W/step.out" "  build record:          attested by $ID_STG" \
  && has "$W/step.out" "  new RELAY_IMAGE:       $new_value" \
  && has "$W/step.out" "  previous RELAY_IMAGE:  $REG/seazn-relay-stg@$D_OLD (ref $OLD@$D_OLD)" \
  && has "$W/step.out" "  RELAY_IMAGE's digest:  d0 -> $(digest_of "$new_value") (Deployed)" \
  && has "$W/step.out" "Rollback: scripts/promote.sh stg $OLD@$D_OLD (it refuses" \
  && [ "$(tail -n 1 "$PLOG/stg.log" | cut -d' ' -f2-)" = "stg $SHA@$D_NEW $new_value $(digest_of "$new_value")" ]; then
  ok "promote P1: stg on the stored login (no token), main's build record verified before any call to web, RELAY_IMAGE set by digest then deployed; previous = the log entry whose Fly digest is live (not the newer d7 one), with its rollback; the promote logged"
else
  bad "promote P1: rc $rc"; show
fi

# P2: the ref was never pushed; P2b: the registry cannot be read (m-5: an error, never "not pushed").
state; refused_before_web "P2: an unpushed main commit" "not pushed: $REG/seazn-relay-stg:$UNPUSHED" -- stg "$UNPUSHED@$D_ANY"
state; touch "$STATE/registry-down"
refused_before_web "P2b (m-5): a registry error, not \"not pushed\"" "the registry could not be read for $REG/seazn-relay-stg:$SHA (only 'not found' means not pushed): ERROR: failed to authorize: 401 Unauthorized" -- stg "$SHA@$D_NEW"
# P2c: flyctl's docker login fails (no stored login): refused before any registry read.
state; touch "$STATE/auth-fails"
refused_before_web "P2c: no stored flyctl login" "flyctl auth docker failed (log in first: flyctl auth login)" -- stg "$SHA@$D_NEW"
if ! grep -q '^docker buildx imagetools' "$STATE/log"; then ok "promote P2c: no registry read after a failed login"; else bad "promote P2c read the registry"; show; fi

# P3: RELAY_IMAGE is an [env] entry: refuse and print the seazn.club diff.
state; echo '{"env":{"RELAY_IMAGE":"registry.example/seazn-relay-stg:old"}}' > "$STATE/config.json"
promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -ne 0 ] && has "$W/step.out" "promote: REFUSED: RELAY_IMAGE is an [env] entry in seazn-club-stg's config, not a secret" && no_set \
  && has "$W/step.out" 'Promote it by a seazn.club PR to fly.stg.toml' && has "$W/step.out" "+  RELAY_IMAGE = \"$new_value\"" \
  && has "$W/step.out" '-  RELAY_IMAGE = "registry.example/seazn-relay-stg:old"'; then
  ok "promote P3: an [env] RELAY_IMAGE is refused, with the fly.stg.toml diff printed"
else
  bad "promote P3: rc $rc"; show
fi
state; echo '{"env":{"RELAY_IMAGE":"x"}}' > "$STATE/config.json"
promote_run -- prod "v1.2.3@$D_PROD" --confirm-prod
if [ "$rc" -ne 0 ] && has "$W/step.out" 'Promote it by a seazn.club PR to fly.toml:' && no_set; then
  ok "promote P3b: prod's [env] diff names fly.toml"
else
  bad "promote P3b: rc $rc"; show
fi
# P3c, P3d: the web app's config or secret list unreadable: fail closed.
state; touch "$STATE/config-fails"
refused_promote_web() { # NAME TEXT -- ARG...: refused after a web read, with nothing set
  local name=$1 text=$2; shift 2
  promote_run "$@"
  if [ "$rc" -ne 0 ] && grep -F 'promote: REFUSED: ' "$W/step.out" | grep -qF -- "$text" && no_set; then
    ok "promote $name :: $(refusal)"
  else
    bad "promote $name: rc $rc, expected: $text"; show
  fi
}
refused_promote_web "P3c: the web app's config unreadable" "seazn-club-stg's config could not be read: Error: app not found (fake)" -- stg "$SHA@$D_NEW"
state; echo '"text"' > "$STATE/config.json"
refused_promote_web "P3d: a config that is no Fly config" "seazn-club-stg's config did not read as a Fly config" -- stg "$SHA@$D_NEW"
state; echo '{"name":"x"}' > "$STATE/secrets.json"
refused_promote_web "P3e: a secret list that is no list" "seazn-club-stg's secret list did not read as a list" -- stg "$SHA@$D_NEW"

# P4: a secret that is not Deployed would ship with RELAY_IMAGE (I-3): Staged, Unknown, Partial, or
# no status at all. The refusal names the app and no flyctl verb.
for status in '"status":"Staged"' '"status":"Unknown"' '"status":"partial"' ''; do
  state
  echo "[{\"name\":\"OTHER_KEY\",\"digest\":\"o1\"${status:+,$status}},{\"name\":\"RELAY_IMAGE\",\"digest\":\"d0\",\"status\":\"Deployed\"}]" > "$STATE/secrets.json"
  promote_run -- stg "$SHA@$D_NEW"
  err=$(refusal || true)
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
promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$STATE/log" "$set_line" && has "$STATE/log" 'flyctl secrets deploy -a seazn-club-stg (token none)'; then
  ok "promote P4e (I-3): RELAY_IMAGE staged alone does not refuse"
else
  bad "promote P4e: rc $rc"; show
fi

# P4f: the sequence: a promote whose deploy fails after --stage, then the rollback to the previous
# ref (pinned by digest) on the same state, which deploys and proves itself.
state; touch "$STATE/deploy-fails"
promote_run -- stg "$SHA@$D_NEW"
r1=$rc; e1=$(refusal || true); staged1=$(jq -r '.[] | select(.name == "RELAY_IMAGE") | "\(.digest) \(.status)"' "$STATE/secrets.json")
logged1=$(grep -c "stg $SHA@$D_NEW " "$PLOG/stg.log" || true)
: > "$STATE/log"
promote_run -- stg "$OLD@$D_OLD"
old_value="$REG/seazn-relay-stg@$D_OLD"
after2=$(jq -r '.[] | select(.name == "RELAY_IMAGE") | "\(.digest) \(.status)"' "$STATE/secrets.json")
if [ "$r1" -ne 0 ] && [[ "$e1" == *"flyctl secrets deploy failed on seazn-club-stg: RELAY_IMAGE is staged, not live"* ]] \
  && [ "$staged1" = "$(digest_of "$new_value") Staged" ] && [ "$logged1" -eq 0 ] && [ "$rc" -eq 0 ] \
  && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$old_value -a seazn-club-stg --stage (token none)" \
  && [ "$after2" = "$(digest_of "$old_value") Deployed" ]; then
  ok "promote P4f (I-3): a failed promote leaves RELAY_IMAGE Staged and unlogged, and the rollback after it deploys ($after2)"
else
  bad "promote P4f: rc $r1/$rc, staged '$staged1', logged $logged1, after '$after2'"; show
fi
# P4g: the set itself fails: refused, nothing deployed.
state; touch "$STATE/set-fails"
promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -ne 0 ] && [[ "$(refusal)" == *"flyctl secrets set failed on seazn-club-stg: nothing deployed"* ]] && ! has "$STATE/log" 'flyctl secrets deploy'; then
  ok "promote P4g: a failed set is refused, and nothing is deployed"
else
  bad "promote P4g: rc $rc"; show
fi

# P5: prod, with --confirm-prod first (the flag's place does not matter); its previous ref (v1.0.0,
# from prod's log) is gone from the registry.
state; promote_run -- --confirm-prod prod "v1.2.3@$D_PROD"
if [ "$rc" -eq 0 ] && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$REG/seazn-relay-prod@$D_PROD -a seazn-club-prod --stage (token none)" \
  && has "$STATE/log" 'flyctl secrets deploy -a seazn-club-prod (token none)' && ! has "$STATE/log" 'seazn-club-stg' \
  && has "$STATE/log" "--cert-identity $WFURL/deploy-prod.yml@refs/tags/v1.2.3 --source-ref refs/tags/v1.2.3 --source-digest $C --signer-digest $C" \
  && has "$W/step.out" "  commit:                $C (on main" \
  && has "$W/step.out" "  previous RELAY_IMAGE:  $REG/seazn-relay-prod@$D_ANY (ref v1.0.0@$D_ANY; no longer in the registry)" \
  && has "$W/step.out" 'Rollback: the previous ref cannot be verified now' \
  && [ "$(tail -n 1 "$PLOG/prod.log" | cut -d' ' -f2-3)" = "prod v1.2.3@$D_PROD" ] && [ "$(wc -l < "$PLOG/stg.log" | tr -d ' ')" -eq 2 ]; then
  ok "promote P5: prod promotes v1.2.3 (its commit on main) on seazn-club-prod; a vanished previous ref is said; logged in prod's log alone"
else
  bad "promote P5: rc $rc"; show
fi

# P6: the arguments, refused before any call; each env takes only its own ref shape (I-2).
refused "P6 a bad env" "env must be stg or prod: 'qa'" -- qa "$SHA@$D_NEW"
refused "P6 an injected ref" "a stg ref is a 40-hex main sha (deploy-stg's tag)@sha256:<digest>: 'main; touch /tmp/x@" -- stg "main; touch /tmp/x@$D_NEW"
refused "P6 a tag for stg" "a stg ref is a 40-hex main sha (deploy-stg's tag)@sha256:<digest>: 'v1.2.3@$D_PROD'" -- stg "v1.2.3@$D_PROD"
refused "P6 a sha for prod" "a prod ref is a vX.Y.Z release tag (deploy-prod's tag)@sha256:<digest>: '$SHA@$D_NEW'" -- prod "$SHA@$D_NEW" --confirm-prod
refused "P6 two digests" "the part after @ must be sha256:<64 hex>: '$SHA@$D_NEW@$D_OLD'" -- stg "$SHA@$D_NEW@$D_OLD"
refused "P6 a malformed digest" "the part after @ must be sha256:<64 hex>: '$SHA@sha256:xyz'" -- stg "$SHA@sha256:xyz"
# P16 (I-4): the pin is required: a bare ref is refused in either env, before any call.
refused "P16 (I-4): a stg ref with no digest" "a stg ref names its digest, <ref>@sha256:<digest> as deploy-stg.yml's summary prints it: the pin is required: '$SHA'" -- stg "$SHA"
refused "P16 (I-4): a prod ref with no digest" "a prod ref names its digest, <ref>@sha256:<digest> as deploy-prod.yml's summary prints it: the pin is required: 'v1.2.3'" -- prod v1.2.3 --confirm-prod
# F: the prod flag, both ways, so a stg invocation can never reach prod; and the other argument errors.
refused "F1: prod without --confirm-prod" "a prod promote needs --confirm-prod (and C17 passed first): nothing promoted" -- prod "v1.2.3@$D_PROD"
refused "F2: stg with --confirm-prod" "--confirm-prod is for a prod promote, and this one is stg: nothing promoted" -- stg "$SHA@$D_NEW" --confirm-prod
refused "F3: an unknown flag" "unknown flag '--force'" -- stg "$SHA@$D_NEW" --force
refused "F4: a third argument" "one env and one ref, not 'prod' too" -- stg "$SHA@$D_NEW" prod
refused "F5: no ref" "no ref: usage: scripts/promote.sh stg" -- stg
refused "F6: nothing at all" "env must be stg or prod: ''" --

# T: the tools, before any call.
state; touch "$STATE/no-buildx"; promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -ne 0 ] && [[ "$(refusal)" == *"docker buildx is not available"* ]] && no_fly; then ok "promote T1: refused when buildx is missing :: $(refusal)"; else bad "promote T1 (no buildx): rc $rc"; show; fi
refused "T2: no flyctl on PATH" "flyctl is not on PATH" PATH="$W/bin-nofly:/usr/bin:/bin" -- stg "$SHA@$D_NEW"

# K: no token is ever used, required or printed: one left in the shell is dropped, flyctl runs on the
# stored login, and the value appears nowhere in the output.
state; promote_run FLY_API_TOKEN=leaked-fly-value FLY_ACCESS_TOKEN=leaked-access-value -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$STATE/log" "$set_line" && ! grep -qE 'token (WRONG|push)' "$STATE/log" \
  && [ "$(grep -c '^flyctl ' "$STATE/log")" -eq "$(grep -c '^flyctl .*(token none)' "$STATE/log")" ] \
  && ! has "$W/step.out" 'leaked-' && ! has "$STATE/log" 'leaked-' \
  && has "$W/step.out" 'promote: FLY_API_TOKEN is set in this shell; it is dropped, and flyctl uses your stored login' \
  && has "$W/step.out" 'promote: FLY_ACCESS_TOKEN is set in this shell; it is dropped'; then
  ok "promote K: tokens left in the shell are dropped, unprinted; all $(grep -c '^flyctl ' "$STATE/log") flyctl calls ran on the stored login"
else
  bad "promote K: rc $rc"; show
fi

# S: main's copy. An edited (or stale) copy of this script refuses; so does a main that cannot be fetched.
cp "$W/op/scripts/promote.sh" "$W/op/scripts/promote-edited.sh"; echo '# a local edit' >> "$W/op/scripts/promote-edited.sh"
refused "S1: a copy that is not main's" "$W/op/scripts/promote-edited.sh is not main's copy (main is $H)" SCRIPT="$W/op/scripts/promote-edited.sh" -- stg "$SHA@$D_NEW"
refused "S2: main cannot be fetched" "main could not be fetched from $W/no-such-repo" PROMOTE_REMOTE="$W/no-such-repo" -- stg "$SHA@$D_NEW"
# S3: main fetched into refs/promote/main, never the operator's own refs.
if [ "$(git -C "$W/op" rev-parse refs/promote/main)" = "$H" ] && [ "$(git -C "$W/op" rev-parse refs/remotes/origin/main)" = "$H" ] \
  && [ -z "$(git -C "$W/op" status --porcelain --untracked-files=no)" ]; then
  ok "promote S3: main fetched into refs/promote/main; the operator's branches and working tree untouched"
else
  bad "promote S3: refs or the working tree changed"; git -C "$W/op" show-ref >&2
fi

# G: where the image came from (I-2), each refused before any Fly call. For stg the tag is the
# commit, so the message reads "<sha> (<sha>) is not on main".
refused "G1 (I-2): a stg sha off main" "$SIDE ($SIDE) is not on main: images are promoted from main only" -- stg "$SIDE@$D_ANY"
refused "G2 (I-2): a sha that is no commit here" "no commit 4444444444444444444444444444444444444444 in main's history" \
  -- stg "4444444444444444444444444444444444444444@$D_ANY"
refused "G3 (I-2): a prod tag off main" "v1.2.4 ($SIDE) is not on main: images are promoted from main only" -- prod "v1.2.4@$D_ANY" --confirm-prod
refused "G4 (I-2): a prod tag that does not exist" "no tag v9.9.9 in $g" -- prod "v9.9.9@$D_ANY" --confirm-prod
refused "G5 (I-2): a main commit whose own tree fails the CEF bump gate" "$BAD ($BAD) fails the CEF bump gate (Owed 22) in its own tree" -- stg "$BAD@$D_ANY"
refused "G6 (I-2): a main commit from before the record" "$PRE ($PRE) has no docker/CEF_STEP0_PASS" -- stg "$PRE@$D_ANY"
refused "G7 (I-2): a prod tag whose tree fails the CEF bump gate" "v1.1.0 ($BAD) fails the CEF bump gate (Owed 22) in its own tree" -- prod "v1.1.0@$D_ANY" --confirm-prod
# G5's red is the gate's own reason, read from the promoted tree, not main's (main's tree passes).
state; promote_run -- stg "$BAD@$D_ANY"
if has "$W/step.out" "cef bump gate: FAIL: docker/GSTCEF_COMMIT=$bump, but C11 Step 0 last passed on $pin (F-009)"; then
  ok "promote G5: the gate ran on the promoted commit's tree (GSTCEF_COMMIT=$bump there)"
else
  bad "promote G5's gate output"; show
fi

# L: the previous RELAY_IMAGE, from the promote log.
state; rm -rf "$PLOG"; promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$W/step.out" "  previous RELAY_IMAGE:  set elsewhere (Fly digest d0; the value is write-only, and no promote logged on this machine set it)" \
  && has "$W/step.out" 'Rollback: no earlier promote of stg on this machine to return to' && [ "$(wc -l < "$PLOG/stg.log" | tr -d ' ')" -eq 1 ]; then
  ok "promote L1: no log on this machine: the previous value is said to be set elsewhere, never guessed; the log is created"
else
  bad "promote L1: rc $rc"; show
fi
# L2 (the sequence): promote, then promote back; the second names the first as its previous value.
state; promote_run -- stg "$SHA@$D_NEW"; r1=$rc
promote_run -- stg "$OLD@$D_OLD"
if [ "$r1" -eq 0 ] && [ "$rc" -eq 0 ] && has "$W/step.out" "  previous RELAY_IMAGE:  $new_value (ref $SHA@$D_NEW)" \
  && has "$W/step.out" "Rollback: scripts/promote.sh stg $SHA@$D_NEW (" && [ "$(wc -l < "$PLOG/stg.log" | tr -d ' ')" -eq 4 ]; then
  ok "promote L2: a second promote names the first (logged by it) as its previous value, with the rollback"
else
  bad "promote L2: rc $r1/$rc"; show
fi
# L3 (P9): the first promote: no RELAY_IMAGE before.
state; echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"}]' > "$STATE/secrets.json"
promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$W/step.out" "  previous RELAY_IMAGE:  unset (this is seazn-club-stg's first RELAY_IMAGE)" \
  && has "$W/step.out" "  RELAY_IMAGE's digest:  unset -> $(digest_of "$new_value") (Deployed)" && has "$W/step.out" 'Rollback: no earlier promote of stg on this machine'; then
  ok "promote L3: the first promote says the value was unset"
else
  bad "promote L3: rc $rc"; show
fi
# L4: the log cannot be written: the promote stands, and says the next one cannot name it.
state; : > "$W/home/not-a-dir"
promote_run PROMOTE_STATE_DIR="$W/home/not-a-dir" -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$W/step.out" "NOT logged ($W/home/not-a-dir/stg.log could not be written): the next promote cannot name this one"; then
  ok "promote L4: an unwritable log is said, and the promote stands"
else
  bad "promote L4: rc $rc"; show
fi
# P15 (m-5): the previous logged ref's tag has moved since: said, and no rollback offered.
state; sed "s|stg $OLD@$D_OLD $REG/seazn-relay-stg@$D_OLD d0|stg $OLD@$D_PROD $REG/seazn-relay-stg@$D_PROD d0|" "$PLOG/stg.log" > "$PLOG/x" && mv "$PLOG/x" "$PLOG/stg.log"
promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$W/step.out" "  previous RELAY_IMAGE:  $REG/seazn-relay-stg@$D_PROD (ref $OLD@$D_PROD; the tag has moved since, to $D_OLD)" \
  && has "$W/step.out" 'Rollback: the previous ref cannot be verified now' && ! has "$W/step.out" 'Rollback: scripts/promote.sh'; then
  ok "promote P15 (m-5): a previous pinned ref whose tag moved is said, with no rollback offered"
else
  bad "promote P15: rc $rc"; show
fi

# P11 to P13 (m-7): the deploy's proof. RELAY_IMAGE gone, still Staged, or another digest: each red.
state; touch "$STATE/deploy-drops-image"; promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -ne 0 ] && [[ "$(refusal)" == *'RELAY_IMAGE is not set on seazn-club-stg after the deploy'* ]]; then
  ok "promote P11: a deploy that leaves RELAY_IMAGE unset is a red"
else
  bad "promote P11: rc $rc"; show
fi
state; touch "$STATE/deploy-leaves-staged"; promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -ne 0 ] && [[ "$(refusal)" == *"RELAY_IMAGE on seazn-club-stg reads 'Staged' after the deploy, not Deployed: not proven live"* ]]; then
  ok "promote P12 (m-7): RELAY_IMAGE still Staged after the deploy is a red"
else
  bad "promote P12: rc $rc"; show
fi
state; touch "$STATE/deploy-moves-digest"; promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -ne 0 ] && [[ "$(refusal)" == *"RELAY_IMAGE's Fly digest on seazn-club-stg is 999 after the deploy, not the $(digest_of "$new_value") this promote staged"* ]]; then
  ok "promote P13 (m-7): another digest after the deploy is a red"
else
  bad "promote P13: rc $rc"; show
fi

# P14 (m-5): a ref pinned to its digest promotes; pinned to another digest (the tag moved) it refuses.
state; promote_run -- stg "$SHA@$D_NEW"
r1=$rc; l1=$(grep -c "flyctl secrets set RELAY_IMAGE=$new_value " "$STATE/log" || true)
state
refused_before_web "P14 (m-5): the tag no longer at the pin" "$REG/seazn-relay-stg:$SHA resolves to $D_NEW, not the $D_OLD this promote names: the tag moved, nothing promoted" -- stg "$SHA@$D_OLD"
if [ "$r1" -eq 0 ] && [ "$l1" -eq 1 ]; then ok "promote P14's positive pair: ref@digest promotes while the tag resolves to it"; else bad "promote P14 positive pair: rc $r1, $l1 set lines"; fi

# P17 (I-4): a squatted tag. Some other push (a branch's workflow reads the repository push token)
# put $D_SQ under main's sha before main's build, and the operator is handed $SHA@$D_SQ: every check
# before it passes (the commit is on main, its tree passes the gate, the tag resolves to the pin).
# Only main's build record can refuse it, so the record is swept field by field: each forged
# attestation differs from main's in ONE field, so each enforcing flag promote passes is needed by
# one row. The positive pair (main's record for D_SQ) promotes, so the refusals are the fields',
# not the fake's.
GOOD_SQ="$D_SQ $ID_STG refs/heads/main $SHA $SHA github-hosted $REPO"
sq_rows=(
  "none|"
  "a branch's workflow ref|$D_SQ $WFURL/deploy-stg.yml@refs/heads/feat/x refs/heads/main $SHA $SHA github-hosted $REPO"
  "a branch's source ref|$D_SQ $ID_STG refs/heads/feat/x $SHA $SHA github-hosted $REPO"
  "another commit's build|$D_SQ $ID_STG refs/heads/main $SIDE $SHA github-hosted $REPO"
  "another signer digest|$D_SQ $ID_STG refs/heads/main $SHA $SIDE github-hosted $REPO"
  "a self-hosted runner|$D_SQ $ID_STG refs/heads/main $SHA $SHA self-hosted $REPO"
  "another workflow on main|$D_SQ $WFURL/soak.yml@refs/heads/main refs/heads/main $SHA $SHA github-hosted $REPO"
  "deploy-prod's identity|$D_SQ $WFURL/deploy-prod.yml@refs/tags/v1.2.3 refs/heads/main $SHA $SHA github-hosted $REPO"
  "another repository|$D_SQ $ID_STG refs/heads/main $SHA $SHA github-hosted evil/seazn.club.compositor"
  "the attestation service down|$GOOD_SQ"
)
sq_checked=0
for row in "${sq_rows[@]}"; do
  label=${row%%|*} line=${row#*|}
  sq_state "$line"
  [ "$label" != "the attestation service down" ] || touch "$STATE/attest-down"
  sq_checked=$((sq_checked + 1))
  refused_before_web "P17 (I-4): a squatted digest with $label" "$REG/seazn-relay-stg:$SHA@$D_SQ is not main's own build: no attestation from $ID_STG for $SHA" -- stg "$SHA@$D_SQ"
done
sq_state "$GOOD_SQ"
promote_run -- stg "$SHA@$D_SQ"
if [ "$rc" -eq 0 ] && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$REG/seazn-relay-stg@$D_SQ -a seazn-club-stg --stage (token none)"; then
  ok "promote P17 (I-4): the positive pair: main's own record of that digest promotes it ($sq_checked forged or missing records refused)"
else
  bad "promote P17 positive pair: rc $rc"; show
fi
[ "$sq_checked" -eq 10 ] || bad "promote P17 swept $sq_checked rows, not 10"
# P17b (I-4): main pushed and attested $D_NEW, then a squat moved the tag to $D_SQ. The operator
# holds main's real pin, which verifies; promote must still refuse, or it would ship a digest nobody
# verified.
sq_state ""
echo "$REG/seazn-relay-stg $D_NEW" > "$STATE/manifests"  # main's push, untagged now, still there by digest
refused_before_web "P17b (I-4): main's own pin once a squat has moved the tag" "$REG/seazn-relay-stg:$SHA resolves to $D_SQ, not the $D_NEW this promote names: the tag moved, nothing promoted" -- stg "$SHA@$D_NEW"
# Prod: the same commit's stg build record does not serve a release, and neither does a branch's.
state
sed "s|^$REG/seazn-relay-prod:v1.2.3 .*|$REG/seazn-relay-prod:v1.2.3 $D_SQ|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"
attest_line "$D_SQ" "$ID_STG" refs/heads/main "$C" >> "$STATE/attestations"
refused_before_web "P17 (I-4): prod refuses a digest whose only record is stg's build of the same commit" "$REG/seazn-relay-prod:v1.2.3@$D_SQ is not main's own build: no attestation from $WFURL/deploy-prod.yml@refs/tags/v1.2.3 for $C" -- prod "v1.2.3@$D_SQ" --confirm-prod

echo "checked: $checked, failed: $failures"
finished=1
if [ "$checked" -eq 0 ]; then
  echo "deploy_checks_test: FAIL: checked nothing" >&2
  exit 1
fi
[ "$failures" -eq 0 ] || exit 1
exit 0
