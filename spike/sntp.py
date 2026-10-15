#!/usr/bin/env python3
"""Clock check (C00): SNTP offset of this Machine vs public NTP servers. offset>0 = this clock is BEHIND."""
import json, socket, struct, sys, time
D = 2208988800
def q(host):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.settimeout(3)
    t0 = time.time(); s.sendto(b"\x23" + 47 * b"\0", (host, 123)); d, _ = s.recvfrom(48); t3 = time.time()
    f = struct.unpack("!12I", d)
    t1 = f[8] - D + f[9] / 2**32; t2 = f[10] - D + f[11] / 2**32
    return {"host": host, "offset_ms": round(((t1 - t0) + (t2 - t3)) / 2 * 1000, 2), "rtt_ms": round(((t3 - t0) - (t2 - t1)) * 1000, 2)}
for h in (sys.argv[1:] or ["time.apple.com", "time.cloudflare.com", "time.google.com"]):
    for _ in range(3):
        try: print("SNTP " + json.dumps(q(h)))
        except Exception as e: print("SNTP " + json.dumps({"host": h, "error": str(e)}))
