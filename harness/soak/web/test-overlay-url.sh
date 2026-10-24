#!/usr/bin/env bash
# overlay-url.mjs (C15, page=real): the relay door's URL and its token, checked against web's own
# definition (apps/web/src/server/relay/tokens.ts, mintRelayToken: HS256 over AUTH_SECRET's UTF-8
# bytes, header {alg HS256, typ JWT}, audience seazn-relay, claims sid and scope relay-page, iat and
# exp) by an independent verifier (python's hmac), not by the code under test. And: the URL is
# written mode 600 and never printed; a wrong key does not verify; a past expiry, a bad fixture and
# a missing key are refused.
#
#   harness/soak/web/test-overlay-url.sh        (needs node and python3; prints `checked: N`)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
checked=0
failures=0
pass() { checked=$((checked + 1)); echo "PASS $1"; }
fail() { checked=$((checked + 1)); failures=$((failures + 1)); echo "FAIL $1${2:+: $2}"; }

key="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
sid=11111111-2222-4333-8444-555555555555
fx=aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee
exp=$(( ($(date +%s) + 3600) * 1000 ))
mint() { node "$HERE/overlay-url.mjs" --base http://m0.vm.soak.internal:3000 --sid "$sid" --fixture "$1" --exp-ms "$2" \
  --delay 3100 --template bar --out "$W/url"; }
rc=0
AUTH_SECRET="$key" mint "$fx" "$exp" >"$W/out.txt" 2>&1 || rc=$?
[ "$rc" -eq 0 ] && pass "minted (exit 0)" || fail "mint" "exit $rc: $(cat "$W/out.txt")"
url="$(cat "$W/url")"
st="$(sed -nE 's/.*[?&]st=([^&]+).*/\1/p' <<<"$url")"
[ "$(stat -c %a "$W/url" 2>/dev/null || stat -f %Lp "$W/url")" = 600 ] && pass "the URL file is mode 600" || fail "the URL file's mode"
[ -n "$st" ] && ! grep -qF "$st" "$W/out.txt" && pass "the token is never printed" || fail "the token printed (or absent)"
[[ "$url" =~ ^http://m0\.vm\.soak\.internal:3000/overlay/fixtures/$fx\?sid=$sid\&st=[A-Za-z0-9_.-]+\&host=compositor\&delay=3100\&template=bar$ ]] \
  && pass "the URL is the door's: /overlay/fixtures/<id>?sid&st&host=compositor&delay&template" || fail "the URL's shape" "$url"

verify() { # <token> <key>: the claims, if the signature holds
  python3 -I - "$1" "$2" <<'PY'
import base64, hashlib, hmac, json, sys
tok, key = sys.argv[1], sys.argv[2]
h, p, s = tok.split(".")
b = lambda x: base64.urlsafe_b64decode(x + "=" * (-len(x) % 4))
want = hmac.new(key.encode("utf-8"), f"{h}.{p}".encode(), hashlib.sha256).digest()
if not hmac.compare_digest(want, b(s)):
    sys.exit(1)
print(json.dumps({"header": json.loads(b(h)), "claims": json.loads(b(p))}))
PY
}
if v="$(verify "$st" "$key")"; then
  pass "the signature verifies: HS256 over the key's UTF-8 bytes"
  jq -e --arg sid "$sid" --argjson exp "$exp" '.header == {alg: "HS256", typ: "JWT"}
    and .claims.aud == "seazn-relay" and .claims.scope == "relay-page" and .claims.sid == $sid
    and .claims.exp == ($exp / 1000 | floor) and (.claims.iat | type == "number") and .claims.iat < .claims.exp' <<<"$v" >/dev/null \
    && pass "header and claims: typ JWT, aud seazn-relay, scope relay-page, sid, iat < exp (s)" || fail "the claims" "$v"
else
  fail "the signature" "did not verify"
fi
verify "$st" "x$key" >/dev/null 2>&1 && fail "a wrong key" "verified" || pass "a wrong key does not verify"

refused() { local rc=0; "$@" >/dev/null 2>&1 || rc=$?; [ "$rc" -ne 0 ]; }
refused env AUTH_SECRET="$key" bash -c "$(declare -f mint); HERE='$HERE' W='$W' sid='$sid' mint '$fx' $(( ($(date +%s) - 10) * 1000 ))" \
  && pass "a past expiry is refused" || fail "a past expiry" "accepted"
refused env AUTH_SECRET="$key" bash -c "$(declare -f mint); HERE='$HERE' W='$W' sid='$sid' mint 'not-a-uuid' $exp" \
  && pass "a fixture that is not a UUID is refused" || fail "a bad fixture" "accepted"
refused env -u AUTH_SECRET bash -c "$(declare -f mint); HERE='$HERE' W='$W' sid='$sid' mint '$fx' $exp" \
  && pass "no AUTH_SECRET is refused" || fail "no key" "accepted"

echo "checked: $checked, $failures failures"
[ "$checked" -gt 0 ] && [ "$failures" -eq 0 ]
