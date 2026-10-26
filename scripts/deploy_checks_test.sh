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
#    main only, the build record's order, the write grants, no promote workflow), and t1 to t27 for
#    rule 16, the moving tags (amendment 2026-10-26, and the PR #94 review's fix round).
# 2. scripts/check-cef-bump.sh: green on the repo; red on the brief's mutant (3), GSTCEF_COMMIT
#    changed without the record, on each malformed record, and on a record whose finding is no
#    Step 0 run on its commit (m-8); green again once the record and its finding follow the bump.
# 3. The deploy workflows' own scripts, extracted as written and run under fake flyctl, docker and gh:
#    the token guards, the provenance steps, "Already pushed?", the Push step (its digest is the one its
#    own push printed, and a tag moved under it is a red: the PR #93 review's C-1), the build record
#    checks (pushed, found, squatted), the login leaving with each step, the summaries, whose
#    promote command the real scripts/promote.sh accepts, and the track steps (amendment 2026-10-26):
#    each moves its tag to the record check's verified digest by a carbon copy, reads it back, and
#    refuses every other shape, alone and chained through the steps' own outputs.
# 4. scripts/promote.sh, as the operator runs it (from a clone of a scratch "GitHub" repository,
#    main: z, a, b, c, h; side: d), under fake flyctl, curl and gh (no docker: the review's I-1): every
#    refusal (the arguments and the prod flag, the test seams, the tools, main's copy, provenance, the
#    registry read by digest and its body's hash, main's build record field by field, the web app's
#    [env] and staged secrets, the secret list after the set and the deploy), the read token never in
#    argv, the output or an error, the previous RELAY_IMAGE from the promote log, the deploy and its
#    proof, and a failed promote followed by its rollback; and --track (TR1 to TR7): the tag's current
#    digest verified as main's build before RELAY_IMAGE names the tag, each refusal, and the sequence
#    track, pin, track.
#
# Needs bash, ruby (YAML), jq, python3, git, and the pinned actionlint and shellcheck on PATH
# (scripts/check-workflows.sh --install DIR). Prints `checked: N` and fails on 0.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
# Physical (pwd -P): on macOS mktemp says /var/..., a cd reports /private/var/..., and a negative grep
# on one spelling passes on the other (the PR #93 review's m-10).
W=$(cd "$(mktemp -d)" && pwd -P)
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
# edit_after FILE MARKER OLD NEW: replace the first OLD after MARKER, which must occur exactly once.
edit_after() {
  python3 -I -c '
import sys
p, m, old, new = sys.argv[1:5]
s = open(p).read()
if s.count(m) != 1:
    sys.exit(f"edit_after: {p}: {s.count(m)} matches for marker {m!r}")
i = s.index(m)
j = s.find(old, i)
if j < 0:
    sys.exit(f"edit_after: {p}: no {old!r} after {m!r}")
open(p, "w").write(s[:j] + new + s[j + len(old):])' "$@"
}
# cut FILE A B: delete the text from marker A up to (not including) marker B, each once; with APPEND=1, put
# it at the end of the file instead.
cut_block() {
  python3 -I -c '
import os, sys
p, a, b = sys.argv[1:4]
s = open(p).read()
if s.count(a) != 1 or s.count(b) != 1:
    sys.exit(f"cut_block: {p}: markers {a!r} x{s.count(a)}, {b!r} x{s.count(b)}")
i, j = s.index(a), s.index(b)
block = s[i:j]
s = s[:i] + s[j:]
if os.environ.get("APPEND") == "1":
    s = s.rstrip("\n") + "\n" + block
open(p, "w").write(s)' "$@"
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
  && grep -qF "promote workflows: 0 lines changing Fly secrets in $n_repo workflows" "$W/check.out" \
  && grep -qF 'logins that leave with their step: 8; actions in the push jobs pinned by commit: 8' "$W/check.out" \
  && grep -qF 'moving tags: 2 of 2 push jobs move theirs (deploy-stg.yml :beta, deploy-prod.yml :stable) to the record check'"'"'s verified digest and read it back; retag lines: 2, each a track step'"'"'s own copy' "$W/check.out"; then
  ok "check-workflows green on the repo ($n_repo workflows, 2 deploy, no environment, 2 relay tokens in their push jobs, no web token, 2 provenance steps, 2 main-only ifs, 2 build records, 5 write grants, no promote workflow, 8 logins that leave with their step, 8 pinned actions, 2 moving tags)"
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
          GH_TOKEN: ${{ github.token }}' '        env:
          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG || secrets.FLY_API_TOKEN_RELAY_PROD }}
          GH_TOKEN: ${{ github.token }}'
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
          moving="$RELAY_REGISTRY/$RELAY_APP:beta"' '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          cd $RUNNER_TEMP
          moving="$RELAY_REGISTRY/$RELAY_APP:beta"'
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
          GH_TOKEN: ${{ github.token }}' '          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}
          WEB: ${{ secrets.FLY_API_TOKEN_WEB_STG }}
          GH_TOKEN: ${{ github.token }}'
expect_red "i1a deploy-stg names a web token" 'deploy-stg.yml names the secret FLY_API_TOKEN_WEB_STG (push): no web-app token is ever in GitHub'

# i1b. A token of unknown reach (the first round's FLY_API_TOKEN_STG) in a deploy workflow.
fresh; edit_all "$W/wf/$STG" 'FLY_API_TOKEN_RELAY_STG' 'FLY_API_TOKEN_STG'
expect_red "i1b deploy-stg names FLY_API_TOKEN_STG" 'deploy-stg.yml names the secret FLY_API_TOKEN_STG (push): a deploy workflow names only its own relay token (FLY_API_TOKEN_RELAY_STG)'

# i1d. A secret named by expression, which no rule can read.
fresh; edit "$W/wf/$STG" '          FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN_RELAY_STG }}
          GH_TOKEN: ${{ github.token }}' "          FLY_API_TOKEN: \${{ secrets['FLY_API_TOKEN_RELAY_STG'] }}
          GH_TOKEN: \${{ github.token }}"
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
          GH_TOKEN: ${{ github.token }}' '          FLY_API_TOKEN: ${{ secrets.fly_api_token_relay_stg }}
          GH_TOKEN: ${{ github.token }}'
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

PUSH_TRAP="          trap 'docker logout \"\$RELAY_REGISTRY\" > /dev/null 2>&1 || true' EXIT"
# n16 (the PR #93 review's m-1). Every secret at once, by toJSON in any case: no rule can read it.
fresh; edit "$W/wf/$STG" '          SKIPPED: ${{ steps.track.outputs.skipped }}
        run: |
          ref=' '          SKIPPED: ${{ steps.track.outputs.skipped }}
          ALL: ${{ toJson(secrets) }}
        run: |
          ref='
expect_red "n16 toJson(secrets) in deploy-stg" 'deploy-stg.yml names a secret by expression'
fresh; edit "$W/wf/soak.yml" '  plan:
    runs-on: ubuntu-24.04' '  plan:
    env:
      ALL: ${{ tojson( secrets ) }}
    runs-on: ubuntu-24.04'
expect_red "n16b tojson( secrets ) in soak.yml" 'soak.yml names a secret by expression'

# n17 (m-1). A reusable workflow outside this repository handed secrets: inherit, or a map.
fresh; edit "$W/wf/soak.yml" '  plan:
    runs-on: ubuntu-24.04' '  ext:
    uses: someone/else/.github/workflows/x.yml@0123456789abcdef0123456789abcdef01234567
    secrets: inherit
  plan:
    runs-on: ubuntu-24.04'
expect_red "n17 secrets: inherit to another repository's workflow" 'soak.yml: job ext hands secrets (inherit) to a workflow outside this repository: someone/else/.github/workflows/x.yml@'
fresh; edit "$W/wf/soak.yml" '  plan:
    runs-on: ubuntu-24.04' '  ext:
    uses: someone/else/.github/workflows/x.yml@0123456789abcdef0123456789abcdef01234567
    secrets:
      T: ${{ secrets.FLY_API_TOKEN_SOAK }}
  plan:
    runs-on: ubuntu-24.04'
expect_red "n17b a secrets map to another repository's workflow" 'soak.yml: job ext hands secrets (a map) to a workflow outside this repository'

# n18 (m-2). A docker login with no logout trap: the token would outlive its step in docker's config.
fresh; edit "$W/wf/$STG" "$PUSH_TRAP"'
          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only' '          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          # Only'
expect_red "n18 deploy-stg's Already pushed? with no logout" 'push.Already pushed? logs docker in (flyctl auth docker) with no'
fresh; edit "$W/wf/$PROD" "$PUSH_TRAP"'
          flyctl auth docker
          identity=' '          flyctl auth docker
          identity='
expect_red "n18b deploy-prod's record check with no logout" 'logs docker in (flyctl auth docker) with no'
# ... and a trap that logs out of another host is no logout.
fresh; edit "$W/wf/$STG" "$PUSH_TRAP"'
          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker tag' "          trap 'docker logout ghcr.io > /dev/null 2>&1 || true' EXIT"'
          flyctl auth docker
          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker tag'
expect_red "n18c deploy-stg's Push logging out of another host" 'push.Push logs docker in (flyctl auth docker) with no'

# n19 (m-2). An action in a push job pinned by tag, not commit.
fresh; edit "$W/wf/$STG" '      - uses: docker/setup-buildx-action@8d2750c68a42422c14e847fe6c8ac0403b4cbd6f # v3.12.0' '      - uses: docker/setup-buildx-action@v3'
expect_red "n19 deploy-stg's setup-buildx by tag" 'uses docker/setup-buildx-action@v3, not pinned by commit'

# n20 to n23 (m-11): each flag the record check must pass, removed in turn.
fresh; edit "$W/wf/$STG" '.github/workflows/deploy-stg.yml@refs/heads/main"' '.github/workflows/deploy-stg.yml@$GITHUB_REF"'
expect_red "n20 deploy-stg's record identity at the run's own ref" "deploy-stg.yml (build record): job push: the record check does not read 'deploy-stg.yml@refs/heads/main'"
fresh; edit "$W/wf/$PROD" '.github/workflows/deploy-prod.yml@refs/tags/$TAG"' '.github/workflows/deploy-prod.yml@$GITHUB_REF"'
expect_red "n20b deploy-prod's record identity at the run's own ref" "deploy-prod.yml (build record): job push: the record check does not read 'deploy-prod.yml@refs/tags/\$TAG'"
fresh; edit "$W/wf/$STG" ' --source-ref refs/heads/main --source-digest' ' --source-digest'
expect_red "n21 deploy-stg's record check without --source-ref" "deploy-stg.yml (build record): job push: the record check does not read '--source-ref refs/heads/main'"
fresh; edit "$W/wf/$PROD" ' --source-ref "refs/tags/$TAG" --source-digest' ' --source-digest'
expect_red "n21b deploy-prod's record check without --source-ref" "deploy-prod.yml (build record): job push: the record check does not read '--source-ref \"refs/tags/\$TAG\"'"
fresh; edit "$W/wf/$STG" " --source-digest \"\$GITHUB_SHA\" \\" " \\"
expect_red "n22 deploy-stg's record check without --source-digest" "deploy-stg.yml (build record): job push: the record check does not read '--source-digest \"\$GITHUB_SHA\"'"
fresh; edit "$W/wf/$STG" ' --deny-self-hosted-runners --limit 1000' ' --limit 1000'
expect_red "n23 deploy-stg's record check without --deny-self-hosted-runners" "deploy-stg.yml (build record): job push: the record check does not read '--deny-self-hosted-runners'"

# n24. A token step's `if` that reads the guard and then widens it (`&& X` is allowed; `|| X` is not).
fresh; edit "$W/wf/$STG" "      - name: Push
        id: push
        if: steps.token.outputs.enabled == 'true' && steps.exists.outputs.exists != 'true'" "      - name: Push
        id: push
        if: steps.token.outputs.enabled == 'true' && steps.exists.outputs.exists != 'true' || github.event_name == 'push'"
expect_red "n24 deploy-stg's Push behind a widened guard" 'push.Push holds FLY_API_TOKEN_RELAY_STG but runs whether or not the guard found it'
fresh; edit "$W/wf/$STG" "      - name: Free disk space
        if: steps.token.outputs.enabled == 'true'
" "      - name: Free disk space
        if: steps.token.outputs.enabled == 'true' && github.event_name == 'push' || always()
"
expect_red "n24b deploy-stg's Free disk space behind a widened guard" 'push.Free disk space comes after the FLY_API_TOKEN_RELAY_STG guard but does not read it'

# t1 to t27 (owner, 2026-10-26: the moving tags, rule 16): each red by its own reason. TRACK_STG and
# TRACK_PROD are each push job's track step, up to the summary.
TRACK_STG='      # The moving tag (owner, 2026-10-26: ":beta" on every merge)'
TRACK_PROD='      # The moving tag (owner, 2026-10-26: ":stable" on every release tag)'
SUMMARY_STEP='      - name: Summary
'
# move_after_copy FILE START (t25, t26: a check after the copy has already moved the tag): cut the block
# from the line START to its fi, and paste it after the copy.
move_after_copy() {
  python3 -I -c '
import sys
p, start = sys.argv[1:3]
s = open(p).read()
assert s.count(start) == 1, f"{s.count(start)} matches for {start!r}"
i = s.index(start)
j = s.index("\n          fi\n", i) + len("\n          fi\n")
block, s = s[i:j], s[:i] + s[j:]
k = s.index("\n", s.index("docker buildx imagetools create --prefer-index=false")) + 1
open(p, "w").write(s[:k] + block + s[k:])
' "$1" "$2"
}

# t1. No track step: nothing moves :beta.
fresh; cut_block "$W/wf/$STG" "$TRACK_STG" "$SUMMARY_STEP"
expect_red "t1 deploy-stg without its track step" 'deploy-stg.yml (moving tag): job push has 0 steps with id track, not 1'

# t2. The track step before the record check: it would move :beta to a digest nothing verified.
fresh; move_before "$W/wf/$STG" "$TRACK_STG" "$SUMMARY_STEP" '      # Both paths: the digest this run pushed'
expect_red "t2 deploy-stg moves :beta before its record check" 'deploy-stg.yml (moving tag): job push: the track step comes before the record check'

# t3. The summary before the track step (the track step appended after it).
fresh; APPEND=1 cut_block "$W/wf/$STG" "$TRACK_STG" "$SUMMARY_STEP"
expect_red "t3 deploy-stg's summary before its track step" 'deploy-stg.yml (moving tag): job push: the summary does not come after the track step'

# t4. The track step on the pushed path only (rule 2 allows `guard && X`; rule 16 does not).
fresh; edit "$W/wf/$STG" "      - name: Move :beta to the verified build
        id: track
        if: steps.token.outputs.enabled == 'true'" "      - name: Move :beta to the verified build
        id: track
        if: steps.token.outputs.enabled == 'true' && steps.exists.outputs.exists != 'true'"
expect_red "t4 deploy-stg's track step skipped on the found path" "deploy-stg.yml (moving tag): job push: the track step runs on if 'steps.token.outputs.enabled == 'true' && steps.exists.outputs.exists != 'true''"

# t5. The track step reads the push's or the found tag's digest directly, not the record check's output.
fresh; edit "$W/wf/$STG" '          DIGEST: ${{ steps.record.outputs.digest }}' '          DIGEST: ${{ steps.push.outputs.digest || steps.exists.outputs.digest }}'
expect_red "t5 deploy-stg's track DIGEST not the record check's" "deploy-stg.yml (moving tag): job push: the track step's DIGEST is"

# t6. The track step re-reads the digest by tag.
fresh; edit "$W/wf/$STG" '          moving="$RELAY_REGISTRY/$RELAY_APP:beta"
          # The old :beta' '          DIGEST=$(docker buildx imagetools inspect "$RELAY_REGISTRY/$RELAY_APP:$TAG" --format '"'"'{{.Manifest.Digest}}'"'"')
          moving="$RELAY_REGISTRY/$RELAY_APP:beta"
          # The old :beta'
expect_red "t6 deploy-stg's track step re-reads DIGEST by tag" 'deploy-stg.yml (moving tag): job push: the track step assigns DIGEST: DIGEST=$(docker buildx imagetools inspect'

# t7. The copy without --prefer-index=false: buildx would wrap the manifest in a new index, a new digest.
fresh; edit "$W/wf/$STG" 'imagetools create --prefer-index=false --tag' 'imagetools create --tag'
expect_red "t7 deploy-stg's copy without --prefer-index=false" 'deploy-stg.yml (moving tag): job push: the track step'"'"'s copy is '"'"'docker buildx imagetools create --tag "$moving"'

# t8. The copy from the sha's tag, not the verified digest.
fresh; edit "$W/wf/$STG" '--tag "$moving" "$RELAY_REGISTRY/$RELAY_APP@$DIGEST"' '--tag "$moving" "$RELAY_REGISTRY/$RELAY_APP:$TAG"'
expect_red "t8 deploy-stg copies from the tag" 'deploy-stg.yml (moving tag): job push: the track step'"'"'s copy is '"'"'docker buildx imagetools create --prefer-index=false --tag "$moving" "$RELAY_REGISTRY/$RELAY_APP:$TAG"'

# t9. stg moves prod's tag name.
fresh; edit "$W/wf/$STG" '          moving="$RELAY_REGISTRY/$RELAY_APP:beta"
          # The old :beta' '          moving="$RELAY_REGISTRY/$RELAY_APP:stable"
          # The old :beta'
expect_red "t9 deploy-stg moves :stable" 'deploy-stg.yml (moving tag): job push: the track step moves '"'"'moving="$RELAY_REGISTRY/$RELAY_APP:stable"'"'"', not :beta'

# t10. A mismatch after the move that does not fail the job.
fresh; edit_after "$W/wf/$STG" 'after the move, not the verified' '            exit 1' '            true'
expect_red "t10 deploy-stg's read-back never exits 1" 'deploy-stg.yml (moving tag): job push: the track step never reads :beta back and exits 1 when it names another digest'

# t11. No read-back at all.
fresh; edit "$W/wf/$STG" '          now=$(docker buildx imagetools inspect "$moving" --format '"'"'{{.Manifest.Digest}}'"'"')
          if [ "$now" != "$DIGEST" ]; then' '          now=$DIGEST
          if [ "$now" != "$DIGEST" ]; then'
expect_red "t11 deploy-stg's track step reads nothing back" 'deploy-stg.yml (moving tag): job push: the track step never reads :beta back'

# t12. The record check writes its digest before it verifies (a found squat would reach the track step).
fresh; edit "$W/wf/$STG" '            # The verified digest, written here and nowhere else: the moving tag'"'"'s step reads only this.
            echo "digest=$DIGEST" >> "$GITHUB_OUTPUT"
' ''
edit "$W/wf/$STG" '          [[ "$DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "::error::no digest for $ref: '"'"'$DIGEST'"'"'"; exit 1; }' '          [[ "$DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "::error::no digest for $ref: '"'"'$DIGEST'"'"'"; exit 1; }
          echo "digest=$DIGEST" >> "$GITHUB_OUTPUT"'
expect_red "t12 deploy-stg's record check outputs its digest before verifying" "deploy-stg.yml (moving tag): job push: the record check does not write digest= to GITHUB_OUTPUT once, on its verified branch alone"

# t13. A retag outside the track step: the Push step pushes :beta before anything is attested.
fresh; edit "$W/wf/$STG" '          docker push "$ref" | tee "$RUNNER_TEMP/push.out"' '          docker push "$ref" | tee "$RUNNER_TEMP/push.out"
          docker tag "$LOCAL_IMAGE" "$RELAY_REGISTRY/$RELAY_APP:beta" && docker push "$RELAY_REGISTRY/$RELAY_APP:beta"'
expect_red "t13 deploy-stg's Push step pushes :beta" 'deploy-stg.yml (moving tag): push.Push retags outside a track step: docker tag "$LOCAL_IMAGE" "$RELAY_REGISTRY/$RELAY_APP:beta"'

# t14. A retag tool in another workflow (any job: rule 5 keeps the relay token out, rule 16 the command).
fresh; cat >> "$W/wf/check.yml" << 'YML'
  retag:
    runs-on: ubuntu-24.04
    steps:
      - run: crane copy ghcr.io/x/y@sha256:0 ghcr.io/x/y:beta
YML
expect_red "t14 a retag in check.yml" 'check.yml (moving tag): retag.step 0 retags outside a track step: crane copy'

# t15. The track step with a key that changes how it runs.
fresh; edit "$W/wf/$PROD" "      - name: Move :stable to the verified build
        id: track
" "      - name: Move :stable to the verified build
        id: track
        working-directory: /tmp
"
expect_red "t15 deploy-prod's track step with working-directory" 'deploy-prod.yml (moving tag): job push: the track step carries working-directory'

# t16. A missing deploy workflow: the count (masked per file by e14's red otherwise) says 1 of 2.
fresh; rm "$W/wf/$PROD"
expect_red "t16 deploy-prod.yml missing: 1 of 2 moving tags" 'moving tags: 1 of 2 push jobs move theirs'

# t17. Each push job owes its own: deploy-prod without its track step, deploy-stg's still there.
fresh; cut_block "$W/wf/$PROD" "$TRACK_PROD" "$SUMMARY_STEP"
expect_red "t17 deploy-prod without its track step" 'deploy-prod.yml (moving tag): job push has 0 steps with id track, not 1: nothing moves :stable'

# t18 to t20 (the PR #94 review's items 2 and 3): the track step checks, before its copy, that this build
# is the newest (stg: main's tip; prod: the highest release tag) and that the old tag is main's own build.

# t18. deploy-stg without the tip check.
fresh; edit "$W/wf/$STG" '          if [ "$tip" != "$GITHUB_SHA" ]; then' '          if false; then'
expect_red "t18 deploy-stg moves :beta without checking main's tip" "deploy-stg.yml (moving tag): job push: the track step never checks, before the copy, that this build is the newest"

# t19. deploy-prod without the highest-tag check.
fresh; edit "$W/wf/$PROD" '          if [ "$highest" != "$TAG" ]; then' '          if [ "$highest" = "" ]; then'
expect_red "t19 deploy-prod moves :stable without checking the highest tag" "deploy-prod.yml (moving tag): job push: the track step never checks, before the copy, that this build is the newest"

# t20. deploy-stg without the old tag's check.
fresh; edit "$W/wf/$STG" '          if [ "$old" != none ] && ! gh attestation verify' '          if false && ! gh attestation verify'
expect_red "t20 deploy-stg moves :beta without checking the old one" "deploy-stg.yml (moving tag): job push: the track step never checks, before the copy, that the old :beta is main's own build"

# t21. (the review's m-3) The record check's output after its fi, so it is written on every path (rule
# 16's after-the-elif half; t12 is the before-the-verify half).
fresh; edit "$W/wf/$STG" '            echo "digest=$DIGEST" >> "$GITHUB_OUTPUT"
          elif' '          elif'
edit "$W/wf/$STG" '          fi
      # The moving tag (owner, 2026-10-26: ":beta"' '          fi
          echo "digest=$DIGEST" >> "$GITHUB_OUTPUT"
      # The moving tag (owner, 2026-10-26: ":beta"'
expect_red "t21 deploy-stg's record check outputs its digest after its fi" 'deploy-stg.yml (moving tag): job push: the record check does not write digest= to GITHUB_OUTPUT once, on its verified branch alone'

# t22 to t24 (the review's m-4): the other ways a step can write a moving tag, each outside a track step.

# t22. docker image push.
fresh; edit "$W/wf/$STG" '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker tag' '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker image push "$RELAY_REGISTRY/$RELAY_APP:beta"
          docker tag'
expect_red "t22 deploy-stg's Push step runs docker image push :beta" 'deploy-stg.yml (moving tag): push.Push retags outside a track step: docker image push'

# t23. docker manifest push.
fresh; edit "$W/wf/$PROD" '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker tag' '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker manifest push "$RELAY_REGISTRY/$RELAY_APP:stable"
          docker tag'
expect_red "t23 deploy-prod's Push step runs docker manifest push :stable" 'deploy-prod.yml (moving tag): push.Push retags outside a track step: docker manifest push'

# t24. buildx build --push, the tag before the flag.
fresh; edit "$W/wf/$STG" '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker tag' '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker buildx build -t "$RELAY_REGISTRY/$RELAY_APP:beta" --push .
          docker tag'
expect_red "t24 deploy-stg's Push step runs buildx build --push with :beta" 'deploy-stg.yml (moving tag): push.Push retags outside a track step: docker buildx build -t'

# t25. deploy-stg's tip check after the copy (an older run would move :beta, then skip).
fresh; move_after_copy "$W/wf/$STG" '          if [ "$tip" != "$GITHUB_SHA" ]; then'
expect_red "t25 deploy-stg checks main's tip after the copy" "deploy-stg.yml (moving tag): job push: the track step never checks, before the copy, that this build is the newest"

# t26. deploy-prod's old-tag check after the copy (it would read the build it just copied).
fresh; move_after_copy "$W/wf/$PROD" '          if [ "$old" != none ] && ! gh attestation verify'
expect_red "t26 deploy-prod checks the old :stable after the copy" "deploy-prod.yml (moving tag): job push: the track step never checks, before the copy, that the old :stable is main's own build"

# t27. A plain docker tag onto the moving tag by its variable, not its name.
fresh; edit "$W/wf/$STG" '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker tag' '          ref="$RELAY_REGISTRY/$RELAY_APP:$TAG"
          docker tag "$ref" "$moving"
          docker tag'
expect_red "t27 deploy-stg's Push step tags \$moving" 'deploy-stg.yml (moving tag): push.Push retags outside a track step: docker tag "$ref" "$moving"'

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
if command -v sha256sum > /dev/null; then sha_hex() { printf '%s' "$1" | sha256sum | cut -d' ' -f1; }
else sha_hex() { printf '%s' "$1" | shasum -a 256 | cut -d' ' -f1; }; fi

# The scratch "GitHub" repository: main z (no CEF_STEP0_PASS yet), a, b (GSTCEF_COMMIT bumped with no
# record: fails the gate), c, h (HEAD); side: d off h. Tags: v1.0.0 = a, v1.1.0 = b, v1.2.3 = c,
# v1.2.4 = d, v1.9.0 = a blob (names no commit). Every commit carries this tree's scripts/promote.sh
# and check-cef-bump.sh, so main's copy is the one under test. The operator's checkout ($W/op) is a
# clone of it. Two more "GitHubs", each one commit past h on main: one whose main has no
# check-cef-bump.sh, one whose main has no promote.sh.
g="$W/git"
mkdir -p "$g/docker" "$g/scripts"
git -C "$g" init -q -b main
gcin() { local r=$1; shift; git -C "$r" add -A && git -C "$r" -c user.email=t@example -c user.name=t commit -q --allow-empty -m "$1" && git -C "$r" rev-parse HEAD; }
gc() { gcin "$g" "$1"; }
cp "$root/scripts/check-cef-bump.sh" "$root/scripts/promote.sh" "$g/scripts/"; cp "$root/_FINDINGS.md" "$g/"; cp "$root/docker/GSTCEF_COMMIT" "$g/docker/"
Z=$(gc z)
cp "$root/docker/CEF_STEP0_PASS" "$g/docker/"; A=$(gc a)
echo "$bump" > "$g/docker/GSTCEF_COMMIT"; B=$(gc b)
cp "$root/docker/GSTCEF_COMMIT" "$g/docker/"; C=$(gc c)
H=$(gc h)
git -C "$g" checkout -q -b side; D=$(gc d); git -C "$g" checkout -q main
git -C "$g" tag v1.0.0 "$A"; git -C "$g" tag v1.1.0 "$B"; git -C "$g" tag v1.2.3 "$C"; git -C "$g" tag v1.2.4 "$D"
git -C "$g" tag v1.9.0 "$(printf 'not a commit\n' | git -C "$g" hash-object -w --stdin)"
git -C "$g" update-ref refs/remotes/origin/main "$H"
git clone -q "$g" "$W/op"
git clone -q "$g" "$W/git-nocef"; git -C "$W/git-nocef" rm -q scripts/check-cef-bump.sh; gcin "$W/git-nocef" "no gate" > /dev/null
git clone -q "$g" "$W/git-nopromote"; git -C "$W/git-nopromote" rm -q scripts/promote.sh; gcin "$W/git-nopromote" "no promote" > /dev/null

# Fakes. State lives in $STATE; every call is logged to $STATE/log. flyctl's secret list is live:
# `secrets set --stage` stages a value (its digest: the value's cksum), `secrets deploy` deploys
# every staged one, unless a flag file says otherwise. flyctl logs which token it ran with: none
# (the stored login, promote's), the push token the test expects (the deploy workflows'), or WRONG,
# and its working directory. `tokens create` prints $STATE/read-token, the read token promote mints.
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
  "tokens create")
    [ ! -f "$STATE/token-fails" ] || { echo "Error: you are not logged in (fake)" >&2; exit 1; }
    cat "$STATE/read-token"
    [ ! -f "$STATE/token-odd" ] || echo "a second line" ;;
  "config show") [ ! -f "$STATE/config-fails" ] || { echo "Error: app not found (fake)" >&2; exit 1; }; cat "$STATE/config.json" ;;
  "secrets list")
    n=$(( $(cat "$STATE/list-calls" 2> /dev/null || echo 0) + 1 )); echo "$n" > "$STATE/list-calls"
    if [ -f "$STATE/list-fails-at" ] && [ "$(cat "$STATE/list-fails-at")" = "$n" ]; then echo "Error: timeout (fake)" >&2; exit 1; fi
    cat "$s" ;;
  "secrets set")
    # The read credential is gone before the web app is touched (m-12): its path is the one curl logged.
    cfg=$(sed -n 's/^curl-config ok //p' "$STATE/log" | tail -n 1)
    if [ -n "$cfg" ] && [ -e "$cfg" ]; then echo "creds-present-at-set $cfg" >> "$STATE/log"; fi
    [ ! -f "$STATE/set-fails" ] || { echo "Error: unauthorized (fake)" >&2; exit 1; }
    [ ! -f "$STATE/set-noop" ] || exit 0
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
# docker, for the deploy workflows' own steps only (promote runs no docker): tags in $STATE/registry
# (`<repo>:<tag> <digest>`). `push` prints docker's PushResult line for $STATE/push-digest and puts
# that digest under the tag, or, with $STATE/squat-after-push, another push's right after it (the
# race of the PR #93 review's C-1). `buildx imagetools create --tag DST SRC` (one source, by digest) is
# buildx v0.38.1's combine for one source with no annotations (util/imagetools/create.go): an index
# ($STATE/index-<digest>) is copied as it is, and so is an image manifest under --prefer-index=false;
# otherwise (the default, --prefer-index=true) the manifest is wrapped in a new index, a new digest.
# $STATE/squat-after-create moves DST to its digest right after (another push holding the token).
cat > "$W/bin/docker" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "docker $*" >> "$STATE/log"
case "$1" in
  logout | tag) exit 0 ;;
  push)
    shift; [ "$1" != -q ] || shift
    ref=$1 t=${1##*:} d=$(cat "$STATE/push-digest")
    echo "The push refers to repository [${ref%:*}]"
    echo "5f70bf18a086: Pushed"
    [ -f "$STATE/push-no-digest" ] || echo "$t: digest: $d size: 1234"
    nd=$d; [ ! -f "$STATE/squat-after-push" ] || nd=$(cat "$STATE/squat-after-push")
    awk -v r="$ref" '$1 != r' "$STATE/registry" > "$STATE/registry.tmp"; echo "$ref $nd" >> "$STATE/registry.tmp"
    mv "$STATE/registry.tmp" "$STATE/registry"
    exit 0 ;;
esac
if [ "$1 ${2:-}" = "buildx version" ]; then echo "github.com/docker/buildx v0.0.0 fake"; exit 0; fi
if [ "$1 ${2:-} ${3:-}" = "buildx imagetools create" ]; then
  shift 3; carbon=no dst='' src=''
  while [ $# -gt 0 ]; do
    case "$1" in
      --prefer-index=false) carbon=yes ;;
      --tag | -t) dst=$2; shift ;;
      -*) echo "fake docker: unexpected create flag $1" >&2; exit 2 ;;
      *) [ -z "$src" ] || { echo "fake docker: this fake models one source" >&2; exit 2; }; src=$1 ;;
    esac
    shift
  done
  [ -n "$dst" ] && [[ "$src" == *@sha256:* ]] || { echo "fake docker: create wants --tag and one <repo>@<digest> source" >&2; exit 2; }
  [ ! -f "$STATE/create-fails" ] || { echo "ERROR: failed to push $dst: 403 Forbidden (fake)" >&2; exit 1; }
  name=${src%@*} d=${src##*@}
  { awk -v n="$name:" -v d="$d" 'index($1, n) == 1 && $2 == d {f = 1} END {exit !f}' "$STATE/registry" \
      || { [ -f "$STATE/manifests" ] && awk -v n="$name" -v d="$d" '$1 == n && $2 == d {f = 1} END {exit !f}' "$STATE/manifests"; }; } \
    || { echo "ERROR: $src: not found" >&2; exit 1; }
  if [ "$carbon" = yes ] || [ -f "$STATE/index-$d" ]; then nd=$d
  elif command -v sha256sum > /dev/null; then nd="sha256:$(printf 'an index over %s' "$d" | sha256sum | cut -d' ' -f1)"
  else nd="sha256:$(printf 'an index over %s' "$d" | shasum -a 256 | cut -d' ' -f1)"; fi
  [ ! -f "$STATE/squat-after-create" ] || nd=$(cat "$STATE/squat-after-create")
  awk -v r="$dst" '$1 != r' "$STATE/registry" > "$STATE/registry.tmp"; echo "$dst $nd" >> "$STATE/registry.tmp"
  mv "$STATE/registry.tmp" "$STATE/registry"
  echo "#1 pushing $nd to $dst (fake)" >&2
  exit 0
fi
[ "$1 $2 $3" = "buildx imagetools inspect" ] || { echo "fake docker: unexpected: $*" >&2; exit 2; }
if [ -f "$STATE/registry-down" ]; then echo "ERROR: failed to authorize: 401 Unauthorized (fake)" >&2; exit 1; fi
d=$(awk -v r="$4" '$1 == r {print $2}' "$STATE/registry")
[ -n "$d" ] || { echo "ERROR: $4: not found" >&2; exit 1; }
echo "$d"
SH
# curl, for promote's registry reads: GET https://<registry>/v2/<app>/manifests/<digest or tag>, served
# from $STATE/blobs/<app>/<digest> (a tag resolves through $STATE/registry, `<registry>/<app>:<tag>`);
# 404 when absent; $STATE/status-<digest> forces a status, $STATE/lie a
# wrong body ($STATE/lie-<digest> for that digest alone), $STATE/curl-fails a curl error whose text
# carries the credential (it must never be
# printed). It checks its -K config: mode -rw-------, exactly `user = "x:<the minted token>"`.
cat > "$W/bin/curl" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "curl $*" >> "$STATE/log"
cfg='' out='' fmt='' url=''
while [ $# -gt 0 ]; do
  case "$1" in
    -K) cfg=$2; shift ;;
    -o) out=$2; shift ;;
    -w) fmt=$2; shift ;;
    -H | --proto | --max-time) shift ;;
    -q | -sS) ;;
    https://*) url=$1 ;;
    *) echo "fake curl: unexpected $1" >&2; exit 2 ;;
  esac
  shift
done
[ "$fmt" = '%{http_code}' ] && [ -n "$out" ] && [ -n "$url" ] || { echo "fake curl: wanted -o, -w %{http_code} and a URL" >&2; exit 2; }
if [ -f "$cfg" ] && [ "$(ls -l "$cfg" | cut -c 1-10)" = "-rw-------" ] && [ "$(cat "$cfg")" = "user = \"x:$(cat "$STATE/read-token")\"" ]; then
  echo "curl-config ok $cfg" >> "$STATE/log"
else
  echo "curl-config BAD $cfg" >> "$STATE/log"
  printf '{"errors":[{"code":"UNAUTHORIZED"}]}' > "$out"; printf '401'; exit 0
fi
if [ -f "$STATE/curl-fails" ]; then echo "curl: (7) Failed to connect as x:$(cat "$STATE/read-token") (fake)" >&2; printf '000'; exit 7; fi
path=${url#https://*/v2/} app=${path%%/manifests/*} digest=${path##*/manifests/}
host=${url#https://}; host=${host%%/*} asked=$digest
if [ -f "$STATE/status-$digest" ]; then printf '{}' > "$out"; cat "$STATE/status-$digest"; exit 0; fi
if [[ "$digest" != sha256:* ]]; then digest=$(awk -v r="$host/$app:$digest" '$1 == r {print $2}' "$STATE/registry"); fi
if [ -n "$digest" ] && [ -f "$STATE/blobs/$app/$digest" ]; then
  if [ -f "$STATE/lie" ] || [ -f "$STATE/lie-$digest" ] || [ -f "$STATE/lie-$asked" ]; then printf '{"not":"it"}' > "$out"; else cp "$STATE/blobs/$app/$digest" "$out"; fi
  printf '200'
else
  printf '{"errors":[{"code":"MANIFEST_UNKNOWN"}]}' > "$out"; printf '404'
fi
SH
cat > "$W/bin/gh" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "gh $*" >> "$STATE/log"
# `gh attestation verify <file | oci://<repo>@<digest>> --repo R --cert-identity I ...`, as gh 2.95.0's
# verify/policy.go enforces it: --cert-identity matches the signing certificate's SAN exactly,
# --source-ref, --source-digest and --signer-digest match those extensions when given (an omitted
# flag enforces nothing), --deny-self-hosted-runners requires a GitHub-hosted runner, and --repo
# scopes the lookup to that repository. A file is hashed (sha256) and its attestations fetched by that
# digest; an oci:// URI reads the manifest by digest from the registry first.
# $STATE/attestations: one per line, `digest identity source_ref source_digest signer_digest runner repo`.
[ "$1 ${2:-}" = "attestation verify" ] || { echo "fake gh: unexpected: $*" >&2; exit 2; }
subject=$3; shift 3
repo='' ident='' ident_re='' sref='' sdig='' gdig='' deny=no fmt=''
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo=$2; shift ;;
    --cert-identity) ident=$2; shift ;;
    --cert-identity-regex) ident_re=$2; shift ;;
    --format) fmt=$2; shift ;;
    --source-ref) sref=$2; shift ;;
    --source-digest) sdig=$2; shift ;;
    --signer-digest) gdig=$2; shift ;;
    --limit) shift ;;
    --deny-self-hosted-runners) deny=yes ;;
    *) echo "fake gh: unexpected attestation flag $1" >&2; exit 2 ;;
  esac
  shift
done
[ -n "$repo" ] || { echo "fake gh: --repo or --owner is required" >&2; exit 2; }
{ [ -n "$ident" ] && [ -z "$ident_re" ]; } || { [ -z "$ident" ] && [ -n "$ident_re" ]; } \
  || { echo "fake gh: one of --cert-identity or --cert-identity-regex (gh: mutually exclusive)" >&2; exit 2; }
[ -z "$fmt" ] || [ "$fmt" = json ] || { echo "fake gh: --format json only" >&2; exit 2; }
if [ -f "$STATE/attest-down" ]; then echo "Error: failed to fetch attestations: HTTP 503 (fake)" >&2; exit 1; fi
if [[ "$subject" == oci://* ]]; then
  name=${subject#oci://} digest=${subject##*@}; name=${name%@*}
  # A manifest stays addressable by digest after its tag moves: $STATE/manifests lists those.
  { awk -v n="$name:" -v d="$digest" 'index($1, n) == 1 && $2 == d {f = 1} END {exit !f}' "$STATE/registry" \
      || { [ -f "$STATE/manifests" ] && awk -v n="$name" -v d="$digest" '$1 == n && $2 == d {f = 1} END {exit !f}' "$STATE/manifests"; }; } \
    || { echo "Error: $name@$digest: not found (fake)" >&2; exit 1; }
else
  [ -f "$subject" ] || { echo "Error: $subject: no such file (fake)" >&2; exit 1; }
  if command -v sha256sum > /dev/null; then digest="sha256:$(sha256sum < "$subject" | cut -d' ' -f1)"
  else digest="sha256:$(shasum -a 256 < "$subject" | cut -d' ' -f1)"; fi
fi
# The regex is passed through the environment: awk would read escapes in a -v value.
matched=$(RE="$ident_re" awk -v d="$digest" -v i="$ident" -v r="$sref" -v s="$sdig" -v g="$gdig" -v deny="$deny" -v repo="$repo" '
    $1 == d && ((i != "" && $2 == i) || (i == "" && $2 ~ ENVIRON["RE"])) && (r == "" || $3 == r) && (s == "" || $4 == s) \
      && (g == "" || $5 == g) && (deny == "no" || $6 == "github-hosted") && $7 == repo' "$STATE/attestations")
if [ -n "$matched" ]; then
  # --format json: one result per attestation that matched, as gh 2.95.0 prints them (measured
  # 2026-10-26 on cli/cli's own release: an array of {attestation, verificationResult}, the
  # certificate's fields under .verificationResult.signature.certificate).
  if [ "$fmt" = json ]; then
    jq -R -s 'split("\n") | map(select(length > 0) | split(" ") | {verificationResult: {signature: {certificate: {
        subjectAlternativeName: .[1], sourceRepositoryRef: .[2], sourceRepositoryDigest: .[3], buildSignerDigest: .[4],
        runnerEnvironment: .[5], sourceRepositoryURI: "https://github.com/\(.[6])"}}}})' <<< "$matched"
  else
    echo "Verification succeeded! (fake)"
  fi
  exit 0
fi
echo "Error: verifying with issuer \"sigstore.dev\": no attestation matched the policy (fake)" >&2
exit 1
SH
chmod +x "$W/bin/flyctl" "$W/bin/docker" "$W/bin/curl" "$W/bin/gh"
# The tools sweep's PATHs: every system command except the ones promote checks for, and a bin of
# those (the fakes, git, jq, the sha tool) from which each case removes one.
mkdir -p "$W/sysbin"
for f in /bin/* /usr/bin/*; do
  n=$(basename "$f")
  case "$n" in git | jq | curl | flyctl | gh | docker | sha256sum | shasum) continue ;; esac
  [ -e "$W/sysbin/$n" ] || ln -s "$f" "$W/sysbin/$n"
done
tools_bin() { # tools_bin MISSING...: a bin of promote's tools without MISSING
  local d="$W/tools-$1" t
  rm -rf "$d"; mkdir -p "$d"
  for t in flyctl curl gh; do ln -s "$W/bin/$t" "$d/$t"; done
  for t in git jq sha256sum shasum; do command -v "$t" > /dev/null && ln -s "$(command -v "$t")" "$d/$t"; done
  for t in "$@"; do rm -f "$d/$t"; done
  echo "$d"
}

SHA=$C OLD=$A UNPUSHED=$H SIDE=$D BAD=$B PRE=$Z
SPOOF=5555555555555555555555555555555555555555
# Manifest bodies, and their digests (promote checks sha256(body) == the pin).
B_NEW='{"m":"main build of c"}' B_OLD='{"m":"main build of a"}' B_PROD='{"m":"release v1.2.3"}'
B_ANY='{"m":"some other push"}' B_SQ='{"m":"a squat"}' B_GONE='{"m":"deleted since"}' B_MISSING='{"m":"never pushed"}'
D_NEW=sha256:$(sha_hex "$B_NEW") D_OLD=sha256:$(sha_hex "$B_OLD") D_PROD=sha256:$(sha_hex "$B_PROD")
D_ANY=sha256:$(sha_hex "$B_ANY") D_SQ=sha256:$(sha_hex "$B_SQ") D_GONE=sha256:$(sha_hex "$B_GONE") D_MISSING=sha256:$(sha_hex "$B_MISSING")
REPO=onryde/seazn.club.compositor
WFURL=https://github.com/$REPO/.github/workflows
ID_STG="$WFURL/deploy-stg.yml@refs/heads/main"
READ_TOKEN='FlyV1 fm2_fakeReadToken7f3a+/=,x'
# attest_line DIGEST IDENTITY SOURCE_REF COMMIT: main's own build record, as deploy-<env>.yml signs it.
attest_line() { echo "$1 $2 $3 $4 $4 github-hosted $REPO"; }
REG=registry.example
digest_of() { printf '%s' "$1" | cksum | cut -d' ' -f1; }
PLOG="$W/home/.local/state/seazn-relay-promote"
blob() { mkdir -p "$STATE/blobs/$1"; printf '%s' "$2" > "$STATE/blobs/$1/sha256:$(sha_hex "$2")"; }
state() {
  STATE="$W/state"; rm -rf "$STATE"; mkdir -p "$STATE"; : > "$STATE/log"; : > "$W/summary"; : > "$W/output"
  export STATE
  printf '%s\n' "$READ_TOKEN" > "$STATE/read-token"
  printf '%s\n' "$REG/seazn-relay-stg:$SHA $D_NEW" "$REG/seazn-relay-stg:$OLD $D_OLD" "$REG/seazn-relay-stg:$SIDE $D_ANY" \
    "$REG/seazn-relay-stg:$BAD $D_ANY" "$REG/seazn-relay-stg:$PRE $D_ANY" \
    "$REG/seazn-relay-prod:v1.2.3 $D_PROD" "$REG/seazn-relay-prod:v1.2.4 $D_ANY" "$REG/seazn-relay-prod:v1.1.0 $D_ANY" \
    "$REG/seazn-relay-stg:beta $D_OLD" "$REG/seazn-relay-prod:stable $D_PROD" > "$STATE/registry"
  blob seazn-relay-stg "$B_NEW"; blob seazn-relay-stg "$B_OLD"; blob seazn-relay-stg "$B_ANY"
  blob seazn-relay-prod "$B_PROD"; blob seazn-relay-prod "$B_ANY"
  # Main's build records for what main pushed: deploy-stg at refs/heads/main for $SHA and $OLD,
  # deploy-prod at refs/tags/v1.2.3 for C. Nothing attests the D_ANY images. The moving tags: :beta
  # names $OLD's build, :stable v1.2.3's.
  { attest_line "$D_NEW" "$ID_STG" refs/heads/main "$SHA"; attest_line "$D_OLD" "$ID_STG" refs/heads/main "$OLD"
    attest_line "$D_PROD" "$WFURL/deploy-prod.yml@refs/tags/v1.2.3" refs/tags/v1.2.3 "$C"; } > "$STATE/attestations"
  echo '{"app":"seazn-club-stg","env":{"OAUTH_BASE_URL":"https://stg.example"}}' > "$STATE/config.json"
  echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"},{"name":"RELAY_IMAGE","digest":"d0","status":"Deployed"}]' > "$STATE/secrets.json"
  # The operator's promote log: stg's newest entry is not what RELAY_IMAGE is now (Fly digest d7,
  # say a later promote's whose deploy failed), the one before it is (d0); prod's names v1.0.0, whose
  # manifest is gone from the registry.
  rm -rf "${W:?}/home"; mkdir -p "$PLOG"
  printf '%s\n' "2026-10-20T10:00:00Z stg $OLD@$D_OLD $REG/seazn-relay-stg@$D_OLD d0" \
    "2026-10-22T10:00:00Z stg $SPOOF@$D_ANY $REG/seazn-relay-stg@$D_ANY d7" > "$PLOG/stg.log"
  echo "2026-10-21T10:00:00Z prod v1.0.0@$D_GONE $REG/seazn-relay-prod@$D_GONE d0" > "$PLOG/prod.log"
}
# run_step SCRIPT VAR=VALUE...: a workflow's block under the runner's shell (bash -eo pipefail), in
# the scratch repository (or STEP_DIR, a checkout of it), fakes first on PATH.
run_step() {
  local script=$1; shift
  if (cd "${STEP_DIR:-$g}" && env -i PATH="$W/bin:$PATH" HOME="$W" RUNNER_TEMP="$W/rt" GITHUB_STEP_SUMMARY="$W/summary" \
    GITHUB_OUTPUT="$W/output" GITHUB_REPOSITORY=onryde/seazn.club.compositor GITHUB_RUN_ID=999 GITHUB_SERVER_URL=https://github.com \
    GITHUB_REF=refs/heads/main GITHUB_SHA="$H" STATE="$STATE" RELAY_REGISTRY="$REG" GH_TOKEN=fake-gh \
    EXPECT_PUSH=fake-stg-push "$@" \
    bash --noprofile --norc -eo pipefail "$script") > "$W/step.out" 2>&1; then rc=0; else rc=$?; fi
}
# promote_run [VAR=VALUE...] -- ARG...: scripts/promote.sh as the operator runs it, from their
# checkout, on their own login (no token in its environment unless a case puts one there), with its
# test seams on. Pseudo-variables: SCRIPT=<path> runs another copy; NOSEAMS=1 drops the seams.
promote_run() {
  local envs=() script="$W/op/scripts/promote.sh" seams=(PROMOTE_TEST_SEAMS=1 PROMOTE_REMOTE="$g" RELAY_REGISTRY="$REG")
  while [ "$#" -gt 0 ] && [ "$1" != -- ]; do
    case "$1" in SCRIPT=*) script=${1#SCRIPT=} ;; NOSEAMS=1) seams=() ;; *) envs+=("$1") ;; esac
    shift
  done
  [ "$#" -eq 0 ] || shift
  if (cd "$W/op" && env -i PATH="$W/bin:$PATH" HOME="$W/home" STATE="$STATE" ${seams[@]+"${seams[@]}"} \
      ${envs[@]+"${envs[@]}"} bash --noprofile --norc "$script" "$@") > "$W/step.out" 2>&1; then rc=0; else rc=$?; fi
}
has() { grep -qF -- "$2" "$1"; }
no_set() { ! grep -qE 'flyctl secrets (set|deploy)' "$STATE/log"; }
# No Fly or registry call: no flyctl, curl or docker at all.
no_fly() { ! grep -qE '^(flyctl|docker|curl) ' "$STATE/log"; }
show() { sed 's/^/    /' "$W/step.out" "$STATE/log" "$W/summary" >&2; }
refusal() { grep -m 1 -F 'promote: REFUSED: ' "$W/step.out" | cut -c 1-220; }
# The minted read token appears in no output, no argv (the fakes log theirs) and no error.
no_token_seen() { ! grep -qF -- "fakeReadToken7f3a" "$W/step.out" && ! grep -qF -- "fakeReadToken7f3a" "$STATE/log"; }
# refused NAME TEXT [VAR=VALUE...] -- ARG...: promote stops, says TEXT, and made no Fly or registry call.
refused() {
  local name=$1 text=$2; shift 2
  state; promote_run "$@"
  if [ "$rc" -ne 0 ] && grep -F 'promote: REFUSED: ' "$W/step.out" | grep -qF -- "$text" && no_fly; then
    ok "promote $name :: $(refusal)"
  else
    bad "promote $name: rc $rc, expected a refusal before any flyctl, curl or docker call saying: $text"; show
  fi
}
# refused_before_web NAME TEXT -- ARG...: promote stops, says TEXT, never called the web app, and
# never showed the read token.
refused_before_web() {
  local name=$1 text=$2; shift 2
  promote_run "$@"
  if [ "$rc" -ne 0 ] && grep -F 'promote: REFUSED: ' "$W/step.out" | grep -qF -- "$text" && no_set \
    && ! grep -q 'seazn-club-' "$STATE/log" && no_token_seen; then
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

# logs_out: the step logged docker in on the push token and out again (m-2).
logs_out() { has "$STATE/log" 'flyctl auth docker (token push)' && has "$STATE/log" "docker logout $REG"; }

# "Already pushed?", stg and prod (m-5): a pushed tag is never pushed again; only "not found" is
# absent; any other registry error is a red with no exists= written. flyctl runs on the push token,
# and the login leaves with the step.
for wf in "$STG:seazn-relay-stg:$SHA:fake-stg-push" "$PROD:seazn-relay-prod:v1.2.3:fake-prod-push"; do
  IFS=: read -r file app pushed token <<< "$wf"
  extract "$file" push exists "$W/exists.sh"
  state; run_step "$W/exists.sh" FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG="$pushed"
  r1=$rc; out1=$(cat "$W/output"); l1=false; logs_out && l1=true
  state; run_step "$W/exists.sh" FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG=v7.7.7
  r2=$rc; out2=$(cat "$W/output")
  state; touch "$STATE/registry-down"; run_step "$W/exists.sh" FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG=v7.7.7
  if [ "$r1" -eq 0 ] && [ "$l1" = true ] && [[ "$out1" == *"exists=true"* && "$out1" == *"digest=sha256:"* ]] \
    && [ "$r2" -eq 0 ] && [[ "$out2" == *"exists=false"* ]] \
    && [ "$rc" -ne 0 ] && has "$W/step.out" "::error::the registry could not be read for $REG/$app:v7.7.7" && [ ! -s "$W/output" ] && logs_out; then
    ok "$file Already pushed?: pushed reads exists=true (on the push token, logged out after); not found exists=false; a registry error is a red"
  else
    bad "$file Already pushed?: rc $r1/$r2/$rc"; echo "$out1 | $out2" >&2; sed 's/^/    /' "$W/step.out" >&2
  fi
done

# Push (the PR #93 review's C-1): the digest attested is the one this push wrote, from docker's own
# PushResult line, never the tag read back; a tag another push moves under this one is a red with no
# digest written; a push that prints no digest line is a red. The old step (`docker push -q`, then
# the digest read back by tag) outputs the squat's digest in the race, so this case reds on it.
for wf in "$STG:seazn-relay-stg:$UNPUSHED:fake-stg-push" "$PROD:seazn-relay-prod:v7.7.7:fake-prod-push"; do
  IFS=: read -r file app tag token <<< "$wf"
  extract "$file" push push "$W/push.sh"
  args=(FLY_API_TOKEN="$token" EXPECT_PUSH="$token" RELAY_APP="$app" TAG="$tag" LOCAL_IMAGE=seazn-relay:local)
  state; echo "$D_NEW" > "$STATE/push-digest"; run_step "$W/push.sh" "${args[@]}"
  r1=$rc; o1=$(cat "$W/output"); l1=false; logs_out && l1=true
  state; echo "$D_NEW" > "$STATE/push-digest"; echo "$D_SQ" > "$STATE/squat-after-push"; run_step "$W/push.sh" "${args[@]}"
  r2=$rc; o2=$(cat "$W/output"); e2=$(grep -m 1 -F '::error::' "$W/step.out" || true); l2=false; logs_out && l2=true
  state; echo "$D_NEW" > "$STATE/push-digest"; touch "$STATE/push-no-digest"; run_step "$W/push.sh" "${args[@]}"
  if [ "$r1" -eq 0 ] && [ "$o1" = "digest=$D_NEW" ] && [ "$l1" = true ] \
    && [ "$r2" -ne 0 ] && [ -z "$o2" ] && [ "$l2" = true ] \
    && [[ "$e2" == *"::error::the tag moved under the push: this run pushed $REG/$app:$tag as $D_NEW, and the tag now names '$D_SQ'"* ]] \
    && [ "$rc" -ne 0 ] && [ ! -s "$W/output" ] && has "$W/step.out" "::error::docker push printed 0 digest lines for $REG/$app:$tag, not 1"; then
    ok "$file Push (C-1): outputs the digest its own push wrote; a tag moved under it is a red and no digest ($e2); no digest line is a red; logged out after"
  else
    bad "$file Push (C-1): rc $r1/$r2/$rc, out '$o1' / '$o2'"; sed 's/^/    /' "$W/step.out" "$STATE/log" >&2
  fi
done

# Main's build record (I-4), on both paths: a pushed or found digest must verify as this commit's
# build by this workflow at its own ref. A squat found under the tag is a red, never "pushed".
record_case() { # record_case FILE NAME WANT_RC TEXT VAR=VALUE...
  local file=$1 name=$2 want=$3 text=$4; shift 4
  extract "$file" push record "$W/record.sh"
  run_step "$W/record.sh" "$@"
  # The verified digest is the step's one output (the track step reads only it), written only on success.
  if { [ "$want" = 0 ] && [ "$rc" -eq 0 ] && has "$W/step.out" "$text" && logs_out \
         && [ "$(cat "$W/output")" = "digest=$(printf '%s\n' "$@" | sed -n 's/^DIGEST=//p')" ]; } \
    || { [ "$want" != 0 ] && [ "$rc" -ne 0 ] && grep -F '::error::' "$W/step.out" | grep -qF -- "$text" && [ ! -s "$W/output" ]; }; then
    ok "$file build record $name :: $(head -n 1 "$W/step.out" | cut -c 1-200)"
  else
    bad "$file build record $name: rc $rc, expected rc $want saying: $text"; show
  fi
}
# sq_state ROW: the squat in the registry under main's sha (and by its digest), the row (if any) as
# the only record of D_SQ.
sq_state() {
  state
  sed "s|^$REG/seazn-relay-stg:$SHA .*|$REG/seazn-relay-stg:$SHA $D_SQ|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"
  blob seazn-relay-stg "$B_SQ"
  [ -z "$1" ] || echo "$1" >> "$STATE/attestations"
}
STG_REC=(RELAY_APP=seazn-relay-stg TAG="$SHA" GITHUB_SHA="$SHA" FLY_API_TOKEN=fake-stg-push)
PROD_REC=(RELAY_APP=seazn-relay-prod TAG=v1.2.3 GITHUB_SHA="$C" GITHUB_REF=refs/tags/v1.2.3 FLY_API_TOKEN=fake-prod-push EXPECT_PUSH=fake-prod-push)
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
  state; run_step "$W/summary.sh" RELAY_APP=seazn-relay-stg TAG="$SHA" FOUND="$found" DIGEST="$D_NEW" OLD="$D_OLD" NEW="$D_NEW"
  common=false
  if [ "$rc" -eq 0 ] && [ "$(summary_cmd)" = "scripts/promote.sh stg $SHA@$D_NEW" ] \
    && has "$W/summary" "It stages \`RELAY_IMAGE=$REG/seazn-relay-stg@$D_NEW\` (the digest alone) on seazn-club-stg, then deploys it." \
    && has "$W/summary" "\`$REG/seazn-relay-stg:beta\` moved: \`$D_OLD\` -> \`$D_NEW\` (this build, after its record verified)." \
    && has "$W/summary" "If RELAY_IMAGE is pinned to a digest, nothing live changed: while pinned, \`:beta\` is not followed." \
    && has "$W/summary" "\`scripts/promote.sh stg --track beta\` is the verified way"; then
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
state; run_step "$W/summary.sh" RELAY_APP=seazn-relay-prod TAG=v1.2.3 FOUND=true DIGEST="$D_PROD" OLD="$D_PROD" NEW="$D_PROD"
if [ "$rc" -eq 0 ] && has "$W/summary" 'Found in the registry, not built by this run' && ! has "$W/summary" 'Pushed' \
  && has "$W/summary" "\`$REG/seazn-relay-prod:stable\` already named this build: \`$D_PROD\` (unchanged)." \
  && has "$W/summary" "\`scripts/promote.sh prod --track stable --confirm-prod\` is the verified way" && ! has "$W/summary" ':beta' \
  && [ "$(summary_cmd)" = "scripts/promote.sh prod v1.2.3@$D_PROD --confirm-prod" ] \
  && has "$W/summary" "It stages \`RELAY_IMAGE=$REG/seazn-relay-prod@$D_PROD\` (the digest alone) on seazn-club-prod"; then
  ok "deploy-prod summary, found: says found, not built by this run, and never Pushed; prints the prod command with --confirm-prod"
else
  bad "deploy-prod summary, found: rc $rc"; sed 's/^/    /' "$W/summary" >&2
fi
promotes_as_printed "deploy-prod summary"
# The summary's own guard: a track step that reported another digest than this build's gets no summary.
for wf in "$STG:seazn-relay-stg:$SHA" "$PROD:seazn-relay-prod:v1.2.3"; do
  IFS=: read -r file app tag <<< "$wf"
  extract "$file" push summary "$W/summary.sh"
  state; run_step "$W/summary.sh" RELAY_APP="$app" TAG="$tag" FOUND=false DIGEST="$D_NEW" OLD=none NEW="$D_SQ"
  if [ "$rc" -ne 0 ] && has "$W/step.out" "::error::the track step reports $REG/$app:" && has "$W/step.out" "as '$D_SQ', not this build's $D_NEW: no summary" && [ ! -s "$W/summary" ]; then
    ok "$file summary: a track step reporting another digest ($D_SQ) is a red, and no summary is written"
  else
    bad "$file summary with NEW != DIGEST: rc $rc"; show
  fi
  # A move the track step skipped (an older build: the PR #94 review's item 2) is no red: the summary says
  # the tag stayed, that nothing live changed, and still prints the pin.
  moving=beta; [ "$file" = "$STG" ] || moving=stable
  state; run_step "$W/summary.sh" RELAY_APP="$app" TAG="$tag" FOUND=false DIGEST="$D_NEW" OLD="$D_OLD" NEW= SKIPPED="this build is older"
  if [ "$rc" -eq 0 ] && has "$W/summary" "\`$REG/$app:$moving\` not moved: this build is older. It still names \`$D_OLD\`, so nothing live changed" \
    && ! has "$W/summary" 'now runs this build' && has "$W/summary" "scripts/promote.sh ${app#seazn-relay-} $tag@$D_NEW"; then
    ok "$file summary: a skipped move is reported as not moved, nothing live changed, with the pin"
  else
    bad "$file summary with a skipped move: rc $rc"; show
  fi
done

# The moving tag (owner, 2026-10-26), each push job's track step. tag_now REF: the digest REF names in the
# fake registry. The model check first: the fake copies like buildx (a different digest without
# --prefer-index=false, the same with it, and an index the same either way), so a track step that lost
# the flag reds on its own read-back below, not by luck.
tag_now() { awk -v r="$1" '$1 == r {print $2}' "$STATE/registry"; }
state
"$W/bin/docker" buildx imagetools create --tag "$REG/seazn-relay-stg:m1" "$REG/seazn-relay-stg@$D_NEW" 2> /dev/null
"$W/bin/docker" buildx imagetools create --prefer-index=false --tag "$REG/seazn-relay-stg:m2" "$REG/seazn-relay-stg@$D_NEW" 2> /dev/null
touch "$STATE/index-$D_NEW"
"$W/bin/docker" buildx imagetools create --tag "$REG/seazn-relay-stg:m3" "$REG/seazn-relay-stg@$D_NEW" 2> /dev/null
m1=$(tag_now "$REG/seazn-relay-stg:m1") m2=$(tag_now "$REG/seazn-relay-stg:m2") m3=$(tag_now "$REG/seazn-relay-stg:m3")
if [[ "$m1" =~ ^sha256:[0-9a-f]{64}$ ]] && [ "$m1" != "$D_NEW" ] && [ "$m2" = "$D_NEW" ] && [ "$m3" = "$D_NEW" ]; then
  ok "the copy model: an image manifest copied with the default --prefer-index=true is a new digest ($m1); with --prefer-index=false, or an index, it is the source's"
else
  bad "the copy model: m1 '$m1' m2 '$m2' m3 '$m3'"; show
fi
# The checkouts a track step runs in: step-h, main at h with the releases v1.0.0, v1.1.0 and v1.2.3 (its
# origin is the scratch GitHub, whose main is h); step-two, v1.2.4 as well (two release tags pushed
# together); step-notags, no tag at all; step-c, an origin whose main is c.
git clone -q "$g" "$W/step-h"; git -C "$W/step-h" tag -d v1.2.4 v1.9.0 > /dev/null
git clone -q "$g" "$W/step-two"; git -C "$W/step-two" tag -d v1.9.0 > /dev/null
git clone -q "$g" "$W/step-notags"; git -C "$W/step-notags" tag -l | xargs git -C "$W/step-notags" tag -d > /dev/null
git clone -q "$g" "$W/main-at-c"; git -C "$W/main-at-c" update-ref refs/heads/main "$C"
git clone -q "$g" "$W/step-c"; git -C "$W/step-c" remote set-url origin "$W/main-at-c"
# Each track step's own check of the old tag (the PR #94 review's item 3), as its argv is logged.
RE_PROD_TAGS='^https://github\.com/onryde/seazn\.club\.compositor/\.github/workflows/deploy-prod\.yml@refs/tags/v[0-9]+\.[0-9]+\.[0-9]+$'
# The track steps: each moves its tag to the DIGEST it is given (the record check's output), never to what
# TAG names now (here a squat moved the sha's, or the release's, tag first); only when this build is the
# newest (stg: main's tip; prod: the highest release tag); and after checking the old tag is main's own.
track_checked=0
for wf in "$STG:seazn-relay-stg:beta:$SHA:fake-stg-push" "$PROD:seazn-relay-prod:stable:v1.2.3:fake-prod-push"; do
  IFS=: read -r file app moving tag token <<< "$wf"
  extract "$file" push track "$W/track.sh"
  args=(RELAY_APP="$app" TAG="$tag" FLY_API_TOKEN="$token" EXPECT_PUSH="$token")
  mv_ref="$REG/$app:$moving"
  if [ "$file" = "$STG" ]; then
    oldcheck="--repo $REPO --cert-identity $ID_STG --source-ref refs/heads/main --deny-self-hosted-runners --limit 1000"
    nonewest_dir=$g nonewest_text="main's tip could not be read"
    older=(GITHUB_SHA="$OLD" TAG="$OLD") older_pin="scripts/promote.sh stg $OLD@$D_NEW"
    older_why="$OLD is not main's tip ($H)" squat_text="it is no main build"
  else
    oldcheck="--repo $REPO --cert-identity-regex $RE_PROD_TAGS --deny-self-hosted-runners --limit 1000"
    nonewest_dir=$W/step-notags nonewest_text="no release tag could be read from this checkout"
    older=(TAG=v1.1.0) older_pin="scripts/promote.sh prod v1.1.0@$D_NEW --confirm-prod"
    older_why="v1.1.0 is not the highest release tag (v1.2.3)" squat_text="it is no release build"
  fi
  # The verified build stays addressable by its digest in this app's repository, whatever its tag names.
  seed() { blob "$app" "$B_NEW"; echo "$REG/$app $D_NEW" >> "$STATE/manifests"; }
  # T1: the verified digest, with the sha's tag already moved to a squat; the old tag is main's build.
  state; seed; old1=$(tag_now "$mv_ref")
  sed "s|^$REG/$app:$tag .*|$REG/$app:$tag $D_SQ|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"; blob "$app" "$B_SQ"
  STEP_DIR=$W/step-h run_step "$W/track.sh" "${args[@]}" DIGEST="$D_NEW"
  r1=$rc o1=$(cat "$W/output") now1=$(tag_now "$mv_ref") l1=false; logs_out && l1=true
  c1=$(grep -c "^docker buildx imagetools create --prefer-index=false --tag $mv_ref $REG/$app@$D_NEW$" "$STATE/log" || true)
  v1=$(grep -cxF "gh attestation verify oci://$REG/$app@$old1 $oldcheck" "$STATE/log" || true)
  bytag1=$(grep -c "imagetools inspect $REG/$app:$tag" "$STATE/log" || true) said1=false
  has "$W/step.out" "$mv_ref: $old1 -> $D_NEW" && said1=true
  # T2: the first move: no tag yet, so no old tag to check.
  state; seed; awk -v r="$mv_ref" '$1 != r' "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"
  STEP_DIR=$W/step-h run_step "$W/track.sh" "${args[@]}" DIGEST="$D_NEW"
  r2=$rc o2=$(cat "$W/output") now2=$(tag_now "$mv_ref") v2=$(grep -c '^gh ' "$STATE/log" || true)
  if [ "$r1" -eq 0 ] && [ "$now1" = "$D_NEW" ] && [ "$o1" = "old=$old1"$'\n'"new=$D_NEW" ] && [ "$l1" = true ] && [ "$c1" -eq 1 ] \
    && [ "$v1" -eq 1 ] && [ "$bytag1" -eq 0 ] && [ "$said1" = true ] && [[ "$old1" =~ ^sha256: ]] \
    && [ "$r2" -eq 0 ] && [ "$now2" = "$D_NEW" ] && [ "$o2" = "old=none"$'\n'"new=$D_NEW" ] && [ "$v2" -eq 0 ]; then
    ok "$file track: :$moving moved $old1 -> $D_NEW, the DIGEST it was given (the sha's tag named a squat: never read), by one carbon copy, read back, logged out after, once the old tag verified as main's build; the first move reads old=none and checks nothing old"
  else
    bad "$file track: rc $r1/$r2, now '$now1'/'$now2', out '$o1' / '$o2', copies $c1, old checks $v1/$v2, logout $l1"; show
  fi
  track_checked=$((track_checked + 1))
  # The refusals: each leaves the tag where it was, and writes no output.
  for t in "no digest||no verified digest to move :$moving to ('')" \
           "a malformed digest|sha256:xyz|no verified digest to move :$moving to ('sha256:xyz')" \
           "the old tag unreadable|registry-down|the registry could not be read for $mv_ref (only 'not found' means none yet)" \
           "the old tag names no digest|junk|$mv_ref resolved to no manifest digest: 'junk'" \
           "the newest build unreadable|nonewest|$nonewest_text" \
           "the copy fails|create-fails|" \
           "the tag moved under the copy|squat-after-create|$mv_ref names '$D_SQ' after the move, not the verified $D_NEW"; do
    IFS='|' read -r label how text <<< "$t"
    state; seed; before=$(tag_now "$mv_ref") digest=$D_NEW sd=$W/step-h
    case "$how" in
      '' | sha256:*) digest=$how ;;
      junk) sed "s|^$mv_ref .*|$mv_ref junk|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"; before=junk ;;
      squat-after-create) echo "$D_SQ" > "$STATE/squat-after-create"; before=$D_SQ ;;
      nonewest) sd=$nonewest_dir ;;
      *) touch "$STATE/$how" ;;
    esac
    STEP_DIR=$sd run_step "$W/track.sh" "${args[@]}" DIGEST="$digest"
    after=$(tag_now "$mv_ref")
    if [ "$rc" -ne 0 ] && [ ! -s "$W/output" ] && [ "$after" = "$before" ] \
      && { [ -z "$text" ] || grep -F '::error::' "$W/step.out" | grep -qF -- "$text"; } \
      && { { [ -n "$how" ] && [[ "$how" != sha256:* ]] && [ "$how" != nonewest ]; } || ! grep -q '^docker buildx imagetools create' "$STATE/log"; }; then
      ok "$file track refuses $label: no output, :$moving still '$after'$( [ -z "$text" ] || echo " :: $text")"
    else
      bad "$file track, $label: rc $rc, :$moving '$before' -> '$after', out '$(cat "$W/output")'"; show
    fi
    track_checked=$((track_checked + 1))
  done
  # T3 (item 2): an older build (stg: not main's tip; prod: not the highest release tag) moves nothing and
  # is no red: a warning that names the pin, and the skip in its output for the summary.
  state; seed; before=$(tag_now "$mv_ref")
  STEP_DIR=$W/step-h run_step "$W/track.sh" "${args[@]}" "${older[@]}" DIGEST="$D_NEW"
  if [ "$rc" -eq 0 ] && [ "$(tag_now "$mv_ref")" = "$before" ] && [ "$(cat "$W/output")" = "old=$before"$'\n'"skipped=$older_why" ] \
    && has "$W/step.out" "::warning::$mv_ref not moved: $older_why" && has "$W/step.out" "To run this build anyway, pin it: $older_pin" \
    && ! grep -q '^docker buildx imagetools create' "$STATE/log" && ! grep -q '^gh ' "$STATE/log"; then
    ok "$file track: an older build ($older_why) leaves :$moving at $before, warns with the pin, and says so to the summary"
  else
    bad "$file track, an older build: rc $rc, out '$(cat "$W/output")'"; show
  fi
  track_checked=$((track_checked + 1))
  # T4 (item 3): the old tag is no main build (a push from outside main's deploy). The move still goes
  # ahead, which restores main's build, and the step fails so the owner sees it.
  state; seed; sed "s|^$mv_ref .*|$mv_ref $D_ANY|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"
  STEP_DIR=$W/step-h run_step "$W/track.sh" "${args[@]}" DIGEST="$D_NEW"
  if [ "$rc" -ne 0 ] && [ "$(tag_now "$mv_ref")" = "$D_NEW" ] && [ "$(cat "$W/output")" = "old=$D_ANY"$'\n'"new=$D_NEW" ] \
    && grep -F '::error::' "$W/step.out" | grep -qF "$mv_ref named $D_ANY before this move, and $squat_text" \
    && has "$W/step.out" "the owner must check who moved it" \
    && [ "$(grep -c '^docker buildx imagetools create' "$STATE/log")" -eq 1 ]; then
    ok "$file track: an old :$moving that is no deploy's build ($D_ANY; $squat_text) is moved back to the verified $D_NEW, and the step fails naming it"
  else
    bad "$file track, a squatted old tag: rc $rc, :$moving '$(tag_now "$mv_ref")', out '$(cat "$W/output")'"; show
  fi
  track_checked=$((track_checked + 1))
done
# T5 (item 2): two release tags pushed together run in either order; :stable ends on the higher one, and
# the lower one's run skips in both orders. :stable starts on v1.1.0's build, here D_OLD (its record added).
extract "$PROD" push track "$W/track.sh"
two_order() { # two_order FIRST SECOND: run each release's track step in that order; prints what :stable names, and the skips
  local t d skips=0
  state; blob seazn-relay-prod "$B_OLD"; blob seazn-relay-prod "$B_NEW"
  printf '%s\n' "$REG/seazn-relay-prod $D_NEW" "$REG/seazn-relay-prod $D_ANY" >> "$STATE/manifests"
  sed "s|^$REG/seazn-relay-prod:stable .*|$REG/seazn-relay-prod:stable $D_OLD|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"
  attest_line "$D_OLD" "$WFURL/deploy-prod.yml@refs/tags/v1.1.0" refs/tags/v1.1.0 "$B" >> "$STATE/attestations"
  for t in "$@"; do
    d=$D_NEW; [ "$t" = v1.2.3 ] || d=$D_ANY
    : > "$W/output"
    STEP_DIR=$W/step-two run_step "$W/track.sh" RELAY_APP=seazn-relay-prod TAG="$t" FLY_API_TOKEN=fake-prod-push EXPECT_PUSH=fake-prod-push DIGEST="$d"
    [ "$rc" -eq 0 ] || { echo "rc$rc"; return; }
    if grep -q '^skipped=v1.2.3 is not the highest release tag (v1.2.4)$' "$W/output"; then skips=$((skips + 1)); fi
  done
  echo "$(tag_now "$REG/seazn-relay-prod:stable") $skips"
}
ab=$(two_order v1.2.3 v1.2.4) ba=$(two_order v1.2.4 v1.2.3)
if [ "$ab" = "$D_ANY 1" ] && [ "$ba" = "$D_ANY 1" ]; then
  ok "deploy-prod track: v1.2.3 and v1.2.4 pushed together, in either order, leave :stable on v1.2.4's build; v1.2.3's run skips both times"
else
  bad "deploy-prod track, two tags at once: v1.2.3 then v1.2.4 '$ab', v1.2.4 then v1.2.3 '$ba'"; show
fi
track_checked=$((track_checked + 1))
# T6 (item 2): release numbers compare as versions, and only vX.Y.Z tags count: v1.10.0 is above v1.9.0,
# and v9.0.0-rc1 is no release (a plain sort would pick v1.9.0 and skip; no filter would pick the rc).
git clone -q "$g" "$W/step-ten"; git -C "$W/step-ten" tag v1.10.0 "$C"; git -C "$W/step-ten" tag v9.0.0-rc1 "$C"
state; blob seazn-relay-prod "$B_NEW"; echo "$REG/seazn-relay-prod $D_NEW" >> "$STATE/manifests"
STEP_DIR=$W/step-ten run_step "$W/track.sh" RELAY_APP=seazn-relay-prod TAG=v1.10.0 FLY_API_TOKEN=fake-prod-push EXPECT_PUSH=fake-prod-push DIGEST="$D_NEW"
if [ "$rc" -eq 0 ] && [ "$(tag_now "$REG/seazn-relay-prod:stable")" = "$D_NEW" ] && ! grep -q '^skipped=' "$W/output"; then
  ok "deploy-prod track: v1.10.0 is the highest release beside v1.9.0 and v9.0.0-rc1, and moves :stable to $D_NEW"
else
  bad "deploy-prod track, v1.10.0 beside v1.9.0 and v9.0.0-rc1: rc $rc, :stable '$(tag_now "$REG/seazn-relay-prod:stable")', out '$(cat "$W/output")'"; show
fi
track_checked=$((track_checked + 1))
[ "$track_checked" -eq 22 ] || bad "the track step sweep checked $track_checked cases, not 22"
echo "track step cases checked: $track_checked"

# The chain, through the steps' own outputs, as the runner threads them (the C-1 rule, then the found
# path): Push's digest -> the record check's output -> the track step's DIGEST; a found tag moves :beta
# only once the record check verified it; a found squat stops at the record check, and a track step
# given its (empty) output refuses.
out_of() { sed -n "s/^$1=//p" "$W/output"; }
extract "$STG" push exists "$W/exists.sh"; extract "$STG" push push "$W/push.sh"; extract "$STG" push record "$W/record.sh"; extract "$STG" push track "$W/track.sh"
chain() { # chain PATH DIR: run the steps for that path, the track step in checkout DIR; sets c_rec (record's rc), c_track (track's rc), c_beta (what :beta names)
  local tag=$SHA
  [ "$1" != pushed ] || tag=$UNPUSHED
  local base=(RELAY_APP=seazn-relay-stg TAG="$tag" GITHUB_SHA="$tag" FLY_API_TOKEN=fake-stg-push)
  : > "$W/output"; run_step "$W/exists.sh" "${base[@]}"
  local found digest
  found=$(out_of exists) digest=$(out_of digest)
  if [ "$found" != true ]; then
    : > "$W/output"; echo "$D_NEW" > "$STATE/push-digest"
    run_step "$W/push.sh" "${base[@]}" LOCAL_IMAGE=seazn-relay:local; digest=$(out_of digest)
    # What the attest step signs: this push's own digest, as main's build of this commit.
    attest_line "$digest" "$ID_STG" refs/heads/main "$tag" >> "$STATE/attestations"
  fi
  : > "$W/output"; run_step "$W/record.sh" "${base[@]}" FOUND="$found" DIGEST="$digest"; c_rec=$rc
  local verified; verified=$(out_of digest)
  : > "$W/output"; STEP_DIR=$2 run_step "$W/track.sh" "${base[@]}" DIGEST="$verified"; c_track=$rc
  c_beta=$(tag_now "$REG/seazn-relay-stg:beta")
}
state; chain pushed "$W/step-h"
if [ "$c_rec" -eq 0 ] && [ "$c_track" -eq 0 ] && [ "$c_beta" = "$D_NEW" ]; then
  ok "chain, pushed: :beta names the digest the push itself printed ($D_NEW), through the record check's output"
else
  bad "chain, pushed: record $c_rec, track $c_track, :beta '$c_beta'"; show
fi
state; chain found "$W/step-c"
if [ "$c_rec" -eq 0 ] && [ "$c_track" -eq 0 ] && [ "$c_beta" = "$D_NEW" ]; then
  ok "chain, found and verified: :beta moved to the found digest only through the record check's output"
else
  bad "chain, found: record $c_rec, track $c_track, :beta '$c_beta'"; show
fi
# The same found build re-run once main has moved on (a re-run of an older merge: item 2): verified, and
# :beta stays on the newer build.
state; chain found "$W/step-h"
if [ "$c_rec" -eq 0 ] && [ "$c_track" -eq 0 ] && [ "$c_beta" = "$D_OLD" ] && has "$W/step.out" "::warning::$REG/seazn-relay-stg:beta not moved: $SHA is not main's tip ($H)"; then
  ok "chain, found on a superseded commit: the record verifies, and :beta stays at $D_OLD with a warning"
else
  bad "chain, found superseded: record $c_rec, track $c_track, :beta '$c_beta'"; show
fi
sq_state ""; chain found "$W/step-c"
if [ "$c_rec" -ne 0 ] && [ "$c_track" -ne 0 ] && [ "$c_beta" = "$D_OLD" ] && has "$W/step.out" "::error::no verified digest to move :beta to ('')"; then
  ok "chain, found squat: the record check refuses, writes no digest, and a track step given that output refuses; :beta still $D_OLD"
else
  bad "chain, found squat: record $c_rec, track $c_track, :beta '$c_beta'"; show
fi

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
cfg_path=$(sed -n 's/^curl-config ok //p' "$STATE/log" | head -n 1)
n_fly=$(grep -c '^flyctl ' "$STATE/log" || true)
n_cwd=$(grep -cE '^flyctl .* \[cwd [^]]*/cwd\]$' "$STATE/log" || true)
line_of() { grep -nF -- "$1" "$STATE/log" | head -n 1 | cut -d: -f1; }
if [ "$rc" -eq 0 ] && has "$STATE/log" "$set_line" \
  && has "$STATE/log" 'flyctl tokens create deploy -a seazn-relay-stg -n promote-read-stg-' && has "$STATE/log" ' -x 10m (token none)' \
  && has "$STATE/log" 'flyctl config show -a seazn-club-stg (token none)' \
  && [ -n "$cfg_path" ] && [ ! -e "$cfg_path" ] && has "$STATE/log" "curl -q -sS --proto =https --max-time 60 -K $cfg_path" \
  && ! has "$STATE/log" 'creds-present-at-set' \
  && has "$STATE/log" "https://$REG/v2/seazn-relay-stg/manifests/$D_NEW" && ! grep -q '^docker ' "$STATE/log" && no_token_seen \
  && [ "$(line_of 'flyctl secrets set')" -lt "$(line_of 'flyctl secrets deploy -a seazn-club-stg (token none)')" ] \
  && ! grep -qE 'token (WRONG|push)' "$STATE/log" && [ "$n_fly" -gt 0 ] && [ "$n_cwd" -eq "$n_fly" ] && ! has "$STATE/log" "[cwd $W/op" \
  && grep -qE "^gh attestation verify /[^ ]+/manifest\.json --repo $REPO --cert-identity $ID_STG --source-ref refs/heads/main --source-digest $SHA --signer-digest $SHA --deny-self-hosted-runners --limit 1000$" "$STATE/log" \
  && [ "$(line_of 'gh attestation verify')" -lt "$(line_of 'flyctl config show')" ] && [ "$(line_of 'curl-config ok')" -lt "$(line_of 'gh attestation verify')" ] \
  && has "$W/step.out" "  commit:                $SHA (on main; the CEF bump gate passed in its tree)" \
  && has "$W/step.out" "  build record:          attested by $ID_STG" \
  && has "$W/step.out" "  new RELAY_IMAGE:       $new_value" \
  && has "$W/step.out" "  previous RELAY_IMAGE:  $REG/seazn-relay-stg@$D_OLD (ref $OLD@$D_OLD)" \
  && has "$W/step.out" "  RELAY_IMAGE's digest:  d0 -> $(digest_of "$new_value") (Deployed)" \
  && has "$W/step.out" "Rollback: scripts/promote.sh stg $OLD@$D_OLD (it refuses" \
  && [ "$(tail -n 1 "$PLOG/stg.log" | cut -d' ' -f2-)" = "stg $SHA@$D_NEW $new_value $(digest_of "$new_value")" ]; then
  ok "promote P1: stg on the stored login, no docker; the manifest read by digest (curl -q first: no ~/.curlrc) on a 10-minute relay-only read token kept in a 0600 curl config (gone before the web app is touched, never in argv or output); main's record verified on that file before any web call; all $n_fly flyctl calls from the empty cwd; RELAY_IMAGE set by digest then deployed; previous = the log entry whose Fly digest is live, with its rollback; logged"
else
  bad "promote P1: rc $rc (flyctl $n_fly, from the empty cwd $n_cwd, config '$cfg_path')"; show
fi

# P2: the digest is not in the registry; P2b: the registry refuses the read (an error, never "not
# pushed", m-5); P2d: curl itself fails, and its error text (which here carries the credential) is
# never printed; P2f: the registry serves a body that is not the pin's.
state; refused_before_web "P2: a digest not in the registry" "not in the registry: $REG/seazn-relay-stg@$D_MISSING (HTTP 404; deploy-stg.yml pushes it)" -- stg "$SHA@$D_MISSING"
state; echo 401 > "$STATE/status-$D_NEW"
refused_before_web "P2b (m-5): a registry refusal, not \"not pushed\"" "the registry could not be read for $REG/seazn-relay-stg@$D_NEW (HTTP 401; only 404 means not pushed)" -- stg "$SHA@$D_NEW"
state; touch "$STATE/curl-fails"
refused_before_web "P2d: curl fails, its error text unprinted" "the registry could not be reached for $REG/seazn-relay-stg@$D_NEW (curl failed; its error text is not printed" -- stg "$SHA@$D_NEW"
state; touch "$STATE/lie"
refused_before_web "P2f: a body that is not the pin's" "the registry served $REG/seazn-relay-stg@$D_NEW as a body whose sha256 is sha256:$(sha_hex '{"not":"it"}'): nothing promoted" -- stg "$SHA@$D_NEW"
# P2c: no stored flyctl login: the read token cannot be minted, so nothing is read.
state; touch "$STATE/token-fails"
refused_before_web "P2c: no stored flyctl login" "flyctl tokens create deploy -a seazn-relay-stg failed (log in first: flyctl auth login): Error: you are not logged in (fake)" -- stg "$SHA@$D_NEW"
if ! grep -q '^curl ' "$STATE/log"; then ok "promote P2c: no registry read without a read token"; else bad "promote P2c read the registry"; show; fi
# P2e: flyctl printed something that is not one token line: refused, and none of it printed.
state; touch "$STATE/token-odd"
refused_before_web "P2e: a minted token of the wrong shape" "the token flyctl minted for seazn-relay-stg is not one line of the expected shape (it is not printed)" -- stg "$SHA@$D_NEW"
state; printf '%s\n' 'FlyV1 fakeReadToken7f3a" -o /tmp/x' > "$STATE/read-token"
refused_before_web "P2g: one line, not a token's shape (a quote would end curl's config string)" "the token flyctl minted for seazn-relay-stg is not one line of the expected shape (it is not printed)" -- stg "$SHA@$D_NEW"

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
# P4h (m-3): the secret list unreadable after the set (call 2) or after the deploy (call 3): each a
# refusal that says what may be live, never a silent stop; neither is logged as a promote.
state; echo 2 > "$STATE/list-fails-at"; promote_run -- stg "$SHA@$D_NEW"
r1=$rc e1=$(grep -m 1 -F 'promote: REFUSED: ' "$W/step.out" || true) d1=$(grep -c 'flyctl secrets deploy' "$STATE/log" || true)
state; echo 3 > "$STATE/list-fails-at"; promote_run -- stg "$SHA@$D_NEW"
e2=$(grep -m 1 -F 'promote: REFUSED: ' "$W/step.out" || true)
if [ "$r1" -ne 0 ] && [[ "$e1" == *"RELAY_IMAGE is staged on seazn-club-stg, and its secret list could not be read after the set: state unproven, nothing deployed"* ]] && [ "$d1" -eq 0 ] \
  && [ "$rc" -ne 0 ] && [[ "$e2" == *"the deploy ran on seazn-club-stg, and its secret list could not be read after it: RELAY_IMAGE may be live as $new_value, unproven and not logged"* ]] \
  && [ "$(wc -l < "$PLOG/stg.log" | tr -d ' ')" -eq 2 ]; then
  ok "promote P4h (m-3): a secret list unreadable after the set, or after the deploy, refuses and says what may be live"
else
  bad "promote P4h: rc $r1/$rc :: $e1 :: $e2"; show
fi
# P4i (m-7): a set that leaves RELAY_IMAGE off the list is refused before any deploy.
state; echo '[{"name":"FLY_RELAY_APP","digest":"f1","status":"Deployed"}]' > "$STATE/secrets.json"; touch "$STATE/set-noop"
promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -ne 0 ] && [[ "$(refusal)" == *"RELAY_IMAGE is not on seazn-club-stg's secret list after the set"* ]] && ! has "$STATE/log" 'flyctl secrets deploy'; then
  ok "promote P4i (m-7): a set that did not land is refused before the deploy"
else
  bad "promote P4i: rc $rc"; show
fi

# P5: prod, with --confirm-prod first (the flag's place does not matter); its previous ref (v1.0.0,
# from prod's log) is gone from the registry.
state; promote_run -- --confirm-prod prod "v1.2.3@$D_PROD"
if [ "$rc" -eq 0 ] && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$REG/seazn-relay-prod@$D_PROD -a seazn-club-prod --stage (token none)" \
  && has "$STATE/log" 'flyctl secrets deploy -a seazn-club-prod (token none)' && ! has "$STATE/log" 'seazn-club-stg' && ! has "$STATE/log" 'seazn-relay-stg' \
  && has "$STATE/log" "--cert-identity $WFURL/deploy-prod.yml@refs/tags/v1.2.3 --source-ref refs/tags/v1.2.3 --source-digest $C --signer-digest $C" \
  && has "$STATE/log" "https://$REG/v2/seazn-relay-prod/manifests/$D_PROD" \
  && has "$W/step.out" "  commit:                $C (on main" \
  && has "$W/step.out" "  previous RELAY_IMAGE:  $REG/seazn-relay-prod@$D_GONE (ref v1.0.0@$D_GONE; no longer in the registry)" \
  && has "$W/step.out" 'Rollback: the previous ref cannot be verified now' \
  && [ "$(tail -n 1 "$PLOG/prod.log" | cut -d' ' -f2-3)" = "prod v1.2.3@$D_PROD" ] && [ "$(wc -l < "$PLOG/stg.log" | tr -d ' ')" -eq 2 ]; then
  ok "promote P5: prod promotes v1.2.3 (its commit on main) on seazn-club-prod from seazn-relay-prod; a vanished previous digest is said; logged in prod's log alone"
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
# M (m-5): a test seam set in the operator's own shell is refused unless the harness turned seams on.
for v in RELAY_REGISTRY=registry.elsewhere PROMOTE_REMOTE=/tmp/elsewhere PROMOTE_STATE_DIR=/tmp/elsewhere; do
  refused "M (m-5): ${v%%=*} in the shell, no seams" "${v%%=*} is set in this shell, and promote reads its registry, its remote and its log only from their defaults" NOSEAMS=1 "$v" -- stg "$SHA@$D_NEW"
done

# T (m-7): the tools, each missing in turn, before any call. All 6 are swept.
tools_checked=0
for t in git jq curl flyctl gh; do
  refused "T: no $t on PATH" "$t is not on PATH" PATH="$(tools_bin "$t"):$W/sysbin" -- stg "$SHA@$D_NEW"
  tools_checked=$((tools_checked + 1))
done
refused "T: neither sha tool on PATH" "neither sha256sum nor shasum is on PATH" PATH="$(tools_bin sha sha256sum shasum):$W/sysbin" -- stg "$SHA@$D_NEW"
tools_checked=$((tools_checked + 1))
[ "$tools_checked" -eq 6 ] || bad "promote T swept $tools_checked tools, not 6"
# ... and the same minimal PATH with nothing removed promotes (the sweep's positive pair).
state; promote_run PATH="$(tools_bin none):$W/sysbin" -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$W/step.out" 'promoted '; then ok "promote T: the sweep's PATH with every tool promotes"; else bad "promote T positive pair: rc $rc"; show; fi

# K: no token is ever used, required or printed: one left in the shell is dropped, flyctl runs on the
# stored login, and the value appears nowhere in the output.
state; promote_run FLY_API_TOKEN=leaked-fly-value FLY_ACCESS_TOKEN=leaked-access-value -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$STATE/log" "$set_line" && ! grep -qE 'token (WRONG|push)' "$STATE/log" \
  && [ "$(grep -c '^flyctl ' "$STATE/log")" -eq "$(grep -c '^flyctl .*(token none)' "$STATE/log")" ] \
  && ! has "$W/step.out" 'leaked-' && ! has "$STATE/log" 'leaked-' && no_token_seen \
  && has "$W/step.out" 'promote: FLY_API_TOKEN is set in this shell; it is dropped, and flyctl uses your stored login' \
  && has "$W/step.out" 'promote: FLY_ACCESS_TOKEN is set in this shell; it is dropped'; then
  ok "promote K: tokens left in the shell are dropped, unprinted; all $(grep -c '^flyctl ' "$STATE/log") flyctl calls ran on the stored login"
else
  bad "promote K: rc $rc"; show
fi

# S: main's copy. An edited (or stale) copy of this script refuses; so does a main that cannot be
# fetched, a copy outside any checkout, and a main that lacks this script or the CEF bump gate.
cp "$W/op/scripts/promote.sh" "$W/op/scripts/promote-edited.sh"; echo '# a local edit' >> "$W/op/scripts/promote-edited.sh"
refused "S1: a copy that is not main's" "$W/op/scripts/promote-edited.sh is not main's copy (main is $H)" SCRIPT="$W/op/scripts/promote-edited.sh" -- stg "$SHA@$D_NEW"
refused "S2: main cannot be fetched" "main could not be fetched from $W/no-such-repo" PROMOTE_REMOTE="$W/no-such-repo" -- stg "$SHA@$D_NEW"
mkdir -p "$W/loose"; cp "$W/op/scripts/promote.sh" "$W/loose/promote.sh"
refused "S4 (m-7): a copy outside any git checkout" "$W/loose/promote.sh is not in a git checkout" SCRIPT="$W/loose/promote.sh" -- stg "$SHA@$D_NEW"
refused "S5 (m-7): a main with no promote.sh" "has no scripts/promote.sh" PROMOTE_REMOTE="$W/git-nopromote" -- stg "$SHA@$D_NEW"
refused "S6 (m-7): a main with no CEF bump gate" "has no scripts/check-cef-bump.sh" PROMOTE_REMOTE="$W/git-nocef" -- stg "$SHA@$D_NEW"
state; promote_run -- stg "$SHA@$D_NEW"
# S3: main fetched into refs/promote/main, never the operator's own refs.
if [ "$rc" -eq 0 ] && [ "$(git -C "$W/op" rev-parse refs/promote/main)" = "$H" ] && [ "$(git -C "$W/op" rev-parse refs/remotes/origin/main)" = "$H" ] \
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
refused "G8 (m-7): a prod tag that names a blob" "the tag v1.9.0 names no commit" -- prod "v1.9.0@$D_ANY" --confirm-prod
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
# P15: the previous logged digest cannot be read now: said, and no rollback offered.
state; echo 500 > "$STATE/status-$D_OLD"
promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$W/step.out" "  previous RELAY_IMAGE:  $REG/seazn-relay-stg@$D_OLD (ref $OLD@$D_OLD; the registry could not be read for it)" \
  && has "$W/step.out" 'Rollback: the previous ref cannot be verified now' && ! has "$W/step.out" 'Rollback: scripts/promote.sh'; then
  ok "promote P15: a previous digest the registry cannot serve now is said, with no rollback offered"
else
  bad "promote P15: rc $rc"; show
fi

state; touch "$STATE/lie-$D_OLD"
promote_run -- stg "$SHA@$D_NEW"
if [ "$rc" -eq 0 ] && has "$W/step.out" "  previous RELAY_IMAGE:  $REG/seazn-relay-stg@$D_OLD (ref $OLD@$D_OLD; the registry could not be read for it)" \
  && ! has "$W/step.out" 'Rollback: scripts/promote.sh'; then
  ok "promote P15b: a previous digest whose manifest no longer hashes to it is said, with no rollback offered"
else
  bad "promote P15b: rc $rc"; show
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

# P14 (m-4): promote never reads a tag. With a squat moved in under main's sha, main's own pin still
# promotes, and every registry read named a digest: a moved tag blocks neither a promote nor a rollback.
sq_state ""
promote_run -- stg "$SHA@$D_NEW"
n_reads=$(grep -c '^curl ' "$STATE/log" || true)
n_digest=$(grep -cE '^curl .*/manifests/sha256:[0-9a-f]{64}$' "$STATE/log" || true)
if [ "$rc" -eq 0 ] && has "$STATE/log" "$set_line" && [ "$n_reads" -gt 0 ] && [ "$n_reads" -eq "$n_digest" ]; then
  ok "promote P14 (m-4): main's pin promotes with the tag moved to a squat; $n_reads of $n_reads registry reads by digest"
else
  bad "promote P14: rc $rc, $n_digest of $n_reads reads by digest"; show
fi

# P17 (I-4): a squatted image. Some other push (a branch's workflow reads the repository push token)
# put $D_SQ in the registry, and the operator is handed $SHA@$D_SQ: every check before the record
# passes (the commit is on main, its tree passes the gate, the manifest is there and hashes to the
# pin). Only main's build record can refuse it, so the record is swept field by field: each forged
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
  refused_before_web "P17 (I-4): a squatted digest with $label" "$REG/seazn-relay-stg@$D_SQ ($SHA) is not main's own build: no attestation from $ID_STG for $SHA" -- stg "$SHA@$D_SQ"
done
sq_state "$GOOD_SQ"
promote_run -- stg "$SHA@$D_SQ"
if [ "$rc" -eq 0 ] && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$REG/seazn-relay-stg@$D_SQ -a seazn-club-stg --stage (token none)"; then
  ok "promote P17 (I-4): the positive pair: main's own record of that digest promotes it ($sq_checked forged or missing records refused)"
else
  bad "promote P17 positive pair: rc $rc"; show
fi
[ "$sq_checked" -eq 10 ] || bad "promote P17 swept $sq_checked rows, not 10"
# Prod: the same commit's stg build record does not serve a release.
state; blob seazn-relay-prod "$B_SQ"
attest_line "$D_SQ" "$ID_STG" refs/heads/main "$C" >> "$STATE/attestations"
refused_before_web "P17 (I-4): prod refuses a digest whose only record is stg's build of the same commit" "$REG/seazn-relay-prod@$D_SQ (v1.2.3) is not main's own build: no attestation from $WFURL/deploy-prod.yml@refs/tags/v1.2.3 for $C" -- prod "v1.2.3@$D_SQ" --confirm-prod

# TR (owner, 2026-10-26): promote --track. RELAY_IMAGE names the moving tag, set once, only after the
# tag's current digest verifies as main's build, field by field like a pin (P17's), plus the commit's
# place on main and its CEF bump gate. Base state: :beta names $OLD's build (D_OLD), :stable v1.2.3's.
track_value="$REG/seazn-relay-stg:beta" stable_value="$REG/seazn-relay-prod:stable"
RE_STG='^https://github\.com/onryde/seazn\.club\.compositor/\.github/workflows/deploy-stg\.yml@refs/heads/main$'
RE_PROD='^https://github\.com/onryde/seazn\.club\.compositor/\.github/workflows/deploy-prod\.yml@refs/tags/v[0-9]+\.[0-9]+\.[0-9]+$'
# TR1: stg, the happy path.
state; promote_run -- stg --track beta
if [ "$rc" -eq 0 ] && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$track_value -a seazn-club-stg --stage (token none)" \
  && has "$STATE/log" 'flyctl secrets deploy -a seazn-club-stg (token none)' \
  && has "$STATE/log" "https://$REG/v2/seazn-relay-stg/manifests/beta" \
  && has "$STATE/log" "--repo $REPO --cert-identity-regex $RE_STG --deny-self-hosted-runners --limit 1000 --format json" \
  && grep -qE "^gh attestation verify /[^ ]+/manifest\.json --repo $REPO --cert-identity $ID_STG --source-ref refs/heads/main --source-digest $OLD --signer-digest $OLD --deny-self-hosted-runners --limit 1000$" "$STATE/log" \
  && [ "$(line_of 'manifests/beta')" -lt "$(line_of 'cert-identity-regex')" ] && [ "$(line_of 'cert-identity-regex')" -lt "$(line_of "--cert-identity $ID_STG")" ] \
  && [ "$(line_of "--cert-identity $ID_STG")" -lt "$(line_of 'flyctl config show')" ] \
  && ! grep -q '^docker ' "$STATE/log" && no_token_seen \
  && has "$W/step.out" "  commit:                $OLD (on main; the CEF bump gate passed in its tree)" \
  && has "$W/step.out" "  build record:          attested by $ID_STG" \
  && has "$W/step.out" "  new RELAY_IMAGE:       $track_value" \
  && has "$W/step.out" "  following:             :beta, which names $D_OLD now ($OLD, main's build of $OLD)" \
  && has "$W/step.out" "Fly runs what it names whenever it creates a session's Machine" \
  && has "$W/step.out" "Nothing checks what it names when Fly follows it: a push by any holder of the repository push token is followed too." \
  && [ "$(tail -n 1 "$PLOG/stg.log" | cut -d' ' -f2-)" = "stg track:beta $track_value $(digest_of "$track_value") $D_OLD" ]; then
  ok "promote TR1: stg --track beta reads :beta (one read by tag), verifies its digest as main's build (discovered by identity, then the pin's exact verify at that commit), sets RELAY_IMAGE=$track_value, deploys, proves it, says what it follows, and logs it"
else
  bad "promote TR1: rc $rc"; show
fi
# TR2: prod, with --confirm-prod.
state; promote_run -- prod --track stable --confirm-prod
if [ "$rc" -eq 0 ] && has "$STATE/log" "flyctl secrets set RELAY_IMAGE=$stable_value -a seazn-club-prod --stage (token none)" \
  && has "$STATE/log" "--cert-identity-regex $RE_PROD" \
  && has "$STATE/log" "--cert-identity $WFURL/deploy-prod.yml@refs/tags/v1.2.3 --source-ref refs/tags/v1.2.3 --source-digest $C --signer-digest $C" \
  && ! has "$STATE/log" 'seazn-club-stg' && has "$W/step.out" "  following:             :stable, which names $D_PROD now (v1.2.3, main's build of $C)"; then
  ok "promote TR2: prod --track stable --confirm-prod follows :stable at v1.2.3's verified build"
else
  bad "promote TR2: rc $rc"; show
fi
# TR3: the arguments, refused before any call.
tr_checked=0
for t in "prod without --confirm-prod|a prod promote needs --confirm-prod (and C17 passed first): nothing promoted|prod --track stable" \
         "stg with --confirm-prod|--confirm-prod is for a prod promote, and this one is stg|stg --track beta --confirm-prod" \
         "stg tracking :stable|stg tracks :beta (deploy-stg.yml moves it once main's build record of a merge verifies), not 'stable'|stg --track stable" \
         "prod tracking :beta|prod tracks :stable (deploy-prod.yml moves it once a release tag's build record verifies), not 'beta'|prod --track beta --confirm-prod" \
         "another tag name|stg tracks :beta (deploy-stg.yml moves it once main's build record of a merge verifies), not 'latest'|stg --track latest" \
         "a track and a pin|--track beta follows a moving tag, and '$SHA@$D_NEW' pins a digest: give one or the other|stg --track beta $SHA@$D_NEW" \
         "no tag after --track|--track names the moving tag to follow: --track beta (stg) or --track stable (prod)|stg --track" \
         "a flag after --track|--track names the moving tag to follow|stg --track --confirm-prod" \
         "two --track|one --track, not two|stg --track beta --track beta"; do
  IFS='|' read -r label text argv <<< "$t"
  read -r -a words <<< "$argv"
  refused "TR3: $label" "$text" -- "${words[@]}"
  tr_checked=$((tr_checked + 1))
done
# TR4: the tag's current digest, refused before any call to the web app: absent, unreadable, a body that
# is not what the tag named, unattested, attested by a branch or a self-hosted runner, two builds, a
# commit off main or failing the CEF bump gate, and a record whose signer is another commit (discovery
# passes; the pin's exact verify at that commit refuses).
set_beta() { sed "s|^$REG/seazn-relay-stg:beta .*|$REG/seazn-relay-stg:beta $1|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"; }
tr4=(
  "absent|rm|not in the registry: $REG/seazn-relay-stg:beta (HTTP 404; deploy-stg.yml creates it once main's build record of a merge verifies)"
  "unreadable|status-beta 500|the registry could not be read for $REG/seazn-relay-stg:beta (HTTP 500; only 404 means absent)"
  "a body that is not the tag's|lie-beta|$REG/seazn-relay-stg:beta (sha256:$(sha_hex '{"not":"it"}')) is not main's own build: no attestation from $ID_STG"
  "unattested|any|$REG/seazn-relay-stg:beta ($D_ANY) is not main's own build: no attestation from $ID_STG"
  "attested by a branch's workflow|any $D_ANY $WFURL/deploy-stg.yml@refs/heads/feat/x refs/heads/feat/x $SHA $SHA github-hosted $REPO|($D_ANY) is not main's own build"
  "attested on a self-hosted runner|any $D_ANY $ID_STG refs/heads/main $SHA $SHA self-hosted $REPO|($D_ANY) is not main's own build"
  "attested as two builds|any2|$REG/seazn-relay-stg:beta ($D_ANY) carries main's attestation for 2 builds, not one"
  "a commit off main|any $D_ANY $ID_STG refs/heads/main $SIDE $SIDE github-hosted $REPO|$SIDE ($SIDE) is not on main: images are promoted from main only"
  "a commit failing the CEF bump gate|any $D_ANY $ID_STG refs/heads/main $BAD $BAD github-hosted $REPO|$BAD ($BAD) fails the CEF bump gate (Owed 22) in its own tree"
  "a signer at another commit|any $D_ANY $ID_STG refs/heads/main $SHA $SIDE github-hosted $REPO|$REG/seazn-relay-stg@$D_ANY ($SHA) is not main's own build: no attestation from $ID_STG for $SHA"
  "a record naming no commit|any $D_ANY $ID_STG refs/heads/main main main github-hosted $REPO|$REG/seazn-relay-stg:beta ($D_ANY) has a build record that names no commit: 'main'"
  "a record at another source ref|any $D_ANY $ID_STG refs/heads/feat/x $SHA $SHA github-hosted $REPO|$REG/seazn-relay-stg:beta ($D_ANY) was built at 'refs/heads/feat/x', not main"
)
for row in "${tr4[@]}"; do
  IFS='|' read -r label how text <<< "$row"
  state
  case "$how" in
    rm) awk -v r="$REG/seazn-relay-stg:beta" '$1 != r' "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry" ;;
    "status-beta 500") echo 500 > "$STATE/status-beta" ;;
    lie-beta) touch "$STATE/lie-beta" ;;
    any) set_beta "$D_ANY" ;;
    any2) set_beta "$D_ANY"; { attest_line "$D_ANY" "$ID_STG" refs/heads/main "$OLD"; attest_line "$D_ANY" "$ID_STG" refs/heads/main "$SHA"; } >> "$STATE/attestations" ;;
    any\ *) set_beta "$D_ANY"; echo "${how#any }" >> "$STATE/attestations" ;;
  esac
  refused_before_web "TR4: :beta $label" "$text" -- stg --track beta
  tr_checked=$((tr_checked + 1))
done
# TR4's positive pair: the same digest with main's own record of a commit on main follows.
state; set_beta "$D_ANY"; attest_line "$D_ANY" "$ID_STG" refs/heads/main "$SHA" >> "$STATE/attestations"
promote_run -- stg --track beta
if [ "$rc" -eq 0 ] && has "$W/step.out" "  following:             :beta, which names $D_ANY now ($SHA, main's build of $SHA)"; then
  ok "promote TR4: the positive pair: main's own record of $D_ANY at a commit on main follows it ($tr_checked refusals before it)"
else
  bad "promote TR4 positive pair: rc $rc"; show
fi
# TR5: prod's tag-shaped refusals: a release off main, and a record at a ref that is not a release tag.
state; sed "s|^$REG/seazn-relay-prod:stable .*|$REG/seazn-relay-prod:stable $D_ANY|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"
attest_line "$D_ANY" "$WFURL/deploy-prod.yml@refs/tags/v1.2.4" refs/tags/v1.2.4 "$SIDE" >> "$STATE/attestations"
refused_before_web "TR5: :stable built from a release off main" "v1.2.4 ($SIDE) is not on main: images are promoted from main only" -- prod --track stable --confirm-prod
tr_checked=$((tr_checked + 1))
state; sed "s|^$REG/seazn-relay-prod:stable .*|$REG/seazn-relay-prod:stable $D_ANY|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"
echo "$D_ANY $WFURL/deploy-prod.yml@refs/tags/v1.2.4 refs/heads/main $C $C github-hosted $REPO" >> "$STATE/attestations"
refused_before_web "TR5: :stable recorded at a ref that is no release tag" "$REG/seazn-relay-prod:stable ($D_ANY) was built at 'refs/heads/main', not a release tag" -- prod --track stable --confirm-prod
tr_checked=$((tr_checked + 1))
# A release tag that names another commit in git now than the one its build record was made from ($A is
# on main and passes the CEF bump gate, so only this compare refuses before the exact verify would).
state; sed "s|^$REG/seazn-relay-prod:stable .*|$REG/seazn-relay-prod:stable $D_ANY|" "$STATE/registry" > "$STATE/r.tmp" && mv "$STATE/r.tmp" "$STATE/registry"
attest_line "$D_ANY" "$WFURL/deploy-prod.yml@refs/tags/v1.2.3" refs/tags/v1.2.3 "$A" >> "$STATE/attestations"
refused_before_web "TR5: :stable built from a commit its release tag no longer names" "v1.2.3 names $C in $g, and $REG/seazn-relay-prod:stable ($D_ANY) was built from $A: nothing promoted" -- prod --track stable --confirm-prod
tr_checked=$((tr_checked + 1))
[ "$tr_checked" -eq 24 ] || bad "promote TR swept $tr_checked refusals, not 24"
echo "promote --track refusals checked: $tr_checked"
# TR6 (the sequence, and the two rollbacks): track, then pin a build (rollback (a): RELAY_IMAGE pinned, so
# :beta is not followed), then track again. Each names the one before as its previous value.
state; promote_run -- stg --track beta; r1=$rc
promote_run -- stg "$SHA@$D_NEW"; r2=$rc; out2=$(cat "$W/step.out")
promote_run -- stg --track beta; r3=$rc
if [ "$r1" -eq 0 ] && [ "$r2" -eq 0 ] && [ "$r3" -eq 0 ] \
  && [[ "$out2" == *"  previous RELAY_IMAGE:  $track_value (tracking :beta; --track verifies what it names when it runs)"* ]] \
  && [[ "$out2" == *"Rollback: scripts/promote.sh stg --track beta (it refuses unless what :beta names then verifies as main's build)"* ]] \
  && [[ "$out2" == *"  following:             no: pinned to $D_NEW. deploy-stg.yml still moves :beta on every merge to main, and while pinned it is not followed. To follow it: scripts/promote.sh stg --track beta"* ]] \
  && has "$W/step.out" "  previous RELAY_IMAGE:  $new_value (ref $SHA@$D_NEW)" && has "$W/step.out" "Rollback: scripts/promote.sh stg $SHA@$D_NEW (" \
  && [ "$(wc -l < "$PLOG/stg.log" | tr -d ' ')" -eq 5 ]; then
  ok "promote TR6: track, pin, track: the pin says :beta is not followed while pinned and offers --track back; the track after it offers the pin back"
else
  bad "promote TR6: rc $r1/$r2/$r3"; echo "$out2" >&2; show
fi
# TR7 (a hand-set value, checked): the owner set RELAY_IMAGE to the tag by hand, still Staged. --track sets
# the same value, so its Fly digest does not move (equal values give equal Fly digests, measured
# 2026-10-25): the warning is the proof the hand-set value was the tag.
state; echo "[{\"name\":\"FLY_RELAY_APP\",\"digest\":\"f1\",\"status\":\"Deployed\"},{\"name\":\"RELAY_IMAGE\",\"digest\":\"$(digest_of "$track_value")\",\"status\":\"Staged\"}]" > "$STATE/secrets.json"
promote_run -- stg --track beta
if [ "$rc" -eq 0 ] && has "$W/step.out" "promote: warning: RELAY_IMAGE's Fly digest did not change ($(digest_of "$track_value")): seazn-club-stg already had this value" \
  && has "$W/step.out" "  previous RELAY_IMAGE:  set elsewhere (Fly digest $(digest_of "$track_value");" \
  && has "$STATE/log" 'flyctl secrets deploy -a seazn-club-stg (token none)'; then
  ok "promote TR7: a hand-set, staged RELAY_IMAGE equal to the tag: --track verifies :beta, deploys, and its unchanged Fly digest says the hand-set value was $track_value"
else
  bad "promote TR7: rc $rc"; show
fi

echo "checked: $checked, failed: $failures"
finished=1
if [ "$checked" -eq 0 ]; then
  echo "deploy_checks_test: FAIL: checked nothing" >&2
  exit 1
fi
[ "$failures" -eq 0 ] || exit 1
exit 0
