#!/usr/bin/env bash
# The workflow check (C16, docs/tasks/C16-deploy.md, with its amendments 2026-10-18, 2026-10-23,
# 2026-10-24 and 2026-10-25). check.yml runs it, and scripts/deploy_checks_test.sh runs it on scratch
# copies, each with one mutant that must red one rule below.
#
#   scripts/check-workflows.sh [DIR]            check the workflows in DIR (default .github/workflows)
#   scripts/check-workflows.sh --install DIR    put the pinned actionlint and shellcheck in DIR
#
# The shape (owner, 2026-10-25, decision log): deploy-stg.yml and deploy-prod.yml push the relay image to
# Fly's registry with a relay-only REPOSITORY secret; no GitHub environment; promotion is
# scripts/promote.sh, run locally with the operator's own flyctl login, so no web-app token is in GitHub.
#
# Rules (each prints what it checked; any FAIL line fails the run, after every rule has reported):
#  1. actionlint (pinned): every workflow with shellcheck off (the older workflows' scripts are not
#     this check's to rewrite), then the deploy workflows again with the pinned shellcheck.
#  2. The Fly token guard: every workflow that names a secrets.FLY_API_TOKEN* has, for each one, a
#     step that warns "::warning::<NAME> unset" and exits 0 when it is empty, and every other step
#     that holds the token runs only when that guard said enabled (its job's `if` reads the guard
#     job's `enabled` output, or its own `if` reads the guard step's, earlier in the same job; in a
#     deploy workflow that `if` is exactly the guard's condition, nothing more). In a deploy workflow,
#     every step after the guard in its job reads it too (its `if` is the guard's condition, alone or
#     followed by `&& ...`), so an unset token is a skipped push, never a red. A job-level env never
#     holds a Fly token, and no expression names two (`a && X || Y` hands a run Y's token whenever X's
#     is empty).
#  3. No secret is echoed: no workflow-level env holds a secret, no run block interpolates
#     ${{ secrets.* }} (a secret goes in by env), and no run line that names a secret-holding var
#     prints it (echo, printf, tee, a >> redirect) or runs under xtrace. A secret-holding var is an
#     env key whose value names a secret, any FLY_API_TOKEN*, GH_TOKEN or GITHUB_TOKEN, and any var
#     a run line assigns from one (an alias launders nothing). A step that holds a secret never
#     dumps the environment (env, printenv, declare/export -p, a bare set) and never hands flyctl a
#     token in argv (-t, --access-token). One print is not an echo: into a file under $RUNNER_TEMP,
#     with no tee and no GitHub command file on the line (soak.yml's leak gate writes the token to a
#     umask-077 pattern file to grep the outputs for it). Into the log, $GITHUB_OUTPUT, $GITHUB_ENV
#     or the step summary is.
#  4. No GitHub environment (owner, 2026-10-25): no job in any workflow declares `environment`. A job
#     that names one makes GitHub create it, bare, on the first run.
#  5. The secrets each workflow may name (blast radius): a deploy workflow names exactly one secret,
#     its own relay token (deploy-stg.yml FLY_API_TOKEN_RELAY_STG, deploy-prod.yml
#     FLY_API_TOKEN_RELAY_PROD, each minted `fly tokens create deploy -a seazn-relay-<env>`), spelled
#     so, and only inside its push job; soak.yml may name FLY_API_TOKEN_SOAK (named exemption: the soak
#     app's, C15); no other workflow names a Fly secret (any spelling: secret names ignore case); and no
#     workflow names a secret by expression (secrets[...], toJSON(secrets) in any case), which no rule
#     here can read, or hands secrets (`secrets: inherit` or a map) to a reusable workflow outside this
#     repository (the PR #93 review's m-1).
#  6. No web-app token, and no web app, anywhere: no workflow names a secret whose name carries WEB or
#     CLUB, and no line hands flyctl a seazn-club-* app (-a, --app, FLY_APP). Only scripts/promote.sh,
#     on the operator's own login, touches the web apps.
#  7. The CEF bump gate (Owed 22): in every deploy workflow, every job that holds a Fly token checks
#     out before it uses the token, and each checkout's very next step is exactly
#     `run: scripts/check-cef-bump.sh` with nothing but a name or id (no if, continue-on-error, shell
#     or env, no `|| true`). Such a job and its steps set no continue-on-error, and no step `if`
#     calls always(), failure() or cancelled() (a step that runs past a failed gate).
#  8. No deploy workflow names the CI build cache, seazn-relay-cache (amendment 2026-10-23), or a
#     test-hooks target (relay-test-hooks, rust-build-test-hooks, test-hooks), and every build target
#     it names is `relay` (amendment 2026-10-24).
#  9. The registry is the RELAY_REGISTRY variable (ADR 0007): a deploy workflow writes
#     registry.fly.io only on a line that reads vars.RELAY_REGISTRY (its default).
# 10. Provenance (I-2): deploy-stg.yml's and deploy-prod.yml's guard job runs a step with id
#     `provenance` before the guard (stg: the ref is refs/heads/main; prod: the tag's commit is on main,
#     by merge-base --is-ancestor), which exits 1 on a refusal and carries no if, continue-on-error or
#     shell.
# 11. Main only, as hygiene (not a guard: a branch's own copy drops it): each push job's `if` is
#     exactly `github.ref == 'refs/heads/main'` (deploy-stg.yml) or
#     `startsWith(github.ref, 'refs/tags/v')` (deploy-prod.yml).
# 12. Main's build record (the PR #85 review's I-4; what keeps a squatted image from being promoted):
#     the push job attests the pushed digest (a step with id `attest` using actions/attest pinned by
#     commit, after the Push step, on steps.push.outputs.digest), then verifies on BOTH paths, pushed
#     and found (a step with id `record`, after the attest step and before the summary, whose `if` is
#     at most the token guard's own condition) with `gh attestation verify --cert-identity` naming
#     deploy-stg.yml@refs/heads/main or deploy-prod.yml@refs/tags/$TAG, its --source-ref,
#     --source-digest "$GITHUB_SHA", --signer-digest "$GITHUB_SHA" and --deny-self-hosted-runners,
#     exiting 1 when it does not verify.
# 13. Write grants (m-11): a permission set to `write`, or write-all, appears only as id-token and
#     attestations on deploy-stg.yml's and deploy-prod.yml's push jobs (each grants both: the attest
#     step signs with them) and as packages on image.yml's cache job (named exemption: it writes the CI
#     build cache), never at workflow level: anywhere else a job could sign as that workflow, or write
#     a package. Prints `N of 5`.
# 14. No promote workflow: no workflow file is named promote*, and no workflow runs `flyctl secrets`
#     set, deploy, unset, import or sync (a promote by what it does). Promotion is scripts/promote.sh.
# 15. The login leaves with its step (the PR #93 review's m-2): in each push job, a step that runs
#     `flyctl auth docker` sets `trap 'docker logout "$RELAY_REGISTRY" …' EXIT`, so no later step or
#     third-party action finds the token in docker's config, and every action it uses is pinned by
#     commit.
#
# The deploy workflows are deploy-stg.yml and deploy-prod.yml, each required, plus any other workflow
# that names the stg or prod relay or web app, or their tokens: a new one is held to the deploy rules
# by what it does, not by its name. Prints `checked N workflows`; N = 0 fails.
#
# Needs bash, ruby (YAML), jq, and actionlint and shellcheck at the versions pinned below.
set -euo pipefail

ACTIONLINT_VERSION=1.7.12
SHELLCHECK_VERSION=0.11.0
REQUIRED_DEPLOY=(deploy-stg.yml deploy-prod.yml)
DEPLOY_PATTERN='seazn-(relay|club)-(stg|prod)|FLY_API_TOKEN_(RELAY_|WEB_)?(STG|PROD)'

root=$(cd "$(dirname "$0")/.." && pwd)

install_tools() {
  local dir=$1 os arch al sc al_sum sc_sum tmp
  os=$(uname -s) arch=$(uname -m)
  case "$os/$arch" in
    Linux/x86_64)
      al=linux_amd64 al_sum=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
      sc=linux.x86_64 sc_sum=b7af85e41cc99489dcc21d66c6d5f3685138f06d34651e6d34b42ec6d54fe6f6 ;;
    Darwin/x86_64)
      al=darwin_amd64 al_sum=5b44c3bc2255115c9b69e30efc0fecdf498fdb63c5d58e17084fd5f16324c644
      sc=darwin.x86_64 sc_sum=c2c15e08df0e8fbc374c335b230a7ee958c313fa5714817a59aa59f1aa594f51 ;;
    Darwin/arm64)
      al=darwin_arm64 al_sum=aba9ced2dee8d27fecca3dc7feb1a7f9a52caefa1eb46f3271ea66b6e0e6953f
      sc=darwin.aarch64 sc_sum=339b930feb1ea764467013cc1f72d09cd6b869ebf1013296ba9055ab2ffbd26f ;;
    *) echo "check-workflows: FAIL: no pinned tools for $os/$arch" >&2; exit 1 ;;
  esac
  mkdir -p "$dir"
  tmp=$(mktemp -d)
  # actionlint's sums are its release's actionlint_<v>_checksums.txt; shellcheck publishes none, so
  # its sums were taken from the release tarballs when pinned (2026-10-25).
  curl -fsSL -o "$tmp/al.tgz" \
    "https://github.com/rhysd/actionlint/releases/download/v${ACTIONLINT_VERSION}/actionlint_${ACTIONLINT_VERSION}_${al}.tar.gz"
  curl -fsSL -o "$tmp/sc.tgz" \
    "https://github.com/koalaman/shellcheck/releases/download/v${SHELLCHECK_VERSION}/shellcheck-v${SHELLCHECK_VERSION}.${sc}.tar.gz"
  check_sum "$tmp/al.tgz" "$al_sum"
  check_sum "$tmp/sc.tgz" "$sc_sum"
  tar -xzf "$tmp/al.tgz" -C "$dir" actionlint
  tar -xzf "$tmp/sc.tgz" -C "$tmp" "shellcheck-v${SHELLCHECK_VERSION}/shellcheck"
  mv "$tmp/shellcheck-v${SHELLCHECK_VERSION}/shellcheck" "$dir/shellcheck"
  rm -rf "$tmp"
  echo "installed actionlint $ACTIONLINT_VERSION and shellcheck $SHELLCHECK_VERSION in $dir"
}

check_sum() {
  local got
  got=$( (sha256sum "$1" 2> /dev/null || shasum -a 256 "$1") | awk '{print $1}')
  if [ "$got" != "$2" ]; then
    echo "check-workflows: FAIL: $(basename "$1") sha256 $got, pinned $2" >&2
    exit 1
  fi
}

if [ "${1:-}" = --install ]; then
  [ -n "${2:-}" ] || { echo "usage: $0 --install DIR" >&2; exit 2; }
  install_tools "$2"
  exit 0
fi

dir=${1:-$root/.github/workflows}

files=()
if [ -d "$dir" ]; then
  while IFS= read -r f; do files+=("$f"); done < <(find "$dir" -maxdepth 1 -type f \( -name '*.yml' -o -name '*.yaml' \) | LC_ALL=C sort)
fi
echo "checked ${#files[@]} workflows in $dir"
if [ "${#files[@]}" -eq 0 ]; then
  echo "check-workflows: FAIL: 0 workflows found: a check of nothing proves nothing" >&2
  exit 1
fi

fails=0
failed() {
  echo "check-workflows: FAIL: $*" >&2
  fails=$((fails + 1))
}

# The deploy set: the two by name (each required), then any other by what it names.
deploy=()
for name in "${REQUIRED_DEPLOY[@]}"; do
  if [ -f "$dir/$name" ]; then
    deploy+=("$dir/$name")
  else
    failed "deploy workflow missing: $name"
  fi
done
for f in "${files[@]}"; do
  case " ${REQUIRED_DEPLOY[*]} " in *" $(basename "$f") "*) continue ;; esac
  if grep -Eq "$DEPLOY_PATTERN" "$f"; then deploy+=("$f"); fi
done
echo "deploy workflows: ${#deploy[@]} ($(for f in ${deploy[@]+"${deploy[@]}"}; do basename "$f"; done | tr '\n' ' '))"
is_deploy() {
  local d
  for d in ${deploy[@]+"${deploy[@]}"}; do [ "$d" = "$1" ] && return 0; done
  return 1
}

# Each workflow as JSON (YAML.load reads `on:` as the key true; nothing here reads triggers).
tmp=$(mktemp -d)
# A run that stops before its verdict is a red: bash 3.2 exits 0 after a `set -u` abort under an
# EXIT trap.
trap 'rm -rf "$tmp"; if [ "${finished:-0}" != 1 ]; then echo "check-workflows: FAIL: stopped before its verdict" >&2; exit 1; fi' EXIT
json() { echo "$tmp/$(basename "$1").json"; }
for f in "${files[@]}"; do
  if ! ruby -ryaml -rjson -e 'puts JSON.generate(YAML.load(File.read(ARGV[0])))' "$f" > "$(json "$f")" 2> "$tmp/yaml.err"; then
    failed "$(basename "$f") is not YAML: $(head -c 300 "$tmp/yaml.err")"
    echo 'null' > "$(json "$f")"
  fi
done

# 1. actionlint.
al_ver=$(actionlint -version 2> /dev/null | head -n 1 || true)
sc_ver=$(shellcheck --version 2> /dev/null | awk '/^version:/ {print $2}' || true)
if [ "$al_ver" != "$ACTIONLINT_VERSION" ] || [ "$sc_ver" != "$SHELLCHECK_VERSION" ]; then
  failed "actionlint ${al_ver:-absent} / shellcheck ${sc_ver:-absent}, pinned $ACTIONLINT_VERSION / $SHELLCHECK_VERSION (scripts/check-workflows.sh --install DIR, then put DIR on PATH)"
else
  if actionlint -shellcheck= -pyflakes= "${files[@]}" > "$tmp/al.log" 2>&1; then
    echo "actionlint $al_ver: ${#files[@]} workflows clean (shellcheck off)"
  else
    cat "$tmp/al.log" >&2
    failed "actionlint: $(grep -c '^' "$tmp/al.log") line(s) above"
  fi
  if [ "${#deploy[@]}" -gt 0 ]; then
    if actionlint -shellcheck="$(command -v shellcheck)" -pyflakes= "${deploy[@]}" > "$tmp/al-sc.log" 2>&1; then
      echo "actionlint $al_ver + shellcheck $sc_ver: ${#deploy[@]} deploy workflows clean"
    else
      cat "$tmp/al-sc.log" >&2
      failed "actionlint with shellcheck on the deploy workflows: $(grep -c '^' "$tmp/al-sc.log") line(s) above"
    fi
  fi
fi

# jq helpers shared by the rules.
read -r -d '' JQ_LIB << 'JQ' || true
def holds_secret($x): tostring | test("secrets\\." + $x + "([^A-Za-z0-9_]|$)");
def holds_fly: tostring | test("secrets\\.FLY_API_TOKEN");
def strip: sub("^\\s+"; "") | sub("\\s+$"; "");
def needs_list: (.needs // []) | if type == "string" then [.] else . end;
def step_name($i): (.name // .id // .uses // "step \($i)");
# A guard reads the token as `${{ secrets.X }}`, exactly (one secret, no expression around it).
def token_value($x): tostring | strip | test("^\\$\\{\\{ *secrets\\." + $x + " *\\}\\}$");
# An `if` without its ${{ }}, for an exact comparison.
def norm_if: tostring | strip | sub("^\\$\\{\\{\\s*"; "") | sub("\\s*\\}\\}$"; "") | strip;
# An if that reads guard condition $c: exactly $c, or $c && <more>, with no || that could run without it.
def reads_guard($c): . == $c or (startswith($c + " && ") and (ltrimstr($c + " && ") | test("\\|\\|") | not));
def has_status_fn: tostring | test("(always|failure|cancelled)\\(\\)");
# Rule 3: the names that hold a secret, and the names a line assigns.
def secret_names($keys): ["FLY_API_TOKEN[A-Za-z0-9_]*", "GH_TOKEN", "GITHUB_TOKEN"] + $keys;
def refs_secret($names): test("\\$\\{?(" + ($names | join("|")) + ")([^A-Za-z0-9_]|$)");
def assigned: [match("(?:^|[\\s;(&|])(?:(?:export|local|readonly|declare|typeset)\\s+(?:-[A-Za-z]+\\s+)*)?([A-Za-z_][A-Za-z0-9_]*)=(?!=)"; "g") | .captures[0].string];
def guard_key($x): [(.env // {}) | to_entries[] | select(.value | token_value($x)) | .key][0];
def is_guard($x):
  guard_key($x) as $k
  | $k != null and ((.id // "") | length) > 0
    and ((.run // "") as $r
      | ($r | (contains("-z \"$" + $k + "\"") or contains("-z \"${" + $k + "}\"")))
        and ($r | contains("::warning::" + $x + " unset"))
        and ($r | test("(^|[;\\s])exit 0([;\\s]|$)")));
# The secrets a value names inside its ${{ }} expressions, as written (secret names ignore case, so this
# reads any spelling; a file called secrets.json in a script is no secret).
def secret_refs: [tostring | match("\\$\\{\\{[^}]*\\}\\}"; "g") | .string | match("secrets\\.([A-Za-z_][A-Za-z0-9_]*)"; "g") | .captures[0].string] | unique;
def envname: if type == "object" then (.name // "") else (. // "" | tostring) end;
JQ

# 2. The Fly token guard.
guarded=0 gated=0 after_guard=0
for f in "${files[@]}"; do
  tokens=$({ grep -oE 'secrets\.FLY_API_TOKEN[A-Z0-9_]*' "$f" || true; } | sed 's/^secrets\.//' | LC_ALL=C sort -u)
  for x in $tokens; do
    strict=false
    if is_deploy "$f"; then strict=true; fi
    out=$(jq -r --arg x "$x" --argjson strict "$strict" "$JQ_LIB"'
      . as $w
      | [ $w.jobs | to_entries[] | .key as $j | (.value.steps // []) | to_entries[]
          | select(.value | is_guard($x)) | {job: $j, idx: .key, id: .value.id} ] as $guards
      | if ($guards | length) == 0 then
          "FAIL no step warns \"::warning::\($x) unset\" and exits 0 when the token is empty"
        else
          "GUARD \($guards | map("\(.job).\(.id)") | join(","))",
          ( $w.jobs | to_entries[] | .key as $j | .value as $J
            | ( if (($J.env // {}) | holds_secret($x)) then
                  "FAIL job \($j) holds \($x) in its env: a token goes only in the steps that use it"
                else empty end ),
              ( ($J.steps // []) | to_entries[] | .key as $i | .value as $s
                | select(($s | holds_secret($x)) and (($s | is_guard($x)) | not))
                | [ $guards[] | select(.job == $j and .idx < $i) | "steps.\(.id).outputs.enabled == '"'"'true'"'"'" ] as $step_gates
                | [ $guards[] | select(.job as $g | .id as $id | ($J | needs_list | any(.[]; . == $g))
                      and (($w.jobs[$g].outputs.enabled // "") | tostring | contains("steps.\($id).outputs.enabled")))
                    | "needs.\(.job).outputs.enabled == '"'"'true'"'"'" ] as $job_gates
                | (if $strict then
                     any($step_gates[]; . as $g | (($s.if // "") | norm_if) | reads_guard($g))
                     or (any($job_gates[]; . as $g | (($J.if // "") | norm_if) == $g)
                       and ((($s.if // "") | has_status_fn) | not))
                   else
                     any($step_gates[]; . as $g | (($s.if // "") | tostring | contains($g)))
                     or any($job_gates[]; . as $g | (($J.if // "") | tostring | contains($g)))
                   end) as $ok
                | if $ok
                  then "GATED \($j).\($s | step_name($i))"
                  else "FAIL \($j).\($s | step_name($i)) holds \($x) but runs whether or not the guard found it" end ) ),
          # A deploy workflow: every step after the guard in its job reads it, so an unset token
          # skips the push instead of failing it.
          ( select($strict) | $guards[] as $g
            | ("steps.\($g.id).outputs.enabled == '"'"'true'"'"'") as $c
            | $w.jobs[$g.job].steps | to_entries[] | select(.key > $g.idx) | .key as $i | .value as $s
            | (($s.if // "") | norm_if) as $if
            | if ($if | reads_guard($c)) then "AFTER \($g.job).\($s | step_name($i))"
              else "FAIL \($g.job).\($s | step_name($i)) comes after the \($x) guard but does not read it (its if must be \($c), alone or followed by &&): an unset token would red the push, not skip it" end )
        end' "$(json "$f")")
    while IFS= read -r line; do
      case "$line" in
        FAIL*) failed "$(basename "$f") $x: ${line#FAIL }" ;;
        GUARD*) guarded=$((guarded + 1)); echo "fly token guard: $(basename "$f") $x: ${line#GUARD }" ;;
        GATED*) gated=$((gated + 1)) ;;
        AFTER*) after_guard=$((after_guard + 1)) ;;
      esac
    done <<< "$out"
  done
done
echo "fly token guards: $guarded, token steps gated by them: $gated, deploy steps after a guard that read it: $after_guard"
[ "$guarded" -gt 0 ] || failed "no Fly token guard found anywhere (the deploy workflows hold tokens)"
# One expression, one Fly token: `a && secrets.X || secrets.Y` hands a run Y's token whenever X's is
# empty, which is how a prod run would reach stg (or the reverse).
fallbacks=0
for f in "${files[@]}"; do
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    fallbacks=$((fallbacks + 1))
    failed "$(basename "$f") names two Fly tokens in one expression (a fallback): $line"
  done < <(grep -n -E 'secrets\.FLY_API_TOKEN[A-Z0-9_]*.*secrets\.FLY_API_TOKEN' "$f" || true)
done
echo "fly token fallbacks: $fallbacks"

# 3. No secret is echoed.
runs=0
for f in "${files[@]}"; do
  out=$(jq -r "$JQ_LIB"'
    . as $w
    | ( if (($w.env // {}) | tostring | test("secrets\\.")) then
          "FAIL the workflow-level env holds a secret: it reaches every step" else empty end ),
      ( $w.jobs | to_entries[] | .key as $j | .value as $J
        | [ ($J.env // {}) | to_entries[] | select(.value | tostring | test("secrets\\.")) | .key ] as $job_keys
        | ($J.steps // []) | to_entries[] | .key as $i | .value as $s | select($s.run != null)
        | ($job_keys + [ ($s.env // {}) | to_entries[] | select(.value | tostring | test("secrets\\.")) | .key ]) as $keys
        | ($s.run | split("\n")) as $lines
        # The aliases: a var a line assigns from a secret-holding var holds the secret too.
        | (reduce range(0; 3) as $_ ($keys; . as $k
            | (. + [ $lines[] | select(refs_secret(secret_names($k))) | assigned[] ]) | unique)) as $held
        | secret_names($held) as $names
        | "RUN",
          ( if ($s.run | test("\\$\\{\\{[^}]*secrets\\.")) then
              "FAIL \($j).\($s | step_name($i)) interpolates a secret into its script: pass it by env" else empty end ),
          ( $lines[] as $line
            | select($line | refs_secret($names))
            | select($line | test("(^|[^A-Za-z0-9_])(echo|printf|tee)([^A-Za-z0-9_]|$)|>>"))
            | select(($line | test("(^|[^A-Za-z0-9_])tee([^A-Za-z0-9_]|$)|GITHUB_(OUTPUT|ENV|STEP_SUMMARY|PATH|STATE)"))
                or ($line | test(">>? *\"?\\$\\{?RUNNER_TEMP[}/]") | not))
            | "FAIL \($j).\($s | step_name($i)) prints a secret: \($line | strip | .[0:100])" ),
          ( select(($keys | length) > 0 and ($s.run | test("set -[a-z]*x|set -o xtrace|bash -[a-z]*x")))
            | "FAIL \($j).\($s | step_name($i)) holds a secret under xtrace" ),
          ( select(($keys | length) > 0) | $lines[] as $line
            | select($line | test("(^|[\\s;&|(!]|\\$\\()printenv([\\s;&|)]|$)|(^|[;&|(!]|\\$\\(|\\b(then|do|else|if|time)\\s)\\s*(env|set)\\s*($|[|>;&)])|\\b(declare|typeset|export|readonly)\\s+-[A-Za-z]*p"))
            | "FAIL \($j).\($s | step_name($i)) dumps the environment while it holds a secret: \($line | strip | .[0:100])" ),
          ( select(($keys | length) > 0) | $lines[] as $line
            | select($line | test("(^|[\\s;&|(])(flyctl|fly)\\s.*\\s(-t|--access-token)(\\s|=|$)"))
            | "FAIL \($j).\($s | step_name($i)) hands flyctl a token in argv: \($line | strip | .[0:100])" ) )' "$(json "$f")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$(basename "$f"): ${line#FAIL }" ;;
      RUN) runs=$((runs + 1)) ;;
    esac
  done <<< "$out"
done
echo "no secret echoed: $runs run blocks read"
[ "$runs" -gt 0 ] || failed "0 run blocks read: the secret rule read nothing"

# 4. No GitHub environment.
env_jobs=0
for f in "${files[@]}"; do
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$(basename "$f"): ${line#FAIL }" ;;
      JOB) env_jobs=$((env_jobs + 1)) ;;
    esac
  done < <(jq -r "$JQ_LIB"'(.jobs // {}) | to_entries[]
    | "JOB", (select(.value | has("environment"))
      | "FAIL job \(.key) declares environment \u0027\(.value.environment | envname)\u0027: no GitHub environment (owner, 2026-10-25); a job that names one makes GitHub create it bare")' "$(json "$f")")
done
echo "no github environment: $env_jobs jobs read in ${#files[@]} workflows"
[ "$env_jobs" -gt 0 ] || failed "0 jobs read: the environment rule read nothing"

# 5. The secrets each workflow may name.
own_tokens=0 secret_refs=0
for f in "${files[@]}"; do
  b=$(basename "$f")
  # Expression functions ignore case (toJSON, tojson): so does this (the PR #93 review's m-1).
  if hits=$(grep -n -i -E 'secrets *\[|tojson\( *secrets *\)' "$f"); then
    failed "$b names a secret by expression, which no rule here can read: $hits"
  fi
  # A job that calls a reusable workflow hands it secrets only from this repository's own files (a
  # ./ path, held to these rules itself): `secrets: inherit` or a secrets map to anyone else's
  # workflow hands it every secret named, relay tokens included (m-1).
  while IFS= read -r line; do
    [ -n "$line" ] && failed "$b: $line"
  done < <(jq -r '(.jobs // {}) | to_entries[] | select((.value.uses // "") != "" and ((.value.uses | tostring) | startswith("./") | not) and (.value | has("secrets")))
    | "job \(.key) hands secrets (\(.value.secrets | if type == "string" then . else "a map" end)) to a workflow outside this repository: \(.value.uses)"' "$(json "$f")")
  case "$b" in
    deploy-stg.yml) own=FLY_API_TOKEN_RELAY_STG ;;
    deploy-prod.yml) own=FLY_API_TOKEN_RELAY_PROD ;;
    *) own='' ;;
  esac
  deploy_wf=false
  if is_deploy "$f"; then deploy_wf=true; fi
  found_own=0
  while IFS=' ' read -r where name; do
    [ -n "$name" ] || continue
    secret_refs=$((secret_refs + 1))
    upper=$(tr '[:lower:]' '[:upper:]' <<< "$name")
    if [ -n "$own" ] && [ "$name" = "$own" ] && [ "$where" = push ]; then
      found_own=$((found_own + 1))
    elif [ -n "$own" ] && [ "$upper" = "$own" ] && [ "$where" = push ]; then
      failed "$b names $name: spell it $own (secret names ignore case, so another spelling hides it from the token rules)"
    elif [ -n "$own" ] && [ "$upper" = "$own" ]; then
      failed "$b names $own outside its push job ($where): the relay token goes only in the push job"
    elif [ "$deploy_wf" = true ]; then
      failed "$b names the secret $name ($where): a deploy workflow names only its own relay token${own:+ ($own)}"
    elif [ "$b" = soak.yml ] && [ "$name" = FLY_API_TOKEN_SOAK ]; then
      : # the named exemption: the soak app's own token (C15)
    elif [[ "$upper" == FLY_* ]]; then
      failed "$b names the Fly secret $name ($where): only deploy-stg.yml, deploy-prod.yml (their relay tokens) and soak.yml (FLY_API_TOKEN_SOAK) may"
    fi
  done < <(jq -r "$JQ_LIB"'
    ((.env // {}) | secret_refs[] | "workflow \(.)"),
    ((.jobs // {}) | to_entries[] | .key as $j | (.value | secret_refs[]) | "\($j) \(.)")' "$(json "$f")")
  if [ -n "$own" ]; then
    if [ "$found_own" -gt 0 ]; then
      own_tokens=$((own_tokens + 1))
      echo "relay token: $b names $own in its push job"
    else
      failed "$b never names $own in its push job: its push could never run"
    fi
  fi
done
echo "secrets named: $secret_refs; deploy workflows naming their own relay token in their push job: $own_tokens of 2"

# 6. No web-app token, and no web app.
web_lines=0
for f in "${files[@]}"; do
  b=$(basename "$f")
  while IFS=' ' read -r where name; do
    [ -n "$name" ] || continue
    web_lines=$((web_lines + 1))
    failed "$b names the secret $name ($where): no web-app token is ever in GitHub (owner, 2026-10-25); only scripts/promote.sh, on the operator's own login, touches the web apps"
  done < <(jq -r "$JQ_LIB"'
    ((.env // {}) | secret_refs[] | "workflow \(.)"),
    ((.jobs // {}) | to_entries[] | .key as $j | (.value | secret_refs[]) | "\($j) \(.)")
    | select(ascii_upcase | test("WEB|CLUB"))' "$(json "$f")")
  while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    web_lines=$((web_lines + 1))
    failed "$b hands flyctl a web app: $hit (only scripts/promote.sh, on the operator's own login, touches seazn-club-*)"
  done < <(grep -n -i -E "(^|[[:space:]])(-a|--app)[[:space:]=]+[\"']?seazn-club-|FLY_APP[\"']?[[:space:]]*[:=][[:space:]]*[\"']?seazn-club-" "$f" || true)
done
echo "web-app tokens and web apps named: $web_lines in ${#files[@]} workflows"

# 7. The CEF bump gate after every checkout of a token-holding deploy job.
gates=0
for f in ${deploy[@]+"${deploy[@]}"}; do
  out=$(jq -r "$JQ_LIB"'
    [ .jobs | to_entries[] | select(.value | holds_fly) ] as $jobs
    | if ($jobs | length) == 0 then "FAIL no job holds a Fly token, so none pushes"
      else $jobs[] | .key as $j | (.value.steps // []) as $steps
        | [ $steps | to_entries[] | select((.value.uses // "") | startswith("actions/checkout@")) | .key ] as $cos
        | ([ $steps | to_entries[] | select(.value | holds_fly) | .key ] | min) as $first
        | ( if ($cos | length) == 0 then "FAIL job \($j) holds a Fly token and checks nothing out, so it never runs the CEF bump gate"
            elif ($cos | min) > $first then "FAIL job \($j) uses its Fly token before it checks out and runs the CEF bump gate"
            else $cos[] as $c | ($steps[$c + 1] // {}) as $g | (($g | keys) - ["name", "id", "run"]) as $extra
              | if (($g.run // "") | strip) != "scripts/check-cef-bump.sh"
                then "FAIL job \($j): the step after its checkout (step \($c + 1)) is not exactly `run: scripts/check-cef-bump.sh`"
                elif ($extra | length) > 0
                then "FAIL job \($j): the CEF bump gate step (step \($c + 1)) carries \($extra | join(", ")): it runs bare, or a failed gate could pass"
                else "GATE \($j)" end
            end ),
          ( if (.value["continue-on-error"] != null) then "FAIL job \($j) holds a Fly token and sets continue-on-error" else empty end ),
          ( $steps | to_entries[] | .key as $i | .value as $s
            | ( if ($s["continue-on-error"] != null) then "FAIL job \($j): \($s | step_name($i)) sets continue-on-error, so its failure would not stop the job" else empty end ),
              ( if (($s.if // "") | has_status_fn) then "FAIL job \($j): \($s | step_name($i)) runs past a failed step (if: \($s.if | tostring))" else empty end ) )
      end' "$(json "$f")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$(basename "$f") (CEF bump gate, Owed 22): ${line#FAIL }" ;;
      GATE*) gates=$((gates + 1)) ;;
    esac
  done <<< "$out"
done
echo "cef bump gates after a checkout: $gates in ${#deploy[@]} deploy workflows"

# 8 and 9: text rules over the deploy workflows.
targets=0 registry_lines=0
for f in ${deploy[@]+"${deploy[@]}"}; do
  b=$(basename "$f")
  if hits=$(grep -n -i -F 'seazn-relay-cache' "$f"); then
    failed "$b names the CI build cache (amendment 2026-10-23: a deploy never reads it): $hits"
  fi
  if hits=$(grep -n -i -E 'test-hooks' "$f"); then
    failed "$b names a test-hooks target (amendment 2026-10-24): $hits"
  fi
  while IFS= read -r t; do
    [ -n "$t" ] || continue
    targets=$((targets + 1))
    [ "$t" = relay ] || failed "$b builds target '$t': a deploy builds relay only"
  done < <(jq -r '[.jobs[]?.steps[]? | .with.target? // empty] | .[] | tostring' "$(json "$f")"; \
           { grep -oE -- '--target[= ]+[^[:space:]]+' "$f" || true; } | sed -E 's/--target[= ]+//')
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    registry_lines=$((registry_lines + 1))
    case "$line" in
      *vars.RELAY_REGISTRY*) ;;
      *) failed "$b hardcodes the registry (ADR 0007: read vars.RELAY_REGISTRY): $line" ;;
    esac
  done < <(grep -n -F 'registry.fly.io' "$f" || true)
done
echo "deploy build targets read: $targets; registry.fly.io lines read: $registry_lines"
[ "$registry_lines" -gt 0 ] || failed "no deploy workflow sets RELAY_REGISTRY's default: the registry rule read nothing"

# 10. Provenance in the push workflows.
provenance=0
for pair in "deploy-stg.yml:refs/heads/main" "deploy-prod.yml:merge-base --is-ancestor"; do
  b=${pair%%:*} want=${pair#*:}
  [ -f "$dir/$b" ] || continue
  out=$(jq -r --arg want "$want" "$JQ_LIB"'
    [ .jobs | to_entries[] | select(any((.value.steps // [])[]; .id == "token")) ] as $jobs
    | if ($jobs | length) != 1 then "FAIL \($jobs | length) jobs hold a step with id token (the guard), not 1"
      else $jobs[0] | .key as $j | (.value.steps // []) as $steps
        | ([ $steps | to_entries[] | select(.value.id == "token") | .key ][0]) as $guard
        | ([ $steps | to_entries[] | select(.value.id == "provenance") | .key ][0]) as $p
        | if $p == null then "FAIL job \($j) has no step with id provenance: nothing refuses an image that is not from main"
          elif $p > $guard then "FAIL job \($j): the provenance step comes after the token guard"
          elif (($steps[$p] | keys) - ["name", "id", "env", "run"] | length) > 0
          then "FAIL job \($j): the provenance step carries \((($steps[$p] | keys) - ["name", "id", "env", "run"]) | join(", ")): it runs bare, or a refusal could pass"
          elif ((($steps[$p].run // "") | contains($want)) and (($steps[$p].run // "") | test("(^|[;\\s])exit 1([;\\s]|$)"))) | not
          then "FAIL job \($j): the provenance step does not check \u0027\($want)\u0027 and exit 1"
          else "PROVENANCE \($j)" end
      end' "$(json "$dir/$b")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$b (provenance): ${line#FAIL }" ;;
      PROVENANCE*) provenance=$((provenance + 1)) ;;
    esac
  done <<< "$out"
done
echo "provenance steps before a push guard: $provenance"

# The push job: the one job that holds the Fly token, named push.
read -r -d '' JQ_PUSH << 'JQ' || true
def push_jobs: [ .jobs | to_entries[] | select(.value | holds_fly) ];
def code_lines: split("\n") | map(select(test("^\\s*#") | not)) | join("\n");
JQ

# 11. Main only, as hygiene.
main_ifs=0
for pair in "deploy-stg.yml:github.ref == 'refs/heads/main'" "deploy-prod.yml:startsWith(github.ref, 'refs/tags/v')"; do
  b=${pair%%:*} want=${pair#*:}
  [ -f "$dir/$b" ] || continue
  out=$(jq -r --arg want "$want" "$JQ_LIB$JQ_PUSH"'
    push_jobs as $jobs
    | if ($jobs | length) != 1 or $jobs[0].key != "push" then "FAIL \($jobs | map(.key) | join(", ")) hold a Fly token, not one job named push"
      elif (($jobs[0].value.if // "") | norm_if) != $want
      then "FAIL job push runs on if \u0027\(($jobs[0].value.if // "") | norm_if)\u0027, not exactly \u0027\($want)\u0027 (hygiene: main\u0027s own copy never pushes another ref)"
      else "MAINIF" end' "$(json "$dir/$b")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$b (main only): ${line#FAIL }" ;;
      MAINIF) main_ifs=$((main_ifs + 1)) ;;
    esac
  done <<< "$out"
done
echo "push jobs behind their ref (hygiene, not a guard): $main_ifs of 2"

# 12. Main's build record.
records=0
# shellcheck disable=SC2016  # a literal $TAG: the text the record check must hold
for spec in 'deploy-stg.yml|deploy-stg.yml@refs/heads/main|--source-ref refs/heads/main' \
            'deploy-prod.yml|deploy-prod.yml@refs/tags/$TAG|--source-ref "refs/tags/$TAG"'; do
  IFS='|' read -r b want sref <<< "$spec"
  [ -f "$dir/$b" ] || continue
  out=$(jq -r --arg want "$want" --arg sref "$sref" "$JQ_LIB$JQ_PUSH"'
    push_jobs as $jobs
    | if ($jobs | length) != 1 then "FAIL \($jobs | length) jobs hold a Fly token, not 1 (the push job)"
      else $jobs[0] | .key as $j | (.value.steps // []) as $steps
        | ([ $steps | to_entries[] | select(.value.id == "push") | .key ][0]) as $push
        | ([ $steps | to_entries[] | select(.value.id == "attest") | .key ][0]) as $a
        | ([ $steps | to_entries[] | select(.value.id == "record") | .key ][0]) as $rec
        | ([ $steps | to_entries[] | select(.value.id == "summary") | .key ][0]) as $sum
        | ([ $steps[] | select(.id == "token") ][0]) as $guard
        | ("steps.token.outputs.enabled == '"'"'true'"'"'") as $gc
        | if $push == null then "FAIL job \($j) has no step with id push"
          elif $a == null then "FAIL job \($j) has no step with id attest: nothing records the push as main\u0027s build"
          elif $a < $push then "FAIL job \($j): the attest step comes before the push"
          elif (($steps[$a].uses // "") | test("^actions/attest@[0-9a-f]{40}$") | not)
          then "FAIL job \($j): the attest step is not actions/attest pinned by commit: \($steps[$a].uses // "no uses")"
          elif ((($steps[$a].with // {})["subject-digest"] // "") | tostring | contains("steps.push.outputs.digest") | not)
          then "FAIL job \($j): the attest step does not attest steps.push.outputs.digest"
          elif $rec == null then "FAIL job \($j) has no step with id record: a found tag is never checked against main\u0027s build"
          elif $rec < $a then "FAIL job \($j): the record check comes before the attest step"
          elif $sum == null or $sum < $rec then "FAIL job \($j): the summary (step id summary) does not come after the record check"
          elif (($steps[$rec] | keys) - ["name", "id", "env", "run", "if"] | length) > 0
          then "FAIL job \($j): the record check carries \((($steps[$rec] | keys) - ["name", "id", "env", "run", "if"]) | join(", ")): it must run on both paths, bare"
          elif ($steps[$rec] | has("if")) and ($guard == null or (($steps[$rec].if | norm_if) != $gc))
          then "FAIL job \($j): the record check runs on if \u0027\($steps[$rec].if | norm_if)\u0027: at most the token guard\u0027s own \u0027\($gc)\u0027, so it runs on both paths, pushed and found"
          else (($steps[$rec].run // "") | code_lines) as $r
            | ([ "gh attestation verify", "--cert-identity", $want, $sref, "--source-digest \"$GITHUB_SHA\"", "--signer-digest \"$GITHUB_SHA\"", "--deny-self-hosted-runners" ]
                | map(select(. as $x | $r | contains($x) | not))) as $missing
            | if ($missing | length) > 0 then "FAIL job \($j): the record check does not read \($missing | map("\u0027" + . + "\u0027") | join(", "))"
              elif ($r | test("(^|[;\\s])exit 1([;\\s]|$)") | not) then "FAIL job \($j): the record check never exits 1"
              else "RECORD \($j)" end
          end
      end' "$(json "$dir/$b")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$b (build record): ${line#FAIL }" ;;
      RECORD*) records=$((records + 1)) ;;
    esac
  done <<< "$out"
done
echo "build records: $records of 2 push jobs attest and verify both paths"
[ "$records" -eq 2 ] || failed "build records: $records of 2: a push job whose image carries no verified record cannot be promoted"

# 13. Write grants.
grants=0
for f in "${files[@]}"; do
  b=$(basename "$f")
  while IFS=' ' read -r kind where scope; do
    [ -n "$kind" ] || continue
    case "$kind $b $where $scope" in
      "GRANT deploy-stg.yml push id-token" | "GRANT deploy-stg.yml push attestations" | \
        "GRANT deploy-prod.yml push id-token" | "GRANT deploy-prod.yml push attestations" | \
        "GRANT image.yml cache packages")
        grants=$((grants + 1)) ;;
      "WORKFLOW "*) failed "$b (write grants): the workflow-level permissions grant $scope: every job in it could write or sign as this workflow" ;;
      *) failed "$b (write grants): job $where grants $scope, and only id-token and attestations on the push jobs of deploy-stg.yml and deploy-prod.yml, or packages on image.yml's cache job, may" ;;
    esac
  done < <(jq -r '
    def writes: if type == "string" then (if . == "write-all" then ["write-all"] else [] end)
      elif type == "object" then [ to_entries[] | select(.value == "write") | .key ]
      else [] end;
    ((.permissions // null) | writes[] | "WORKFLOW workflow \(.)"),
    ((.jobs // {}) | to_entries[] | .key as $j | (.value.permissions // null) | writes[] | "GRANT \($j) \(.)")' "$(json "$f")")
done
for pair in deploy-stg.yml:push deploy-prod.yml:push; do
  b=${pair%%:*} j=${pair#*:}
  [ -f "$dir/$b" ] || continue
  both=$(jq -r --arg j "$j" '(.jobs[$j].permissions // {}) | if type == "object" then (.["id-token"] == "write" and .attestations == "write") else false end' "$(json "$dir/$b")")
  [ "$both" = true ] || failed "$b (write grants): job $j does not grant both id-token: write and attestations: write: its attest step cannot sign"
done
echo "write grants: $grants of 5 allowed (id-token and attestations on the two push jobs, packages on image.yml's cache job); ${#files[@]} workflows read"
[ "$grants" -eq 5 ] || failed "write grants: $grants of 5 allowed grants found: a push job's attest step cannot sign without both, and the grants this rule allows are exactly five"

# 15. The login leaves with its step (the PR #93 review's m-2).
logins=0 pinned=0
for b in deploy-stg.yml deploy-prod.yml; do
  [ -f "$dir/$b" ] || continue
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$b (the login leaves with its step): ${line#FAIL }" ;;
      LOGIN*) logins=$((logins + 1)) ;;
      PINNED*) pinned=$((pinned + 1)) ;;
    esac
  done < <(jq -r "$JQ_LIB$JQ_PUSH"'
    push_jobs[] | .key as $j | (.value.steps // []) | to_entries[] | .key as $i | .value as $s
    | (if ($s.uses // "") != "" then
         (if ($s.uses | test("@[0-9a-f]{40}$")) then "PINNED"
          else "FAIL \($j).\($s | step_name($i)) uses \($s.uses), not pinned by commit: it runs in the job that holds the token" end)
       else empty end),
      ((($s.run // "") | code_lines) as $r
       | if ($r | test("flyctl\\s+auth\\s+docker")) then
           (if ($r | test("trap \u0027docker logout \"\\$RELAY_REGISTRY\"[^\u0027]*\u0027 EXIT")) then "LOGIN"
            else "FAIL \($j).\($s | step_name($i)) logs docker in (flyctl auth docker) with no `trap \u0027docker logout \"$RELAY_REGISTRY\" …\u0027 EXIT`: the token would stay in docker\u0027s config for every later step and action" end)
         else empty end)' "$(json "$dir/$b")")
done
echo "logins that leave with their step: $logins; actions in the push jobs pinned by commit: $pinned"
if [ "$logins" -eq 0 ] || [ "$pinned" -eq 0 ]; then failed "the login rule read nothing: $logins logins, $pinned actions"; fi

# 14. No promote workflow.
promote_lines=0
for f in "${files[@]}"; do
  b=$(basename "$f")
  case "$b" in
    promote*) failed "$b: a promote workflow (owner, 2026-10-25: promotion is scripts/promote.sh, run locally; no web-app token in GitHub)" ;;
  esac
  while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    promote_lines=$((promote_lines + 1))
    failed "$b changes Fly secrets: $hit (a promote by what it does; promotion is scripts/promote.sh, run locally)"
  done < <(grep -n -E '(flyctl|fly)[[:space:]]+secrets[[:space:]]+(set|deploy|unset|import|sync)' "$f" || true)
done
echo "promote workflows: $promote_lines lines changing Fly secrets in ${#files[@]} workflows (promotion is scripts/promote.sh)"

finished=1
if [ "$fails" -gt 0 ]; then
  echo "check-workflows: $fails failure(s)" >&2
  exit 1
fi
echo "check-workflows: OK"
