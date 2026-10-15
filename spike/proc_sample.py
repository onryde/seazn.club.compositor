#!/usr/bin/env python3
"""Process-tree + machine sampler (C00 spike, THROWAWAY).

  proc_sample.py --root-pid PID --seconds N [--interval 1] [--tag T]

Every interval it reads, for every process in PID's tree (PID plus all descendants, which
includes every gstcefsubprocess child):
  utime+stime  -> cores used in the interval
  VmRSS, Pss   -> memory (RSS double-counts shared pages; PSS does not)
plus machine-wide busy cores from /proc/stat and MemTotal-MemAvailable.
Prints one line `SAMPLE {...}` per interval and `PROCSUMMARY {...}` at the end. Prints only
argv[0] basenames and Chromium `--type=` values, never full command lines.
"""
import argparse
import json
import os
import re
import statistics
import time

TCK = os.sysconf("SC_CLK_TCK")
NCPU = os.cpu_count()


def read(path):
    try:
        with open(path, "rb") as f:
            return f.read().decode("utf-8", "replace")
    except OSError:
        return None


def children_map():
    m = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        s = read(f"/proc/{d}/stat")
        if not s:
            continue
        rest = s[s.rfind(")") + 2:].split()
        m.setdefault(int(rest[1]), []).append(int(d))
    return m


def tree(root):
    m = children_map()
    out, stack = [], [root]
    while stack:
        p = stack.pop()
        out.append(p)
        stack.extend(m.get(p, []))
    return out


def proc_info(pid):
    s = read(f"/proc/{pid}/stat")
    if not s:
        return None
    rest = s[s.rfind(")") + 2:].split()
    ticks = int(rest[11]) + int(rest[12])
    rss = pss = 0
    st = read(f"/proc/{pid}/status") or ""
    for line in st.splitlines():
        if line.startswith("VmRSS:"):
            rss = int(line.split()[1])
    sr = read(f"/proc/{pid}/smaps_rollup") or ""
    for line in sr.splitlines():
        if line.startswith("Pss:"):
            pss = int(line.split()[1])
    # Chromium rewrites its argv in place (process title), so read comm, and find --type= anywhere.
    name = (read(f"/proc/{pid}/comm") or "?").strip()
    if name.startswith("python"):
        name = "host(python)"
    m = re.search(r"--type=([a-z-]+)", read(f"/proc/{pid}/cmdline") or "")
    return ticks, rss, pss, f"{name}[{m.group(1)}]" if m else name


def machine_cpu():
    line = read("/proc/stat").splitlines()[0].split()[1:]
    vals = [int(x) for x in line]
    idle = vals[3] + vals[4]
    return sum(vals), idle


def machine_mem_mb():
    mi = {}
    for line in read("/proc/meminfo").splitlines():
        k, v = line.split(":", 1)
        mi[k] = int(v.split()[0])
    return (mi["MemTotal"] - mi["MemAvailable"]) / 1024


def pct(vals, q):
    if not vals:
        return None
    s = sorted(vals)
    return s[min(len(s) - 1, int(round(q / 100 * (len(s) - 1))))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root-pid", type=int, required=True)
    ap.add_argument("--seconds", type=float, required=True)
    ap.add_argument("--interval", type=float, default=1.0)
    ap.add_argument("--tag", default="")
    a = ap.parse_args()

    prev = {}
    mprev = machine_cpu()
    tprev = time.monotonic()
    samples = []
    by_name_cores = {}
    seen = {}
    end = time.monotonic() + a.seconds
    while time.monotonic() < end:
        time.sleep(a.interval)
        if not os.path.exists(f"/proc/{a.root_pid}"):
            print(json.dumps({"tag": a.tag, "note": "root pid exited"}), flush=True)
            break
        now = time.monotonic()
        dt = now - tprev
        tprev = now
        pids = tree(a.root_pid)
        cores = rss = pss = 0.0
        names = {}
        for p in pids:
            info = proc_info(p)
            if not info:
                continue
            ticks, r, ps, name = info
            seen[p] = name
            # A pid seen for the first time has no baseline, so it contributes from its 2nd interval.
            d = ticks - prev[p] if p in prev else 0
            prev[p] = ticks
            c = d / TCK / dt
            cores += c
            rss += r
            pss += ps
            names[name] = names.get(name, 0.0) + c
        mtot, midle = machine_cpu()
        dtot, didle = mtot - mprev[0], midle - mprev[1]
        mprev = (mtot, midle)
        mcores = (1 - didle / dtot) * NCPU if dtot else 0.0
        smp = {
            "t": round(time.time(), 3),
            "tree_cores": round(cores, 3),
            "machine_cores": round(mcores, 3),
            "tree_rss_mb": round(rss / 1024, 1),
            "tree_pss_mb": round(pss / 1024, 1),
            "machine_mem_mb": round(machine_mem_mb(), 1),
            "nproc": len(pids),
        }
        samples.append(smp)
        for k, v in names.items():
            by_name_cores.setdefault(k, []).append(v)
        print("SAMPLE " + json.dumps(smp), flush=True)

    if not samples:
        print("PROCSUMMARY " + json.dumps({"tag": a.tag, "samples": 0, "verdict": "NO SAMPLES"}), flush=True)
        return
    # drop the first sample (pids first seen there carry no delta)
    body = samples[1:] or samples

    def col(k):
        return [s[k] for s in body]

    summary = {
        "tag": a.tag,
        "samples": len(body),
        "ncpu": NCPU,
        "tree_cores_avg": round(statistics.mean(col("tree_cores")), 3),
        "tree_cores_p95": round(pct(col("tree_cores"), 95), 3),
        "tree_cores_max": round(max(col("tree_cores")), 3),
        "machine_cores_avg": round(statistics.mean(col("machine_cores")), 3),
        "machine_cores_p95": round(pct(col("machine_cores"), 95), 3),
        "machine_cores_max": round(max(col("machine_cores")), 3),
        "tree_rss_mb_avg": round(statistics.mean(col("tree_rss_mb")), 1),
        "tree_rss_mb_max": max(col("tree_rss_mb")),
        "tree_pss_mb_avg": round(statistics.mean(col("tree_pss_mb")), 1),
        "tree_pss_mb_max": max(col("tree_pss_mb")),
        "tree_pss_mb_first": body[0]["tree_pss_mb"],
        "tree_pss_mb_last": body[-1]["tree_pss_mb"],
        "machine_mem_mb_max": max(col("machine_mem_mb")),
        "cores_by_process": {k: round(statistics.mean(v[1:] or v), 3) for k, v in sorted(by_name_cores.items())},
        "processes_seen": sorted(set(seen.values())),
    }
    print("PROCSUMMARY " + json.dumps(summary), flush=True)


if __name__ == "__main__":
    main()
