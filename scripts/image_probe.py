#!/usr/bin/env python3
"""The relay image's live-mode probe (C12; Owed 58 and 68, F-016, F-017). Run by image.yml, stdlib only.

Starts the image in LIVE mode against a fake control plane on this host (host network), with a
session whose overlay is C11's harness page served from here and whose input and target are local
ports nothing listens on, so nothing leaves the host. Every session credential is a probe value.

Phase A, one container, once CEF's children exist:

  - `/proc/<pid>/limits` of tini, the entrypoint's bash, Xvfb, the compositor and every CEF child:
    `Max core file size` is 1 byte soft and hard (the kernel's refusal value for a pipe) (Owed 68, the PR #36 review's I1 and m2);
    the compositor is non-dumpable (its `/proc/<pid>/status` belongs to root, while its parent's, the
    waiter's, a dumpable bash, belongs to the image user; the image user cannot read the compositor's
    environment, and can read the waiter's); `core_pattern` is printed;
  - Xvfb's environment, read with CAP_SYS_PTRACE (`docker exec --privileged`): SESSION_ID in it (the
    control: a real environment was read) and no JOB_TOKEN (the review's m2);
  - no session credential or token in any process's `/proc/<pid>/cmdline` (the review's m3);
  - TMPDIR: `/dev/shm/seazn`, mode 0700, owned by the relay user, on a tmpfs, with the CEF profile
    inside (Owed 58); no GST_CEF_LOG_SEVERITY or GST_DEBUG in the image or the compositor (Owed 52);
  - a forced abort of a CEF child (SIGABRT): the kernel refuses its core (for a piped core_pattern,
    the kernel log's "RLIMIT_CORE … set to 1" line names its pid); then of the compositor itself:
    exit 134, the entrypoint's one `level=error msg=the compositor was killed by SIGABRT (status 134)`
    line (the waiter's `wait` prints no job report of its own since F-019), and no `core dumped`.

The CEF children's environment, at exec (the PR #36 final review's I-1), in one dedicated container:
Chromium rewrites a child's `/proc/<pid>/environ` for its process title (F-020), so that file
witnesses nothing. `scripts/cef_env_witness.sh`, bind-mounted (never in the image) and named by
gstcefsrc's GST_CEF_SUBPROCESS_PATH, records for each child CEF execs whether JOB_TOKEN and three
controls are set, then execs the real gstcefsubprocess. Checked: it ran (N > 0), every live CEF
process came through it (itself or its zygote), each control is set N of N, JOB_TOKEN unset N of N.

Phase B, `--exits` containers (32 by default; F-017, the review's C1): a live exit with CEF up, in
cycles of 8 alternating 720p and 1080p (the review's m7): 4 `docker stop` (SIGTERM through tini: the
drain), 2 `docker kill -s INT` (R1's stop: the Final hold, inside R1's grace), the session's deadline
(the hold) and a killed CEF zygote (C11b's in-place restart, 75): 24 stops in all. Every exit status is exact, with its own `exit
code=` line, the right last POST, and no abort text (a double free after the exit line was status
134 once in six such exits before `hard_exit::now`). Of the zygote kills, the 2nd and 3rd of each 4
come at once after the ready (the window each ready's listing closed) and the others after two
child polls (the restart alone). `/dev/shm`'s peak use is printed.

Then a SIGTERM and a SIGINT 0.3 s into a container's boot (the review's m1): held by the entrypoint
until the compositor's handlers exist, then the compositor's own exit 0.

Phase C, a measurement, not a check (F-016): SIGTERM straight to the compositor once CEF is up.

No log may carry a byte of any credential.

Usage: image_probe.py <image> [--keep-log <path>] [--exits N]
"""

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "contracts/fixtures/relay-job.v1"
P720 = FIXTURES / "session-composed-720p.json"
P1080 = FIXTURES / "session-composed-1080p.json"
PAGE = ROOT / "harness/pages/overlay-test.html"
TOKEN = "probe-job-token-not-a-secret-7f3a9c"
PAGE_TOKEN = "probe-page-token-not-a-secret-2b8d"
STREAM_KEY = "probe-stream-key-not-a-secret-5e1b"
STREAM_ID = "probe-stream-id-not-a-secret-9c4d"
PASSPHRASE = "probe-passphrase-not-a-secret-3a7f"
SECRETS = (TOKEN, PAGE_TOKEN, STREAM_KEY, STREAM_ID, PASSPHRASE)
# What a process that died abnormally leaves in the container's log (glibc, bash's job report, and
# `supervise`'s own report of the compositor's signal death, which the waiter's `wait` does not print).
KILLED = "the compositor was killed by"
ABORT = re.compile(r"double free|corruption|Aborted|core dumped|Segmentation fault|malloc\(\)|free\(\)|"
                   r"stack smashing|Illegal instruction|Trace/breakpoint trap|Killed|" + KILLED)
# `supervise.sh`'s report of the compositor's SIGABRT, from Python's signal table, never the script.
ABRT_LINE = f"level=error msg={KILLED} {signal.SIGABRT.name} (status {128 + signal.SIGABRT})"
NAME = "relay-probe"
CORE_LINE = "Max core file size"
# The exec-time witness (test only): where the one witness container mounts it, the directory its
# records go to (read from the script, its one authority) and the controls it must see set.
WITNESS = ROOT / "scripts/cef_env_witness.sh"
WITNESS_AT = "/probe/cef-env-witness.sh"
WITNESS_DIR = re.search(r"^dir=(/\S+)$", WITNESS.read_text(), re.M).group(1)
CONTROLS = ("DISPLAY", "SESSION_ID", "CEF_SUBPROCESS_PATH")

posts = []
fetches = []
ROOT_LIB = ROOT / "crates/compositor/src/lib.rs"
# The CEF adapter: `CHILD_POLL_MS`, the child poll's period, is read from it.
CEF_LIB = ROOT / "crates/adapters/src/cef_graphics.rs"


def session(port, fixture):
    s = json.loads(fixture.read_text())
    sid = s["sessionId"]
    s["pageToken"] = PAGE_TOKEN
    s["target"]["streamKey"] = STREAM_KEY
    s["input"]["streamId"] = STREAM_ID
    s["input"]["passphrase"] = PASSPHRASE
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


def in_container(script, root=False):
    return sh("docker", "exec", *(["-u", "0"] if root else []), NAME, "sh", "-c", script).stdout


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


def cmdlines():
    """{pid: (ppid, argv joined by spaces)} of every process in the container, read as root."""
    out = in_container(
        'for d in /proc/[0-9]*; do p=${d#/proc/}; '
        'pp=$(sed -n "s/^PPid:[[:space:]]*//p" $d/status 2>/dev/null) || continue; '
        'a=$(tr "\\0" " " < $d/cmdline 2>/dev/null) || continue; echo "$p|$pp|$a"; done',
        root=True,
    )
    rows = {}
    for line in out.splitlines():
        pid, ppid, argv = line.split("|", 2)
        rows[int(pid)] = (int(ppid or 0), argv)
    return rows


def lib_const(name, path=ROOT_LIB):
    """A `u64` constant from its one authority in this repo, `path` (by default
    `crates/compositor/src/lib.rs`)."""
    m = re.search(rf"pub const {name}: u64 = ([0-9_]+);", path.read_text())
    if not m:
        raise SystemExit(f"{name} not found in {path.relative_to(ROOT)}")
    return int(m.group(1).replace("_", ""))


def exec_witness(table, tree, raw):
    """The verdicts on `cef_env_witness.sh`'s records, as `[(ok, what)]`. `table` is `procs()`, `tree`
    `cmdlines()` and `raw` the records' text, read after both (a process that shows as gstcef* has
    exec'd, so its record was written first)."""
    recs = {}
    for line in raw.splitlines():
        f = dict(kv.split("=", 1) for kv in line.split() if "=" in kv)
        if f.get("pid", "").isdigit():
            recs[int(f["pid"])] = f
    n = len(recs)
    print("the witness's records (pid, type):", sorted((p, f.get("type")) for p, f in recs.items()))
    out = [(n > 0, f"the witness ran for each CEF exec: {n} record(s)")]
    # Every live CEF process came through the witness: an exec'd child of the compositor, or a fork of
    # one (a zygote's renderer or GPU process), whose exec ancestor holds a record.
    comp = next((r[0] for r in table if r[1] == "compositor"), 0)
    live = [r[0] for r in table if r[1].startswith("gstcef") and r[0] in tree]
    covered = 0
    for p in live:
        q, hops = p, 0
        while q in tree and tree[q][0] != comp and hops < 8:
            q, hops = tree[q][0], hops + 1
        covered += 1 if comp and q in recs and tree.get(q, (0, ""))[0] == comp else 0
    out.append((len(live) >= 1 and covered == len(live),
                f"every live CEF process came through the witness (itself or its zygote): {covered} of {len(live)}"))
    for k in CONTROLS:
        got = sum(1 for f in recs.values() if f.get(k) == "set")
        out.append((n > 0 and got == n, f"the control {k} is set at each CEF child's exec: {got} of {n}"))
    clean = sum(1 for f in recs.values() if f.get("JOB_TOKEN") == "unset")
    out.append((n > 0 and clean == n, f"no JOB_TOKEN at any CEF child's exec: {clean} of {n}"))
    out.append((not any(x in raw for x in SECRETS), "the witness's records carry no credential"))
    return out


def kernel_log():
    r = sh("sudo", "-n", "dmesg", check=False)
    return r.stdout if r.returncode == 0 else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("image")
    ap.add_argument("--keep-log")
    ap.add_argument("--exits", type=int, default=32)
    a = ap.parse_args()
    failures = []
    kept = []
    shm_peak = [0, 0]  # used KiB, size KiB

    def check(ok, what):
        print(("ok   " if ok else "FAIL ") + what)
        if not ok:
            failures.append(what)
        return ok

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    grace = lib_const("R1_STOP_GRACE_MS") / 1000
    child_poll = lib_const("CHILD_POLL_MS", CEF_LIB) / 1000
    # The kernel's value, not ours: at exactly 1 byte it refuses a core to a piped core_pattern
    # (fs/coredump.c), and 1 is below the ELF minimum for a file one. Never read from the code under test.
    limit = "1"

    def sample_shm():
        r = sh("docker", "exec", NAME, "df", "-k", "/dev/shm", check=False)
        rows = r.stdout.splitlines()
        if r.returncode == 0 and len(rows) >= 2:
            size, used = (int(x) for x in rows[-1].split()[1:3])
            shm_peak[0] = max(shm_peak[0], used)
            shm_peak[1] = size

    def start(fixture, deadline_in=600, extra=()):
        posts.clear()
        fetches.clear()
        Handler.sess = session(port, fixture)
        deadline = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(time.time() + deadline_in))
        sh("docker", "rm", "-f", NAME, check=False)
        sh(
            "docker", "run", "-d", "--name", NAME, "--network", "host", *extra,
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
        ready = "msg=cef ready gen=1" in logs
        check("msg=programme started" in logs, f"programme started ({fixture.name})")
        check(ready, f"the harness page readied (cef ready gen=1, {fixture.name})")
        sample_shm()
        return ready

    def running():
        return sh("docker", "inspect", "-f", "{{.State.Running}}", NAME).stdout.strip() == "true"

    def wait_exit(limit_s):
        """(seconds waited, the exit status, the logs). The status reads `running` when the container
        outlived `limit_s`: `docker inspect`'s ExitCode of a running container is 0, which would pass
        for a clean exit (image run 40015873298)."""
        t0 = time.time()
        while time.time() - t0 < limit_s:
            if not running():
                break
            sample_shm()
            time.sleep(0.2)
        took = time.time() - t0
        code = "running" if running() else sh("docker", "inspect", "-f", "{{.State.ExitCode}}", NAME).stdout.strip()
        logs = sh("docker", "logs", NAME).stderr
        kept.append(logs)
        sh("docker", "rm", "-f", NAME, check=False)
        return took, code, logs

    def compositor_pid():
        comp = [r for r in procs() if r[1] == "compositor"]
        return comp[0][0] if len(comp) == 1 else 0

    try:
        # ---- phase A: the process facts, then forced aborts ----
        start(P720)
        rows = procs()
        print("processes (pid, comm, core soft, core hard):")
        for r in rows:
            print(f"  {r}")
        comp = [r for r in rows if r[1] == "compositor"]
        cef = [r for r in rows if r[1].startswith("gstcef")]
        check(len(comp) == 1, f"one compositor process ({len(comp)})")
        check(len(cef) >= 1, f"CEF children exist ({len(cef)})")
        named = {"tini", "bash", "Xvfb"}
        held = [r for r in rows if r[1] in named or r in comp or r in cef]
        check({r[1] for r in held} >= named, f"tini, bash and Xvfb found: {sorted({r[1] for r in held})}")
        for r in held:
            check(r[2:] == (limit, limit), f"{r[1]} pid {r[0]}: core limit {r[2]} {r[3]}, want {limit} {limit}")
        check(len(held) >= 5, f"core limits checked: {len(held)}")
        pid = comp[0][0] if comp else 0
        argv = cmdlines()
        # Non-dumpable: the kernel hands a non-dumpable process's /proc files to root (`task_dump_owner`;
        # the /proc/<pid> directory itself keeps the process's euid either way, so it witnesses
        # nothing). The control is the compositor's parent, `supervise`'s waiter subshell, found by ppid
        # (the first bash in /proc's glob order is the waiter or the entrypoint, by the pids' digits:
        # "20" sorts before "7"): a dumpable bash, so its files stay the image user's (the PR #36 final
        # review's m6).
        me = in_container("id -u").strip()
        waiter = argv.get(pid, (0, ""))[0] if pid else 0
        waiter_comm = next((r_[1] for r_ in rows if r_[0] == waiter), "")
        check(waiter > 1 and waiter_comm == "bash",
              f"the compositor's parent is the waiter, a bash (pid {waiter}, {waiter_comm!r})")
        control = in_container(f"stat -c %u /proc/{waiter}/status").strip() if waiter > 1 else ""
        check(me not in ("", "0") and control == me,
              f"the control: the waiter (pid {waiter}) owns its /proc files (uid {control!r}, the image user {me!r})")
        owner = in_container(f"stat -c %u /proc/{pid}/status").strip() if pid else ""
        check(owner == "0", f"the compositor is non-dumpable (/proc/{pid}/status owned by uid {owner!r})")

        def environ(pid_, privileged=True):
            """`(readable, lines)` of /proc/<pid>/environ. Reading another process's environment needs
            ptrace access: the same uid and a dumpable target, or CAP_SYS_PTRACE, which Docker's default
            set lacks even for root; `--privileged` grants it to this probe's exec only. An unreadable
            environment is reported as such, never as a clean one."""
            r_ = sh("docker", "exec", *(["--privileged", "-u", "0"] if privileged else []), NAME,
                    "sh", "-c", f"tr '\\0' '\\n' < /proc/{pid_}/environ", check=False)
            lines_ = r_.stdout.splitlines()
            return r_.returncode == 0 and len(lines_) > 0, lines_

        # The image user cannot read the non-dumpable compositor's environment (it holds JOB_TOKEN from
        # the exec), and can read the dumpable waiter's: the pair witnesses PR_SET_DUMPABLE from outside.
        mine, _ = environ(pid, privileged=False) if pid else (True, [])
        check(not mine, "the image user cannot read the compositor's environment (non-dumpable)")
        waiter_env, _ = environ(waiter, privileged=False) if waiter > 1 else (False, [])
        check(waiter_env, "the image user reads the dumpable waiter's environment (the control)")
        pattern = Path("/proc/sys/kernel/core_pattern").read_text().strip()
        print(f"core_pattern (the host's, shared with the container): {pattern!r}")
        leaked = [(p_, a_) for p_, (_, a_) in argv.items() if any(x in a_ for x in SECRETS)]
        check(len(argv) >= 5 and not leaked, f"no credential in any of {len(argv)} command lines: {leaked}")
        readable, env = environ(pid) if pid else (False, [])
        check(readable, f"the compositor's environment is readable with CAP_SYS_PTRACE ({len(env)} lines)")
        check("TMPDIR=/dev/shm/seazn" in env, "the compositor's TMPDIR is /dev/shm/seazn")
        for bad in ("GST_CEF_LOG_SEVERITY", "GST_DEBUG", "TINI_KILL_PROCESS_GROUP"):
            check(readable and not any(l.startswith(bad + "=") for l in env), f"no {bad} in its environment")
        st = in_container("stat -c '%a %U' /dev/shm/seazn; stat -f -c %T /dev/shm/seazn; ls -A /dev/shm/seazn").split("\n")
        check(st[0] == "700 relay", f"TMPDIR mode and owner: {st[0]!r}")
        check(st[1] == "tmpfs", f"TMPDIR filesystem: {st[1]!r}")
        check(any(x.strip() for x in st[2:]), f"the CEF profile is under TMPDIR: {st[2:]}")
        # Xvfb starts without the job token (`env -u`, the PR #36 review's m2). Its environment file is
        # its own (no process title rewrites it), and SESSION_ID, passed to the container beside
        # JOB_TOKEN, is the control: present, so the read saw a real environment, not padding. A CEF
        # child's file is rewritten by Chromium (F-020), so the children are witnessed at exec instead
        # (below, the PR #36 final review's I-1).
        xvfb = [r_ for r_ in rows if r_[1] == "Xvfb"]
        check(len(xvfb) == 1, f"one Xvfb ({len(xvfb)})")
        for r_ in xvfb:
            readable, e_ = environ(r_[0])
            seen = readable and any(l.startswith("SESSION_ID=") for l in e_)
            check(seen, f"Xvfb pid {r_[0]}: its environment is read, the control SESSION_ID in it ({len(e_)} lines)")
            check(seen and not any(TOKEN in l or l.startswith("JOB_TOKEN=") for l in e_),
                  f"Xvfb pid {r_[0]}: no JOB_TOKEN in its environment")
        image_env = sh("docker", "inspect", "-f", "{{json .Config.Env}}", a.image).stdout
        for bad in ("GST_CEF_LOG_SEVERITY", "GST_DEBUG", "TINI_KILL_PROCESS_GROUP"):
            check(f'"{bad}=' not in image_env, f"the image sets no {bad}")
        print(f"compositor signal masks: {in_container(f'grep -E ^Sig /proc/{pid}/status').split()}")

        # A CEF child's abort: its core is refused by the kernel, which says so for a piped pattern.
        # Counted by comm, not pid: the kernel may print the pid of another pid namespace.
        refused = re.compile(r"\(([^)]*)\).*RLIMIT_CORE (is )?set to 1")

        def refusals(comm_prefix):
            klog = kernel_log()
            if klog is None:
                return None
            return sum(1 for m in refused.finditer(klog) if m.group(1).startswith(comm_prefix))

        child = next((p_ for p_, (pp, a_) in argv.items() if pp == pid and "--type=gpu-process" in a_), 0) or (
            cef[0][0] if cef else 0)
        if check(child > 0, f"a CEF child to abort ({child})"):
            before = refusals("gstcef")
            sh("docker", "exec", NAME, "kill", "-ABRT", str(child), check=False)
            time.sleep(2)
            after = refusals("gstcef")
            if pattern.startswith("|"):
                if check(before is not None and after is not None, "the kernel log is readable (sudo dmesg)"):
                    check(after > before, f"the kernel refused CEF child {child}'s core: refusals {before} -> {after}")
            else:
                cores = in_container("ls -1 /home/relay /tmp 2>/dev/null | grep -c '^core' || true", root=True).strip()
                check(cores == "0", f"no core file from CEF child {child} (file core_pattern): {cores}")
        # The compositor's own abort: exit 134, and no core of it (it is non-dumpable, so the kernel
        # refuses it silently; a refusal line for it would mean the 1-byte limit stopped it instead).
        if pid:
            before = refusals("compositor")
            sh("docker", "exec", NAME, "kill", "-ABRT", str(pid), check=False)
            took, code, logs = wait_exit(15)
            check(code == "134", f"the compositor's abort exits 134 (got {code})")
            check(logs.count(ABRT_LINE) == 1 and logs.count(KILLED) == 1,
                  f"the entrypoint reports the abort once: {ABRT_LINE!r} (seen {logs.count(KILLED)})")
            check("core dumped" not in logs, "no core of the compositor")
            print(f"kernel refusal lines for the compositor: {before} -> {refusals('compositor')}")

        # ---- the CEF children's environment, witnessed at exec (the PR #36 final review's I-1) ----
        # One dedicated container: gstcefsrc execs every CEF child through the bind-mounted witness,
        # which records whether JOB_TOKEN and each control is set, then execs the real subprocess.
        start(P720, extra=("-v", f"{WITNESS}:{WITNESS_AT}:ro", "-e", f"GST_CEF_SUBPROCESS_PATH={WITNESS_AT}"))
        table = procs()
        tree = cmdlines()
        # Read last: a process that shows as gstcef* has exec'd, so its record was written before.
        raw = in_container(f"cat {WITNESS_DIR}/* 2>/dev/null || true")
        for ok, what in exec_witness(table, tree, raw):
            check(ok, what)
        kept.append(sh("docker", "logs", NAME).stderr)
        sh("docker", "rm", "-f", NAME, check=False)

        # ---- phase B: live exits with CEF up, each kind at both profiles ----
        term = ("SIGTERM (docker stop)", "0", "signalled", False)
        sigint = ("SIGINT (docker kill -s INT)", "0", "signalled", True)
        # A cycle of 8: 6 stops (Fly's 50c6643 probe: 6 of 12 stops aborted after the exit line), a
        # deadline (1 of 2 aborted there, and Fly restarted the ended session) and a dead zygote (75).
        kinds = [term, sigint, term, ("the deadline", "0", "deadline", True), term,
                 ("a killed CEF zygote", "75", "restart", False), term, sigint]
        hold = 10
        exits = stops = zygote_kills = 0
        for i in range(a.exits):
            kind, want, reason, final = kinds[i % len(kinds)]
            fixture = P1080 if (i // len(kinds)) % 2 else P720
            what = f"exit {i + 1} of {a.exits}, {kind}, {fixture.name}"
            deadline_in = 15 if kind == "the deadline" else 600
            t0 = time.time()
            if not start(fixture, deadline_in):
                sh("docker", "rm", "-f", NAME, check=False)
                check(False, f"{what}: CEF never came up")
                continue
            if kind.startswith("SIGTERM"):
                sh("docker", "stop", "-t", str(int(grace)), NAME, timeout=60)
                took, code, logs = wait_exit(5)
            elif kind.startswith("SIGINT"):
                sh("docker", "kill", "-s", "INT", NAME)
                took, code, logs = wait_exit(grace + 10)
                check(took < grace, f"{what}: inside R1's stop grace ({took:.1f} s)")
            elif kind == "the deadline":
                check(time.time() - t0 < deadline_in, f"{what}: CEF was up before the deadline")
                took, code, logs = wait_exit(deadline_in + hold + grace + 10)
            else:
                cpid = compositor_pid()
                zyg = [p_ for p_, (pp, a_) in cmdlines().items() if pp == cpid and "--type=zygote" in a_]
                check(len(zyg) >= 1, f"{what}: a zygote of the compositor ({zyg})")
                # Kills 2 and 3 of each 4 come at once after the ready, inside the window the watch was
                # blind in before each ready listed the zygotes (image run 40015873298: that run's fastest
                # ready-to-stop gaps, on other exits, were 0.100-0.157 s, so its kill most likely came
                # before the 2nd poll). Kills 1 and 4 wait out two child polls, so they prove the restart
                # alone. In 32 exits: 14 and 22 at once, 6 and 30 after the wait (both at both profiles).
                immediate = zygote_kills % 4 in (1, 2)
                zygote_kills += 1
                what += ", at once after the ready" if immediate else ", after two child polls"
                if not immediate:
                    time.sleep(2 * child_poll)
                for z in zyg[:1]:
                    pp, argv = cmdlines().get(z, (0, ""))
                    check(cpid > 0 and pp == cpid and "--type=zygote" in argv,
                          f"{what}: {z} is still a zygote of the compositor at the kill (parent {pp})")
                    kill = sh("docker", "exec", NAME, "kill", "-KILL", str(z), check=False)
                    check(kill.returncode == 0, f"{what}: kill -KILL {z} returned {kill.returncode}")
                took, code, logs = wait_exit(30)
            last = posts[-1] if posts else {}
            ok = check(code == want, f"{what}: exit status {code}, want {want} ({took:.1f} s)")
            ok &= check(f"msg=exit code={want}" in logs, f"{what}: the compositor's own exit line")
            bad = ABORT.findall(logs)
            ok &= check(not bad, f"{what}: no abort text ({bad})")
            ok &= check(last.get("exitReason") == reason, f"{what}: last POST {last.get('exitReason')}, want {reason}")
            ok &= check(any(p_.get("programmePhase") == "final" for p_ in posts[:-1]) == final,
                        f"{what}: an earlier POST carries final: {final}")
            exits += 1 if ok else 0
            stops += 1 if ok and kind.startswith("SIG") else 0
        check(exits == a.exits and a.exits > 0, f"clean live exits: {exits} of {a.exits}")
        check(stops >= 20 or a.exits < 32, f"clean live stops (SIGTERM and SIGINT): {stops}")
        print(f"/dev/shm peak use: {shm_peak[0]} KiB of {shm_peak[1]} KiB")

        # ---- a stop while the container boots (the PR #36 review's m1): held, then the compositor's ----
        for sig in ("TERM", "INT"):
            posts.clear()
            Handler.sess = session(port, P720)
            deadline = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(time.time() + 600))
            sh("docker", "rm", "-f", NAME, check=False)
            sh("docker", "run", "-d", "--name", NAME, "--network", "host",
               "-e", f"SESSION_ID={Handler.sess['sessionId']}", "-e", f"JOB_TOKEN={TOKEN}",
               "-e", f"APP_URL=http://127.0.0.1:{port}", "-e", f"RELAY_DEADLINE_AT={deadline}", a.image)
            time.sleep(0.3)
            sh("docker", "kill", "-s", sig, NAME, check=False)
            took, code, logs = wait_exit(grace + 10)
            check(code == "0", f"SIG{sig} 0.3 s into the boot: exit {code}, want 0 ({took:.1f} s)")
            check("msg=exit code=0" in logs, f"SIG{sig} 0.3 s into the boot: the compositor's own exit line")

        # ---- phase C (F-016, a measurement): SIGTERM straight to the compositor, around tini ----
        start(P720)
        cpid = compositor_pid()
        if cpid:
            sh("docker", "exec", NAME, "kill", "-TERM", str(cpid))
            took, code, logs = wait_exit(grace + 10)
            print(f"measured (F-016): a direct SIGTERM once CEF is up: exit {code} after {took:.1f} s; "
                  f"the compositor's exit line {'present' if 'msg=exit code=' in logs else 'ABSENT'}; "
                  f"posts {len(posts)}, last exitReason {(posts[-1] if posts else {}).get('exitReason')}")
        leaks = [x for x in SECRETS for logs in kept if x in logs]
        check(not leaks and len(kept) >= a.exits, f"no credential in {len(kept)} container logs ({len(set(leaks))} leaked)")
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
