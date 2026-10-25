#!/usr/bin/env bash
# CEF bump gate (C16 amendment 2026-10-18; compositor docs/_INDEX.md Owed 22). A CEF bump past
# Chrome 142 is expected to break the overlay page's loopback phase channel (Local Network Access is
# a permission no header satisfies), so C11 Step 0 is re-run on every bump and recorded before the
# bump ships.
#
# The record, docker/CEF_STEP0_PASS, is two lines: the docker/GSTCEF_COMMIT value C11 Step 0 last
# passed on (outcome (a), or (a) with a recorded grant), and that run's _FINDINGS.md F-id. Never
# update it over a failing run. This gate fails when:
#   - docker/GSTCEF_COMMIT is not 40 hex, or the record is missing or is not exactly two lines;
#   - GSTCEF_COMMIT differs from the record's line 1 (a bump with no Step 0 re-run on it);
#   - line 2 is not an F-id with a `## F-0xx` heading in _FINDINGS.md.
# check.yml runs it, and so do deploy-stg.yml, deploy-prod.yml and promote.yml, as the step right
# after each checkout (scripts/check-workflows.sh checks that they do).
#
#   scripts/check-cef-bump.sh [ROOT]     (ROOT: the repo root; scripts/deploy_checks_test.sh points
#                                         it at scratch copies)
#
# Prints `cef bump gate: GSTCEF_COMMIT=<sha> step0=<sha> <F-id>` on a pass.
set -euo pipefail

root=${1:-$(cd "$(dirname "$0")/.." && pwd)}

fail() {
  echo "cef bump gate: FAIL: $*" >&2
  echo "::error::CEF bump without a passing C11 Step 0 re-run (compositor docs/_INDEX.md Owed 22)"
  exit 1
}

pin_file="$root/docker/GSTCEF_COMMIT"
record="$root/docker/CEF_STEP0_PASS"
findings="$root/_FINDINGS.md"

[ -f "$pin_file" ] || fail "no docker/GSTCEF_COMMIT under $root"
commit=$(tr -d '[:space:]' < "$pin_file")
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || fail "docker/GSTCEF_COMMIT is not a 40-hex commit: '$commit'"

[ -f "$record" ] || fail "no docker/CEF_STEP0_PASS: C11 Step 0 has no recorded pass"
lines=()
while IFS= read -r line || [ -n "$line" ]; do
  lines+=("$(tr -d '[:space:]' <<< "$line")")
done < "$record"
[ "${#lines[@]}" -eq 2 ] || fail "docker/CEF_STEP0_PASS has ${#lines[@]} lines, not 2 (the commit, then the F-id)"
step0=${lines[0]}
fid=${lines[1]}
[[ "$step0" =~ ^[0-9a-f]{40}$ ]] || fail "docker/CEF_STEP0_PASS line 1 is not a 40-hex commit: '$step0'"
[[ "$fid" =~ ^F-[0-9]{3}$ ]] || fail "docker/CEF_STEP0_PASS line 2 is not an F-id: '$fid'"

[ -f "$findings" ] || fail "no _FINDINGS.md under $root"
grep -Eq "^## ${fid}([^0-9]|$)" "$findings" \
  || fail "_FINDINGS.md has no '## $fid' heading: the record names no finding"

if [ "$commit" != "$step0" ]; then
  fail "docker/GSTCEF_COMMIT=$commit, but C11 Step 0 last passed on $step0 ($fid): re-run Step 0 on the bump and record it"
fi

echo "cef bump gate: GSTCEF_COMMIT=$commit step0=$step0 $fid"
