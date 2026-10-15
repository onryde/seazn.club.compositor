#!/usr/bin/env python3
"""C00: which elementary streams does the SRT playback carry? (THROWAWAY)

  probe_tracks.py --env SPIKE_FIX_SRT_PLAY_URL --seconds 12

srtsrc ! tsdemux; every demux pad is linked through decodebin to a fakesink, and its demux caps,
decoded caps (codec/rate/channels or size/fps) and buffer count are printed as TRACK lines.
"""
import argparse, json, os, sys, time
import gi
gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scrub import scrub

ap = argparse.ArgumentParser(); ap.add_argument("--env", default="SPIKE_FIX_SRT_PLAY_URL"); ap.add_argument("--seconds", type=float, default=12)
a = ap.parse_args(); Gst.init(None)
p = Gst.parse_launch("srtsrc name=src ! queue ! tsdemux name=d")
p.get_by_name("src").set_property("uri", os.environ[a.env])
tracks = {}

def on_pad(_d, pad):
    caps = (pad.get_current_caps() or pad.query_caps(None)).to_string()
    t = tracks.setdefault(pad.get_name(), {"demux_caps": scrub(caps), "decoded_caps": None, "buffers": 0})
    q = Gst.ElementFactory.make("queue"); db = Gst.ElementFactory.make("decodebin")
    for e in (q, db): p.add(e)
    q.link(db); pad.link(q.get_static_pad("sink"))
    def on_dec(_db, dpad):
        t["decoded_caps"] = scrub((dpad.get_current_caps() or dpad.query_caps(None)).to_string())
        fs = Gst.ElementFactory.make("fakesink"); fs.set_property("sync", False); p.add(fs)
        fs.sync_state_with_parent(); dpad.link(fs.get_static_pad("sink"))
        def cnt(_p, _i):
            t["buffers"] += 1
            if t["buffers"] == 5:
                t["decoded_caps"] = scrub(_p.get_current_caps().to_string())
            return Gst.PadProbeReturn.OK
        fs.get_static_pad("sink").add_probe(Gst.PadProbeType.BUFFER, cnt)
    db.connect("pad-added", on_dec)
    for e in (q, db): e.sync_state_with_parent()

p.get_by_name("d").connect("pad-added", on_pad)
loop = GLib.MainLoop()
def on_msg(_b, m):
    if m.type == Gst.MessageType.ERROR:
        print("ERROR " + scrub(m.parse_error()[0].message), flush=True); loop.quit()
    return True
bus = p.get_bus(); bus.add_signal_watch(); bus.connect("message", on_msg)
p.set_state(Gst.State.PLAYING)
GLib.timeout_add(int(a.seconds * 1000), loop.quit); loop.run()
for k, v in tracks.items():
    print("TRACK " + json.dumps({"pad": k, **v}), flush=True)
print("TRACKS " + json.dumps({"n": len(tracks)}), flush=True)
p.set_state(Gst.State.NULL); os._exit(0)
