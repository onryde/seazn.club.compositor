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
#   - line 2 is not an F-id with a `## F-0xx` heading in _FINDINGS.md;
#   - that heading does not say "C11 Step 0" (the record names a finding that is no Step 0 run:
#     F-004, the YouTube audio finding, would otherwise pass);
#   - that finding's section (its heading to the next `## `) does not name line 1's commit in full
#     (every Step 0 run prints the image's BUILDINFO, `gstcefsrc_commit=<sha>`; F-009 names
#     204b01b…), so a bump recorded as `<new sha>` + an old run's F-id fails;
#   - GSTCEF_COMMIT differs from the record's line 1 (a bump with no Step 0 re-run on it).
# It cannot see whether the run PASSED: the F-id's outcome is the recorder's word (never record a
# failing run).
# check.yml runs it, and so do deploy-stg.yml, deploy-prod.yml and promote.yml, as the step right
# after each checkout (scripts/check-workflows.sh checks that they do).
#
#   scripts/check-cef-bump.sh [ROOT]     (ROOT: the repo root; scripts/deploy_checks_test.sh points
#                                         it at scratch copies)
#
# Prints `cef bump gate: GSTCEF_COMMIT=<sha> step0=<sha> <F-id> (a C11 Step 0 finding naming <sha>)`
# on a pass.
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
heading=$(grep -E "^## ${fid}([^0-9]|$)" "$findings" | head -n 1 || true)
[ -n "$heading" ] || fail "_FINDINGS.md has no '## $fid' heading: the record names no finding"
[[ "$heading" == *"C11 Step 0"* ]] \
  || fail "_FINDINGS.md's $fid heading does not say C11 Step 0: the record names a finding that is no Step 0 run"
section=$(H="$heading" awk '$0 == ENVIRON["H"] { p = 1; next } p && /^## / { exit } p' "$findings")
grep -qF -- "$step0" <<< "$section" \
  || fail "_FINDINGS.md's $fid does not name $step0: that finding did not run on the record's commit"

if [ "$commit" != "$step0" ]; then
  fail "docker/GSTCEF_COMMIT=$commit, but C11 Step 0 last passed on $step0 ($fid): re-run Step 0 on the bump and record it"
fi

echo "cef bump gate: GSTCEF_COMMIT=$commit step0=$step0 $fid (a C11 Step 0 finding naming $step0)"
