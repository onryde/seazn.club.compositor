#!/usr/bin/env bash
# Promote a relay image (C16, docs/tasks/C16-deploy.md, amendment 2026-10-25; it replaces promote.yml):
# point a web app's RELAY_IMAGE at an image deploy-stg.yml or deploy-prod.yml pushed and attested. This
# is the live change: every NEW relay session on that web app takes the image (running Machines keep
# theirs). The bump is explicit, never automatic (decision log 2026-10-14): the owner runs this, or the
# coordinator on the owner's explicit go-ahead, on their own machine.
#
#   scripts/promote.sh stg  <40-hex main sha>@sha256:<64 hex>
#   scripts/promote.sh prod <vX.Y.Z>@sha256:<64 hex> --confirm-prod     (only after C17 passes)
#
# deploy-<env>.yml's summary prints the exact command. Rollback is this script with the previous ref,
# which every promote prints when it can prove it.
#
# No token: this script never reads, prints or asks for one, and no web-app token is ever in GitHub
# (decision log 2026-10-25). flyctl runs on the operator's STORED login (`flyctl auth login`):
# FLY_API_TOKEN and FLY_ACCESS_TOKEN are dropped from its environment, so a token left in the shell is
# never the one used. gh runs on the operator's gh login.
#
# Every refusal, in order. Before the first Fly or registry call:
#   1. the arguments: the env, that env's ref shape, the digest pin (required: the PR #85 review's I-4),
#      and --confirm-prod exactly when the env is prod, so a stg invocation can never reach prod;
#   2. the tools: git, jq, flyctl, docker with buildx, gh;
#   3. main (and a prod tag), fetched from GitHub into refs/promote/*, never into the operator's own
#      refs; and this file is main's copy (a stale or edited copy could lack a refusal);
#   4. where the image came from (I-2): the ref's commit is on main, and its own tree passes main's copy
#      of the CEF bump gate (Owed 22).
# Before the first call to the web app:
#   5. the registry, read through `flyctl auth docker` (Fly's registry refuses anonymous reads): the tag
#      resolves to the pin, compared unconditionally, so a moved tag refuses; only containerd's
#      "<ref>: not found" means not pushed, any other error is an error (m-5);
#   6. main's build record (I-4): the pin carries the attestation deploy-<env>.yml signs for each push
#      (stg: at refs/heads/main; prod: at refs/tags/<tag>), for the ref's commit, from a GitHub-hosted
#      runner. A branch's workflow can push under any tag (the push token is a repository secret) but
#      cannot sign as either, so its image is refused here.
# Then the web app, seazn-club-<env>:
#   7. RELAY_IMAGE must not be an [env] entry: promotion is then a seazn.club PR, and the diff is printed;
#   8. `secrets deploy` ships EVERY staged secret, so any other secret not Deployed refuses (I-3; any
#      status but Deployed, or none, refuses; RELAY_IMAGE's own never does, so a rollback can follow a
#      promote whose deploy failed after --stage);
#   9. `secrets set RELAY_IMAGE=<registry>/seazn-relay-<env>@<pin> --stage` (the digest alone, m-10),
#      `secrets deploy`, and the proof (m-7): RELAY_IMAGE reads Deployed, with the digest Fly gave the
#      staged value.
#
# The previous RELAY_IMAGE: Fly's secret values are write-only, so each promote appends its ref, its
# value and RELAY_IMAGE's Fly digest after the deploy to a log on this machine
# ($PROMOTE_STATE_DIR/<env>.log, default ~/.local/state/seazn-relay-promote). The previous value is the
# newest entry whose Fly digest is the one RELAY_IMAGE has now (equal values give equal Fly digests,
# measured 2026-10-25), so it is proven, never guessed; a value set from another machine, or by hand,
# reads "set elsewhere". A rollback is offered only while that entry's tag still resolves to its pin.
#
# Overrides, for scripts/deploy_checks_test.sh, which runs this under fake flyctl, docker and gh:
# RELAY_REGISTRY (default registry.fly.io, deploy-<env>.yml's default), PROMOTE_REMOTE (default the
# GitHub repository) and PROMOTE_STATE_DIR.
set -euo pipefail

REPO=onryde/seazn.club.compositor
SERVER=https://github.com
registry=${RELAY_REGISTRY:-registry.fly.io}
remote=${PROMOTE_REMOTE:-$SERVER/$REPO.git}
state_dir=${PROMOTE_STATE_DIR:-$HOME/.local/state/seazn-relay-promote}
self=${BASH_SOURCE[0]}

fail() {
  echo "promote: REFUSED: $*" >&2
  exit 1
}
usage() {
  echo "usage: scripts/promote.sh stg <40-hex main sha>@sha256:<64 hex>"
  echo "       scripts/promote.sh prod <vX.Y.Z>@sha256:<64 hex> --confirm-prod"
}

# 1. The arguments.
env='' ref='' confirm_prod=no
for a in "$@"; do
  case "$a" in
    --confirm-prod) confirm_prod=yes ;;
    -h | --help) usage; exit 0 ;;
    -*) fail "unknown flag '$a' ($(usage | head -n 1))" ;;
    *)
      if [ -z "$env" ]; then env=$a
      elif [ -z "$ref" ]; then ref=$a
      else fail "one env and one ref, not '$a' too"; fi ;;
  esac
done
case "$env" in
  stg)
    shape='^[0-9a-f]{40}$' what="a 40-hex main sha (deploy-stg's tag)"
    [ "$confirm_prod" = no ] || fail "--confirm-prod is for a prod promote, and this one is stg: nothing promoted" ;;
  prod)
    shape='^v[0-9]+\.[0-9]+\.[0-9]+$' what="a vX.Y.Z release tag (deploy-prod's tag)"
    [ "$confirm_prod" = yes ] || fail "a prod promote needs --confirm-prod (and C17 passed first): nothing promoted" ;;
  *) fail "env must be stg or prod: '$env'" ;;
esac
[ -n "$ref" ] || fail "no ref: $(usage | head -n 1)"
tag=${ref%%@*} pin=${ref#*@}
[ "$tag" != "$ref" ] || fail "a $env ref names its digest, <ref>@sha256:<digest> as deploy-$env.yml's summary prints it: the pin is required: '$ref'"
[[ "$tag" =~ $shape ]] || fail "a $env ref is $what@sha256:<digest>: '$ref'"
[[ "$pin" =~ ^sha256:[0-9a-f]{64}$ ]] || fail "the part after @ must be sha256:<64 hex>: '$ref'"

# 2. The tools.
for t in git jq flyctl docker gh; do
  command -v "$t" > /dev/null || fail "$t is not on PATH"
done
docker buildx version > /dev/null 2>&1 || fail "docker buildx is not available: it reads the registry (docker buildx imagetools inspect)"
# The stored login, never a token from the shell. Only whether each is set is read, never its value.
for v in FLY_API_TOKEN FLY_ACCESS_TOKEN; do
  if [ -n "${!v+x}" ]; then
    echo "promote: $v is set in this shell; it is dropped, and flyctl uses your stored login"
    unset "$v"
  fi
done

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
root=$(git -C "$(dirname "$self")" rev-parse --show-toplevel 2> /dev/null) || fail "$self is not in a git checkout"
G() { git -C "$root" "$@"; }
# flyctl from an empty directory, so no fly.toml lying around picks the app.
on_fly() { (cd "$tmp/cwd" && flyctl "$@"); }
mkdir -p "$tmp/cwd"

# 3. Main, from GitHub, and this file is main's copy.
G fetch --quiet --no-tags "$remote" "+refs/heads/main:refs/promote/main" 2> "$tmp/git.err" \
  || fail "main could not be fetched from $remote: $(head -c 300 "$tmp/git.err")"
main=$(G rev-parse -q --verify "refs/promote/main^{commit}") || fail "no main fetched from $remote"
G show "refs/promote/main:scripts/promote.sh" > "$tmp/main-promote.sh" 2> /dev/null || fail "main ($main) has no scripts/promote.sh"
cmp -s "$tmp/main-promote.sh" "$self" \
  || fail "$self is not main's copy (main is $main): update this checkout to main and run it again"

# 4. Where the image came from: the ref's commit is on main, and its own tree passes the CEF bump gate.
if [ "$env" = prod ]; then
  G fetch --quiet --no-tags "$remote" "+refs/tags/$tag:refs/promote/tags/$tag" 2> "$tmp/git.err" \
    || fail "no tag $tag in $remote: $(head -c 200 "$tmp/git.err")"
  commit=$(G rev-parse -q --verify "refs/promote/tags/$tag^{commit}") || fail "the tag $tag names no commit"
else
  commit=$(G rev-parse -q --verify "$tag^{commit}") || fail "no commit $tag in main's history"
fi
G merge-base --is-ancestor "$commit" refs/promote/main || fail "$tag ($commit) is not on main: images are promoted from main only"
tree="$tmp/promoted-tree"
mkdir -p "$tree/docker"
for f in docker/GSTCEF_COMMIT docker/CEF_STEP0_PASS _FINDINGS.md; do
  G show "$commit:$f" > "$tree/$f" 2> /dev/null || fail "$tag ($commit) has no $f, so the CEF bump gate cannot read its tree"
done
G show "refs/promote/main:scripts/check-cef-bump.sh" > "$tmp/check-cef-bump.sh" 2> /dev/null || fail "main ($main) has no scripts/check-cef-bump.sh"
bash "$tmp/check-cef-bump.sh" "$tree" || fail "$tag ($commit) fails the CEF bump gate (Owed 22) in its own tree"

# 5. The registry: the tag still resolves to the pin.
web="seazn-club-$env"
repo="$registry/seazn-relay-$env"
# resolve TAG: its manifest digest. Returns 2 when the registry says "not found" (containerd's
# "<ref>: not found", the only text that means absent) and 1 on any other error.
resolve() {
  local d
  if d=$(docker buildx imagetools inspect "$repo:$1" --format '{{.Manifest.Digest}}' 2> "$tmp/inspect.err"); then
    [[ "$d" =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "$repo:$1 resolved to no manifest digest: '$d'" > "$tmp/inspect.err"; return 1; }
    echo "$d"
  elif grep -qF "$repo:$1: not found" "$tmp/inspect.err"; then
    return 2
  else
    return 1
  fi
}
on_fly auth docker > "$tmp/auth.out" 2>&1 \
  || fail "flyctl auth docker failed (log in first: flyctl auth login): $(tail -c 300 "$tmp/auth.out")"
if digest=$(resolve "$tag"); then rc=0; else rc=$?; fi
case $rc in
  0) ;;
  2) fail "not pushed: $repo:$tag (deploy-$env.yml pushes it)" ;;
  *) fail "the registry could not be read for $repo:$tag (only 'not found' means not pushed): $(head -c 300 "$tmp/inspect.err")" ;;
esac
[ "$digest" = "$pin" ] || fail "$repo:$tag resolves to $digest, not the $pin this promote names: the tag moved, nothing promoted"
new="$repo@$pin"  # m-10: the attested digest alone; the tag only named it

# 6. Main built it: the pinned digest carries deploy-<env>.yml's attestation for this commit.
if [ "$env" = prod ]; then source_ref="refs/tags/$tag"; else source_ref=refs/heads/main; fi
identity="$SERVER/$REPO/.github/workflows/deploy-$env.yml@$source_ref"
gh attestation verify "oci://$repo@$pin" --repo "$REPO" --cert-identity "$identity" \
    --source-ref "$source_ref" --source-digest "$commit" --signer-digest "$commit" --deny-self-hosted-runners \
    > "$tmp/attest.out" 2>&1 \
  || fail "$repo:$tag@$pin is not main's own build: no attestation from $identity for $commit (a squat, or an image deploy-$env.yml never pushed), nothing promoted: $(tail -c 300 "$tmp/attest.out")"

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
if [ -n "$prev_line" ]; then
  read -r _ _ prev_ref prev_value _ <<< "$prev_line"
  prev_tag=${prev_ref%%@*} prev_pin=${prev_ref#*@}
  if prev_digest=$(resolve "$prev_tag"); then rc=0; else rc=$?; fi
  if [ "$rc" -eq 2 ]; then
    previous="$prev_value (ref $prev_ref; no longer in the registry)"
  elif [ "$rc" -ne 0 ]; then
    previous="$prev_value (ref $prev_ref; the registry could not be read for it)"
  elif [ "$prev_digest" != "$prev_pin" ]; then
    previous="$prev_value (ref $prev_ref; the tag has moved since, to $prev_digest)"
  else
    previous="$prev_value (ref $prev_ref)"
    rollback=$prev_ref
  fi
elif [ "$before" = unset ]; then
  previous="unset (this is $web's first RELAY_IMAGE)"
else
  previous="set elsewhere (Fly digest $before; the value is write-only, and no promote logged on this machine set it)"
fi

# 9. Stage, deploy, and prove it took (m-7).
on_fly secrets set "RELAY_IMAGE=$new" -a "$web" --stage || fail "flyctl secrets set failed on $web: nothing deployed"
staged=$(on_fly secrets list -a "$web" --json | jq -r '[.[] | select(.name == "RELAY_IMAGE") | .digest][0] // ""')
[ -n "$staged" ] || fail "RELAY_IMAGE is not on $web's secret list after the set"
on_fly secrets deploy -a "$web" \
  || fail "flyctl secrets deploy failed on $web: RELAY_IMAGE is staged, not live. Roll back with the previous promote command, or run this one again"
after=$(on_fly secrets list -a "$web" --json | jq -c '[.[] | select(.name == "RELAY_IMAGE")][0] // {}')
after_digest=$(jq -r '.digest // ""' <<< "$after")
after_status=$(jq -r '.status // "no status"' <<< "$after")
[ -n "$after_digest" ] || fail "RELAY_IMAGE is not set on $web after the deploy"
[ "$after_digest" = "$staged" ] || fail "RELAY_IMAGE's Fly digest on $web is $after_digest after the deploy, not the $staged this promote staged"
[ "$(tr '[:upper:]' '[:lower:]' <<< "$after_status")" = deployed ] || fail "RELAY_IMAGE on $web reads '$after_status' after the deploy, not Deployed: not proven live"
[ "$after_digest" != "$before" ] || echo "promote: warning: RELAY_IMAGE's Fly digest did not change ($after_digest): $web already had this value"
if mkdir -p "$state_dir" && printf '%s %s %s %s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$env" "$ref" "$new" "$after_digest" >> "$log"; then
  logged="logged in $log"
else
  logged="NOT logged ($log could not be written): the next promote cannot name this one"
fi

suffix=''
[ "$env" = stg ] || suffix=' --confirm-prod'
echo "promoted $new to $web (Fly digest $before -> $after_digest, Deployed; $logged)"
echo "  web app:               $web"
echo "  commit:                $commit (on main; the CEF bump gate passed in its tree)"
echo "  build record:          attested by $identity"
echo "  new RELAY_IMAGE:       $new"
echo "  previous RELAY_IMAGE:  $previous"
echo "  RELAY_IMAGE's digest:  $before -> $after_digest (Deployed)"
if [ -n "$rollback" ]; then
  echo "Rollback: scripts/promote.sh $env $rollback$suffix (it refuses if that tag has moved or its attestation does not verify)"
elif [ -n "$prev_line" ]; then
  echo "Rollback: the previous ref cannot be verified now ($previous)"
else
  echo "Rollback: no earlier promote of $env on this machine to return to"
fi
