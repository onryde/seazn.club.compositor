#!/usr/bin/env python3
"""C00: every N s, the TCP sockets to remote port 443 (the RTMPS push) from /proc/net/tcp{,6}:
tx_queue (send-q bytes), rx_queue, retransmit timer count; plus machine-wide Tcp RetransSegs/OutSegs.
No addresses are printed. `ss` is not in the image."""
import json, sys, time
secs, every = float(sys.argv[1]), float(sys.argv[2])
def socks():
    out = []
    for f in ("/proc/net/tcp", "/proc/net/tcp6"):
        try:
            for ln in open(f).read().splitlines()[1:]:
                x = ln.split()
                if x[2].split(":")[1] == "01BB" and x[3] == "01":   # remote :443, ESTABLISHED
                    tx, rx = (int(v, 16) for v in x[4].split(":"))
                    out.append({"send_q": tx, "recv_q": rx, "retrnsmt": int(x[6], 16)})
        except OSError:
            pass
    return out
def snmp():
    L = [l.split() for l in open("/proc/net/snmp") if l.startswith("Tcp:")]
    d = dict(zip(L[0][1:], map(int, L[1][1:])))
    return d["OutSegs"], d["RetransSegs"]
t0 = time.time(); o0, r0 = snmp()
while time.time() - t0 < secs:
    o, r = snmp()
    print("TCPSTAT " + json.dumps({"wall": round(time.time(), 3), "socks": socks(), "out_segs": o - o0, "retrans_segs": r - r0}), flush=True)
    time.sleep(every)
