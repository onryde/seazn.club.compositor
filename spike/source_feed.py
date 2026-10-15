#!/usr/bin/env python3
"""Test camera feed into CF Input A (C00 items 2/3/5, THROWAWAY).

  source_feed.py [--target rtmps|srt] [--seconds 0] [--width 1280 --height 720 --kbps 3000]

Mimics the capture app's encode (seazn.club.capture Link.kt: 1280x720 @30, <=3 Mbps, AAC 128k):
  videotestsrc(ball) -> x264enc -> appsink
  [python inserts an H.264 SEI user_data_unregistered NAL into EVERY access unit:
   UUID "SEAZN-C00-SEI-v1" + "wall_ms=<epoch ms>;seq=<n>"]
  -> appsrc -> h264parse -> flvmux -> rtmp2sink   (target rtmps: SPIKE_A_RTMPS_PUSH_URL)
                          -> mpegtsmux -> srtsink (target srt:   SPIKE_A_SRT_PUSH_URL)
  audiotestsrc -> voaacenc 128k -> mux
The target URL is read from the environment and set as an element property, never argv.
Logs `SRCSTAT {...}` every 10 s (frames pushed, encoder-output gap stats).
"""
import argparse
import json
import os
import sys
import time

import gi

gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst  # noqa: E402

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scrub import scrub  # noqa: E402
from sei import insert_sei  # noqa: E402


def pct(vals, q):
    s = sorted(vals)
    return s[min(len(s) - 1, int(round(q / 100 * (len(s) - 1))))] if s else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--target", choices=["rtmps", "srt", "file"], default="rtmps")
    ap.add_argument("--no-hrd", action="store_true", help="drop nal-hrd=cbr:filler=1 (diagnostic)")
    ap.add_argument("--file", default="/tmp/c00/feed.flv")
    ap.add_argument("--seconds", type=float, default=0)
    ap.add_argument("--width", type=int, default=1280)
    ap.add_argument("--height", type=int, default=720)
    ap.add_argument("--kbps", type=int, default=3000)
    ap.add_argument("--no-sei", action="store_true")
    a = ap.parse_args()

    env = {"rtmps": "SPIKE_A_RTMPS_PUSH_URL", "srt": "SPIKE_A_SRT_PUSH_URL", "file": None}[a.target]
    url = os.environ.get(env) if env else a.file
    if not url:
        print(f"FATAL {env} is not set", flush=True)
        sys.exit(2)

    Gst.init(None)
    # async=false: the sink sits behind an appsrc fed from the encoder branch of this SAME pipeline,
    # so an async sink would wait for a preroll that only PLAYING can produce (measured: stuck PAUSED).
    # appsrc min-latency + mux latency: the python round trip makes video slightly late; give the
    # live aggregator an explicit budget (without it its latency query fails).
    mux = {
        "rtmps": "flvmux name=mux streamable=true latency=200000000 ! rtmp2sink name=sink async=false",
        "srt": "mpegtsmux name=mux alignment=7 latency=200000000 ! srtsink name=sink wait-for-connection=false async=false",
        "file": "flvmux name=mux streamable=true latency=200000000 ! filesink name=sink async=false",
    }[a.target]
    hrd = "" if a.no_hrd else 'vbv-buf-capacity=1000 option-string="nal-hrd=cbr:filler=1" ' 
    desc = (
        f"videotestsrc is-live=true pattern=ball ! video/x-raw,width={a.width},height={a.height},framerate=30/1 "
        "! queue max-size-buffers=3 ! videoconvert ! video/x-raw,format=I420 ! queue max-size-buffers=3 "
        f"! x264enc tune=zerolatency speed-preset=veryfast bitrate={a.kbps} key-int-max=60 bframes=0 "
        + hrd +
        "! video/x-h264,profile=main,stream-format=byte-stream,alignment=au "
        "! queue max-size-buffers=10 ! appsink name=enc sync=false emit-signals=true max-buffers=30 "
        "appsrc name=inj is-live=true format=time do-timestamp=false max-bytes=8000000 min-latency=100000000 "
        "! h264parse config-interval=-1 ! queue ! mux. "
        "audiotestsrc is-live=true wave=sine freq=440 volume=0.2 ! audioconvert ! audioresample "
        "! audio/x-raw,rate=48000,channels=2 ! voaacenc bitrate=128000 ! aacparse ! queue ! mux. "
        + mux
    )
    pipe = Gst.parse_launch(desc)
    sink = pipe.get_by_name("sink")
    if a.target in ("rtmps", "file"):
        sink.set_property("location", url)
    else:
        sink.set_property("uri", url)
    del url

    inj = pipe.get_by_name("inj")
    st = {"seq": 0, "last": None, "gaps": [], "caps_set": False, "t0": time.monotonic()}

    def on_enc(appsink):
        s = appsink.emit("pull-sample")
        b = s.get_buffer()
        if not st["caps_set"]:
            inj.set_property("caps", s.get_caps())
            st["caps_set"] = True
        now = time.monotonic()
        if st["last"] is not None:
            st["gaps"].append((now - st["last"]) * 1000)
        st["last"] = now
        ok, mi = b.map(Gst.MapFlags.READ)
        data = bytes(mi.data)
        b.unmap(mi)
        st["seq"] += 1
        if not a.no_sei:
            data = insert_sei(data, f"wall_ms={time.time_ns() // 1_000_000};seq={st['seq']}")
        nb = Gst.Buffer.new_wrapped(data)
        # x264enc's output segment starts at 1000 h (room for negative DTS); appsrc sends a DEFAULT
        # segment (start 0), so raw PTS would sit 1000 h in the future and flvmux/mpegtsmux never
        # pick the video pad (measured: audio-only FLV, CF ERR_LIVE_VIDEO_TRANSITION_FAILED).
        # Re-express both timestamps as running time.
        seg = s.get_segment()
        nb.pts = seg.to_running_time(Gst.Format.TIME, b.pts)
        nb.dts = seg.to_running_time(Gst.Format.TIME, b.dts) if b.dts != Gst.CLOCK_TIME_NONE else Gst.CLOCK_TIME_NONE
        nb.duration = b.duration
        if b.has_flags(Gst.BufferFlags.DELTA_UNIT):
            nb.set_flags(Gst.BufferFlags.DELTA_UNIT)
        inj.emit("push-buffer", nb)
        return Gst.FlowReturn.OK

    pipe.get_by_name("enc").connect("new-sample", on_enc)
    loop = GLib.MainLoop()

    def on_msg(_bus, m):
        if m.type == Gst.MessageType.ERROR:
            e, _ = m.parse_error()
            print(f"SRCERROR {m.src.get_name()}: {scrub(e.message)}", flush=True)
            loop.quit()
        elif m.type == Gst.MessageType.EOS:
            print("SRCEOS", flush=True)
            loop.quit()
        elif m.type == Gst.MessageType.WARNING:
            e, _ = m.parse_warning()
            print(f"SRCWARN {m.src.get_name()}: {scrub(e.message)}", flush=True)
        return True

    pipe.get_bus().add_watch(GLib.PRIORITY_DEFAULT, on_msg)

    def report():
        g = st["gaps"]
        st["gaps"] = []
        print("SRCSTAT " + json.dumps({
            "t": round(time.time(), 3), "target": a.target, "frames": st["seq"],
            "enc_gap_ms_p50": round(pct(g, 50), 1) if g else None,
            "enc_gap_ms_max": round(max(g), 1) if g else None,
        }), flush=True)
        if a.seconds and time.monotonic() - st["t0"] > a.seconds:
            loop.quit()
            return False
        return True

    GLib.timeout_add_seconds(10, report)
    print(f"SRCSTART {json.dumps({'t': round(time.time(), 3), 'target': a.target, 'w': a.width, 'h': a.height, 'kbps': a.kbps, 'sei': not a.no_sei})}", flush=True)
    pipe.set_state(Gst.State.PLAYING)
    loop.run()
    pipe.set_state(Gst.State.NULL)
    print("SRCEND", flush=True)


if __name__ == "__main__":
    main()
