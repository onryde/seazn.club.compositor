#!/usr/bin/env python3
"""C00 item 2 owner session: phone background/reconnect watcher (THROWAWAY).

  bg_watch.py --env SPIKE_FIX_SRT_PLAY_URL --seconds 150 [--stall 5]

Keeps one SRT playback reader (srtsrc ! tsdemux ! h264parse, per-AU arrival probe) alive for N s:
  - logs every inter-AU gap > 300 ms as GAP {start_wall, end_wall, ms}
  - on ERROR/EOS logs CLOSE {wall, why} and reconnects at once
  - if no AU for --stall s (spec input_stall_after_ms), logs STALL and rebuilds the reader
  - after every (re)connect logs FIRST {wall, after_s} for the first AU
All walls are this Machine's clock (epoch ms).
"""
import argparse, json, os, sys, time
import gi
gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scrub import scrub

ap = argparse.ArgumentParser()
ap.add_argument("--env", default="SPIKE_FIX_SRT_PLAY_URL")
ap.add_argument("--seconds", type=float, default=150)
ap.add_argument("--stall", type=float, default=5.0)
a = ap.parse_args()
Gst.init(None)
url = os.environ[a.env]
now_ms = lambda: time.time_ns() // 1_000_000
st = {"pipe": None, "gen": 0, "conn_ms": None, "first": False, "last_au": None, "aus": 0, "gaps": [], "events": []}
loop = GLib.MainLoop()

def log(kind, **kw):
    kw["gen"] = st["gen"]; st["events"].append({"kind": kind, **kw})
    print(kind + " " + json.dumps(kw), flush=True)

def probe(_pad, info):
    t = now_ms()
    if not st["first"]:
        st["first"] = True
        log("FIRST", wall=t, after_s=round((t - st["conn_ms"]) / 1000, 3))
    elif st["last_au"] is not None and t - st["last_au"] > 300:
        st["gaps"].append(t - st["last_au"]); log("GAP", start_wall=st["last_au"], end_wall=t, ms=t - st["last_au"])
    st["last_au"] = t; st["aus"] += 1
    return Gst.PadProbeReturn.OK

def on_msg(_b, m):
    if m.src is None or st["pipe"] is None or m.src.get_parent() is None and m.src != st["pipe"]:
        pass
    if m.type in (Gst.MessageType.ERROR, Gst.MessageType.EOS):
        why = scrub(m.parse_error()[0].message) if m.type == Gst.MessageType.ERROR else "eos"
        log("CLOSE", wall=now_ms(), why=why, src=m.src.get_name() if m.src else None)
        GLib.idle_add(rebuild, "close")
    return True

def build():
    st["gen"] += 1
    p = Gst.parse_launch("srtsrc name=src ! queue ! tsdemux name=d d. ! queue ! h264parse name=hp ! fakesink sync=false")
    p.get_by_name("src").set_property("uri", url)
    p.get_by_name("hp").get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, probe)
    bus = p.get_bus(); bus.add_signal_watch(); bus.connect("message", on_msg)
    st.update(pipe=p, conn_ms=now_ms(), first=False, last_au=None)
    log("CONNECT", wall=st["conn_ms"])
    p.set_state(Gst.State.PLAYING)

def rebuild(why):
    old = st["pipe"]
    st["pipe"] = None
    if old is not None:
        old.get_bus().remove_signal_watch()
        old.set_state(Gst.State.NULL)
    log("REBUILD", wall=now_ms(), why=why)
    build()
    return False

def watchdog():
    ref = st["last_au"] if st["last_au"] is not None else st["conn_ms"]
    if ref is not None and now_ms() - ref > a.stall * 1000:
        log("STALL", wall=now_ms(), silent_ms=now_ms() - ref)
        rebuild("stall")
    return True

build()
GLib.timeout_add(250, watchdog)
GLib.timeout_add(int(a.seconds * 1000), loop.quit)
loop.run()
print("BGRESULT " + json.dumps({"aus": st["aus"], "gaps_over_300ms": st["gaps"], "connects": st["gen"],
      "closes": sum(1 for e in st["events"] if e["kind"] == "CLOSE"), "stalls": sum(1 for e in st["events"] if e["kind"] == "STALL")}), flush=True)
os._exit(0)
