#!/usr/bin/env python3
"""F-007 probe (THROWAWAY): what does the Machines API report while restart policy on-failure restarts a crash?

  fly_restart_probe.py create <name> <exit_code> <sleep_s> <max_retries>    -> prints MACHINE <id>
  fly_restart_probe.py poll   <id> <seconds> <interval_s> <out.jsonl> [--stop-after-stopped S]
  fly_restart_probe.py start  <id>
  fly_restart_probe.py destroy <id>

The API token comes from `fly auth token` captured in-process (never argv, never printed). Each poll line
records local wall ms, HTTP status, machine.state and any NEW events[] entries (type, status, source,
timestamp, request.exit_event). Only state transitions and new events are echoed to stdout.
"""
import json, subprocess, sys, time, urllib.request, urllib.error

APP = "seazn-relay-spike"
BASE = f"https://api.machines.dev/v1/apps/{APP}"


def token():
    r = subprocess.run(["fly", "auth", "token"], capture_output=True, text=True, check=True)
    t = r.stdout.strip().splitlines()[-1].strip()
    assert t and " " not in t, "no token"
    return t


TOK = token()


def call(method, path, body=None, _retry=True):
    global TOK
    req = urllib.request.Request(BASE + path, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Authorization": f"Bearer {TOK}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        if e.code in (401, 403) and _retry:   # `fly auth token` tokens expire after ~5 min (403 seen at 4.7 min)
            TOK = token()
            return call(method, path, body, _retry=False)
        return e.code, {"error": e.read()[:300].decode(errors="replace")}
    except Exception as e:  # noqa: BLE001
        return 0, {"error": type(e).__name__}


def ms():
    return time.time_ns() // 1_000_000


def ev_key(e):
    return (e.get("id"), e.get("timestamp"), e.get("type"), e.get("status"))


def ev_view(e):
    req = e.get("request") or {}
    return {"type": e.get("type"), "status": e.get("status"), "source": e.get("source"), "ts": e.get("timestamp"),
            "exit_event": req.get("exit_event"), "restart_count": req.get("restart_count")}


cmd = sys.argv[1]
if cmd == "create":
    name, code, sl, mr = sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
    st, b = call("POST", "/machines", {"name": name, "region": "lhr", "config": {
        "image": "alpine:3.20",
        "init": {"exec": ["/bin/sh", "-c", f"echo PROC_START; sleep {sl}; echo PROC_EXIT {code}; exit {code}"]},
        "guest": {"cpu_kind": "shared", "cpus": 1, "memory_mb": 256},
        "restart": {"policy": "on-failure", "max_retries": mr},
        "auto_destroy": False}})
    print("CREATE", st, json.dumps({"id": b.get("id"), "state": b.get("state"), "restart": (b.get("config") or {}).get("restart"),
                                     "error": b.get("error")}), flush=True)
    if b.get("id"):
        print("MACHINE", b["id"], flush=True)
elif cmd == "poll":
    mid, secs, iv, out = sys.argv[2], float(sys.argv[3]), float(sys.argv[4]), sys.argv[5]
    stop_after = float(sys.argv[sys.argv.index("--stop-after-stopped") + 1]) if "--stop-after-stopped" in sys.argv else None
    seen, last_state, t0, stopped_since, n, n429 = set(), None, time.time(), None, 0, 0
    with open(out, "a") as f:
        while time.time() - t0 < secs:
            t = ms(); st, b = call("GET", f"/machines/{mid}"); n += 1
            if st == 429:
                n429 += 1
            state = b.get("state") if isinstance(b, dict) else None
            new = [e for e in (b.get("events") or [] if isinstance(b, dict) else []) if ev_key(e) not in seen]
            for e in new:
                seen.add(ev_key(e))
            rec = {"t": t, "http": st, "state": state, "new_events": [ev_view(e) for e in sorted(new, key=lambda e: e.get("timestamp") or 0)]}
            f.write(json.dumps(rec) + "\n")
            f.flush()
            if state != last_state or rec["new_events"]:
                print("POLL", json.dumps(rec), flush=True)
                last_state = state
            if state in ("stopped", "failed"):
                stopped_since = stopped_since or time.time()
                if stop_after and time.time() - stopped_since > stop_after:
                    break
            elif state is not None:
                stopped_since = None
            time.sleep(max(0.0, iv - (ms() - t) / 1000))
    print("POLLDONE", json.dumps({"requests": n, "http_429": n429, "last_state": last_state}), flush=True)
elif cmd == "start":
    st, b = call("POST", f"/machines/{sys.argv[2]}/start")
    print("START", st, json.dumps(b)[:200], ms(), flush=True)
elif cmd == "destroy":
    st, b = call("DELETE", f"/machines/{sys.argv[2]}?force=true")
    print("DESTROY", st, json.dumps(b)[:200], flush=True)
