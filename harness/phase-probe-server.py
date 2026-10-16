#!/usr/bin/env python3
"""C11 Step 0 probe server: a stand-in for C12's loopback phase endpoint. Harness only, never shipped.

Python stdlib only. Bound to 127.0.0.1:7419, web's RELAY_PHASE_PORT (lib/relay-phase.ts, R2007 PR B).

  --variant none   no CORS header at all, on any answer
  --variant pna    OPTIONS answers 204 with Access-Control-Allow-Origin: <the request's Origin>,
                   Access-Control-Allow-Methods: GET, Access-Control-Allow-Private-Network: true and
                   Vary: Origin; GET answers carry the same Access-Control-Allow-Origin
  --hold-ms N      fixed mode (the default): wait N ms (default 2000), then answer
                   GET /phase?after=S with {"phase":"live","seq":S+1}
  --script P:MS,…  script mode (coordinator decision G1): serve that phase sequence instead. Each entry
                   holds for its ms and seq rises by one per entry; after the last entry it serves
                   live. The script clock starts at the first GET /phase, not at server start. A poll
                   behind the current seq answers at once; a poll on it is answered at the next
                   change, or at RELAY_PHASE_LONG_POLL_MAX_MS with the current value. A phase outside
                   the five programme phases is refused at start-up (exit 2), never served.

Every request logs one `request …` line to stdout at arrival (method, path, Origin,
Access-Control-Request-Private-Network, Access-Control-Request-Method, then every header as JSON);
every answer logs one `reply …` line (with `cors=<the CORS and Vary headers sent, as JSON>`; added after
the F-009 runs, whose reply lines carry no `cors=`), and every script transition one
`transition <phase> seq=<n> at=<epoch ms>` line. Start-up and refusals go to stderr, so the first
stdout line is the first request.
"""

import argparse
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

HOST = "127.0.0.1"
PORT = 7419  # RELAY_PHASE_PORT (web lib/relay-phase.ts)
PHASE_PATH = "/phase"  # RELAY_PHASE_PATH
LONG_POLL_MAX_MS = 10_000  # RELAY_PHASE_LONG_POLL_MAX_MS
PROGRAMME_PHASES = ("waiting", "live", "frozen", "reconnecting", "final")  # PROGRAMME_PHASES
DEFAULT_HOLD_MS = 2_000  # the C11 brief, Step 0

_out_lock = threading.Lock()


def now_ms():
    return int(time.time() * 1000)


def log(line):
    with _out_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


class Script:
    """The scripted phase sequence. Nothing moves until `start()` (the first GET /phase)."""

    def __init__(self, entries):
        self.entries = entries  # [(phase, hold_ms)]
        self.cond = threading.Condition()
        self.phase = None
        self.seq = 0
        self.started = False

    def _set(self, phase, seq):
        with self.cond:
            self.phase, self.seq = phase, seq
            log(f"transition {phase} seq={seq} at={now_ms()}")
            self.cond.notify_all()

    def start(self):
        with self.cond:
            if self.started:
                return
            self.started = True
            t0 = time.monotonic()
            self._set(self.entries[0][0], 1)
        threading.Thread(target=self._drive, args=(t0,), daemon=True).start()

    def _drive(self, t0):
        offset_ms = 0
        nexts = [p for p, _ in self.entries[1:]] + ["live"]
        for i, phase in enumerate(nexts):
            offset_ms += self.entries[i][1]
            delay = t0 + offset_ms / 1000 - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            self._set(phase, i + 2)

    def answer(self, after):
        """The phase for a poll on `after`: at once if behind, else at the next change or the cap."""
        self.start()
        deadline = time.monotonic() + LONG_POLL_MAX_MS / 1000
        with self.cond:
            while self.seq <= after:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                self.cond.wait(remaining)
            return self.phase, self.seq


def make_handler(variant, hold_ms, script):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        server_version = "phase-probe"
        sys_version = ""

        def log_message(self, fmt, *args):  # the stdlib's own access log goes nowhere
            pass

        def _log_request(self):
            h = self.headers
            fields = [
                f"method={self.command}",
                f"path={self.path}",
                f"origin={h.get('Origin', '-')}",
                f"acrpn={h.get('Access-Control-Request-Private-Network', '-')}",
                f"acrm={h.get('Access-Control-Request-Method', '-')}",
                f"at={now_ms()}",
                "headers=" + json.dumps(dict(h.items()), separators=(",", ":")),
            ]
            log("request " + " ".join(fields))

        def _send(self, status, body=b"", cors=None, held_ms=None):
            try:
                self.send_response(status)
                if status in (200, 204):
                    self.send_header("Cache-Control", "no-store")
                for k, v in (cors or {}).items():
                    self.send_header(k, v)
                if body:
                    self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                if body:
                    self.wfile.write(body)
                self.wfile.flush()
                result = "ok"
            except OSError as e:  # the client went away while the poll was held
                result = f"failed:{type(e).__name__}"
            extra = f" held_ms={held_ms}" if held_ms is not None else ""
            shown = body.decode() if body else "-"
            sent = json.dumps(cors or {}, separators=(",", ":"))  # the CORS and Vary headers sent, verbatim
            log(f"reply method={self.command} path={self.path} status={status}{extra} "
                f"write={result} cors={sent} at={now_ms()} body={shown}")

        def _route(self):
            """The `after` value for a well-formed /phase request, or None once refused."""
            parts = urlsplit(self.path)
            if parts.path != PHASE_PATH:
                self._send(404)
                return None
            return parts

        def do_OPTIONS(self):
            self._log_request()
            if self._route() is None:
                return
            cors = {}
            if variant == "pna":
                origin = self.headers.get("Origin")
                if origin is not None:
                    cors["Access-Control-Allow-Origin"] = origin
                cors["Access-Control-Allow-Methods"] = "GET"
                cors["Access-Control-Allow-Private-Network"] = "true"
                cors["Vary"] = "Origin"
            self._send(204, cors=cors)

        def do_GET(self):
            self._log_request()
            parts = self._route()
            if parts is None:
                return
            values = parse_qs(parts.query, keep_blank_values=True).get("after", [])
            if len(values) != 1 or not values[0].isdigit() or not values[0].isascii():
                self._send(400)
                return
            after = int(values[0])
            t0 = time.monotonic()
            if script is None:
                time.sleep(hold_ms / 1000)
                phase, seq = "live", after + 1
            else:
                phase, seq = script.answer(after)
            body = json.dumps({"phase": phase, "seq": seq}, separators=(",", ":")).encode()
            cors = {}
            if variant == "pna":
                origin = self.headers.get("Origin")
                if origin is not None:
                    cors["Access-Control-Allow-Origin"] = origin
                cors["Vary"] = "Origin"
            self._send(200, body, cors=cors, held_ms=int((time.monotonic() - t0) * 1000))

        def _refuse_method(self):
            self._log_request()
            self._send(405)

        do_POST = do_PUT = do_DELETE = do_PATCH = do_HEAD = _refuse_method

    return Handler


def parse_script(parser, text):
    entries = []
    for raw in text.split(","):
        phase, sep, ms = raw.partition(":")
        if not sep or not ms.isdigit() or not ms.isascii() or int(ms) <= 0:
            parser.error(f"--script entry {raw!r} is not <phase>:<positive ms>")
        if phase not in PROGRAMME_PHASES:
            parser.error(f"--script phase {phase!r} is not one of {', '.join(PROGRAMME_PHASES)}")
        entries.append((phase, int(ms)))
    return entries


def main(argv=None):
    parser = argparse.ArgumentParser(prog="phase-probe-server.py", description=__doc__.split("\n")[0])
    parser.add_argument("--variant", required=True, choices=("none", "pna"))
    parser.add_argument("--hold-ms", type=int, default=None, help=f"fixed mode only (default {DEFAULT_HOLD_MS})")
    parser.add_argument("--script", default=None, help="e.g. waiting:5000,reconnecting:5000,final:5000")
    args = parser.parse_args(argv)
    script = None
    if args.script is not None:
        if args.hold_ms is not None:
            parser.error("--hold-ms applies to the fixed answer only; --script sets its own timing")
        if args.script == "":
            parser.error("--script is empty")
        script = Script(parse_script(parser, args.script))
    hold_ms = DEFAULT_HOLD_MS if args.hold_ms is None else args.hold_ms
    if hold_ms < 0:
        parser.error("--hold-ms must be 0 or more")
    server = ThreadingHTTPServer((HOST, PORT), make_handler(args.variant, hold_ms, script))
    server.daemon_threads = True
    mode = f"script={args.script}" if script else f"hold_ms={hold_ms}"
    print(f"phase-probe-server listening {HOST}:{PORT} variant={args.variant} {mode}", file=sys.stderr, flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
