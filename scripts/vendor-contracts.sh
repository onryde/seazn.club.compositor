#!/usr/bin/env bash
# usage: scripts/vendor-contracts.sh /abs/path/to/seazn.club <web-commit-sha>
# Byte-copies the relay-job contract (docs/contracts/relay-job.v1.json and its fixtures) from the web
# repo at an exact commit and records sha256 pins in contracts/VENDORED.json. Run by hand; never in
# CI (AGENTS.md §8). Never hand-edit contracts/: crates/adapters/tests/contract.rs pins every file.
set -euo pipefail
web="$1"; rev="$2"; here="$(cd "$(dirname "$0")/.." && pwd)"
# Record the full commit sha, never a branch name or an abbreviation that moves or collides.
sha="$(git -C "$web" rev-parse --verify "$rev^{commit}")"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
git -C "$web" archive "$sha" docs/contracts/relay-job.v1.json docs/contracts/fixtures/relay-job.v1 | tar -x -C "$tmp"
rm -rf "$here/contracts"; mkdir -p "$here/contracts"
cp -R "$tmp/docs/contracts/." "$here/contracts/"
# LC_ALL=C: the pin order is byte order on every machine.
( cd "$here/contracts" && find . -type f ! -name VENDORED.json | LC_ALL=C sort | while read -r f; do
    printf '    "%s": "%s",\n' "${f#./}" "$(shasum -a 256 "$f" | cut -d' ' -f1)"; done ) > "$tmp/pins"
{ echo '{'; echo "  \"source\": \"onryde/seazn.club\","; echo "  \"commit\": \"$sha\","; echo '  "files": {';
  sed '$ s/,$//' "$tmp/pins"; echo '  }'; echo '}'; } > "$here/contracts/VENDORED.json"
echo "vendored $(grep -c '": "' "$tmp/pins") files at $sha"
