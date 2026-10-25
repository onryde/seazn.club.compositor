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
#     job's `enabled` output, or its own `if` reads the guard step's, earlier in the same job). A
#     job-level env never holds a Fly token, and no expression names two (`a && X || Y` hands a run
#     Y's token whenever X's is empty: a prod run would reach stg, or the reverse).
#  3. No secret is echoed: no workflow-level env holds a secret, no run block interpolates
#     ${{ secrets.* }} (a secret goes in by env), and no run line that names a secret-holding env
#     var prints it (echo, printf, tee, a >> redirect) or runs under xtrace. One print is not an
#     echo: into a file under $RUNNER_TEMP, with no tee and no GitHub command file on the line
#     (soak.yml's leak gate writes the token to a umask-077 pattern file to grep the outputs for it).
#     Into the log, $GITHUB_OUTPUT, $GITHUB_ENV or the step summary is.
#  4. promote.yml: every job that holds a Fly token declares a relay-* environment, whose required
#     reviewers are the human approval (the owner ruling: the RELAY_IMAGE bump is explicit).
#  5. The CEF bump gate (Owed 22): in every deploy workflow, every job that holds a Fly token checks
#     out before it uses the token, and each checkout's very next step runs scripts/check-cef-bump.sh.
#  6. No deploy workflow names the CI build cache, seazn-relay-cache (amendment 2026-10-23).
#  7. No deploy workflow names a test-hooks target (relay-test-hooks, rust-build-test-hooks,
#     test-hooks), and every build target it names is `relay` (amendment 2026-10-24).
#  8. The registry is the RELAY_REGISTRY variable (ADR 0007): a deploy workflow writes
#     registry.fly.io only on a line that reads vars.RELAY_REGISTRY (its default).
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
DEPLOY_PATTERN='seazn-(relay|club)-(stg|prod)|FLY_API_TOKEN_(STG|PROD)'

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
trap 'rm -rf "$tmp"' EXIT
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
# where <env> is X's own suffix (promote.yml: each env's token reaches only its own runs).
def token_value($x):
  (tostring | strip) as $v
  | ($v | test("^\\$\\{\\{ *secrets\\." + $x + " *\\}\\}$"))
    or (([$v | capture("^\\$\\{\\{ *inputs\\.env *== *'(?<e>[a-z]+)' *&& *secrets\\." + $x + " *\\|\\| *'' *\\}\\}$")][0]) as $c
        | $c != null and $c.e == ($x | sub("^FLY_API_TOKEN_"; "") | ascii_downcase));
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
    out=$(jq -r --arg x "$x" "$JQ_LIB"'
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
                | if any($step_gates[]; . as $g | (($s.if // "") | tostring | contains($g)))
                     or any($job_gates[]; . as $g | (($J.if // "") | tostring | contains($g)))
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
        | "RUN",
          ( if ($s.run | test("\\$\\{\\{[^}]*secrets\\.")) then
              "FAIL \($j).\($s | step_name($i)) interpolates a secret into its script: pass it by env" else empty end ),
          ( ($s.run | split("\n")[]) as $line | $keys[] as $k
            | select($line | test("\\$\\{?" + $k + "([^A-Za-z0-9_]|$)"))
            | select($line | test("(^|[^A-Za-z0-9_])(echo|printf|tee)([^A-Za-z0-9_]|$)|>>"))
            | select(($line | test("(^|[^A-Za-z0-9_])tee([^A-Za-z0-9_]|$)|GITHUB_(OUTPUT|ENV|STEP_SUMMARY|PATH|STATE)"))
                or ($line | test(">>? *\"?\\$\\{?RUNNER_TEMP[}/]") | not))
            | "FAIL \($j).\($s | step_name($i)) prints \($k): \($line | strip | .[0:100])" ),
          ( select(($keys | length) > 0 and ($s.run | test("set -[a-z]*x|set -o xtrace|bash -[a-z]*x")))
            | "FAIL \($j).\($s | step_name($i)) holds a secret under xtrace" ) )' "$(json "$f")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "$(basename "$f"): ${line#FAIL }" ;;
      RUN) runs=$((runs + 1)) ;;
    esac
  done <<< "$out"
done
echo "no secret echoed: $runs run blocks read"
[ "$runs" -gt 0 ] || failed "0 run blocks read: the secret rule read nothing"

# 4. promote.yml asks a human.
if [ -f "$dir/promote.yml" ]; then
  envs=0
  out=$(jq -r "$JQ_LIB"'
    [ .jobs | to_entries[] | select(.value | holds_fly)
      | {job: .key, env: ((.value.environment // "") | if type == "object" then (.name // "") else . end | tostring)} ]
    | if length == 0 then "FAIL no job holds a Fly token"
      else .[] | if (.env | test("^relay-")) then "ENV \(.job) \(.env)"
        else "FAIL job \(.job) holds a Fly token and declares no relay-* environment: no reviewer approves the promote" end
      end' "$(json "$dir/promote.yml")")
  while IFS= read -r line; do
    case "$line" in
      FAIL*) failed "promote.yml: ${line#FAIL }" ;;
      ENV*) envs=$((envs + 1)); echo "promote environment: ${line#ENV }" ;;
    esac
  done <<< "$out"
  echo "promote jobs behind an environment: $envs"
fi

# 5. The CEF bump gate after every checkout of a token-holding deploy job.
gates=0
for f in ${deploy[@]+"${deploy[@]}"}; do
  out=$(jq -r "$JQ_LIB"'
    [ .jobs | to_entries[] | select(.value | holds_fly) ] as $jobs
    | if ($jobs | length) == 0 then "FAIL no job holds a Fly token, so none pushes or promotes"
      else $jobs[] | .key as $j | (.value.steps // []) as $steps
        | [ $steps | to_entries[] | select((.value.uses // "") | startswith("actions/checkout@")) | .key ] as $cos
        | ([ $steps | to_entries[] | select(.value | holds_fly) | .key ] | min) as $first
        | if ($cos | length) == 0 then "FAIL job \($j) holds a Fly token and checks nothing out, so it never runs the CEF bump gate"
          elif ($cos | min) > $first then "FAIL job \($j) uses its Fly token before it checks out and runs the CEF bump gate"
          else $cos[] as $c
            | if (($steps[$c + 1].run // "") | strip | test("^scripts/check-cef-bump\\.sh( |$)"))
              then "GATE \($j)"
              else "FAIL job \($j): the step after its checkout (step \($c + 1)) is not scripts/check-cef-bump.sh" end
          end
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

if [ "$fails" -gt 0 ]; then
  echo "check-workflows: $fails failure(s)" >&2
  exit 1
fi
echo "check-workflows: OK"
