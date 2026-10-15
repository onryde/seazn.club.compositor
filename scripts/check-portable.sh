#!/usr/bin/env bash
# Portability guard (ADR 0007). The compositor runs unchanged on any OCI host that meets the host
# contract, so nothing under crates/ or docker/ may name Fly. Fly is allowed only in the deploy
# workflows (C16), the soak runner (C15) and seazn.club's runner driver.
#
# Fails on any case-insensitive hit of `fly` (which also covers `FLY_` and `registry.fly.io`) or
# `.internal` (Fly's private-network DNS suffix), in every file, comments included: a comment naming
# Fly is how a dependency starts. A word that only contains the letters ("on the fly") is a hit too;
# reword it rather than exempting it.
#
# Prints `files scanned: N` and fails when N is 0: a scan of nothing proves nothing.
# Fails on any symlink under the roots: `find -type f` never follows one, so a link would carry
# content past this scan.
set -euo pipefail

cd "$(dirname "$0")/.."

ROOTS=(crates docker)
PATTERN='fly|\.internal'

files=()
links=()
for root in "${ROOTS[@]}"; do
  if [ ! -d "$root" ]; then
    echo "check-portable: $root/ is absent, skipped"
    continue
  fi
  while IFS= read -r -d '' f; do
    files+=("$f")
  done < <(find "$root" -type f -print0)
  while IFS= read -r -d '' l; do
    links+=("$l")
  done < <(find "$root" -type l -print0)
done

echo "files scanned: ${#files[@]}"
echo "symlinks found: ${#links[@]}"
if [ "${#files[@]}" -eq 0 ]; then
  echo "check-portable: FAIL: scanned 0 files under ${ROOTS[*]}; the scan itself is broken" >&2
  exit 1
fi
if [ "${#links[@]}" -gt 0 ]; then
  echo "check-portable: FAIL: symlink(s) under ${ROOTS[*]}, which this scan cannot follow:" >&2
  printf '  %s\n' "${links[@]}" >&2
  exit 1
fi

status=0
hits=$(grep -n -i -E -H -- "$PATTERN" "${files[@]}") || status=$?
if [ "$status" -gt 1 ]; then
  echo "check-portable: FAIL: grep exited $status" >&2
  exit 1
fi
if [ -n "$hits" ]; then
  echo "check-portable: FAIL: Fly reference in crates/ or docker/ (ADR 0007):" >&2
  echo "$hits" >&2
  exit 1
fi
echo "check-portable: OK, no Fly reference in ${ROOTS[*]}"
