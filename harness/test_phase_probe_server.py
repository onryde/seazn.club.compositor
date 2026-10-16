#!/usr/bin/env python3
"""Self-test for harness/phase-probe-server.py (C11 Step 0). Python stdlib only.

Run from the repo root: `python3 -m unittest harness/test_phase_probe_server.py -v`.
Each test starts the real server as a subprocess on 127.0.0.1:7419 (the port is fixed by the brief),
so nothing else may listen there. Expected values come from the C11 brief's Step 0 text and from
web's `lib/relay-phase.ts` (PROGRAMME_PHASES, RELAY_PHASE_LONG_POLL_MAX_MS), never from the server.
"""

import http.client
import json
import os
import re
import socket
import subprocess
import sys
import threading
import time
import unittest

SERVER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "phase-probe-server.py")
HOST, PORT = "127.0.0.1", 7419  # brief: bound to 127.0.0.1:7419 (RELAY_PHASE_PORT)
LONG_POLL_MAX_S = 10.0  # web lib/relay-phase.ts RELAY_PHASE_LONG_POLL_MAX_MS = 10_000
ORIGIN = "https://probe-origin.example"
ACAO, ACAM, ACAPN = (
    "Access-Control-Allow-Origin",
    "Access-Control-Allow-Methods",
    "Access-Control-Allow-Private-Network",
)


def port_open():
    with socket.socket() as s:
        s.settimeout(0.2)
        return s.connect_ex((HOST, PORT)) == 0


class Server:
    """The real server as a child process; stdout is collected line by line."""

    def __init__(self, *args):
        self.proc = subprocess.Popen(
            [sys.executable, "-u", SERVER, *args],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.lines = []
        self._reader = threading.Thread(target=self._read, daemon=True)
        self._reader.start()
        deadline = time.monotonic() + 5
        while not port_open():
            if self.proc.poll() is not None or time.monotonic() > deadline:
                raise AssertionError("server never bound: " + self.proc.stderr.read())
            time.sleep(0.05)

    def _read(self):
        for line in self.proc.stdout:
            self.lines.append(line.rstrip("\n"))

    def stop(self):
        self.proc.terminate()
        self.proc.wait(5)
        self._reader.join(2)
        self.proc.stdout.close()
        self.proc.stderr.close()

    def requests(self):
        return [ln for ln in self.lines if ln.startswith("request ")]

    def replies(self):
        return [ln for ln in self.lines if ln.startswith("reply ")]

    def transitions(self):
        return [ln for ln in self.lines if ln.startswith("transition ")]


def call(method, path, headers=None, timeout=15):
    conn = http.client.HTTPConnection(HOST, PORT, timeout=timeout)
    t0 = time.monotonic()
    conn.request(method, path, headers=headers or {})
    resp = conn.getresponse()
    body = resp.read().decode()
    elapsed = time.monotonic() - t0
    conn.close()
    return resp.status, {k.lower(): v for k, v in resp.getheaders()}, body, elapsed


def cors_headers(headers):
    return sorted(k for k in headers if k.startswith("access-control-"))


class PhaseProbeServerTest(unittest.TestCase):
    def setUp(self):
        self.assertFalse(port_open(), f"{HOST}:{PORT} is already in use; the test needs it free")
        self.server = None

    def tearDown(self):
        if self.server:
            self.server.stop()

    def start(self, *args):
        self.server = Server(*args)
        return self.server

    def test_pna_variant_answers_the_preflight_and_the_get_for_the_request_origin(self):
        self.start("--variant", "pna", "--hold-ms", "300")
        pre = {
            "Origin": ORIGIN,
            "Access-Control-Request-Method": "GET",
            "Access-Control-Request-Private-Network": "true",
        }
        status, h, body, _ = call("OPTIONS", "/phase?after=0", pre)
        self.assertEqual(status, 204)
        self.assertEqual(body, "")
        self.assertEqual(h.get(ACAO.lower()), ORIGIN)
        self.assertEqual(h.get(ACAM.lower()), "GET")
        self.assertEqual(h.get(ACAPN.lower()), "true")
        self.assertEqual(h.get("vary"), "Origin")
        # A second call, with another `after`: the answer is S+1 each time, after the hold.
        for after in (0, 41):
            status, h, body, elapsed = call("GET", f"/phase?after={after}", {"Origin": ORIGIN})
            self.assertEqual(status, 200)
            self.assertEqual(json.loads(body), {"phase": "live", "seq": after + 1})
            self.assertEqual(h.get(ACAO.lower()), ORIGIN)
            self.assertGreaterEqual(elapsed, 0.3, "the GET must be held --hold-ms before it is answered")
            self.assertLess(elapsed, 0.3 + 1.0)

    def test_none_variant_sends_no_cors_header_at_all(self):
        self.start("--variant", "none", "--hold-ms", "0")
        pre = {
            "Origin": ORIGIN,
            "Access-Control-Request-Method": "GET",
            "Access-Control-Request-Private-Network": "true",
        }
        status, h, _, _ = call("OPTIONS", "/phase?after=0", pre)
        self.assertEqual(status, 204)
        self.assertEqual(cors_headers(h), [])
        status, h, body, _ = call("GET", "/phase?after=0", {"Origin": ORIGIN})
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {"phase": "live", "seq": 1})
        self.assertEqual(cors_headers(h), [])

    def test_hold_defaults_to_two_seconds(self):
        self.start("--variant", "pna")
        status, _, _, elapsed = call("GET", "/phase?after=0", {"Origin": ORIGIN})
        self.assertEqual(status, 200)
        self.assertGreaterEqual(elapsed, 2.0)
        self.assertLess(elapsed, 3.0)

    def test_every_request_logs_one_line_with_the_brief_fields(self):
        server = self.start("--variant", "pna", "--hold-ms", "0")
        sent = [
            ("OPTIONS", "/phase?after=0", {
                "Origin": ORIGIN,
                "Access-Control-Request-Method": "GET",
                "Access-Control-Request-Private-Network": "true",
            }),
            ("GET", "/phase?after=0", {"Origin": ORIGIN}),
            ("GET", "/phase?after=7", {}),
            ("GET", "/nowhere", {}),
            ("POST", "/phase?after=0", {"Origin": ORIGIN}),
        ]
        for method, path, headers in sent:
            call(method, path, headers)
        time.sleep(0.2)
        logged = server.requests()
        print(f"\nrequest lines checked: {len(logged)}")
        self.assertEqual(len(logged), len(sent))
        for (method, path, headers), line in zip(sent, logged):
            self.assertIn(f"method={method} ", line)
            self.assertIn(f"path={path} ", line)
            self.assertIn(f"origin={headers.get('Origin', '-')} ", line)
            self.assertIn(f"acrpn={headers.get('Access-Control-Request-Private-Network', '-')} ", line)
            self.assertIn(f"acrm={headers.get('Access-Control-Request-Method', '-')} ", line)
            self.assertRegex(line, r" at=\d{13} ")

    def test_every_reply_logs_the_cors_headers_it_sent(self):
        # The run record, not the code, must show what a preflight was answered with (review M1). The
        # expected value is what the client actually received, header by header.
        pre = {
            "Origin": ORIGIN,
            "Access-Control-Request-Method": "GET",
            "Access-Control-Request-Private-Network": "true",
        }
        checked = 0
        for variant in ("pna", "none"):
            server = self.start("--variant", variant, "--hold-ms", "0")
            got = []
            for method, headers in (("OPTIONS", pre), ("GET", {"Origin": ORIGIN})):
                _, h, _, _ = call(method, "/phase?after=0", headers)
                got.append({k: v for k, v in h.items() if k.startswith("access-control-") or k == "vary"})
            time.sleep(0.2)
            replies = server.replies()
            self.assertEqual(len(replies), 2, replies)
            for line, received in zip(replies, got):
                m = re.search(r" cors=(\{[^ ]*\}) ", line)
                self.assertIsNotNone(m, line)
                logged = {k.lower(): v for k, v in json.loads(m.group(1)).items()}
                self.assertEqual(logged, received, line)
                checked += 1
            if variant == "pna":
                self.assertEqual(got[0].get(ACAPN.lower()), "true", "the positive pair: pna logs a non-empty set")
            else:
                self.assertEqual(got, [{}, {}])
            server.stop()
            self.server = None
        print(f"\nreply lines checked: {checked}")
        self.assertEqual(checked, 4)

    def test_bad_requests_are_refused(self):
        self.start("--variant", "pna", "--hold-ms", "0")
        cases = [
            ("GET", "/nowhere", 404),
            ("GET", "/phase", 400),
            ("GET", "/phase?after=abc", 400),
            ("GET", "/phase?after=-1", 400),
            ("GET", "/phase?after=1.5", 400),
            ("POST", "/phase?after=0", 405),
            ("PUT", "/phase?after=0", 405),
        ]
        for method, path, want in cases:
            status, _, _, _ = call(method, path, {"Origin": ORIGIN})
            self.assertEqual(status, want, f"{method} {path}")
        print(f"\nbad requests checked: {len(cases)}")

    def test_a_held_get_does_not_block_a_preflight(self):
        self.start("--variant", "pna", "--hold-ms", "1500")
        held = {}
        t = threading.Thread(target=lambda: held.update(r=call("GET", "/phase?after=0", {"Origin": ORIGIN})))
        t.start()
        time.sleep(0.2)
        status, _, _, elapsed = call("OPTIONS", "/phase?after=0", {"Origin": ORIGIN})
        self.assertEqual(status, 204)
        self.assertLess(elapsed, 0.5)
        t.join(5)
        self.assertEqual(held["r"][0], 200)

    def test_script_serves_the_sequence_from_the_first_get_then_live(self):
        server = self.start("--variant", "pna", "--script", "waiting:600,reconnecting:600,final:600")
        # The script clock starts at the first GET, not at server start: 0.9 s after start a clock
        # started at boot would already be in `reconnecting`.
        time.sleep(0.9)
        self.assertEqual(server.transitions(), [], "no transition before the first GET")
        h = {"Origin": ORIGIN}
        status, hdrs, body, elapsed = call("GET", "/phase?after=0", h)
        self.assertEqual((status, json.loads(body)), (200, {"phase": "waiting", "seq": 1}))
        self.assertEqual(hdrs.get(ACAO.lower()), ORIGIN)
        self.assertLess(elapsed, 0.3, "a long-poll behind the current seq answers at once")
        want = [("reconnecting", 2), ("final", 3), ("live", 4)]
        for (phase, seq), after in zip(want, (1, 2, 3)):
            status, _, body, elapsed = call("GET", f"/phase?after={after}", h)
            self.assertEqual((status, json.loads(body)), (200, {"phase": phase, "seq": seq}))
            self.assertGreater(elapsed, 0.3, f"after={after} is answered at the next change, not at once")
            self.assertLess(elapsed, 0.6 + 0.4)
        # After the last entry it serves live; a poll on the last seq is held to the long-poll cap.
        status, _, body, elapsed = call("GET", "/phase?after=4", h)
        self.assertEqual((status, json.loads(body)), (200, {"phase": "live", "seq": 4}))
        self.assertGreaterEqual(elapsed, LONG_POLL_MAX_S - 0.05)
        self.assertLess(elapsed, LONG_POLL_MAX_S + 1.0)
        # A poll that is behind answers at once with the current value (a second, late reader).
        status, _, body, elapsed = call("GET", "/phase?after=1", h)
        self.assertEqual((status, json.loads(body)), (200, {"phase": "live", "seq": 4}))
        self.assertLess(elapsed, 0.3)
        lines = server.transitions()
        print(f"\ntransitions checked: {len(lines)}")
        parsed = [re.fullmatch(r"transition (\w+) seq=(\d+) at=(\d{13})", ln) for ln in lines]
        self.assertTrue(all(parsed), lines)
        got = [(m.group(1), int(m.group(2))) for m in parsed]
        self.assertEqual(got, [("waiting", 1), ("reconnecting", 2), ("final", 3), ("live", 4)])
        ats = [int(m.group(3)) for m in parsed]
        for a, b in zip(ats, ats[1:]):
            self.assertGreaterEqual(b - a, 600 - 20)
            self.assertLess(b - a, 600 + 200)

    def test_refusals_at_start_up_exit_2_and_never_bind(self):
        # Each refusal names its reason: a bare exit 2 is also what Python returns for a missing
        # script file, so the code alone cannot tell a refusal from a server that never ran.
        bad = [
            (["--variant", "pna", "--script", "waiting:500,paused:500"], "paused"),
            (["--variant", "pna", "--script", "waiting:abc"], "waiting:abc"),
            (["--variant", "pna", "--script", "waiting:0"], "waiting:0"),
            (["--variant", "pna", "--script", "waiting"], "waiting"),
            (["--variant", "pna", "--script", ""], "--script"),
            (["--variant", "pna", "--script", "waiting:500", "--hold-ms", "100"], "--hold-ms"),
            (["--variant", "pna", "--hold-ms", "-1"], "--hold-ms"),
            (["--variant", "bogus"], "bogus"),
            ([], "--variant"),
        ]
        for args, needle in bad:
            proc = subprocess.run([sys.executable, SERVER, *args], capture_output=True, text=True, timeout=5)
            self.assertEqual(proc.returncode, 2, f"{args}: {proc.stderr}")
            self.assertIn("usage:", proc.stderr, f"{args}: not an argument refusal: {proc.stderr}")
            self.assertIn(needle, proc.stderr.split("error:", 1)[-1], f"{args}: {proc.stderr}")
            self.assertFalse(port_open(), f"{args} bound the port")
        print(f"\nrefusals checked: {len(bad)}")
        # The positive pair: every one of the five programme phases is accepted.
        phases = ["waiting", "live", "frozen", "reconnecting", "final"]  # web lib/relay-phase.ts
        server = self.start("--variant", "pna", "--script", ",".join(f"{p}:200" for p in phases))
        self.assertIsNone(server.proc.poll())


if __name__ == "__main__":
    unittest.main()
