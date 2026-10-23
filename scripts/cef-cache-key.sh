#!/usr/bin/env bash
# The tag key of image.yml's GHCR build cache, `ghcr.io/<owner>/seazn-relay-cache:cef-<key>`
# (decision log "CI: CEF layer from GHCR"): 16 hex of a sha256 over docker/GSTCEF_COMMIT and the
# Dockerfile's gst-base and cef-build stages, the two stages the CEF tree is built from (cef-build is
# FROM gst-base). A pin bump or an edit to either stage names a new tag, so a branch on another CEF
# never reads or overwrites the cache main reads.
#
# The key only names the tag. What keeps a stale layer out is BuildKit's own per-instruction cache
# key (the instruction, the build args it sees, the base image's digest, the files it copies). The
# rust-build, cef-test and runtime stages are left out on purpose: they change with every commit.
#
# Refuses (exit 1, one `cef-cache-key:` line on stderr) a commit that is not 40 lower-case hex, and a
# Dockerfile whose stages are not one gst-base, then one `FROM gst-base AS cef-build`, then one
# rust-build: the hashed range would otherwise drift without a sound. Tested by
# scripts/cef_cache_key_test.py (check.yml).
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

{ printf '%s\n' "$commit"; sed -n "${gst},$((rust - 1))p" "$dockerfile"; } | sha256sum | cut -c1-16
