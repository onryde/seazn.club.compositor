#!/usr/bin/env python3
"""Control feed (C00, THROWAWAY): no appsrc, no SEI. videotestsrc -> x264 -> flvmux -> rtmp2sink.
Location from SPIKE_A_RTMPS_PUSH_URL, set as a property. Prints rtmp2sink byte stats every 10 s."""
import json, os, sys, time
import gi
gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst
sys.path.insert(0, "/spike")
from scrub import scrub
Gst.init(None)
p = Gst.parse_launch(
    "videotestsrc is-live=true pattern=ball ! video/x-raw,width=1280,height=720,framerate=30/1 ! videoconvert "
    "! x264enc tune=zerolatency speed-preset=veryfast bitrate=3000 key-int-max=60 bframes=0 "
    "! video/x-h264,profile=main ! h264parse ! queue ! mux. "
    "audiotestsrc is-live=true wave=sine freq=440 volume=0.2 ! audioconvert ! audioresample "
    "! audio/x-raw,rate=48000,channels=2 ! voaacenc bitrate=128000 ! aacparse ! queue ! mux. "
    "flvmux name=mux streamable=true ! rtmp2sink name=sink")
s = p.get_by_name("sink")
s.set_property("location", os.environ["SPIKE_A_RTMPS_PUSH_URL"])
loop = GLib.MainLoop()
def on_msg(_b, m):
    if m.type in (Gst.MessageType.ERROR, Gst.MessageType.WARNING):
        e, _ = m.parse_error() if m.type == Gst.MessageType.ERROR else m.parse_warning()
        print(f"PLAIN{m.type.value_nicks[0].upper()} {m.src.get_name()}: {scrub(e.message)}", flush=True)
        if m.type == Gst.MessageType.ERROR: loop.quit()
    return True
p.get_bus().add_watch(GLib.PRIORITY_DEFAULT, on_msg)
def stat():
    st = s.get_property("stats")
    keep = {k: st.get_value(k) for k in ("out-bytes-total", "out-bytes-acked", "in-bytes-total") if st and st.has_field(k)}
    print("PLAINSTAT " + json.dumps({"t": round(time.time(), 3), **keep}), flush=True)
    return True
GLib.timeout_add_seconds(10, stat)
print("PLAINSTART " + json.dumps({"t": round(time.time(), 3)}), flush=True)
p.set_state(Gst.State.PLAYING)
loop.run()
p.set_state(Gst.State.NULL)
