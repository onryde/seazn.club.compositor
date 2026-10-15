#!/usr/bin/env python3
"""cefsrc probe (C00 item 1, THROWAWAY).

  cef_probe.py --url URL [--js-ready] [--seconds 30] [--composite] [--dump-dir D] [--tag T]

Builds  cefsrc url=URL max-video-framerate=30/1 listen-for-js-signals=<js-ready>
          ! video/x-raw,format=BGRA,width=1920,height=1080 ! tee
          -> appsink "raw"                      (arrival times, PTS, probe pixels)
          -> compositor over opaque black -> appsink "mixed"   (--composite: what blending does)
set_state(PLAYING) runs on a worker thread, because with listen-for-js-signals=true cefsrc's
start() blocks until the page signals ready. Teardown (set_state(NULL)) also runs on a worker
with a timeout, so a hang is reported instead of hanging the probe.
Prints `RESULT {json}` at the end. The URL is never printed (the page is ours or a public stg URL).
"""
import argparse
import json
import os
import statistics
import threading
import time

import gi

gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst  # noqa: E402

W, H = 1920, 1080
# (name, x, y, expected straight BGRA, expected premultiplied BGRA)
PROBES = [
    ("blue_opaque", 50, 50, (255, 0, 0, 255), (255, 0, 0, 255)),
    ("white50", 200, 350, (255, 255, 255, 128), (128, 128, 128, 128)),
    ("red50", 500, 350, (0, 0, 255, 128), (0, 0, 128, 128)),
    ("green25", 800, 350, (0, 255, 0, 64), (0, 64, 0, 64)),
    ("empty", 1500, 150, (0, 0, 0, 0), (0, 0, 0, 0)),
]


def pct(vals, q):
    s = sorted(vals)
    return s[min(len(s) - 1, int(round(q / 100 * (len(s) - 1))))] if s else None


def px(data, x, y):
    i = (y * W + x) * 4
    return tuple(data[i:i + 4])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--js-ready", action="store_true")
    ap.add_argument("--seconds", type=float, default=30.0)
    ap.add_argument("--wait-first", type=float, default=20.0, help="give up if no buffer by then")
    ap.add_argument("--composite", action="store_true")
    ap.add_argument("--dump-dir", default="")
    ap.add_argument("--dump-at", type=float, default=5.0)
    ap.add_argument("--teardown-timeout", type=float, default=10.0)
    ap.add_argument("--tag", default="")
    a = ap.parse_args()

    Gst.init(None)
    t0 = time.monotonic()
    wall0 = time.time()
    desc = (
        f'cefsrc name=cef url="{a.url}" max-video-framerate=30/1 '
        f'listen-for-js-signals={"true" if a.js_ready else "false"} '
        f"! video/x-raw,format=BGRA,width={W},height={H} ! tee name=t "
        "t. ! queue max-size-buffers=4 leaky=downstream ! appsink name=raw sync=false emit-signals=true max-buffers=4 drop=true "
    )
    if a.composite:
        desc += (
            "t. ! queue max-size-buffers=4 leaky=downstream ! mix.sink_1 "
            f"videotestsrc is-live=true pattern=black ! video/x-raw,format=BGRA,width={W},height={H},framerate=30/1 ! mix.sink_0 "
            "compositor name=mix background=black sink_0::zorder=0 sink_1::zorder=1 "
            f"! video/x-raw,format=BGRA,width={W},height={H},framerate=30/1 "
            "! appsink name=mixed sync=false emit-signals=true max-buffers=4 drop=true "
        )
    pipe = Gst.parse_launch(desc)

    st = {
        "arrivals": [], "pts": [], "first_buffer_s": None, "first_buffer_wall": None,
        "probes_raw": None, "probes_mixed": None, "dumped": [], "mixed_count": 0,
    }
    lock = threading.Lock()

    def on_raw(sink):
        s = sink.emit("pull-sample")
        b = s.get_buffer()
        now = time.monotonic()
        with lock:
            if b.get_size() != W * H * 4:
                st["bad_size"] = st.get("bad_size", 0) + 1
            if b.pts == Gst.CLOCK_TIME_NONE:
                st["no_pts"] = st.get("no_pts", 0) + 1
            st["last_buf"] = b
            if st["first_buffer_s"] is None:
                st["first_buffer_s"] = now - t0
                st["first_buffer_wall"] = time.time()
            st["arrivals"].append(now)
            st["pts"].append(b.pts)
            since = now - (t0 + st["first_buffer_s"])
            want = st["probes_raw"] is None and since >= a.dump_at
        if want:
            ok, mi = b.map(Gst.MapFlags.READ)
            if ok:
                data = bytes(mi.data)
                b.unmap(mi)
                st["probes_raw"] = {n: px(data, x, y) for n, x, y, _, _ in PROBES}
                if a.dump_dir:
                    p = os.path.join(a.dump_dir, f"{a.tag or 'cef'}-raw.bgra")
                    with open(p, "wb") as f:
                        f.write(data)
                    st["dumped"].append(p)
        return Gst.FlowReturn.OK

    def on_mixed(sink):
        s = sink.emit("pull-sample")
        b = s.get_buffer()
        with lock:
            st["mixed_count"] += 1
            fb = st["first_buffer_s"]
            want = st["probes_mixed"] is None and fb is not None and (time.monotonic() - t0 - fb) >= a.dump_at + 1
        if want:
            ok, mi = b.map(Gst.MapFlags.READ)
            if ok:
                data = bytes(mi.data)
                b.unmap(mi)
                st["probes_mixed"] = {n: px(data, x, y) for n, x, y, _, _ in PROBES}
                if a.dump_dir:
                    p = os.path.join(a.dump_dir, f"{a.tag or 'cef'}-mixed.bgra")
                    with open(p, "wb") as f:
                        f.write(data)
                    st["dumped"].append(p)
        return Gst.FlowReturn.OK

    pipe.get_by_name("raw").connect("new-sample", on_raw)
    if a.composite:
        pipe.get_by_name("mixed").connect("new-sample", on_mixed)

    msgs = []

    def on_msg(_bus, m):
        t = round(time.monotonic() - t0, 3)
        if m.type == Gst.MessageType.PROGRESS:
            typ, code, text = m.parse_progress()
            msgs.append({"t": t, "type": "progress", "ptype": typ.value_nick, "code": code, "text": text})
        elif m.type == Gst.MessageType.ERROR:
            e, dbg = m.parse_error()
            msgs.append({"t": t, "type": "error", "src": m.src.get_name(), "text": e.message, "debug": (dbg or "")[:240]})
        elif m.type == Gst.MessageType.WARNING:
            e, dbg = m.parse_warning()
            msgs.append({"t": t, "type": "warning", "src": m.src.get_name(), "text": e.message})
        elif m.type == Gst.MessageType.EOS:
            msgs.append({"t": t, "type": "eos"})
        elif m.type == Gst.MessageType.STATE_CHANGED and m.src == pipe:
            old, new, _ = m.parse_state_changed()
            msgs.append({"t": t, "type": "state", "from": old.value_nick, "to": new.value_nick})
        return True

    bus = pipe.get_bus()
    bus.add_watch(GLib.PRIORITY_DEFAULT, on_msg)

    play = {"returned_s": None, "ret": None}

    def do_play():
        r = pipe.set_state(Gst.State.PLAYING)
        play["ret"] = r.value_nick
        play["returned_s"] = round(time.monotonic() - t0, 3)

    loop = GLib.MainLoop()
    threading.Thread(target=do_play, daemon=True).start()

    def check():
        el = time.monotonic() - t0
        fb = st["first_buffer_s"]
        if fb is None and el > a.wait_first:
            loop.quit()
            return False
        if fb is not None and el > fb + a.seconds:
            loop.quit()
            return False
        return True

    GLib.timeout_add(100, check)
    loop.run()

    window_end = time.monotonic()
    td = {"returned": False, "seconds": None}

    def do_null():
        pipe.set_state(Gst.State.NULL)
        td["returned"] = True
        td["seconds"] = round(time.monotonic() - window_end, 3)

    th = threading.Thread(target=do_null, daemon=True)
    th.start()
    th.join(a.teardown_timeout)
    # drain bus messages posted during teardown
    ctx = GLib.MainContext.default()
    while ctx.iteration(False):
        pass

    if st["probes_raw"] is None and st.get("last_buf") is not None:
        # a static page paints rarely: probe (and dump) the last frame it did paint
        ok, mi = st["last_buf"].map(Gst.MapFlags.READ)
        if ok:
            data = bytes(mi.data)
            st["last_buf"].unmap(mi)
            st["probes_raw"] = {n: px(data, x, y) for n, x, y, _, _ in PROBES}
            st["probes_raw_from"] = "last frame"
            if a.dump_dir:
                p = os.path.join(a.dump_dir, f"{a.tag or 'cef'}-raw.bgra")
                with open(p, "wb") as f:
                    f.write(data)
                st["dumped"].append(p)
    arr = st["arrivals"]
    res = {"tag": a.tag, "js_ready": a.js_ready, "composite": a.composite, "wall_start": round(wall0, 3)}
    res["set_state_playing"] = play
    res["first_buffer_s"] = None if st["first_buffer_s"] is None else round(st["first_buffer_s"], 3)
    res["first_buffer_wall"] = st["first_buffer_wall"]
    res["buffers"] = len(arr)
    res["buffers_wrong_size"] = st.get("bad_size", 0)
    res["buffers_without_pts"] = st.get("no_pts", 0)
    if len(arr) >= 2:
        span = arr[-1] - arr[0]
        gaps = [(arr[i] - arr[i - 1]) * 1000 for i in range(1, len(arr))]
        per_sec = {}
        for x in arr:
            k = int(x - arr[0])
            per_sec[k] = per_sec.get(k, 0) + 1
        full = [per_sec.get(k, 0) for k in range(int(span))]
        pts = [p for p in st["pts"] if p != Gst.CLOCK_TIME_NONE]
        res.update({
            "window_s": round(span, 3),
            "fps_arrival_avg": round((len(arr) - 1) / span, 3) if span else None,
            "fps_pts_avg": round((len(pts) - 1) / ((pts[-1] - pts[0]) / 1e9), 3) if len(pts) > 1 and pts[-1] > pts[0] else None,
            "fps_per_second_min": min(full) if full else None,
            "seconds_below_29": sum(1 for c in full if c < 29),
            "gap_ms_p50": round(pct(gaps, 50), 2),
            "gap_ms_p99": round(pct(gaps, 99), 2),
            "gap_ms_max": round(max(gaps), 2),
            "gap_ms_mean": round(statistics.mean(gaps), 2),
        })
    res["probes_raw"] = st["probes_raw"]
    res["probes_raw_from"] = st.get("probes_raw_from", f"frame at +{a.dump_at}s")
    res["probes_mixed"] = st["probes_mixed"]
    res["mixed_buffers"] = st["mixed_count"]
    res["expected"] = {n: {"straight": s, "premultiplied": p} for n, _, _, s, p in PROBES}
    res["dumped"] = st["dumped"]
    res["bus"] = msgs
    res["teardown"] = td
    print("RESULT " + json.dumps(res), flush=True)
    os._exit(0)


if __name__ == "__main__":
    main()
