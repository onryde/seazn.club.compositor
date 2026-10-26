#!/usr/bin/env bash
# Promote a relay image (C16, docs/tasks/C16-deploy.md, amendments 2026-10-25 and 2026-10-26; it replaces
# promote.yml): point a web app's RELAY_IMAGE at an image deploy-stg.yml or deploy-prod.yml pushed and
# attested, or at the moving tag those workflows keep on main's latest verified build. This is the live
# change: every NEW relay session on that web app takes the image (running Machines keep theirs). The
# owner runs this, or the coordinator on the owner's explicit go-ahead, on their own machine.
#
#   scripts/promote.sh stg  <40-hex main sha>@sha256:<64 hex>                (a pin)
#   scripts/promote.sh prod <vX.Y.Z>@sha256:<64 hex> --confirm-prod         (a pin; only after C17 passes)
#   scripts/promote.sh stg  --track beta                                     (follow :beta)
#   scripts/promote.sh prod --track stable --confirm-prod                    (follow :stable; after C17)
#
# Tracking (owner, 2026-10-26: ":beta" on every merge, ":stable" on every release tag). deploy-stg.yml
# moves seazn-relay-stg:beta, and deploy-prod.yml seazn-relay-prod:stable, to each build once its build
# record verifies. RELAY_IMAGE is set ONCE per env, by the owner: by hand, or with --track, which is the
# verified way and the way to check a hand-set value. --track sets RELAY_IMAGE=<registry>/seazn-relay-
# <env>:<tag>, and only after that tag's current manifest, read by the tag, verifies as main's build
# (stg: deploy-stg.yml at refs/heads/main; prod: deploy-prod.yml at a release tag) of a commit on main
# whose tree passes the CEF bump gate, by the same checks as a pin. From then on Fly runs what the tag
# names whenever it creates a session's Machine: the bump is automatic, on merge (stg) or release tag
# (prod). deploy-<env>.yml verifies each build before it moves the tag, but nothing checks what the tag
# names when Fly follows it: a push by any holder of the repository push token is followed too (the
# owner declined a check at Machine create, 2026-10-26; the next deploy run reds on such a push).
#
# A pin is the way to freeze an env or roll it back: RELAY_IMAGE then names one digest, and while
# pinned the tag is not followed. Rollback while tracking: pin a previous build with this script, or
# let the next merge (or release tag) move the tag on. Every promote prints its own rollback when it
# can prove the previous value, and says whether the env follows its tag.
#
# deploy-<env>.yml's summary prints the exact pin command.
#
# No Docker (the owner's standing rule; the PR #93 review's I-1): it needs git, jq, curl, flyctl, gh and
# sha256sum (or shasum). No web-app token is ever in GitHub (decision log 2026-10-25): flyctl runs on the
# operator's STORED login (`flyctl auth login`), and FLY_API_TOKEN and FLY_ACCESS_TOKEN are dropped from
# its environment, so a token left in the shell is never the one used. gh runs on the operator's gh login.
# The registry read takes one token, minted below for the relay app alone and for 10 minutes, written
# straight into curl's config file in this run's private temporary directory (0600, removed on exit):
# never in argv, the environment, the output or an error.
#
# Every refusal, in order. Before the first Fly or registry call:
#   1. the arguments: the env, that env's ref shape, the digest pin (required: the PR #85 review's I-4),
#      and --confirm-prod exactly when the env is prod, so a stg invocation can never reach prod; and
#      no test seam set in the shell (the PR #93 review's m-5: see the end of this header);
#   2. the tools;
#   3. main (and a prod tag), fetched from GitHub into refs/promote/*, never into the operator's own
#      refs; and this file is main's copy (a stale or edited copy could lack a refusal);
#   4. where the image came from (I-2): the ref's commit is on main, and its own tree passes main's copy
#      of the CEF bump gate (Owed 22).
# Before the first call to the web app:
#   5. the registry: the pinned manifest, read BY ITS DIGEST from <registry>/v2/seazn-relay-<env>/
#      manifests/<pin> with curl (Fly's registry answers an anonymous read 401, Basic). Only a 404 means
#      not pushed; any other failure is an error (m-5). The body's sha256 must be the pin. The tag is
#      never read: RELAY_IMAGE is the digest alone, and step 6 binds the digest to the commit and the
#      workflow, so a moved tag can neither help a squat nor block main's real build or its rollback.
#      With --track, the moving tag is read instead (only a 404 means absent) and its body's sha256 is
#      the digest checked from here on. Which build that is comes from its attestations: `gh attestation
#      verify --cert-identity-regex <deploy-<env>.yml at main, or at a release tag> --format json` must
#      name exactly one commit and source ref (stg: refs/heads/main; prod: refs/tags/vX.Y.Z), and step 4
#      then runs on that commit;
#   6. main's build record (I-4): `gh attestation verify` on that manifest file, so gh hashes it and
#      fetches its attestations from GitHub's API by that digest (gh 2.95.0, `gh attestation verify
#      --help`: a file path is an artifact; attestations come "via the GitHub API using the values
#      provided to --owner or --repo"; only an oci:// URI needs registry credentials). It must carry the
#      attestation deploy-<env>.yml signs for each push (stg: at refs/heads/main; prod: at
#      refs/tags/<tag>), for the ref's commit, from a GitHub-hosted runner. A branch's workflow can push
#      under any tag (the push token is a repository secret) but cannot sign as either.
# Then the web app, seazn-club-<env>:
#   7. RELAY_IMAGE must not be an [env] entry: promotion is then a seazn.club PR, and the diff is printed;
#   8. `secrets deploy` ships EVERY staged secret, so any other secret not Deployed refuses (I-3; any
#      status but Deployed, or none, refuses; RELAY_IMAGE's own never does, so a rollback can follow a
#      promote whose deploy failed after --stage);
#   9. `secrets set RELAY_IMAGE=<registry>/seazn-relay-<env>@<pin> --stage` (the digest alone, m-10),
#      `secrets deploy`, and the proof (m-7): RELAY_IMAGE reads Deployed, with the digest Fly gave the
#      staged value. A secret list that cannot be read after the set or the deploy is a refusal that
#      says what may be live (the PR #93 review's m-3), never a silent stop.
#
# The previous RELAY_IMAGE: Fly's secret values are write-only, so each promote appends its ref, its
# value and RELAY_IMAGE's Fly digest after the deploy to a log on this machine
# (~/.local/state/seazn-relay-promote/<env>.log). The previous value is the newest entry whose Fly
# digest is the one RELAY_IMAGE has now (equal values give equal Fly digests, measured 2026-10-25), so it
# is proven, never guessed; a value set from another machine, or by hand, reads "set elsewhere". A
# rollback is offered only while that entry's manifest is still in the registry.
#
# Test seams, for scripts/deploy_checks_test.sh, which runs this under fake flyctl, curl and gh:
# RELAY_REGISTRY (default registry.fly.io, deploy-<env>.yml's default), PROMOTE_REMOTE (default the
# GitHub repository) and PROMOTE_STATE_DIR. Each is read only under PROMOTE_TEST_SEAMS=1, and refused
# otherwise: a stale RELAY_REGISTRY in the operator's shell would set RELAY_IMAGE at another host.
set -euo pipefail

REPO=onryde/seazn.club.compositor
SERVER=https://github.com
self=${BASH_SOURCE[0]}

fail() {
  echo "promote: REFUSED: $*" >&2
  exit 1
}
usage() {
  echo "usage: scripts/promote.sh stg <40-hex main sha>@sha256:<64 hex>"
  echo "       scripts/promote.sh prod <vX.Y.Z>@sha256:<64 hex> --confirm-prod"
  echo "       scripts/promote.sh stg --track beta"
  echo "       scripts/promote.sh prod --track stable --confirm-prod"
}

# 1. The arguments.
env='' ref='' confirm_prod=no track=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --confirm-prod) confirm_prod=yes ;;
    --track)
      [ -z "$track" ] || fail "one --track, not two"
      { [ "$#" -ge 2 ] && [ -n "$2" ] && [[ "$2" != -* ]]; } \
        || fail "--track names the moving tag to follow: --track beta (stg) or --track stable (prod)"
      track=$2
      shift ;;
    -h | --help) usage; exit 0 ;;
    -*) fail "unknown flag '$1' ($(usage | head -n 1))" ;;
    *)
      if [ -z "$env" ]; then env=$1
      elif [ -z "$ref" ]; then ref=$1
      else fail "one env and one ref, not '$1' too"; fi ;;
  esac
  shift
done
case "$env" in
  stg)
    shape='^[0-9a-f]{40}$' what="a 40-hex main sha (deploy-stg's tag)"
    moving=beta event="merge to main" moved_when="once main's build record of a merge verifies"
    [ "$confirm_prod" = no ] || fail "--confirm-prod is for a prod promote, and this one is stg: nothing promoted" ;;
  prod)
    shape='^v[0-9]+\.[0-9]+\.[0-9]+$' what="a vX.Y.Z release tag (deploy-prod's tag)"
    moving=stable event="release tag" moved_when="once a release tag's build record verifies"
    [ "$confirm_prod" = yes ] || fail "a prod promote needs --confirm-prod (and C17 passed first): nothing promoted" ;;
  *) fail "env must be stg or prod: '$env'" ;;
esac
suffix=''
[ "$env" = stg ] || suffix=' --confirm-prod'
if [ -n "$track" ]; then
  # A track names no build: which one it is comes from the tag's own attestations (step 5).
  [ -z "$ref" ] || fail "--track $track follows a moving tag, and '$ref' pins a digest: give one or the other (a pin freezes $env; --track follows :$moving)"
  [ "$track" = "$moving" ] || fail "$env tracks :$moving (deploy-$env.yml moves it $moved_when), not '$track'"
  tag='' pin=''
else
  [ -n "$ref" ] || fail "no ref: $(usage | head -n 1)"
  tag=${ref%%@*} pin=${ref#*@}
  [ "$tag" != "$ref" ] || fail "a $env ref names its digest, <ref>@sha256:<digest> as deploy-$env.yml's summary prints it: the pin is required: '$ref'"
  [[ "$tag" =~ $shape ]] || fail "a $env ref is $what@sha256:<digest>: '$ref'"
  [[ "$pin" =~ ^sha256:[0-9a-f]{64}$ ]] || fail "the part after @ must be sha256:<64 hex>: '$ref'"
fi

registry=registry.fly.io remote=$SERVER/$REPO.git state_dir=$HOME/.local/state/seazn-relay-promote
if [ "${PROMOTE_TEST_SEAMS:-}" = 1 ]; then
  registry=${RELAY_REGISTRY:-$registry} remote=${PROMOTE_REMOTE:-$remote} state_dir=${PROMOTE_STATE_DIR:-$state_dir}
  echo "promote: test seams on (PROMOTE_TEST_SEAMS=1): registry $registry, remote $remote, log $state_dir"
else
  for v in RELAY_REGISTRY PROMOTE_REMOTE PROMOTE_STATE_DIR; do
    [ -z "${!v+x}" ] || fail "$v is set in this shell, and promote reads its registry, its remote and its log only from their defaults: unset $v (it is a test seam, read only under PROMOTE_TEST_SEAMS=1)"
  done
fi

# 2. The tools.
for t in git jq curl flyctl gh; do
  command -v "$t" > /dev/null || fail "$t is not on PATH"
done
if command -v sha256sum > /dev/null; then sha256_of() { sha256sum | cut -d' ' -f1; }
elif command -v shasum > /dev/null; then sha256_of() { shasum -a 256 | cut -d' ' -f1; }
else fail "neither sha256sum nor shasum is on PATH"; fi
# The stored login, never a token from the shell. Only whether each is set is read, never its value.
for v in FLY_API_TOKEN FLY_ACCESS_TOKEN; do
  if [ -n "${!v+x}" ]; then
    echo "promote: $v is set in this shell; it is dropped, and flyctl uses your stored login"
    unset "$v"
  fi
done

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
chmod 700 "$tmp"
root=$(git -C "$(dirname "$self")" rev-parse --show-toplevel 2> /dev/null) || fail "$self is not in a git checkout"
G() { git -C "$root" "$@"; }
# flyctl from an empty directory, so no fly.toml lying around picks the app.
on_fly() { (cd "$tmp/cwd" && flyctl "$@"); }
mkdir -p "$tmp/cwd"

# 3. Main, from GitHub, and this file is main's copy.
G fetch --quiet --no-tags "$remote" "+refs/heads/main:refs/promote/main" 2> "$tmp/git.err" \
  || fail "main could not be fetched from $remote: $(head -c 300 "$tmp/git.err")"
# A fetch that succeeded wrote the ref; this is the assertion that it did.
main=$(G rev-parse -q --verify "refs/promote/main^{commit}") || fail "no main fetched from $remote"
G show "refs/promote/main:scripts/promote.sh" > "$tmp/main-promote.sh" 2> /dev/null || fail "main ($main) has no scripts/promote.sh"
cmp -s "$tmp/main-promote.sh" "$self" \
  || fail "$self is not main's copy (main is $main): update this checkout to main and run it again"

# 4. Where the image came from: the ref's commit is on main, and its own tree passes the CEF bump gate.
# A pin names its ref, so this runs now; a track learns its ref from the tag's attestations (step 5).
provenance() {
  if [ "$env" = prod ]; then
    G fetch --quiet --no-tags "$remote" "+refs/tags/$tag:refs/promote/tags/$tag" 2> "$tmp/git.err" \
      || fail "no tag $tag in $remote: $(head -c 200 "$tmp/git.err")"
    commit=$(G rev-parse -q --verify "refs/promote/tags/$tag^{commit}") || fail "the tag $tag names no commit"
  else
    commit=$(G rev-parse -q --verify "$tag^{commit}") || fail "no commit $tag in main's history"
  fi
  G merge-base --is-ancestor "$commit" refs/promote/main || fail "$tag ($commit) is not on main: images are promoted from main only"
  local tree="$tmp/promoted-tree" f
  mkdir -p "$tree/docker"
  for f in docker/GSTCEF_COMMIT docker/CEF_STEP0_PASS _FINDINGS.md; do
    G show "$commit:$f" > "$tree/$f" 2> /dev/null || fail "$tag ($commit) has no $f, so the CEF bump gate cannot read its tree"
  done
  G show "refs/promote/main:scripts/check-cef-bump.sh" > "$tmp/check-cef-bump.sh" 2> /dev/null || fail "main ($main) has no scripts/check-cef-bump.sh"
  bash "$tmp/check-cef-bump.sh" "$tree" || fail "$tag ($commit) fails the CEF bump gate (Owed 22) in its own tree"
}
[ -n "$track" ] || provenance

# 5. The registry: the pinned manifest, by its digest.
web="seazn-club-$env"
app="seazn-relay-$env"
repo="$registry/$app"
# The read token: the relay app's alone, 10 minutes, minted on the operator's login into a file only
# this user can read; curl takes it from its config file. Which string Fly's registry takes as the
# Basic password (with its `FlyV1 ` prefix, as `tokens create` prints it, or without) is unproven:
# docs/_INDEX.md open item (e), proved by the first promote.
creds="$tmp/registry.curl"
(
  umask 077
  on_fly tokens create deploy -a "$app" -n "promote-read-$env-$(date -u +%Y%m%dT%H%M%SZ)" -x 10m > "$tmp/token" 2> "$tmp/token.err"
) || fail "flyctl tokens create deploy -a $app failed (log in first: flyctl auth login): $(head -c 200 "$tmp/token.err")"
if ! { [ "$(wc -l < "$tmp/token" | tr -d ' ')" -le 1 ] && grep -Eqx '(FlyV1 )?[A-Za-z0-9_+/=,.:-]+' "$tmp/token"; }; then
  fail "the token flyctl minted for $app is not one line of the expected shape (it is not printed)"
fi
(
  umask 077
  awk 'NR == 1 { printf "user = \"x:%s\"\n", $0 }' "$tmp/token" > "$creds"
)
rm -f "$tmp/token"
accept='application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json'
# fetch DIGEST OUT: the HTTP status of GET manifests/DIGEST, 000 when curl itself failed. curl's error
# text is never printed: with -K it could carry the credential. -q comes first, so no ~/.curlrc is read
# (the PR #93 confirm pass's m-13): this config and these flags are the whole request.
fetch() {
  local code
  code=$(curl -q -sS --proto '=https' --max-time 60 -K "$creds" -H "Accept: $accept" -o "$2" -w '%{http_code}' \
    "https://$registry/v2/$app/manifests/$1" 2> /dev/null) || code=000
  echo "$code"
}
manifest="$tmp/manifest.json"
if [ -n "$track" ]; then
  # The tag, read once: its body is what it names now, and its sha256 is the digest every check below
  # binds (a body a registry serves for the tag is verified as it is, never trusted by name).
  code=$(fetch "$moving" "$manifest")
  case $code in
    200) ;;
    404) fail "not in the registry: $repo:$moving (HTTP 404; deploy-$env.yml creates it $moved_when)" ;;
    000) fail "the registry could not be reached for $repo:$moving (curl failed; its error text is not printed, since it could carry the credential)" ;;
    *) fail "the registry could not be read for $repo:$moving (HTTP $code; only 404 means absent)" ;;
  esac
  pin="sha256:$(sha256_of < "$manifest")"
  # Which build it is: the attestations deploy-<env>.yml signed for that digest, at main (stg) or at a
  # release tag (prod), from a GitHub-hosted runner. They must name one commit and one source ref.
  if [ "$env" = prod ]; then
    at='@refs/tags/v[0-9]+\.[0-9]+\.[0-9]+$' who="$SERVER/$REPO/.github/workflows/deploy-prod.yml at a release tag"
  else
    at='@refs/heads/main$' who="$SERVER/$REPO/.github/workflows/deploy-stg.yml@refs/heads/main"
  fi
  base="$SERVER/$REPO/.github/workflows/deploy-$env.yml"
  re="^${base//./\\.}$at"
  gh attestation verify "$manifest" --repo "$REPO" --cert-identity-regex "$re" --deny-self-hosted-runners \
      --limit 1000 --format json > "$tmp/found.json" 2> "$tmp/found.err" \
    || fail "$repo:$moving ($pin) is not main's own build: no attestation from $who (a squat, or a tag deploy-$env.yml never moved), nothing promoted: $(tail -c 300 "$tmp/found.err")"
  builds=$(jq -ec 'if type == "array" then [.[] | .verificationResult.signature.certificate
      | [.sourceRepositoryDigest // "", .sourceRepositoryRef // ""]] | unique else error("not a list") end' "$tmp/found.json" 2> /dev/null) \
    || fail "gh's verification of $repo:$moving ($pin) did not read as a list of results"
  n=$(jq -r length <<< "$builds")
  [ "$n" = 1 ] || fail "$repo:$moving ($pin) carries main's attestation for $n builds, not one: pin the build to run instead (scripts/promote.sh $env <ref>@$pin$suffix)"
  commit=$(jq -r '.[0][0]' <<< "$builds") source_ref=$(jq -r '.[0][1]' <<< "$builds")
  [[ "$commit" =~ ^[0-9a-f]{40}$ ]] || fail "$repo:$moving ($pin) has a build record that names no commit: '$commit'"
  if [ "$env" = prod ]; then
    [[ "$source_ref" =~ ^refs/tags/v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "$repo:$moving ($pin) was built at '$source_ref', not a release tag"
    tag=${source_ref#refs/tags/}
  else
    [ "$source_ref" = refs/heads/main ] || fail "$repo:$moving ($pin) was built at '$source_ref', not main"
    tag=$commit
  fi
  built=$commit
  provenance
  [ "$commit" = "$built" ] || fail "$tag names $commit in $remote, and $repo:$moving ($pin) was built from $built: nothing promoted"
  new="$repo:$moving"  # the tag itself: Fly resolves it whenever it creates a session's Machine
else
  code=$(fetch "$pin" "$manifest")
  case $code in
    200) ;;
    404) fail "not in the registry: $repo@$pin (HTTP 404; deploy-$env.yml pushes it)" ;;
    000) fail "the registry could not be reached for $repo@$pin (curl failed; its error text is not printed, since it could carry the credential)" ;;
    *) fail "the registry could not be read for $repo@$pin (HTTP $code; only 404 means not pushed)" ;;
  esac
  got="sha256:$(sha256_of < "$manifest")"
  [ "$got" = "$pin" ] || fail "the registry served $repo@$pin as a body whose sha256 is $got: nothing promoted"
  new="$repo@$pin"  # m-10: the attested digest alone; the tag only names the commit
fi

# 6. Main built it: the manifest carries deploy-<env>.yml's attestation for this commit (with --track,
# the commit step 5 found, so its discovery is bound field by field exactly as a pin is).
if [ "$env" = prod ]; then source_ref="refs/tags/$tag"; else source_ref=refs/heads/main; fi
identity="$SERVER/$REPO/.github/workflows/deploy-$env.yml@$source_ref"
# --limit (the PR #93 review's m-6): a branch can mint attestations for any digest; 1000 keeps main's
# in the window unless that many are minted (a refusal then, never a pass).
gh attestation verify "$manifest" --repo "$REPO" --cert-identity "$identity" \
    --source-ref "$source_ref" --source-digest "$commit" --signer-digest "$commit" --deny-self-hosted-runners \
    --limit 1000 > "$tmp/attest.out" 2>&1 \
  || fail "$repo@$pin ($tag) is not main's own build: no attestation from $identity for $commit (a squat, or an image deploy-$env.yml never pushed), nothing promoted: $(tail -c 300 "$tmp/attest.out")"

# 7. An [env] entry would make this a seazn.club PR, not a secret (C16 brief, step 3).
config=$(on_fly config show -a "$web" 2> "$tmp/fly.err") || fail "$web's config could not be read: $(head -c 300 "$tmp/fly.err")"
has_env=$(jq -er 'if type == "object" then ((.env // {}) | has("RELAY_IMAGE") | tostring) else error("not an object") end' <<< "$config" 2> /dev/null) \
  || fail "$web's config did not read as a Fly config"
if [ "$has_env" = true ]; then
  toml=fly.toml
  [ "$env" = prod ] || toml="fly.$env.toml"
  echo "RELAY_IMAGE is an [env] entry in $web's config. Promote it by a seazn.club PR to $toml:"
  echo "-  RELAY_IMAGE = \"$(jq -r '.env.RELAY_IMAGE' <<< "$config")\""
  echo "+  RELAY_IMAGE = \"$new\""
  fail "RELAY_IMAGE is an [env] entry in $web's config, not a secret: promotion is a seazn.club PR (the diff is above)"
fi

# 8. Every other secret Deployed, or `secrets deploy` would ship it with RELAY_IMAGE.
secrets=$(on_fly secrets list -a "$web" --json 2> "$tmp/fly.err") || fail "$web's secrets could not be listed: $(head -c 300 "$tmp/fly.err")"
pending=$(jq -er 'if type == "array" then [.[] | select(.name != "RELAY_IMAGE" and ((.status // "") | ascii_downcase) != "deployed")
    | "\(.name) (\(.status // "no status"))"] | join(", ") else error("not a list") end' <<< "$secrets" 2> /dev/null) \
  || fail "$web's secret list did not read as a list"
[ -z "$pending" ] || fail "$web has secrets that are not Deployed: $pending. A promote would ship them with RELAY_IMAGE: the owner must resolve staged secrets on $web first."
before=$(jq -r '[.[] | select(.name == "RELAY_IMAGE") | .digest][0] // "unset"' <<< "$secrets")

# The previous RELAY_IMAGE: the newest promote logged on this machine whose Fly digest is RELAY_IMAGE's now.
log="$state_dir/$env.log"
prev_line=''
if [ "$before" != unset ] && [ -f "$log" ]; then
  prev_line=$(awk -v d="$before" -v e="$env" '$2 == e && $5 == d {l = $0} END {print l}' "$log")
fi
rollback=''
prev_track=''
if [ -n "$prev_line" ] && [[ "$(awk '{print $3}' <<< "$prev_line")" == track:* ]]; then
  # A track: what the tag names is checked when it is followed again, so there is nothing to read now.
  read -r _ _ prev_ref prev_value _ <<< "$prev_line"
  prev_track=${prev_ref#track:}
  previous="$prev_value (tracking :$prev_track; --track verifies what it names when it runs)"
  rollback="--track $prev_track"
elif [ -n "$prev_line" ]; then
  read -r _ _ prev_ref prev_value _ <<< "$prev_line"
  prev_pin=${prev_ref#*@}
  prev_code=$(fetch "$prev_pin" "$tmp/previous.json")
  if [ "$prev_code" = 404 ]; then
    previous="$prev_value (ref $prev_ref; no longer in the registry)"
  elif [ "$prev_code" != 200 ] || [ "sha256:$(sha256_of < "$tmp/previous.json")" != "$prev_pin" ]; then
    previous="$prev_value (ref $prev_ref; the registry could not be read for it)"
  else
    previous="$prev_value (ref $prev_ref)"
    rollback=$prev_ref
  fi
elif [ "$before" = unset ]; then
  previous="unset (this is $web's first RELAY_IMAGE)"
else
  previous="set elsewhere (Fly digest $before; the value is write-only, and no promote logged on this machine set it)"
fi
# The registry's last read is above: the credential goes now, not at exit (m-12). The token itself
# expires 10 minutes after it was minted; it is not revoked here.
rm -f "$creds"

# 9. Stage, deploy, and prove it took (m-7).
on_fly secrets set "RELAY_IMAGE=$new" -a "$web" --stage || fail "flyctl secrets set failed on $web: nothing deployed"
listed=$(on_fly secrets list -a "$web" --json 2> "$tmp/fly.err") \
  || fail "RELAY_IMAGE is staged on $web, and its secret list could not be read after the set: state unproven, nothing deployed. Read it with flyctl secrets list -a $web"
staged=$(jq -r '[.[] | select(.name == "RELAY_IMAGE") | .digest][0] // ""' <<< "$listed" 2> /dev/null) \
  || fail "RELAY_IMAGE is staged on $web, and its secret list did not read as a list after the set: state unproven, nothing deployed"
[ -n "$staged" ] || fail "RELAY_IMAGE is not on $web's secret list after the set"
on_fly secrets deploy -a "$web" \
  || fail "flyctl secrets deploy failed on $web: RELAY_IMAGE is staged, not live. Roll back with the previous promote command, or run this one again"
listed=$(on_fly secrets list -a "$web" --json 2> "$tmp/fly.err") \
  || fail "the deploy ran on $web, and its secret list could not be read after it: RELAY_IMAGE may be live as $new, unproven and not logged. Read it with flyctl secrets list -a $web"
after=$(jq -c '[.[] | select(.name == "RELAY_IMAGE")][0] // {}' <<< "$listed" 2> /dev/null) \
  || fail "the deploy ran on $web, and its secret list did not read as a list after it: RELAY_IMAGE may be live as $new, unproven and not logged"
after_digest=$(jq -r '.digest // ""' <<< "$after")
after_status=$(jq -r '.status // "no status"' <<< "$after")
[ -n "$after_digest" ] || fail "RELAY_IMAGE is not set on $web after the deploy"
[ "$after_digest" = "$staged" ] || fail "RELAY_IMAGE's Fly digest on $web is $after_digest after the deploy, not the $staged this promote staged"
[ "$(tr '[:upper:]' '[:lower:]' <<< "$after_status")" = deployed ] || fail "RELAY_IMAGE on $web reads '$after_status' after the deploy, not Deployed: not proven live"
[ "$after_digest" != "$before" ] || echo "promote: warning: RELAY_IMAGE's Fly digest did not change ($after_digest): $web already had this value"
# A track logs as track:<tag>, with the digest it verified last.
log_ref=$ref log_tail=''
[ -z "$track" ] || log_ref="track:$moving" log_tail=" $pin"
if mkdir -p "$state_dir" && printf '%s %s %s %s %s%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$env" "$log_ref" "$new" "$after_digest" "$log_tail" >> "$log"; then
  logged="logged in $log"
else
  logged="NOT logged ($log could not be written): the next promote cannot name this one"
fi

echo "promoted $new to $web (Fly digest $before -> $after_digest, Deployed; $logged)"
echo "  web app:               $web"
echo "  commit:                $commit (on main; the CEF bump gate passed in its tree)"
echo "  build record:          attested by $identity"
echo "  new RELAY_IMAGE:       $new"
echo "  previous RELAY_IMAGE:  $previous"
echo "  RELAY_IMAGE's digest:  $before -> $after_digest (Deployed)"
if [ -n "$track" ]; then
  echo "  following:             :$moving, which names $pin now ($tag, main's build of $commit)"
  echo "Fly runs what it names whenever it creates a session's Machine: deploy-$env.yml moves it on every $event, once that build's record verifies, and running Machines keep theirs. Nothing checks what it names when Fly follows it: a push by any holder of the repository push token is followed too. To freeze $env at this build: scripts/promote.sh $env $tag@$pin$suffix"
else
  echo "  following:             no: pinned to $pin. deploy-$env.yml still moves :$moving on every $event, and while pinned it is not followed. To follow it: scripts/promote.sh $env --track $moving$suffix"
fi
if [ -n "$prev_track" ]; then
  echo "Rollback: scripts/promote.sh $env $rollback$suffix (it refuses unless what :$prev_track names then verifies as main's build)"
elif [ -n "$rollback" ]; then
  echo "Rollback: scripts/promote.sh $env $rollback$suffix (it refuses unless that digest's attestation still verifies)"
elif [ -n "$prev_line" ]; then
  echo "Rollback: the previous ref cannot be verified now ($previous)"
else
  echo "Rollback: no earlier promote of $env on this machine to return to"
fi
