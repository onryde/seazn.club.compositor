#!/usr/bin/env python3
"""CF Input A SRT playback reader (C00 items 2/3, THROWAWAY).

  srt_reader.py --seconds N --tag T [--decode] [--dump-ts PATH --dump-seconds S]

  srtsrc(uri = $SPIKE_A_SRT_PLAY_URL, set as a property, never argv) ! tsdemux
     video -> h264parse(byte-stream, au) -> [probe: arrival time, PTS, keyframe, SEI parse]
              -> (--decode) avdec_h264 -> fakesink  [probe: decoded frame count]
     audio -> fakesink [probe: arrival]
  (--dump-ts) the raw TS of the first S seconds also goes to PATH for an ffprobe cross-check.

Stops after N seconds, or on the first EOS / ERROR (so a kick is observed, not papered over).
Every bus message text is printed through the same redaction as redact.py by the wrapper.
Prints `RESULT {json}` at the end.
"""
import argparse
import json
import os
import re
import statistics
import sys
import threading
import time

import gi

gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst  # noqa: E402

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scrub import scrub  # noqa: E402
from sei import SEAZN_UUID, X264_UUID, parse_seis  # noqa: E402


def pct(vals, q):
    s = sorted(vals)
    return s[min(len(s) - 1, int(round(q / 100 * (len(s) - 1))))] if s else None


def stats(vals):
    if not vals:
        return None
    return {
        "n": len(vals), "p50": round(pct(vals, 50), 2), "p90": round(pct(vals, 90), 2),
        "p99": round(pct(vals, 99), 2), "max": round(max(vals), 2), "min": round(min(vals), 2),
        "mean": round(statistics.mean(vals), 2),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seconds", type=float, required=True)
    ap.add_argument("--tag", default="r")
    ap.add_argument("--decode", action="store_true")
    ap.add_argument("--dump-ts", default="")
    ap.add_argument("--dump-seconds", type=float, default=20)
    ap.add_argument("--first-frame-timeout", type=float, default=30)
    a = ap.parse_args()

    url = os.environ.get("SPIKE_A_SRT_PLAY_URL")
    if not url:
        print("FATAL SPIKE_A_SRT_PLAY_URL is not set", flush=True)
        sys.exit(2)

    Gst.init(None)
    pipe = Gst.Pipeline.new("reader")
    src = Gst.ElementFactory.make("srtsrc", "src")
    src.set_property("uri", url)
    del url
    q0 = Gst.ElementFactory.make("queue", "q0")
    tee = Gst.ElementFactory.make("tee", "tee")
    qd = Gst.ElementFactory.make("queue", "qd")
    demux = Gst.ElementFactory.make("tsdemux", "demux")
    for e in (src, q0, tee, qd, demux):
        pipe.add(e)
    src.link(q0)
    q0.link(tee)
    tee.link(qd)
    qd.link(demux)

    t0 = time.monotonic()
    wall0 = time.time()
    st = {
        "v_arr": [], "v_pts": [], "v_wall": [], "kf": [], "a_arr": [], "dec": 0,
        "sei_seazn": 0, "sei_x264": 0, "sei_other": 0, "au_without_seazn": 0, "delays": [],
        "seqs": [], "sei_text_bad": 0, "nal_types": {}, "first_v": None, "dump_bytes": 0,
        "sample_texts": [],
    }
    lock = threading.Lock()

    if a.dump_ts:
        qf = Gst.ElementFactory.make("queue", "qf")
        fs = Gst.ElementFactory.make("filesink", "fs")
        fs.set_property("location", a.dump_ts)
        fs.set_property("sync", False)
        pipe.add(qf)
        pipe.add(fs)
        tee.link(qf)
        qf.link(fs)

        def dump_probe(_pad, info):
            if time.monotonic() - t0 > a.dump_seconds + (st["first_v"] or 0):
                return Gst.PadProbeReturn.DROP
            st["dump_bytes"] += info.get_buffer().get_size()
            return Gst.PadProbeReturn.OK

        qf.get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, dump_probe)

    def v_probe(_pad, info):
        b = info.get_buffer()
        now = time.monotonic()
        wall_ms = time.time_ns() / 1e6
        ok, mi = b.map(Gst.MapFlags.READ)
        data = bytes(mi.data) if ok else b""
        if ok:
            b.unmap(mi)
        seis, types = parse_seis(data)
        with lock:
            if st["first_v"] is None:
                st["first_v"] = now - t0
            st["v_arr"].append(now)
            st["v_pts"].append(b.pts)
            st["kf"].append(not b.has_flags(Gst.BufferFlags.DELTA_UNIT))
            for t in types:
                st["nal_types"][t] = st["nal_types"].get(t, 0) + 1
            got = False
            for uuid, body in seis:
                if uuid == SEAZN_UUID:
                    got = True
                    st["sei_seazn"] += 1
                    m = re.fullmatch(rb"wall_ms=(\d+);seq=(\d+)", body)
                    if m:
                        st["delays"].append(wall_ms - int(m.group(1)))
                        st["seqs"].append(int(m.group(2)))
                        if len(st["sample_texts"]) < 3:
                            st["sample_texts"].append(body.decode("ascii", "replace"))
                    else:
                        st["sei_text_bad"] += 1
                elif uuid == X264_UUID:
                    st["sei_x264"] += 1
                else:
                    st["sei_other"] += 1
            if not got:
                st["au_without_seazn"] += 1
        return Gst.PadProbeReturn.OK

    def a_probe(_pad, info):
        with lock:
            st["a_arr"].append(time.monotonic())
        return Gst.PadProbeReturn.OK

    def dec_probe(_pad, info):
        with lock:
            st["dec"] += 1
        return Gst.PadProbeReturn.OK

    def on_pad(_d, pad):
        caps = pad.get_current_caps() or pad.query_caps(None)
        name = caps.get_structure(0).get_name()
        if name.startswith("video/x-h264"):
            parse = Gst.ElementFactory.make("h264parse", None)
            cf = Gst.ElementFactory.make("capsfilter", None)
            cf.set_property("caps", Gst.Caps.from_string("video/x-h264,stream-format=byte-stream,alignment=au"))
            chain = [parse, cf]
            if a.decode:
                chain += [Gst.ElementFactory.make("avdec_h264", None)]
            sink = Gst.ElementFactory.make("fakesink", None)
            sink.set_property("sync", False)
            chain.append(sink)
            for e in chain:
                pipe.add(e)
            for x, y in zip(chain, chain[1:]):
                x.link(y)
            for e in chain:
                e.sync_state_with_parent()
            pad.link(parse.get_static_pad("sink"))
            cf.get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, v_probe)
            if a.decode:
                chain[2].get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, dec_probe)
        else:
            sink = Gst.ElementFactory.make("fakesink", None)
            sink.set_property("sync", False)
            pipe.add(sink)
            sink.sync_state_with_parent()
            pad.link(sink.get_static_pad("sink"))
            sink.get_static_pad("sink").add_probe(Gst.PadProbeType.BUFFER, a_probe)

    demux.connect("pad-added", on_pad)

    msgs = []
    end = {"reason": "timeout"}
    loop = GLib.MainLoop()
    srt_stats = []

    def on_msg(_bus, m):
        t = round(time.monotonic() - t0, 3)
        if m.type == Gst.MessageType.ERROR:
            e, dbg = m.parse_error()
            msgs.append({"t": t, "type": "error", "src": m.src.get_name(), "text": scrub(e.message), "debug": scrub((dbg or "")[:300])})
            end["reason"] = "error"
            loop.quit()
        elif m.type == Gst.MessageType.EOS:
            msgs.append({"t": t, "type": "eos"})
            end["reason"] = "eos"
            loop.quit()
        elif m.type == Gst.MessageType.WARNING:
            e, dbg = m.parse_warning()
            msgs.append({"t": t, "type": "warning", "src": m.src.get_name(), "text": scrub(e.message), "debug": scrub((dbg or "")[:300])})
        elif m.type == Gst.MessageType.ELEMENT and m.get_structure() is not None:
            msgs.append({"t": t, "type": "element", "src": m.src.get_name(), "name": m.get_structure().get_name()})
        return True

    pipe.get_bus().add_watch(GLib.PRIORITY_DEFAULT, on_msg)

    def tick():
        el = time.monotonic() - t0
        try:
            s = src.get_property("stats")
            if s is not None:
                srt_stats.append({"t": round(el, 1), "s": scrub(s.to_string()[:600])})
        except Exception:  # noqa: BLE001
            pass
        if el > a.seconds:
            loop.quit()
            return False
        if st["first_v"] is None and el > a.first_frame_timeout:
            end["reason"] = "no-first-frame"
            loop.quit()
            return False
        return True

    GLib.timeout_add(5000, tick)
    GLib.timeout_add(int(a.seconds * 1000) + 50, lambda: (loop.quit(), False)[1])
    print("READERSTART " + json.dumps({"tag": a.tag, "t": round(wall0, 3)}), flush=True)
    pipe.set_state(Gst.State.PLAYING)
    loop.run()
    stop_t = time.monotonic()
    threading.Thread(target=lambda: pipe.set_state(Gst.State.NULL), daemon=True).start()
    time.sleep(1)

    with lock:
        arr = st["v_arr"]
        gaps = [(arr[i] - arr[i - 1]) * 1000 for i in range(1, len(arr))]
        pts = [p for p in st["v_pts"] if p != Gst.CLOCK_TIME_NONE]
        pts_gaps = [(pts[i] - pts[i - 1]) / 1e6 for i in range(1, len(pts))]
        kf_idx = [i for i, k in enumerate(st["kf"]) if k]
        per_min = {}
        for i, g in enumerate(gaps):
            k = int((arr[i + 1] - arr[0]) // 60)
            per_min[k] = max(per_min.get(k, 0), g)
        seqs = st["seqs"]
        seq_missing = sum(max(0, seqs[i] - seqs[i - 1] - 1) for i in range(1, len(seqs)))
        seq_backwards = sum(1 for i in range(1, len(seqs)) if seqs[i] <= seqs[i - 1])
        a_arr = st["a_arr"]
        a_gaps = [(a_arr[i] - a_arr[i - 1]) * 1000 for i in range(1, len(a_arr))]
        res = {
            "tag": a.tag, "wall_start": round(wall0, 3), "end_reason": end["reason"],
            "ran_s": round(stop_t - t0, 3),
            "time_to_first_frame_s": None if st["first_v"] is None else round(st["first_v"], 3),
            "video_aus": len(arr),
            "video_fps_arrival": round((len(arr) - 1) / (arr[-1] - arr[0]), 3) if len(arr) > 1 else None,
            "gap_ms": stats(gaps),
            "gaps_over_ms": {str(th): sum(1 for g in gaps if g > th) for th in (100, 250, 500, 1000, 2000, 3000)},
            "max_gap_per_minute_ms": {str(k): round(v, 1) for k, v in sorted(per_min.items())},
            "pts_gap_ms": stats(pts_gaps),
            "keyframes": len(kf_idx),
            "keyframe_interval_frames": stats([kf_idx[i] - kf_idx[i - 1] for i in range(1, len(kf_idx))]),
            "audio_buffers": len(a_arr), "audio_gap_ms": stats(a_gaps),
            "decoded_frames": st["dec"] if a.decode else None,
            "sei": {
                "au_with_seazn_sei": st["sei_seazn"], "au_without_seazn_sei": st["au_without_seazn"],
                "x264_version_sei": st["sei_x264"], "other_uuid_sei": st["sei_other"],
                "text_unparseable": st["sei_text_bad"], "seq_missing": seq_missing,
                "seq_backwards": seq_backwards, "sample_texts": st["sample_texts"],
                "delay_ms": stats(st["delays"]),
            },
            "nal_type_counts": {str(k): v for k, v in sorted(st["nal_types"].items())},
            "dump_bytes": st["dump_bytes"] if a.dump_ts else None,
            "bus": msgs, "srt_stats_last": srt_stats[-1] if srt_stats else None,
        }
    print("RESULT " + json.dumps(res), flush=True)
    os._exit(0)


if __name__ == "__main__":
    main()
