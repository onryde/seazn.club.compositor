#!/usr/bin/env bash
# Does a BuildKit registry-cache manifest (`docker buildx imagetools inspect --raw <ref>`, saved to a
# file) hold the cef-build tree? image.yml asks twice: the `cache` job after it writes (a write without
# the tree sends every cef-test build back to a CEF rebuild), and the `image` job before it trusts a
# tag as warm (a tag without the tree is a cold cache, so the run falls back to GitHub's cache).
#
# The tree is the one layer of 1 GB or more: 1.86 GB at CEF 139 (`[cef-build 3/3]`, the whole
# /src/gstcefsrc), where the next largest is gst-base's apt layer, about 230 MB. A slimmer cef-build
# stage lowers this floor. Both manifest shapes count: an index (`manifests`, BuildKit's default) and
# an image manifest (`layers`, `image-manifest=true`); the cache config blob is never a layer.
#
# Prints `<n> layers, the largest <bytes> bytes`. Exit 0 when it holds the tree; 1, with a
# `cef-cache-holds-tree:` line on stderr, when it does not or the file is not a manifest.
# Tested by scripts/cef_cache_key_test.py (check.yml).
#
# Usage: cef-cache-holds-tree.sh MANIFEST_JSON
set -euo pipefail

floor=1000000000
refuse() {
  echo "cef-cache-holds-tree: $*" >&2
  exit 1
}

[ "$#" -eq 1 ] && [ -r "$1" ] || refuse "usage: $0 MANIFEST_JSON (a readable file)"
summary=$(jq -er '((.manifests // []) + (.layers // []))
  | map(select((.mediaType // "") | test("cacheconfig") | not) | (.size // 0))
  | "\(length) \(max // 0)"' "$1" 2> /dev/null) || refuse "$1 is not a cache manifest"
read -r layers largest <<< "$summary"
echo "$layers layers, the largest $largest bytes"
# No separate "no layers" refusal: an empty list's largest is 0, which the floor refuses (that check
# was an equivalent mutant, t03).
[ "$largest" -ge "$floor" ] || refuse "no layer of $floor bytes or more ($layers layers): the cef-build tree is missing"
