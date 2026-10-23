#!/usr/bin/env bash
# The tag key of image.yml's GHCR build cache, `ghcr.io/<owner>/seazn-relay-cache:cef-<key>`
# (decision log "CI: CEF layer from GHCR"): 16 hex of a sha256 over docker/GSTCEF_COMMIT, the text of
# the Dockerfile's gst-base and cef-build stages (the two the CEF tree is built from; cef-build is FROM
# gst-base), and the digest each external base image in them resolves to now. `FROM ubuntu:24.04`'s
# text stays the same when the tag moves; its digest does not. So a pin bump, an edit to either stage
# or a moved base names a new tag: a branch on another CEF never reads or overwrites the cache main
# reads, and a moved base reads as a cold cache, not as a warm one that misses every layer.
#
# The key only names the tag. What keeps a stale layer out is BuildKit's own per-instruction cache
# key (the instruction, the build args it sees, the base image's digest, the files it copies). The
# rust-build, cef-test and runtime stages are left out on purpose: they change with every commit.
#
# Refuses (exit 1, one `cef-cache-key:` line on stderr):
#   - a commit that is not 40 lower-case hex;
#   - a Dockerfile whose stages are not one gst-base, then one `FROM gst-base AS cef-build`, then one
#     rust-build: the hashed range would otherwise drift without a sound;
#   - an input of the hashed stages that the key cannot see: a COPY or ADD (its files), a RUN --mount
#     (a bind mount's files, a secret), an ARG other than GSTCEF_COMMIT (its value comes from the
#     workflow), a FROM naming a variable;
#   - a base image that does not resolve to a digest.
# Tested by scripts/cef_cache_key_test.py (check.yml).
#
# The resolver is `docker buildx imagetools inspect <ref>`'s `Digest:` line. CEF_CACHE_KEY_RESOLVE
# names another (an executable taking the ref and printing `sha256:<64 hex>`), for the test.
#
# Usage: cef-cache-key.sh [DOCKERFILE [COMMIT_FILE]]   (default: docker/Dockerfile docker/GSTCEF_COMMIT)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
dockerfile=${1:-$root/docker/Dockerfile}
commit_file=${2:-$root/docker/GSTCEF_COMMIT}

refuse() {
  echo "cef-cache-key: $*" >&2
  exit 1
}

[ -r "$commit_file" ] || refuse "cannot read $commit_file"
[ -r "$dockerfile" ] || refuse "cannot read $dockerfile"
commit=$(tr -d '[:space:]' < "$commit_file")
[[ $commit =~ ^[0-9a-f]{40}$ ]] || refuse "$commit_file is not a 40-hex commit: '$commit'"

# The line number of the one FROM line matching $1 (stage $2).
stage_line() {
  local found count
  found=$(grep -nE "$1" "$dockerfile" | cut -d: -f1) || true
  count=$(printf '%s' "$found" | grep -c . || true)
  case $count in
    1) echo "$found" ;;
    0) refuse "$dockerfile has no $2 stage ($1)" ;;
    *) refuse "$dockerfile has $count $2 stages ($1)" ;;
  esac
}

gst=$(stage_line '^FROM [^ ]+ AS gst-base$' gst-base)
cef=$(stage_line '^FROM gst-base AS cef-build$' cef-build)
rust=$(stage_line '^FROM [^ ]+ AS rust-build$' rust-build)
if ! [ "$gst" -lt "$cef" ] || ! [ "$cef" -lt "$rust" ]; then
  refuse "$dockerfile's stages are not gst-base, cef-build, rust-build in that order (lines $gst, $cef, $rust)"
fi
hashed() { sed -n "${gst},$((rust - 1))p" "$dockerfile"; }

# Inputs of the hashed stages the key cannot see.
unseen=$(hashed | grep -iE '^[[:space:]]*(COPY|ADD)[[:space:]]|--mount=' | head -n 1) || true
if [ -z "$unseen" ]; then
  unseen=$(hashed | grep -iE '^[[:space:]]*ARG[[:space:]]' \
    | grep -ivE '^[[:space:]]*ARG[[:space:]]+GSTCEF_COMMIT([[:space:]=]|$)' | head -n 1) || true
fi
[ -z "$unseen" ] || refuse "the gst-base/cef-build stages read an input the key cannot see: '${unseen:0:120}'"

resolve() {
  if [ -n "${CEF_CACHE_KEY_RESOLVE:-}" ]; then
    "$CEF_CACHE_KEY_RESOLVE" "$1"
  else
    docker buildx imagetools inspect "$1" | awk '$1 == "Digest:" && d == "" { d = $2 } END { print d }'
  fi
}

# Each FROM in the range: a stage defined above it, `scratch` or an `@sha256:` pin is in the text
# already; any other base is resolved, and its digest goes into the key.
bases=
while read -r n base; do
  case $base in
    *'$'*) refuse "line $n's FROM names a variable: '$base'" ;;
    scratch | *@sha256:*) continue ;;
  esac
  if awk -v n="$n" -v b="$base" 'NR < n && toupper($1) == "FROM" && toupper($(NF - 1)) == "AS" && $NF == b { f = 1 }
                                 END { exit !f }' "$dockerfile"; then
    continue
  fi
  if out=$(resolve "$base" 2>&1); then digest=$out; else digest=; fi
  [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] \
    || refuse "cannot resolve the base image $base to a digest: '$(printf '%s' "$out" | tr '\n' ' ' | cut -c1-120)'"
  bases+="base $base $digest"$'\n'
done < <(hashed | grep -nE '^FROM[[:space:]]' \
  | awk -v first="$gst" '{ n = $0; sub(/:.*/, "", n); line = $0; sub(/^[0-9]+:/, "", line);
                          m = split(line, t, /[[:space:]]+/);
                          for (i = 2; i <= m; i++) if (t[i] !~ /^--/) { print n + first - 1, t[i]; break } }')

{ printf '%s\n' "$commit"; hashed; printf '%s' "$bases"; } | sha256sum | cut -c1-16
