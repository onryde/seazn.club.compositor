#!/usr/bin/env bash
# The workflow check (C16, docs/tasks/C16-deploy.md, with its amendments 2026-10-18, 2026-10-23 and
# 2026-10-24). check.yml runs it, and scripts/deploy_checks_test.sh runs it on scratch copies, each
# with one mutant that must red one rule below.
#
#   scripts/check-workflows.sh [DIR]            check the workflows in DIR (default .github/workflows)
#   scripts/check-workflows.sh --install DIR    put the pinned actionlint and shellcheck in DIR
#
# Rules (each prints what it checked; any FAIL line fails the run, after every rule has reported):
#  1. actionlint (pinned): every workflow with shellcheck off (the older workflows' scripts are not
#     this check's to rewrite), then the deploy workflows again with the pinned shellcheck.
#  2. The Fly token guard: every workflow that names a secrets.FLY_API_TOKEN* has, for each one, a
#     step that warns "::warning::<NAME> unset" and exits 0 when it is empty, and every other step
#     that holds the token runs only when that guard said enabled (its job's `if` reads the guard
#     job's `enabled` output, or its own `if` reads the guard step's, earlier in the same job; in a
#     deploy workflow that `if` is exactly the guard's condition, nothing more). A job-level env
#     never holds a Fly token, and no expression names two (`a && X || Y` hands a run Y's token
#     whenever X's is empty: a prod run would reach stg, or the reverse).
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
#  4. Every Fly token behind its workflow's environment (the PR #85 review's I-1 and I-4): every job
#     that holds one declares exactly relay-stg-push (deploy-stg.yml), relay-prod-push
#     (deploy-prod.yml) or `relay-${{ inputs.env }}` (promote.yml, whose inputs.env is the choice
#     [stg, prod], so relay-stg or relay-prod, whose required reviewers are the human approval: the
#     owner ruling, the RELAY_IMAGE bump is explicit). A job outside an environment could read only a
#     repository secret, which every branch's workflow reads; any other deploy workflow that holds a
#     Fly token has no known environment and is a red.
#  5. The CEF bump gate (Owed 22): in every deploy workflow, every job that holds a Fly token checks
#     out before it uses the token, and each checkout's very next step is exactly
#     `run: scripts/check-cef-bump.sh` with nothing but a name or id (no if, continue-on-error, shell
#     or env, no `|| true`). Such a job and its steps set no continue-on-error, and no step `if`
#     calls always(), failure() or cancelled() (a step that runs past a failed gate).
#  6. No deploy workflow names the CI build cache, seazn-relay-cache (amendment 2026-10-23).
#  7. No deploy workflow names a test-hooks target (relay-test-hooks, rust-build-test-hooks,
#     test-hooks), and every build target it names is `relay` (amendment 2026-10-24).
#  8. The registry is the RELAY_REGISTRY variable (ADR 0007): a deploy workflow writes
#     registry.fly.io only on a line that reads vars.RELAY_REGISTRY (its default).
#  9. The tokens by blast radius (the PR #85 review's I-1): a deploy workflow names only the four
#     Fly tokens below, whose reach the C16 handoff's minting steps fix (each an environment secret,
#     rule 4); a web token
#     (FLY_API_TOKEN_WEB_*, which sets seazn-club-<env>'s secrets) appears in promote.yml alone, in a
#     job behind relay-${{ inputs.env }}; and no workflow names a secret by expression (secrets[...],
#     toJSON(secrets)), which no rule here could read.
# 10. promote.yml's refusals come first: exactly one step holds a web token past the guard, and its
#     script reads the environment's required reviewers and deployment branch policies, checks the
#     ref's commit is on main (merge-base --is-ancestor) and runs the CEF bump gate on that commit's
#     tree, each on a line before its first flyctl.
# 11. Provenance in the push workflows (I-2): deploy-stg.yml's and deploy-prod.yml's guard job runs a
#     step with id `provenance` before the guard (stg: the ref is refs/heads/main; prod: the tag's
#     commit is on main, by merge-base --is-ancestor), which exits 1 on a refusal and carries no if,
#     continue-on-error or shell.
# 12. The push environment holds (I-4): deploy-stg.yml's and deploy-prod.yml's push job runs a step
#     with id `environment` before its first Fly token step, which reads the environment and its
#     deployment policies through the REST API, requires custom policies of exactly branch:main
#     (relay-stg-push) or tag:v* (relay-prod-push), and exits 1 otherwise; nothing but a name, id,
#     env and run.
# 13. Main's build record (I-4): the push job attests the pushed digest (a step with id `attest`
#     using actions/attest pinned by commit, after the Push step, on steps.push.outputs.digest), then
#     verifies on BOTH paths (a step with id `record`, no if, before the summary) with
#     `gh attestation verify --cert-identity` naming deploy-stg.yml@refs/heads/main or
#     deploy-prod.yml@refs/tags/$TAG, --source-digest "$GITHUB_SHA" and --deny-self-hosted-runners,
#     exiting 1 when it does not verify; and promote.yml's step runs `gh attestation verify` with
#     --cert-identity, --source-digest and --deny-self-hosted-runners before its first call to web.
#
# The deploy workflows are deploy-stg.yml, deploy-prod.yml and promote.yml, each required, plus any
# other workflow that names the stg or prod relay or web app, or their tokens: a new one is held to
# the deploy rules by what it does, not by its name. Prints `checked N workflows`; N = 0 fails.
#
# Needs bash, ruby (YAML), jq, and actionlint and shellcheck at the versions pinned below.
set -euo pipefail

ACTIONLINT_VERSION=1.7.12
SHELLCHECK_VERSION=0.11.0
REQUIRED_DEPLOY=(deploy-stg.yml deploy-prod.yml promote.yml)
DEPLOY_PATTERN='seazn-(relay|club)-(stg|prod)|FLY_API_TOKEN_(RELAY_|WEB_)?(STG|PROD)'
# Rule 9: each name's reach is fixed by how the C16 handoff mints it (fly tokens create deploy -a <app>).
DEPLOY_TOKENS=(FLY_API_TOKEN_RELAY_STG_PUSH FLY_API_TOKEN_RELAY_PROD_PUSH FLY_API_TOKEN_WEB_STG FLY_API_TOKEN_WEB_PROD)

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

# The deploy set: the three by name (each required), then any other by what it names.
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

# jq helpers shared by rules 2 to 5.
read -r -d '' JQ_LIB << 'JQ' || true
def holds_secret($x): tostring | test("secrets\\." + $x + "([^A-Za-z0-9_]|$)");
def holds_fly: tostring | test("secrets\\.FLY_API_TOKEN");
def strip: sub("^\\s+"; "") | sub("\\s+$"; "");
def needs_list: (.needs // []) | if type == "string" then [.] else . end;
def step_name($i): (.name // .id // .uses // "step \($i)");
# A guard reads the token as `${{ secrets.X }}`, or as `${{ inputs.env == '<env>' && secrets.X || '' }}`
# where X's name carries <env> as a word (_STG, _PROD): promote.yml's tokens reach only their own runs.
def token_value($x):
  (tostring | strip) as $v
  | ($v | test("^\\$\\{\\{ *secrets\\." + $x + " *\\}\\}$"))
    or (([$v | capture("^\\$\\{\\{ *inputs\\.env *== *'(?<e>[a-z]+)' *&& *secrets\\." + $x + " *\\|\\| *'' *\\}\\}$")][0]) as $c
        | $c != null and ($x | test("_" + ($c.e | ascii_upcase) + "(_|$)")));
# An `if` without its ${{ }}, for an exact comparison.
def norm_if: tostring | strip | sub("^\\$\\{\\{\\s*"; "") | sub("\\s*\\}\\}$"; "") | strip;
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
JQ

# 2. The Fly token guard.
guarded=0 gated=0
for f in "${files[@]}"; do
  tokens=$({ grep -oE 'secrets\.FLY_API_TOKEN[A-Z0-9_]*' "$f" || true; } | sed 's/^secrets\.//' | LC_ALL=C sort -u)
  for x in $tokens; do
    strict=false
    for d in ${deploy[@]+"${deploy[@]}"}; do [ "$d" = "$f" ] && strict=true; done
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
                     any($step_gates[]; . as $g | (($s.if // "") | norm_if) == $g)
                     or (any($job_gates[]; . as $g | (($J.if // "") | norm_if) == $g)
                       and ((($s.if // "") | has_status_fn) | not))
                   else
                     any($step_gates[]; . as $g | (($s.if // "") | tostring | contains($g)))
                     or any($job_gates[]; . as $g | (($J.if // "") | tostring | contains($g)))
                   end) as $ok
                | if $ok
                  then "GATED \($j).\($s | step_name($i))"
                  else "FAIL \($j).\($s | step_name($i)) holds \($x) but runs whether or not the guard found it" end ) )
        end' "$(json "$f")")
    while IFS= read -r line; do
      case "$line" in
        FAIL*) failed "$(basename "$f") $x: ${line#FAIL }" ;;
        GUARD*) guarded=$((guarded + 1)); echo "fly token guard: $(basename "$f") $x: ${line#GUARD }" ;;
        GATED*) gated=$((gated + 1)) ;;
      esac
    done <<< "$out"
  done
done
echo "fly token guards: $guarded, token steps gated by them: $gated"
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

# 4. Every Fly token behind its workflow's environment.
envs=0
for f in ${deploy[@]+"${deploy[@]}"}; do
  b=$(basename "$f")
  # shellcheck disable=SC2016  # a literal ${{ }}: the expression text the rule matches
  case "$b" in
    deploy-stg.yml) want='^relay-stg-push$' shown=relay-stg-push ;;
    deploy-prod.yml) want='^relay-prod-push$' shown=relay-prod-push ;;
    promote.yml) want='^relay-\$\{\{ *inputs\.env *\}\}$' shown='relay-${{ inputs.env }}' ;;
    *) want='' shown='' ;;
  esac
  out=$(jq -r --arg b "$b" --arg want "$want" --arg shown "$shown" "$JQ_LIB"'
    ( if $b != "promote.yml" then empty
      else (.["true"].workflow_dispatch.inputs.env // {}) as $in
        | if ($in.type == "choice" and $in.options == ["stg", "prod"]) then empty
          else "FAIL inputs.env is not the choice [stg, prod]: relay-${{ inputs.env }} could name an environment nobody protects" end
      end ),
    ( [ .jobs | to_entries[] | select(.value | holds_fly)
        | {job: .key, env: ((.value.environment // "") | if type == "object" then (.name // "") else . end | tostring)} ]
      | if length == 0 then "FAIL no job holds a Fly token"
        else .[] | if $want != "" and (.env | test($want)) then "ENV \(.job) \(.env)"
          elif $want == "" then "FAIL job \(.job) holds a Fly token, and no environment is known for this workflow: the token could only be a repository secret, which every branch\u0027s workflow reads"
          elif $b == "promote.yml" then "FAIL job \(.job) holds a Fly token and declares environment \u0027\(.env)\u0027, not exactly relay-${{ inputs.env }}: no reviewer of relay-stg or relay-prod approves the promote"
          else "FAIL job \(.job) holds a Fly token and declares environment \u0027\(.env)\u0027, not exactly \($shown): outside it the token can only be a repository secret, which every branch\u0027s workflow reads (the PR #85 review\u0027s I-4)" end
        end )' "$(json "$f")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$b: ${line#FAIL }" ;;
      ENV*) envs=$((envs + 1)); echo "token environment: $b ${line#ENV }" ;;
    esac
  done <<< "$out"
done
echo "fly token jobs behind their workflow's environment: $envs"
[ "$envs" -gt 0 ] || failed "no Fly token job behind an environment: the environment rule read nothing"

# 5. The CEF bump gate after every checkout of a token-holding deploy job.
gates=0
for f in ${deploy[@]+"${deploy[@]}"}; do
  out=$(jq -r "$JQ_LIB"'
    [ .jobs | to_entries[] | select(.value | holds_fly) ] as $jobs
    | if ($jobs | length) == 0 then "FAIL no job holds a Fly token, so none pushes or promotes"
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

# 6, 7 and 8: text rules over the deploy workflows.
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

# 9. The tokens by blast radius.
names_read=0 web_jobs=0
for f in "${files[@]}"; do
  b=$(basename "$f")
  if hits=$(grep -n -E 'secrets *\[|toJSON\( *secrets *\)' "$f"); then
    failed "$b names a secret by expression, which no rule here can read: $hits"
  fi
  if [ "$b" != promote.yml ] && hits=$(grep -n -E 'secrets\.FLY_API_TOKEN_WEB_' "$f"); then
    failed "$b names a web token (it sets seazn-club-<env>'s secrets; only promote.yml, behind relay-<env>, may): $hits"
  fi
done
for f in ${deploy[@]+"${deploy[@]}"}; do
  for x in $({ grep -oE 'secrets\.FLY_API_TOKEN[A-Z0-9_]*' "$f" || true; } | sed 's/^secrets\.//' | LC_ALL=C sort -u); do
    names_read=$((names_read + 1))
    case " ${DEPLOY_TOKENS[*]} " in
      *" $x "*) ;;
      *) failed "$(basename "$f") names $x: a deploy workflow names only ${DEPLOY_TOKENS[*]} (what any other name reaches is unknown)" ;;
    esac
  done
done
if [ -f "$dir/promote.yml" ]; then
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "promote.yml: ${line#FAIL }" ;;
      WEB*) web_jobs=$((web_jobs + 1)) ;;
    esac
  done < <(jq -r '.jobs | to_entries[] | select(.value | tostring | test("secrets\\.FLY_API_TOKEN_WEB_"))
    | ((.value.environment // "") | if type == "object" then (.name // "") else . end | tostring) as $e
    | if ($e | test("^relay-\\$\\{\\{ *inputs\\.env *\\}\\}$")) then "WEB \(.key)"
      else "FAIL job \(.key) holds a web token outside relay-${{ inputs.env }} (environment \u0027\($e)\u0027)" end' "$(json "$dir/promote.yml")")
fi
echo "deploy token names read: $names_read; promote jobs holding a web token, behind relay-<env>: $web_jobs"
[ "$names_read" -gt 0 ] || failed "no Fly token name read in the deploy workflows: the token rule read nothing"

# 10. promote.yml's refusals come before its first flyctl.
if [ -f "$dir/promote.yml" ]; then
  checks=0
  out=$(jq -r "$JQ_LIB"'
    [ .jobs[] | (.steps // [])[] | . as $s
      | select(($s | tostring | test("secrets\\.FLY_API_TOKEN_WEB_"))
          and ([ "FLY_API_TOKEN_WEB_STG", "FLY_API_TOKEN_WEB_PROD" ] | any(. as $x | $s | is_guard($x)) | not)) ] as $users
    | if ($users | length) != 1 then "FAIL \($users | length) steps hold a web token past the guard, not exactly 1 (the one whose refusals this rule reads)"
      else ($users[0].run // "" | split("\n") | to_entries | map(select(.value | test("^\\s*#") | not))) as $lines
        | ([ $lines[] | select(.value | test("(^|[^A-Za-z0-9_])flyctl([^A-Za-z0-9_]|$)")) | .key ] | min) as $fly
        | if $fly == null then "FAIL the promote step runs no flyctl"
          else ( ["environments/", "required_reviewers", "deployment-branch-policies", "merge-base --is-ancestor", "scripts/check-cef-bump.sh"][] as $want
            | ([ $lines[] | select(.value | contains($want)) | .key ] | min) as $at
            | if $at != null and $at < $fly then "CHECK \($want)"
              else "FAIL the promote step does not read \u0027\($want)\u0027 before its first flyctl (line \($fly + 1)): a refusal after a Fly call refuses nothing" end )
          end
      end' "$(json "$dir/promote.yml")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "promote.yml: ${line#FAIL }" ;;
      CHECK*) checks=$((checks + 1)) ;;
    esac
  done <<< "$out"
  echo "promote refusals before the first flyctl: $checks of 5"
fi

# 11. Provenance in the push workflows.
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

# The push job: the one job holding a Fly token in a step other than the guard (id token).
read -r -d '' JQ_PUSH << 'JQ' || true
def push_jobs: [ .jobs | to_entries[] | select((.value | holds_fly) and (any((.value.steps // [])[]; .id == "token") | not)) ];
def code_lines: split("\n") | map(select(test("^\\s*#") | not)) | join("\n");
JQ

# 12. The push environment holds.
env_checks=0
for pair in "deploy-stg.yml:branch:main" "deploy-prod.yml:tag:v*"; do
  b=${pair%%:*} want=${pair#*:}
  [ -f "$dir/$b" ] || continue
  out=$(jq -r --arg want "$want" "$JQ_LIB$JQ_PUSH"'
    push_jobs as $jobs
    | if ($jobs | length) != 1 then "FAIL \($jobs | length) jobs hold a Fly token beyond the guard, not 1 (the push job)"
      else $jobs[0] | .key as $j | (.value.steps // []) as $steps
        | ([ $steps | to_entries[] | select(.value | holds_fly) | .key ] | min) as $first
        | ([ $steps | to_entries[] | select(.value.id == "environment") | .key ][0]) as $e
        | if $e == null then "FAIL job \($j) has no step with id environment: nothing proves the push token is still \($want)\u0027s alone"
          elif $e > $first then "FAIL job \($j): the environment check comes after its first Fly token step"
          elif (($steps[$e] | keys) - ["name", "id", "env", "run"] | length) > 0
          then "FAIL job \($j): the environment check carries \((($steps[$e] | keys) - ["name", "id", "env", "run"]) | join(", ")): it runs bare, or a refusal could pass"
          else (($steps[$e].run // "") | code_lines) as $r
            | ([ "environments/", "deployment-branch-policies", "custom_branch_policies", $want ] | map(select(. as $x | $r | contains($x) | not))) as $missing
            | if ($missing | length) > 0 then "FAIL job \($j): the environment check does not read \($missing | map("\u0027" + . + "\u0027") | join(", "))"
              elif ($r | test("(^|[;\\s])exit 1([;\\s]|$)") | not) then "FAIL job \($j): the environment check never exits 1"
              else "ENVCHECK \($j)" end
          end
      end' "$(json "$dir/$b")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$b (push environment): ${line#FAIL }" ;;
      ENVCHECK*) env_checks=$((env_checks + 1)) ;;
    esac
  done <<< "$out"
done
echo "push environment checks before the first push-token step: $env_checks of 2"

# 13. Main's build record.
records=0 promote_verifies=0
# shellcheck disable=SC2016  # a literal $TAG: the text the record check must hold
for pair in 'deploy-stg.yml:deploy-stg.yml@refs/heads/main' 'deploy-prod.yml:deploy-prod.yml@refs/tags/$TAG'; do
  b=${pair%%:*} want=${pair#*:}
  [ -f "$dir/$b" ] || continue
  out=$(jq -r --arg want "$want" "$JQ_LIB$JQ_PUSH"'
    push_jobs as $jobs
    | if ($jobs | length) != 1 then "FAIL \($jobs | length) jobs hold a Fly token beyond the guard, not 1 (the push job)"
      else $jobs[0] | .key as $j | (.value.steps // []) as $steps
        | ([ $steps | to_entries[] | select(.value.id == "push") | .key ][0]) as $push
        | ([ $steps | to_entries[] | select(.value.id == "attest") | .key ][0]) as $a
        | ([ $steps | to_entries[] | select(.value.id == "record") | .key ][0]) as $rec
        | ([ $steps | to_entries[] | select(.value.id == "summary") | .key ][0]) as $sum
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
          elif (($steps[$rec] | keys) - ["name", "id", "env", "run"] | length) > 0
          then "FAIL job \($j): the record check carries \((($steps[$rec] | keys) - ["name", "id", "env", "run"]) | join(", ")): it must run on both paths, bare"
          else (($steps[$rec].run // "") | code_lines) as $r
            | ([ "gh attestation verify", "--cert-identity", $want, "--source-digest \"$GITHUB_SHA\"", "--deny-self-hosted-runners" ]
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
if [ -f "$dir/promote.yml" ]; then
  out=$(jq -r "$JQ_LIB"'
    [ .jobs[] | (.steps // [])[] | select((.run // "") | test("(^|[^A-Za-z0-9_])web_fly\\s")) ] as $s
    | if ($s | length) != 1 then "FAIL \($s | length) steps call web_fly, not exactly 1 (the promote step)"
      else ($s[0].run | split("\n") | to_entries | map(select(.value | test("^\\s*#") | not))) as $lines
        | ([ $lines[] | select(.value | test("(^|[^A-Za-z0-9_])web_fly\\s")) | .key ] | min) as $web
        | ( "gh attestation verify", "--cert-identity", "--source-digest", "--deny-self-hosted-runners" ) as $want
        | ([ $lines[] | select(.value | contains($want)) | .key ] | min) as $at
        | if $at != null and $at < $web then "VERIFY \($want)"
          else "FAIL the promote step does not read \u0027\($want)\u0027 before its first call to web (line \($web + 1)): an unattested digest would reach web" end
      end' "$(json "$dir/promote.yml")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "promote.yml (build record): ${line#FAIL }" ;;
      VERIFY*) promote_verifies=$((promote_verifies + 1)) ;;
    esac
  done <<< "$out"
fi
echo "build records: $records of 2 push jobs attest and verify both paths; promote reads $promote_verifies of 4 before its first call to web"

finished=1
if [ "$fails" -gt 0 ]; then
  echo "check-workflows: $fails failure(s)" >&2
  exit 1
fi
echo "check-workflows: OK"
