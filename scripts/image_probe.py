#!/usr/bin/env python3
"""The relay image's live-mode probe (C12; Owed 58 and 68). Run by image.yml, stdlib only.

Starts the image in LIVE mode against a fake control plane on this host (host network), with a
session whose overlay is C11's harness page served from here and whose input and target are local
ports nothing listens on, so nothing leaves the host. Once CEF's children exist, it reads, from
inside the container:

  - `/proc/<pid>/limits` of the compositor and of every CEF child: `Max core file size 0 0`
    (Owed 68: a CEF abort writes a 93-117 MB core holding the page token);
  - TMPDIR: `/dev/shm/seazn`, mode 0700, owned by the relay user, on a tmpfs, and the CEF profile
    inside it (Owed 58);
  - the image's and the compositor's environment: no GST_CEF_LOG_SEVERITY, no GST_DEBUG (Owed 52).

Then three stops, one container each:

  - `docker kill -s INT` (R1's stop signal), through tini and the entrypoint: the Final hold, then the
    drain; exit 0 inside R1's stop grace, an earlier POST carrying `programmePhase: final`, the last
    POST `signalled`;
  - `docker stop` (SIGTERM, the platform stop): the drain at once; exit 0, the last POST `signalled`,
    no POST carrying `final`;
  - a measurement, not a check (F-015): SIGTERM sent straight to the compositor once CEF is up,
    around the entrypoint. CEF's own handlers take SIGINT and SIGTERM in the process, which is why
    the entrypoint forwards them as SIGUSR1 and SIGUSR2.

No run's log may carry a byte of either token.

Usage: image_probe.py <image> [--keep-log <path>]
"""

import argparse
import json
import os
import re
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "contracts/fixtures/relay-job.v1/session-composed-720p.json"
PAGE = ROOT / "harness/pages/overlay-test.html"
TOKEN = "probe-job-token-not-a-secret-7f3a9c"
PAGE_TOKEN = "probe-page-token-not-a-secret-2b8d"
NAME = "relay-probe"
CORE_LINE = "Max core file size"

posts = []
fetches = []
ROOT_LIB = ROOT / "crates/compositor/src/lib.rs"


def session(port):
    s = json.loads(FIXTURE.read_text())
    sid = s["sessionId"]
    s["pageToken"] = PAGE_TOKEN
    s["overlayUrl"] = (
        f"http://127.0.0.1:{port}/overlay-test.html?sid={sid}&st={PAGE_TOKEN}&host=compositor"
        f"&delay={s['overlayDelayMs']}"
    )
    # Ports nothing listens on: the input waits, the output link retries; nothing leaves the host.
    s["input"]["url"] = "srt://127.0.0.1:9"
    s["target"]["url"] = "rtmp://127.0.0.1:9/live/"
    return s


class Handler(BaseHTTPRequestHandler):
    sess = None

    def log_message(self, *_):
        pass

    def reply(self, code, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def authed(self):
        return self.headers.get("Authorization") == f"Bearer {TOKEN}"

    def do_GET(self):
        base = f"/api/internal/relay/sessions/{self.sess['sessionId']}"
        if self.path.split("?")[0] == "/overlay-test.html":
            return self.reply(200, PAGE.read_bytes(), "text/html; charset=utf-8")
        if self.path == base and self.authed():
            fetches.append(time.time())
            return self.reply(200, {"ok": True, "data": self.sess})
        return self.reply(404, {"ok": False, "code": "NOT_FOUND"})

    def do_POST(self):
        base = f"/api/internal/relay/sessions/{self.sess['sessionId']}"
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n)
        if self.path == f"{base}/heartbeat" and self.authed():
            try:
                posts.append(json.loads(body))
            except ValueError:
                posts.append({"unparsed": body.decode("utf-8", "replace")})
            return self.reply(200, {"ok": True, "data": {"desiredState": "live"}})
        return self.reply(404, {"ok": False, "code": "NOT_FOUND"})


def sh(*args, check=True, timeout=120):
    r = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if check and r.returncode != 0:
        raise SystemExit(f"{' '.join(args)} exited {r.returncode}: {r.stderr.strip()}")
    return r


def in_container(script):
    return sh("docker", "exec", NAME, "sh", "-c", script).stdout


def procs():
    """(pid, comm, core soft, core hard) of every process in the container."""
    out = in_container(
        'for d in /proc/[0-9]*; do p=${d#/proc/}; c=$(cat $d/comm 2>/dev/null) || continue; '
        f'l=$(grep "^{CORE_LINE}" $d/limits 2>/dev/null) || continue; echo "$p|$c|$l"; done'
    )
    rows = []
    for line in out.splitlines():
        pid, comm, limits = line.split("|", 2)
        soft, hard = limits[len(CORE_LINE):].split()[:2]
        rows.append((int(pid), comm, soft, hard))
    return rows


def grace_ms():
    """R1's stop grace, read from its one authority in this repo (`R1_STOP_GRACE_MS`)."""
    m = re.search(r"pub const R1_STOP_GRACE_MS: u64 = ([0-9_]+);", ROOT_LIB.read_text())
    if not m:
        raise SystemExit("R1_STOP_GRACE_MS not found in crates/compositor/src/lib.rs")
    return int(m.group(1).replace("_", ""))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("image")
    ap.add_argument("--keep-log")
    a = ap.parse_args()
    failures = []
    kept = []

    def check(ok, what):
        print(("ok   " if ok else "FAIL ") + what)
        if not ok:
            failures.append(what)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    Handler.sess = session(port)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    grace = grace_ms() / 1000

    def start():
        posts.clear()
        fetches.clear()
        deadline = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(time.time() + 600))
        sh("docker", "rm", "-f", NAME, check=False)
        sh(
            "docker", "run", "-d", "--name", NAME, "--network", "host",
            "-e", f"SESSION_ID={Handler.sess['sessionId']}", "-e", f"JOB_TOKEN={TOKEN}",
            "-e", f"APP_URL=http://127.0.0.1:{port}", "-e", f"RELAY_DEADLINE_AT={deadline}",
            a.image,
        )
        until = time.time() + 90
        while time.time() < until:
            if sh("docker", "inspect", "-f", "{{.State.Running}}", NAME).stdout.strip() != "true":
                break
            if "msg=cef ready gen=1" in sh("docker", "logs", NAME).stderr:
                break
            time.sleep(0.5)
        logs = sh("docker", "logs", NAME).stderr
        check("msg=programme started" in logs, "programme started (live)")
        check("msg=cef ready gen=1" in logs, "the harness page readied (cef ready gen=1)")

    def wait_exit(limit):
        t0 = time.time()
        while time.time() - t0 < limit:
            if sh("docker", "inspect", "-f", "{{.State.Running}}", NAME).stdout.strip() != "true":
                break
            time.sleep(0.1)
        took = time.time() - t0
        code = sh("docker", "inspect", "-f", "{{.State.ExitCode}}", NAME).stdout.strip()
        logs = sh("docker", "logs", NAME).stderr
        kept.append(logs)
        sh("docker", "rm", "-f", NAME, check=False)
        return took, code, logs

    try:
        # ---- run 1: the process facts, then R1's SIGINT ----
        start()
        rows = procs()
        print("processes (pid, comm, core soft, core hard):")
        for r in rows:
            print(f"  {r}")
        comp = [r for r in rows if r[1] == "compositor"]
        cef = [r for r in rows if r[1].startswith("gstcef")]
        check(len(comp) == 1, f"one compositor process ({len(comp)})")
        check(len(cef) >= 1, f"CEF children exist ({len(cef)})")
        checked = 0
        for r in comp + cef:
            check(r[2:] == ("0", "0"), f"{r[1]} pid {r[0]}: core limit {r[2]} {r[3]}, want 0 0")
            checked += 1
        check(checked >= 2, f"core limits checked: {checked}")
        pid = comp[0][0] if comp else 0
        # CEF may mark the process non-dumpable, which hides its environ; the profile-under-TMPDIR
        # check below then stands for TMPDIR, and the image env check for the variables.
        r = sh("docker", "exec", NAME, "sh", "-c", f"tr '\\0' '\\n' < /proc/{pid}/environ", check=False)
        if pid and r.returncode == 0:
            env = r.stdout.splitlines()
            check("TMPDIR=/dev/shm/seazn" in env, "the compositor's TMPDIR is /dev/shm/seazn")
            for bad in ("GST_CEF_LOG_SEVERITY", "GST_DEBUG"):
                check(not any(l.startswith(bad + "=") for l in env), f"no {bad} in its environment")
        else:
            print(f"note: the compositor's environ is unreadable ({r.stderr.strip()!r})")
        st = in_container("stat -c '%a %U' /dev/shm/seazn; stat -f -c %T /dev/shm/seazn; ls -A /dev/shm/seazn").split("\n")
        check(st[0] == "700 relay", f"TMPDIR mode and owner: {st[0]!r}")
        check(st[1] == "tmpfs", f"TMPDIR filesystem: {st[1]!r}")
        check(any(x.strip() for x in st[2:]), f"the CEF profile is under TMPDIR: {st[2:]}")
        image_env = sh("docker", "inspect", "-f", "{{json .Config.Env}}", a.image).stdout
        for bad in ("GST_CEF_LOG_SEVERITY", "GST_DEBUG"):
            check(f'"{bad}=' not in image_env, f"the image sets no {bad}")
        print(f"compositor signal masks: {in_container(f'grep -E ^Sig /proc/{pid}/status').split()}")

        sh("docker", "kill", "-s", "INT", NAME)
        took, code, logs = wait_exit(grace + 10)
        print(f"SIGINT to exit: {took:.1f} s, exit {code} (R1's grace {grace:.0f} s)")
        check(code == "0", f"SIGINT: exit status 0 (got {code})")
        check(took < grace, f"SIGINT: exit inside R1's stop grace ({took:.1f} s)")
        check("msg=exit code=0" in logs, "SIGINT: the compositor's own exit line")
        check(any(p.get("programmePhase") == "final" for p in posts[:-1]), "SIGINT: an earlier POST carries final")
        last = posts[-1] if posts else {}
        check(last.get("exitReason") == "signalled", f"SIGINT: last POST signalled ({last.get('exitReason')})")
        check(len(fetches) == 1, f"one session fetch ({len(fetches)})")

        # ---- run 2: the platform's SIGTERM ----
        start()
        sh("docker", "stop", "-t", str(int(grace)), NAME, timeout=60)
        took, code, logs = wait_exit(5)
        print(f"docker stop to exit: {took:.1f} s after stop returned, exit {code}")
        check(code == "0", f"SIGTERM: exit status 0 (got {code})")
        check("msg=exit code=0" in logs, "SIGTERM: the compositor's own exit line")
        check(all(p.get("programmePhase") != "final" for p in posts), "SIGTERM: no POST carries final")
        last = posts[-1] if posts else {}
        check(last.get("exitReason") == "signalled", f"SIGTERM: last POST signalled ({last.get('exitReason')})")

        # ---- run 3 (F-015, a measurement): SIGTERM straight to the compositor, around tini ----
        start()
        pid = [r for r in procs() if r[1] == "compositor"]
        if pid:
            sh("docker", "exec", NAME, "kill", "-TERM", str(pid[0][0]))
            took, code, logs = wait_exit(grace + 10)
            print(f"measured (F-015): a direct SIGTERM once CEF is up: exit {code} after {took:.1f} s; "
                  f"the compositor's exit line {'present' if 'msg=exit code=' in logs else 'ABSENT'}; "
                  f"posts {len(posts)}, last exitReason {(posts[-1] if posts else {}).get('exitReason')}")
        for logs in kept:
            check(TOKEN not in logs and PAGE_TOKEN not in logs, "no token in a container log")
        if a.keep_log:
            Path(a.keep_log).write_text("\n==== next run ====\n".join(kept))
    finally:
        sh("docker", "rm", "-f", NAME, check=False)
        server.shutdown()
    if failures:
        print(f"{len(failures)} check(s) failed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
