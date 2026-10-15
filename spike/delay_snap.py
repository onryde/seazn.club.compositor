#!/usr/bin/env python3
"""C00 item 2 owner session: camera->compositor delay snapshots (THROWAWAY).

  delay_snap.py --env SPIKE_FIX_SRT_PLAY_URL --seconds 80 --every 5 --out /tmp/c00/snap

srtsrc(uri from $ENV, set as a property) ! tsdemux ! h264parse ! avdec_h264 [probe: wall ms the frame
left the decoder, keyed by PTS] ! videoconvert ! jpegenc ! appsink. Every --every s one frame is written
as <out>/snap-<wall_ms>.jpg; wall_ms is the Machine clock when that frame left the decoder.
"""
import argparse, json, os, sys, time
import gi
gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scrub import scrub

ap = argparse.ArgumentParser()
ap.add_argument("--env", default="SPIKE_FIX_SRT_PLAY_URL")
ap.add_argument("--seconds", type=float, default=80)
ap.add_argument("--every", type=float, default=5)
ap.add_argument("--out", default="/tmp/c00/snap")
a = ap.parse_args()
os.makedirs(a.out, exist_ok=True)
Gst.init(None)
p = Gst.parse_launch(
    "srtsrc name=src ! queue ! tsdemux name=d d. ! queue ! h264parse ! avdec_h264 name=dec "
    "! videoconvert ! jpegenc quality=92 ! appsink name=snk emit-signals=true sync=false max-buffers=8")
p.get_by_name("src").set_property("uri", os.environ[a.env])
stamp, st = {}, {"next": None, "saved": [], "frames": 0, "first_wall": None, "t0": time.time()}

def dec_probe(_pad, info):
    b = info.get_buffer()
    stamp[b.pts] = time.time_ns() // 1_000_000
    if len(stamp) > 300:
        for k in list(stamp)[:100]:
            stamp.pop(k, None)
    return Gst.PadProbeReturn.OK

p.get_by_name("dec").get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, dec_probe)

def on_sample(sink):
    s = sink.emit("pull-sample"); b = s.get_buffer()
    w = stamp.pop(b.pts, None)
    st["frames"] += 1
    if w is None:
        return Gst.FlowReturn.OK
    if st["first_wall"] is None:
        st["first_wall"] = w; st["next"] = w / 1000 + 1.0
    if w / 1000 >= st["next"]:
        st["next"] += a.every
        ok, mi = b.map(Gst.MapFlags.READ)
        if ok:
            path = os.path.join(a.out, f"snap-{w}.jpg")
            with open(path, "wb") as f:
                f.write(bytes(mi.data))
            b.unmap(mi)
            st["saved"].append(w)
            print("SNAP " + json.dumps({"wall_ms": w, "file": os.path.basename(path)}), flush=True)
    return Gst.FlowReturn.OK

p.get_by_name("snk").connect("new-sample", on_sample)
loop = GLib.MainLoop()
def on_msg(_b, m):
    if m.type in (Gst.MessageType.ERROR, Gst.MessageType.EOS):
        txt = m.parse_error()[0].message if m.type == Gst.MessageType.ERROR else "eos"
        print("SNAPEND " + json.dumps({"why": scrub(txt)}), flush=True); loop.quit()
    return True
p.get_bus().add_watch(GLib.PRIORITY_DEFAULT, on_msg)
GLib.timeout_add(int(a.seconds * 1000), loop.quit)
p.set_state(Gst.State.PLAYING)
loop.run()
print("SNAPRESULT " + json.dumps({"frames": st["frames"], "saved": len(st["saved"]),
      "first_frame_after_s": None if st["first_wall"] is None else round(st["first_wall"] / 1000 - st["t0"], 3)}), flush=True)
os._exit(0)
