#!/usr/bin/env python3
"""Full composed chain, one process (C00 item 5, THROWAWAY).

  chain.py --height 720|1080 --seconds 290 [--overlay URL]

  srtsrc($SPIKE_A_SRT_PLAY_URL) ! tsdemux
     video -> h264parse -> avdec_h264 -> videoconvert -> videoscale -> I420 WxH -> queue qcam(leaky) -> mix.sink_0
     audio -> fakesink (camera audio is not used here)
  cefsrc(overlay, listen-for-js-signals=false by default, max-video-framerate=30/1) BGRA 1920x1080 -> queue qgfx(leaky) -> mix.sink_1 (scaled by the pad to WxH)
  compositor(AYUV WxH @30) -> videoconvert -> I420 -> queue qenc(leaky=downstream, 30 buffers)
     -> x264enc (C09 settings) -> h264parse -> flvmux -> rtmp2sink($SPIKE_B_RTMPS_URL)
  audiotestsrc(silence) -> voaacenc 128k -> flvmux

Counts: queue overruns (= dropped frames) per queue, encoder output fps per 10 s window
(encoder speed = fps / 30), compositor output fps, decoded camera frames, cef frames, rtmp2sink
acked bytes. Prints `CHAINSTAT {...}` every 10 s and `RESULT {...}` at the end.
"""
import argparse
import json
import os
import statistics
import sys
import threading
import time

import gi

gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst  # noqa: E402

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scrub import scrub  # noqa: E402

KBPS = {720: 4000, 1080: 6000}  # C02 OutputProfile P720 / P1080


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--height", type=int, choices=[720, 1080], default=720)
    ap.add_argument("--seconds", type=float, default=290)
    ap.add_argument("--overlay", default="file:///spike/overlay-test.html")
    ap.add_argument("--warmup", type=float, default=15)
    ap.add_argument("--js-ready", action="store_true", help="listen-for-js-signals=true (F-001: errors beside other live sources)")
    ap.add_argument("--snapshot", default="", help="write one raw AYUV compositor frame here at --snapshot-at s")
    ap.add_argument("--snapshot-at", type=float, default=60)
    # encoder knobs (YouTube health follow-up): defaults are the C09 settings measured in F-005
    ap.add_argument("--x264-bitrate", type=int, default=0, help="kbps; 0 = profile default (720: 4000, 1080: 6000)")
    ap.add_argument("--x264-opts", default="nal-hrd=cbr:filler=1", help="x264enc option-string")
    ap.add_argument("--vbv-ms", type=int, default=1000, help="x264enc vbv-buf-capacity in ms; -1 = leave at element default")
    ap.add_argument("--bg", choices=["camera", "snow"], default="camera",
                    help="camera = SRT playback of Input A; snow = local high-entropy videotestsrc (diagnostic)")
    a = ap.parse_args()
    H = a.height
    W = 1280 if H == 720 else 1920
    fps = 30
    kbps = a.x264_bitrate or KBPS[H]

    play = os.environ.get("SPIKE_A_SRT_PLAY_URL")
    out = os.environ.get("SPIKE_B_RTMPS_URL")
    if not play or not out:
        print("FATAL SPIKE_A_SRT_PLAY_URL / SPIKE_B_RTMPS_URL not set", flush=True)
        sys.exit(2)

    Gst.init(None)
    desc = (
        ("srtsrc name=src ! queue name=qsrt ! tsdemux name=demux " if a.bg == "camera" else
         f"videotestsrc name=bg is-live=true pattern=snow ! video/x-raw,format=I420,width={W},height={H},framerate={fps}/1 "
         f"! queue name=qcam leaky=downstream max-size-buffers={fps} max-size-bytes=0 max-size-time=0 ! mix.sink_0 ") +
        f'cefsrc name=cef url="{a.overlay}" max-video-framerate=30/1 '
        f"listen-for-js-signals={'true' if a.js_ready else 'false'} "
        "! video/x-raw,format=BGRA,width=1920,height=1080 "
        "! queue name=qgfx leaky=downstream max-size-buffers=2 max-size-bytes=0 max-size-time=0 ! mix.sink_1 "
        f"compositor name=mix background=black sink_0::zorder=0 sink_1::zorder=1 sink_1::width={W} sink_1::height={H} "
        f"! video/x-raw,format=AYUV,width={W},height={H},framerate={fps}/1 ! videoconvert ! video/x-raw,format=I420 "
        f"! queue name=qenc leaky=downstream max-size-buffers={fps} max-size-bytes=0 max-size-time=0 "
        f"! x264enc name=enc tune=zerolatency speed-preset=veryfast bitrate={kbps} key-int-max={fps * 2} bframes=0 "
        + (f"vbv-buf-capacity={a.vbv_ms} " if a.vbv_ms >= 0 else "")
        + (f'option-string="{a.x264_opts}" ' if a.x264_opts else "") +
        "! video/x-h264,profile=main ! h264parse config-interval=-1 ! queue name=qmux "
        "! flvmux name=mux streamable=true ! rtmp2sink name=out "
        "audiotestsrc is-live=true wave=silence ! audioconvert ! audioresample ! audio/x-raw,rate=48000,channels=2 "
        "! voaacenc bitrate=128000 ! aacparse ! queue ! mux. "
    )
    pipe = Gst.parse_launch(desc)
    if a.bg == "camera":
        pipe.get_by_name("src").set_property("uri", play)
    pipe.get_by_name("out").set_property("location", out)
    del play, out

    mix = pipe.get_by_name("mix")
    t0 = time.monotonic()
    wall0 = time.time()
    lock = threading.Lock()
    c = {"enc": [], "kf": [], "pts": [], "bytes": 0, "filler_bytes": 0, "sei_bytes": 0, "mix": 0, "cam": 0, "cef": 0, "over": {"qcam": [], "qgfx": [], "qenc": []},
         "qos": 0, "first_enc_s": None, "first_cam_s": None, "first_cef_s": None}

    def counter(key, first=None):
        def probe(_pad, _info):
            with lock:
                c[key] += 1
                if first and c[first] is None:
                    c[first] = time.monotonic() - t0
            return Gst.PadProbeReturn.OK
        return probe

    def nal_sizes(data):
        """(nal_type, size) for each NAL in an access unit, Annex-B or 4-byte AVC length-prefixed."""
        out = []
        if data[:4] == b"\x00\x00\x00\x01" or data[:3] == b"\x00\x00\x01":
            starts, i = [], 0
            while True:
                j = data.find(b"\x00\x00\x01", i)
                if j < 0:
                    break
                starts.append(j + 3)
                i = j + 3
            for k, st in enumerate(starts):
                end = (starts[k + 1] - 3) if k + 1 < len(starts) else len(data)
                out.append((data[st] & 0x1F, end - st))
        else:
            i = 0
            while i + 4 <= len(data):
                n = int.from_bytes(data[i:i + 4], "big")
                if n <= 0 or i + 4 + n > len(data):
                    break
                out.append((data[i + 4] & 0x1F, n))
                i += 4 + n
        return out

    def enc_probe(_pad, info):
        now = time.monotonic()
        b = info.get_buffer()
        key = not b.has_flags(Gst.BufferFlags.DELTA_UNIT)
        ok, mi = b.map(Gst.MapFlags.READ)
        nals = nal_sizes(bytes(mi.data)) if ok else []
        size = b.get_size()
        if ok:
            b.unmap(mi)
        with lock:
            c["enc"].append(now)
            c["pts"].append(b.pts)
            if key:
                c["kf"].append(len(c["enc"]) - 1)
            if now - t0 > a.warmup:
                c["bytes"] += size
                c["filler_bytes"] += sum(n for t, n in nals if t == 12)
                c["sei_bytes"] += sum(n for t, n in nals if t == 6)
            if c["first_enc_s"] is None:
                c["first_enc_s"] = now - t0
        return Gst.PadProbeReturn.OK

    pipe.get_by_name("enc").get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, enc_probe)
    mix.get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, counter("mix"))
    snap = {"done": not a.snapshot}

    def snap_probe(_pad, info):
        if not snap["done"] and time.monotonic() - t0 >= a.snapshot_at:
            snap["done"] = True
            b = info.get_buffer()
            ok, mi = b.map(Gst.MapFlags.READ)
            if ok:
                with open(a.snapshot, "wb") as f:
                    f.write(bytes(mi.data))
                b.unmap(mi)
        return Gst.PadProbeReturn.OK

    mix.get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, snap_probe)
    pipe.get_by_name("cef").get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, counter("cef", "first_cef_s"))

    def overrun(name):
        def cb(_q):
            with lock:
                c["over"][name].append(time.monotonic() - t0)
        return cb

    for qn in ("qgfx", "qenc"):
        pipe.get_by_name(qn).connect("overrun", overrun(qn))

    def on_pad(_d, pad):
        caps = pad.get_current_caps() or pad.query_caps(None)
        name = caps.get_structure(0).get_name()
        if name.startswith("video/x-h264"):
            els = [Gst.ElementFactory.make(n, None) for n in ("h264parse", "avdec_h264", "videoconvert", "videoscale")]
            cf = Gst.ElementFactory.make("capsfilter", None)
            cf.set_property("caps", Gst.Caps.from_string(f"video/x-raw,format=I420,width={W},height={H}"))
            q = Gst.ElementFactory.make("queue", "qcam")
            q.set_property("leaky", 2)
            q.set_property("max-size-buffers", fps)
            q.set_property("max-size-bytes", 0)
            q.set_property("max-size-time", 0)
            q.connect("overrun", overrun("qcam"))
            chain = els + [cf, q]
            for e in chain:
                pipe.add(e)
            for x, y in zip(chain, chain[1:]):
                x.link(y)
            sinkpad = mix.request_pad_simple("sink_0")
            q.get_static_pad("src").link(sinkpad)
            for e in chain:
                e.sync_state_with_parent()
            pad.link(els[0].get_static_pad("sink"))
            els[1].get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, counter("cam", "first_cam_s"))
        else:
            s = Gst.ElementFactory.make("fakesink", None)
            s.set_property("sync", False)
            pipe.add(s)
            s.sync_state_with_parent()
            pad.link(s.get_static_pad("sink"))

    if a.bg == "camera":
        pipe.get_by_name("demux").connect("pad-added", on_pad)
    else:
        pipe.get_by_name("bg").get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, counter("cam", "first_cam_s"))
        pipe.get_by_name("qcam").connect("overrun", overrun("qcam"))

    msgs = []
    loop = GLib.MainLoop()

    def on_msg(_bus, m):
        t = round(time.monotonic() - t0, 3)
        if m.type == Gst.MessageType.ERROR:
            e, dbg = m.parse_error()
            msgs.append({"t": t, "type": "error", "src": m.src.get_name(), "text": scrub(e.message), "debug": scrub((dbg or "")[:300])})
            loop.quit()
        elif m.type == Gst.MessageType.WARNING:
            e, dbg = m.parse_warning()
            if len(msgs) < 50:
                msgs.append({"t": t, "type": "warning", "src": m.src.get_name(), "text": scrub(e.message)})
        elif m.type == Gst.MessageType.QOS:
            c["qos"] += 1
        elif m.type == Gst.MessageType.EOS:
            msgs.append({"t": t, "type": "eos"})
            loop.quit()
        return True

    pipe.get_bus().add_watch(GLib.PRIORITY_DEFAULT, on_msg)
    out_el = pipe.get_by_name("out")
    windows = []
    last = {"enc": 0, "mix": 0, "cam": 0, "cef": 0, "t": t0, "acked": None}

    def acked_bytes():
        try:
            s = out_el.get_property("stats")
            # CF's RTMP ingest sent no acknowledgements in a 50 s control run (out-bytes-acked stayed 0),
            # so egress is measured on bytes SENT.
            ok, v = s.get_uint64("out-bytes-total")
            return v if ok else None
        except Exception:  # noqa: BLE001
            return None

    def report():
        now = time.monotonic()
        with lock:
            n_enc, n_mix, n_cam, n_cef = len(c["enc"]), c["mix"], c["cam"], c["cef"]
            over = {k: len(v) for k, v in c["over"].items()}
        dt = now - last["t"]
        ab = acked_bytes()
        w = {
            "t": round(now - t0, 1),
            "enc_fps": round((n_enc - last["enc"]) / dt, 2), "mix_fps": round((n_mix - last["mix"]) / dt, 2),
            "cam_fps": round((n_cam - last["cam"]) / dt, 2), "cef_fps": round((n_cef - last["cef"]) / dt, 2),
            "overruns": over, "egress_kbps": None if ab is None or last["acked"] is None else round((ab - last["acked"]) * 8 / 1000 / dt, 1),
        }
        last.update({"enc": n_enc, "mix": n_mix, "cam": n_cam, "cef": n_cef, "t": now, "acked": ab})
        windows.append(w)
        print("CHAINSTAT " + json.dumps(w), flush=True)
        if now - t0 > a.seconds:
            loop.quit()
            return False
        return True

    GLib.timeout_add_seconds(10, report)
    print("CHAINSTART " + json.dumps({"t": round(wall0, 3), "w": W, "h": H, "kbps": kbps, "x264_opts": a.x264_opts,
                                      "vbv_ms": a.vbv_ms, "bg": a.bg}), flush=True)
    pipe.set_state(Gst.State.PLAYING)
    loop.run()
    end = time.monotonic()
    pipe.send_event(Gst.Event.new_eos())
    time.sleep(1.5)
    threading.Thread(target=lambda: pipe.set_state(Gst.State.NULL), daemon=True).start()
    time.sleep(1.5)

    steady = [w for w in windows if w["t"] > a.warmup]
    enc = c["enc"]
    enc_steady = [x for x in enc if x - t0 > a.warmup]
    span = (enc_steady[-1] - enc_steady[0]) if len(enc_steady) > 1 else 0
    enc_fps = (len(enc_steady) - 1) / span if span else None
    over_steady = {k: sum(1 for x in v if x > a.warmup) for k, v in c["over"].items()}
    # keyframe interval (frames, and seconds by PTS) and media-time vs wall-time over the steady window
    kf = c["kf"]
    kf_frames = [y - x for x, y in zip(kf, kf[1:])]
    pts = c["pts"]
    kf_secs = [(pts[y] - pts[x]) / 1e9 for x, y in zip(kf, kf[1:]) if pts[x] != Gst.CLOCK_TIME_NONE and pts[y] != Gst.CLOCK_TIME_NONE]
    idx = [i for i, x in enumerate(enc) if x - t0 > a.warmup]
    media_per_wall = None
    if len(idx) > 1 and pts[idx[0]] != Gst.CLOCK_TIME_NONE and pts[idx[-1]] != Gst.CLOCK_TIME_NONE:
        media_per_wall = round(((pts[idx[-1]] - pts[idx[0]]) / 1e9) / (enc[idx[-1]] - enc[idx[0]]), 4)
    steady_s = (enc[idx[-1]] - enc[idx[0]]) if len(idx) > 1 else 0
    def kb(x):
        return round(x * 8 / 1000 / steady_s, 1) if steady_s else None
    encoder_bits = {
        "video_kbps_total": kb(c["bytes"]), "filler_kbps": kb(c["filler_bytes"]), "sei_kbps": kb(c["sei_bytes"]),
        "video_kbps_excluding_filler": kb(c["bytes"] - c["filler_bytes"]),
        "filler_share": round(c["filler_bytes"] / c["bytes"], 4) if c["bytes"] else None,
        "keyframes": len(kf), "keyframe_interval_frames": sorted(set(kf_frames))[:8],
        "keyframe_interval_s_max": round(max(kf_secs), 3) if kf_secs else None,
        "keyframe_interval_s_min": round(min(kf_secs), 3) if kf_secs else None,
        "media_seconds_per_wall_second": media_per_wall,
    }
    res = {
        "wall_start": round(wall0, 3), "w": W, "h": H, "kbps": kbps, "x264_opts": a.x264_opts, "vbv_ms": a.vbv_ms, "bg": a.bg,
        "encoder_bits": encoder_bits, "ran_s": round(end - t0, 1),
        "first_cam_frame_s": None if c["first_cam_s"] is None else round(c["first_cam_s"], 2),
        "first_cef_frame_s": None if c["first_cef_s"] is None else round(c["first_cef_s"], 2),
        "first_enc_frame_s": None if c["first_enc_s"] is None else round(c["first_enc_s"], 2),
        "encoded_frames": len(enc), "mix_frames": c["mix"], "cam_frames": c["cam"], "cef_frames": c["cef"],
        "encoder_fps_steady": round(enc_fps, 3) if enc_fps else None,
        "encoder_speed_steady": round(enc_fps / fps, 4) if enc_fps else None,
        "encoder_speed_min_10s_window": round(min(w["enc_fps"] for w in steady) / fps, 4) if steady else None,
        "mix_fps_steady_avg": round(statistics.mean(w["mix_fps"] for w in steady), 3) if steady else None,
        "cam_fps_steady_avg": round(statistics.mean(w["cam_fps"] for w in steady), 3) if steady else None,
        "cef_fps_steady_avg": round(statistics.mean(w["cef_fps"] for w in steady), 3) if steady else None,
        "egress_kbps_steady_avg": round(statistics.mean(w["egress_kbps"] for w in steady if w["egress_kbps"] is not None), 1)
        if any(w["egress_kbps"] is not None for w in steady) else None,
        "overruns_total": {k: len(v) for k, v in c["over"].items()},
        "overruns_after_warmup": over_steady,
        "windows_with_qenc_overrun": sum(1 for i, w in enumerate(steady) if i and w["overruns"]["qenc"] > steady[i - 1]["overruns"]["qenc"]),
        "qos_messages": c["qos"], "bus": msgs, "steady_windows": len(steady),
    }
    print("RESULT " + json.dumps(res), flush=True)
    os._exit(0)


if __name__ == "__main__":
    main()
