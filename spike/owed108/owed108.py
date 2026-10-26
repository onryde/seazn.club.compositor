#!/usr/bin/env python3
"""Owed 108 (docs/_INDEX.md): a publisher drops and a new one starts on a real Cloudflare live input. How
long until the input's SRT playback decodes again, and what each of the relay's input connects in that
window sees. A measurement probe, not product code: nothing in crates/ depends on it.

  owed108.py pub [--ingest srt|rtmps]
      The publisher, a child of `run` (killed with SIGKILL, as a force-quit phone is: SRT is UDP, so
      nothing reaches Cloudflare). F-002's feed, which mimics the capture app: videotestsrc 1280x720@30,
      x264 zerolatency/veryfast 3000 kbps, a keyframe every 60 frames, no B-frames, a 440 Hz AAC 128k
      tone, mpegtsmux alignment=7, srtsink. Its uri is SPIKE_A_SRT_PUSH_URL, set as a property.
      rtmps: flvmux -> rtmp2sink to SPIKE_A_RTMPS_PUSH_URL; "connected" is 100 kB written.

  owed108.py run --part probes|relay [--trials 3] [--warm 45] [--gaps 5.5[,g2,...]] [--after 60[,a2,...]]
                 [--cooldown 240] [--ingest srt|rtmps] [--links base[,close_rebuild,cap2]]
                 [--drop kill|clean] [--hls FILE] [--note TEXT]
      Each trial: publisher P1 up; at --warm s P1 is killed; a gap later publisher P2 starts (and is
      started again a second after any exit, as the app retries). The trial ends --after s past the kill.
      A negative gap starts no P2. --gaps and --after take one value per trial (the last one repeats).
      --drop clean ends each publisher with SIGTERM instead: it sets its pipeline to NULL, which closes
      the SRT socket and so sends SRT's shutdown (SIGKILL after 3 s, reported, if it has not exited).
      --hls FILE polls the input's HLS master and first variant once a second from each trial's start,
      as R0 section 4a did (seazn.club R0-memo.md): both HTTP statuses, ENDLIST, the media-sequence head
      and an 8-hex hash of the variant path. FILE holds the URL (a public playback URL, uploaded to the
      Machine as a file); it is never printed, and any line holding it or its host or uid is withheld.
      --note is recorded in run_start (the input's recording settings, which this probe cannot read).
      probes: a warm reader R0 from 15 s that is never rebuilt, a burst of 8 simultaneous readers before
              the kill (a control), and a fresh reader every second from 5 s before the kill to the
              end, each living until its first decoded frame or --probe-life s.
      relay:  one reader per --links policy under the relay's own input rules
              (crates/domain/src/input_link.rs with Thresholds::default): stall 5 s, connect timeout
              5 s, back-off 1, 2, 4, 8, 10 s, an abandoned generation left running (gate shut) until the
              next build retires it. close_rebuild: an error on a handshaked current generation (also
              after abandon) rebuilds at the next tick, keeping the attempt count; cap2: back-off cap 2 s.
      F-032 (_FINDINGS.md) has the runs, their command lines and what they found.
      Every reader is built as adapters::gst_input builds the relay's input: srtsrc (auto-reconnect off,
      mode=caller, latency 1000) -> tsdemux -> leaky 1 s queue -> h264parse -> avdec_h264
      (output-corrupt off) -> videoconvertscale -> I420 1280x720 -> appsink; and the audio branch.
      Its uri is SPIKE_A_SRT_PLAY_URL's three parts, rebuilt as GstInput::srt_uri does.

Every line is JSON on stdout. A line holding any form of either URL's credentials is withheld and counted
(raw, percent-encoded in either case). GST_DEBUG must stay unset (srtobject:5 prints both raw, Owed 47).
"""
import argparse
import json
import os
import signal
import hashlib
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

import gi

gi.require_version("Gst", "1.0")
from gi.repository import GLib, Gst  # noqa: E402

PUSH_ENV = "SPIKE_A_SRT_PUSH_URL"
PLAY_ENV = "SPIKE_A_SRT_PLAY_URL"
RTMPS_ENV = "SPIKE_A_RTMPS_PUSH_URL"
HLS_ENV = "SPIKE_A_HLS_URL"
UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0 Safari/537.36"
CLEAN_GRACE_S = 3

# Restated from Thresholds::default() (crates/domain/src/config.rs); a probe cannot link the crate.
STALL_MS = 5_000  # input_stall_after_ms
CONNECT_TIMEOUT_MS = 5_000  # input_connect_timeout_ms
BACKOFF_BASE_MS = 1_000  # input_backoff_base_ms
BACKOFF_CAP_MS = 10_000  # input_backoff_cap_ms
SRT_LATENCY_MS = 1_000  # input_srt_latency_ms
TICK_MS = 33  # the supervisor ticks 30 times a second
# adapters::gst_input's bounds.
VIDEO_QUEUE_NS = 1_000_000_000
AUDIO_QUEUE_NS = 400_000_000
W, H = 1280, 720

T0 = time.monotonic()
LOCK = threading.Lock()
STATE = {"withheld": 0, "lines": 0, "forms": []}


def now_ms():
    return round((time.monotonic() - T0) * 1000, 1)


def encode(v, lower):
    out = []
    for b in v.encode():
        c = chr(b)
        if c.isascii() and (c.isalnum() or c in "-._~"):
            out.append(c)
        else:
            out.append(f"%{b:02x}" if lower else f"%{b:02X}")
    return "".join(out)


def split(full):
    """`srt://host:port?passphrase=…&streamid=…` into (origin, stream id, passphrase), percent-decoded."""
    origin, _, query = full.partition("?")
    sid = pw = ""
    for pair in query.split("&"):
        k, _, v = pair.partition("=")
        if k == "streamid":
            sid = urllib.parse.unquote(v)
        elif k == "passphrase":
            pw = urllib.parse.unquote(v)
    return origin, sid, pw


def srt_uri(origin, sid, pw):
    """GstInput::srt_uri: mode=caller, each credential RFC 3986 upper-case encoded, then the latency."""
    uri = f"{origin}?mode=caller"
    if sid:
        uri += "&streamid=" + encode(sid, False)
    if pw:
        uri += "&passphrase=" + encode(pw, False)
    return uri + f"&latency={SRT_LATENCY_MS}"


def load_forms():
    forms = set()
    for name in (PUSH_ENV, PLAY_ENV):
        full = os.environ.get(name, "")
        if not full:
            continue
        origin, sid, pw = split(full)
        forms.update([full, urllib.parse.unquote(full), srt_uri(origin, sid, pw)])
        _, _, query = full.partition("?")
        for pair in query.split("&"):
            forms.add(pair.partition("=")[2])
        for v in (sid, pw):
            forms.update([v, encode(v, False), encode(v, True)])
    rtmps = os.environ.get(RTMPS_ENV, "")
    if rtmps:
        forms.update([rtmps, urllib.parse.unquote(rtmps)])
        key = rtmps.split("/live/", 1)[-1] if "/live/" in rtmps else ""
        forms.update([key, encode(key, False), encode(key, True)])
    hls = os.environ.get(HLS_ENV, "")
    if hls:
        parts = urllib.parse.urlsplit(hls)
        forms.update([hls, parts.hostname or ""] + [seg for seg in parts.path.split("/") if len(seg) >= 8])
    STATE["forms"] = sorted((f for f in forms if len(f) >= 8), key=len, reverse=True)


def emit(kind, **kw):
    rec = {"t_ms": now_ms(), "wall": round(time.time(), 3), "kind": kind}
    rec.update(kw)
    line = json.dumps(rec)
    with LOCK:
        STATE["lines"] += 1
        if any(f in line for f in STATE["forms"]):
            STATE["withheld"] += 1
            line = json.dumps({"t_ms": rec["t_ms"], "kind": "withheld", "of": kind})
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def fields(st):
    if st is None:
        return {}
    out = {}
    for i in range(st.n_fields()):
        k = st.nth_field_name(i)
        v = st.get_value(k)
        if isinstance(v, (int, float, str, bool)):
            out[k] = v
    return out


def chain(*els):
    for x, y in zip(els, els[1:]):
        if not x.link(y):
            raise RuntimeError(f"could not link {x.get_name()} to {y.get_name()}")


def guarded(fn):
    """A main-loop callback that raises ends the run (exit 4), so a bug cannot pass as a measurement."""
    def wrapper(*args):
        try:
            return fn(*args)
        except Exception:  # noqa: BLE001 - reported, then the process exits
            import traceback
            emit("fatal", what="callback raised", trace=traceback.format_exc()[-1500:])
            os._exit(4)
    return wrapper


def later(ms, fn):
    """GLib.timeout_add with a guarded one-shot callback."""
    def once():
        fn()
        return False
    GLib.timeout_add(int(ms), guarded(once))


def make(factory, name=None):
    e = Gst.ElementFactory.make(factory, name)
    if e is None:
        raise RuntimeError(f"no element {factory}")
    return e


# ---------------------------------------------------------------------------------------------- publisher

def pub(ingest):
    load_forms()
    env = RTMPS_ENV if ingest == "rtmps" else PUSH_ENV
    url = os.environ.get(env, "")
    if not url:
        emit("fatal", what=f"{env} is not set")
        return 2
    Gst.init(None)
    tail = ("flvmux name=mux streamable=true ! rtmp2sink name=sink async=false" if ingest == "rtmps" else
            "mpegtsmux name=mux alignment=7 ! srtsink name=sink wait-for-connection=false async=false")
    p = Gst.parse_launch(
        "videotestsrc is-live=true pattern=ball ! video/x-raw,width=1280,height=720,framerate=30/1 ! videoconvert "
        "! x264enc tune=zerolatency speed-preset=veryfast bitrate=3000 key-int-max=60 bframes=0 "
        "! video/x-h264,profile=main ! h264parse ! queue ! mux. "
        "audiotestsrc is-live=true wave=sine freq=440 volume=0.2 ! audioconvert ! audioresample "
        "! audio/x-raw,rate=48000,channels=2 ! voaacenc bitrate=128000 ! aacparse ! queue ! mux. " + tail)
    sink = p.get_by_name("sink")
    sink.set_property("location" if ingest == "rtmps" else "uri", url)
    del url
    loop = GLib.MainLoop()
    seen = {"connected": False, "last": 0.0}

    def on_msg(_bus, m):
        if m.type in (Gst.MessageType.ERROR, Gst.MessageType.WARNING):
            err, dbg = m.parse_error() if m.type == Gst.MessageType.ERROR else m.parse_warning()
            emit("pub_" + ("error" if m.type == Gst.MessageType.ERROR else "warning"),
                 src=m.src.get_name(), text=err.message, debug=dbg or "")
            if m.type == Gst.MessageType.ERROR:
                loop.quit()
        elif m.type == Gst.MessageType.EOS:
            emit("pub_eos")
            loop.quit()
        return True

    def poll():
        f = fields(sink.get_property("stats"))
        if ingest == "rtmps" and f.get("out-bytes-total", 0) >= 100_000:
            f["rtt-ms"] = -1  # rtmp2sink has no rtt: 100 kB written (past the handshake) stands for the connect
        if not seen["connected"] and "rtt-ms" in f:
            seen["connected"] = True
            emit("pub_connected", stats=f)
        elif seen["connected"] and time.monotonic() - seen["last"] >= 5:
            seen["last"] = time.monotonic()
            emit("pub_stats", stats={k: f[k] for k in ("bytes-sent", "packets-sent", "rtt-ms", "out-bytes-acked", "out-bytes-total") if k in f})
        return True

    def on_term():
        # --drop clean: NULL closes srtsink's socket (srt_close), which sends SRT's shutdown to the peer.
        t = time.monotonic()
        emit("pub_closing")
        p.set_state(Gst.State.NULL)
        emit("pub_closed", close_ms=round((time.monotonic() - t) * 1000, 1))
        loop.quit()
        return False

    p.get_bus().add_watch(GLib.PRIORITY_DEFAULT, on_msg)
    GLib.timeout_add(50, guarded(poll))
    GLib.unix_signal_add(GLib.PRIORITY_HIGH, signal.SIGTERM, guarded(on_term))
    emit("pub_start")
    p.set_state(Gst.State.PLAYING)
    loop.run()
    p.set_state(Gst.State.NULL)
    return 1


class Publisher:
    """A publisher child process; `respawn` starts it again a second after any exit, until `stop`."""

    def __init__(self, tag, respawn, ingest="srt"):
        self.tag, self.respawn, self.ingest = tag, respawn, ingest
        self.proc, self.alive, self.spawns = None, True, 0
        self.connected_at = None
        self.spawn()

    def spawn(self):
        if not self.alive:
            return False
        self.spawns += 1
        self.proc = subprocess.Popen([sys.executable, "-u", os.path.abspath(__file__), "pub", "--ingest", self.ingest],
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True,
                                     text=True)
        emit("pub_spawn", pub=self.tag, n=self.spawns, pid=self.proc.pid)
        threading.Thread(target=self.read, args=(self.proc,), daemon=True).start()
        return False

    def read(self, proc):
        for line in proc.stdout:
            line = line.rstrip("\n")
            try:
                rec = json.loads(line)
            except ValueError:
                rec = {"kind": "pub_text", "text": line[:400]}
            kind = rec.pop("kind", "pub_text")
            rec.pop("t_ms", None)
            rec.pop("wall", None)
            if kind == "pub_connected" and self.connected_at is None:
                self.connected_at = now_ms()
            emit(kind, pub=self.tag, **rec)
        rc = proc.wait()
        emit("pub_exit", pub=self.tag, rc=rc)
        if self.alive and self.respawn:
            GLib.timeout_add(1000, guarded(self.spawn))

    def kill(self, clean=False):
        """SIGKILL to the child's process group: a force-quit. clean: SIGTERM, so the child closes its SRT
        socket (SRT's shutdown sent); SIGKILL after CLEAN_GRACE_S if it is still running, reported."""
        self.alive = False
        proc = self.proc
        if not (proc and proc.poll() is None):
            return
        if not clean:
            os.killpg(proc.pid, signal.SIGKILL)
            emit("pub_killed", pub=self.tag, pid=proc.pid, how="kill")
            return
        os.kill(proc.pid, signal.SIGTERM)
        emit("pub_killed", pub=self.tag, pid=proc.pid, how="clean")

        def force():
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGKILL)
                emit("pub_clean_timeout", pub=self.tag, pid=proc.pid)
        later(CLEAN_GRACE_S * 1000, force)


class HlsPoll:
    """The input's HLS master, then its first variant, once a second (R0 section 4a). Prints statuses,
    ENDLIST, the media-sequence head (sequence + segments) and an 8-hex hash of the variant path."""

    def __init__(self, url, trial, every=1.0):
        self.url, self.trial, self.every, self.alive = url, trial, every, True
        threading.Thread(target=self.loop, daemon=True).start()

    @staticmethod
    def get(url):
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        try:
            with urllib.request.urlopen(req, timeout=4) as r:
                return r.status, r.read().decode("utf-8", "replace")
        except urllib.error.HTTPError as e:
            return e.code, ""
        except Exception as e:  # noqa: BLE001 - the class only: a message could carry the URL
            return type(e).__name__, ""

    def loop(self):
        while self.alive:
            t = time.monotonic()
            ms, mbody = self.get(self.url)
            rec = {"trial": self.trial, "master": ms}
            if ms == 200:
                v = next((ln.strip() for ln in mbody.splitlines() if ln.strip() and not ln.startswith("#")), None)
                if v:
                    vurl = urllib.parse.urljoin(self.url, v)
                    rec["variant_id"] = hashlib.sha1(urllib.parse.urlsplit(vurl).path.encode()).hexdigest()[:8]
                    vs, vbody = self.get(vurl)
                    rec["variant"] = vs
                    if vs == 200:
                        seq = next((int(ln.split(":", 1)[1]) for ln in vbody.splitlines()
                                    if ln.startswith("#EXT-X-MEDIA-SEQUENCE:")), 0)
                        rec["head"] = seq + sum(1 for ln in vbody.splitlines() if ln.startswith("#EXTINF"))
                        rec["endlist"] = "#EXT-X-ENDLIST" in vbody
            if self.alive:
                emit("hls", **rec)
            time.sleep(max(0.0, self.every - (time.monotonic() - t)))

    def stop(self):
        self.alive = False


# ---------------------------------------------------------------------------------------------- reader

class Reader:
    """One input build, as adapters::gst_input makes it. Callbacks run on the main loop."""

    def __init__(self, tag, uri, on_frame=None, on_first=None, on_lost=None, gaps=False, on_late_error=None):
        self.tag = tag
        self.on_frame, self.on_first, self.on_lost = on_frame, on_first, on_lost
        self.on_late_error = on_late_error
        self.gaps = gaps
        self.rec = {"tag": tag, "built": now_ms(), "handshake": None, "first_byte": None, "first_au": None,
                    "first_frame": None, "frames": 0, "aus": 0, "bytes": 0, "audio": 0, "errors": [],
                    "warnings": [], "eos": None, "ended": None, "end": None, "resumes": []}
        self.last_frame = None
        self.closed = self.torn = False
        self.pipe = Gst.Pipeline.new(tag)
        self.src = make("srtsrc", "src")
        self.src.set_property("uri", uri)
        self.src.set_property("auto-reconnect", False)
        demux = make("tsdemux", "demux")
        vq = make("queue", "vqueue")
        Gst.util_set_object_arg(vq, "leaky", "downstream")
        vq.set_property("max-size-time", VIDEO_QUEUE_NS)
        vparse = make("h264parse", "vparse")
        vdec = make("avdec_h264", "vdec")
        vdec.set_property("output-corrupt", False)
        scale = make("videoconvertscale", "scale")
        scale.set_property("add-borders", True)
        vcaps = make("capsfilter", "vcaps")
        vcaps.set_property("caps", Gst.Caps.from_string(
            f"video/x-raw,format=I420,width={W},height={H},pixel-aspect-ratio=1/1,colorimetry=bt709"))
        vsink = self.appsink("vsink")
        aq = make("queue", "aqueue")
        Gst.util_set_object_arg(aq, "leaky", "downstream")
        aq.set_property("max-size-time", AUDIO_QUEUE_NS)
        aparse, adec = make("aacparse", "aparse"), make("avdec_aac", "adec")
        aconv, ares = make("audioconvert", "aconvert"), make("audioresample", "aresample")
        acaps = make("capsfilter", "acaps")
        acaps.set_property("caps", Gst.Caps.from_string(
            "audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved"))
        asink = self.appsink("asink")
        for e in (self.src, demux, vq, vparse, vdec, scale, vcaps, vsink, aq, aparse, adec, aconv, ares, acaps, asink):
            self.pipe.add(e)
        chain(self.src, demux)
        chain(vq, vparse, vdec, scale, vcaps, vsink)
        chain(aq, aparse, adec, aconv, ares, acaps, asink)

        def pad_added(_d, pad):
            name = pad.get_name()
            target = vq if name.startswith("video") else aq if name.startswith("audio") else None
            if target is not None and not target.get_static_pad("sink").is_linked():
                pad.link(target.get_static_pad("sink"))

        demux.connect("pad-added", guarded(pad_added))
        self.src.get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, self.byte_probe)
        vparse.get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, self.au_probe)
        vsink.connect("new-sample", guarded(self.video_sample))
        asink.connect("new-sample", guarded(self.audio_sample))
        self.watch = self.pipe.get_bus().add_watch(GLib.PRIORITY_DEFAULT, guarded(self.on_bus))
        self.poll_id = GLib.timeout_add(100, guarded(self.poll))
        emit("build", reader=tag)
        threading.Thread(target=self.pipe.set_state, args=(Gst.State.PLAYING,), daemon=True).start()

    @staticmethod
    def appsink(name):
        s = make("appsink", name)
        s.set_property("max-buffers", 2)
        s.set_property("drop", True)
        s.set_property("sync", False)
        s.set_property("async", False)
        s.set_property("emit-signals", True)
        return s

    def byte_probe(self, _pad, info):
        b = info.get_buffer()
        with LOCK:
            self.rec["bytes"] += b.get_size()
            first = self.rec["first_byte"] is None
            if first:
                self.rec["first_byte"] = now_ms()
        if first:
            GLib.idle_add(guarded(lambda: emit("first_byte", reader=self.tag) and False))
        return Gst.PadProbeReturn.OK

    def au_probe(self, _pad, _info):
        with LOCK:
            self.rec["aus"] += 1
            if self.rec["first_au"] is None:
                self.rec["first_au"] = now_ms()
        return Gst.PadProbeReturn.OK

    def video_sample(self, sink):
        sink.emit("pull-sample")
        t = now_ms()
        with LOCK:
            self.rec["frames"] += 1
            first = self.rec["first_frame"] is None
            if first:
                self.rec["first_frame"] = t
            gap = None if self.last_frame is None else t - self.last_frame
            self.last_frame = t
        if first:
            GLib.idle_add(guarded(self.first_frame), t)
        elif gap is not None and gap > 500:
            self.rec["resumes"].append({"t": t, "gap_ms": round(gap, 1)})
            if self.gaps:
                GLib.idle_add(guarded(lambda: emit("frames_resume", reader=self.tag, gap_ms=round(gap, 1)) and False))
        if self.on_frame:
            self.on_frame(self, t)
        return Gst.FlowReturn.OK

    def first_frame(self, t):
        emit("first_frame", reader=self.tag, after_build_ms=round(t - self.rec["built"], 1))
        if self.on_first and not self.closed:
            self.on_first(self, t)
        return False

    def audio_sample(self, sink):
        sink.emit("pull-sample")
        with LOCK:
            self.rec["audio"] += 1
        return Gst.FlowReturn.OK

    def poll(self):
        # Until the teardown, not the gate: an abandoned generation's late handshake is still a fact.
        if self.torn:
            return False
        f = fields(self.src.get_property("stats"))
        if self.rec["handshake"] is None and "rtt-ms" in f:
            self.rec["handshake"] = now_ms()
            emit("handshake", reader=self.tag, after_build_ms=round(self.rec["handshake"] - self.rec["built"], 1),
                 rtt_ms=f.get("rtt-ms"))
        return True

    def on_bus(self, _bus, m):
        if m.type in (Gst.MessageType.ERROR, Gst.MessageType.WARNING):
            is_err = m.type == Gst.MessageType.ERROR
            err, dbg = m.parse_error() if is_err else m.parse_warning()
            item = {"t": now_ms(), "src": m.src.get_name(), "text": err.message, "debug": (dbg or "")[:600]}
            self.rec["errors" if is_err else "warnings"].append(item)
            emit("error" if is_err else "warning", reader=self.tag, **item)
            if is_err and self.on_lost and not self.closed:
                self.on_lost(self, "Error")
            elif is_err and self.closed and self.on_late_error and len(self.rec["errors"]) == 1:
                self.on_late_error(self)
        elif m.type == Gst.MessageType.EOS:
            self.rec["eos"] = now_ms()
            emit("eos", reader=self.tag)
            if self.on_lost and not self.closed:
                self.on_lost(self, "Eos")
        return True

    def close(self, why):
        """Shut the gate: nothing of this build reaches anyone again (its pipeline may keep running)."""
        if not self.closed:
            self.closed = True
            self.rec["ended"] = why
            self.rec["end"] = now_ms()
            emit("closed", reader=self.tag, why=why)

    def teardown(self):
        if self.torn:
            return
        self.torn = True
        self.close(self.rec["ended"] or "retired")
        self.pipe.get_bus().remove_watch()
        threading.Thread(target=self.pipe.set_state, args=(Gst.State.NULL,), daemon=True).start()


# ---------------------------------------------------------------------------------------------- the relay's rules

class Link:
    """crates/domain/src/input_link.rs: Backoff -> Connecting -> Running, failure f waits delay(f - 1)."""

    def __init__(self, uri, log, policy="base"):
        """policy: base (the domain today); close_rebuild (an error on a generation whose SRT handshake
        completed rebuilds at the next tick, keeping the attempt count: Cloudflare's close of a dead
        publisher's playback is the moment it serves again); cap2 (back-off capped at 2 s)."""
        self.uri, self.log, self.policy = uri, log, policy
        self.gen, self.state, self.current = 0, ("backoff", 0, 0.0), None
        self.last_frame = None
        self.on_air = []
        self.tick_id = GLib.timeout_add(TICK_MS, guarded(self.tick))

    def delay(self, attempt):
        cap = 2_000 if self.policy == "cap2" else BACKOFF_CAP_MS
        return min(BACKOFF_BASE_MS * (2 ** attempt), cap)

    def back_off(self, failed_before, why):
        wait = self.delay(failed_before)
        self.state = ("backoff", failed_before + 1, now_ms() + wait)
        emit("link", gen=self.gen, to="backoff", why=why, attempt=failed_before + 1, wait_ms=wait, policy=self.policy)
        if why == "lost_Error" and self.policy == "close_rebuild" and self.current is not None \
                and self.current.rec["handshake"] is not None:
            self.state = ("backoff", failed_before + 1, now_ms())
            emit("link", gen=self.gen, to="backoff", why="close_rebuild", attempt=failed_before + 1, wait_ms=0,
                 policy=self.policy)

    def tick(self):
        t = now_ms()
        st = self.state
        if st[0] == "backoff" and t >= st[2]:
            if self.current is not None:
                self.current.teardown()
                self.log.append(self.current.rec)
            self.gen += 1
            self.state = ("connecting", st[1], t)
            emit("link", gen=self.gen, to="connecting", attempt=st[1], policy=self.policy)
            self.current = Reader(f"{self.policy}.gen{self.gen}", self.uri, on_frame=self.frame,
                                  on_first=self.connected, on_lost=self.lost, on_late_error=self.late_error)
        elif st[0] == "connecting" and t - st[2] >= CONNECT_TIMEOUT_MS:
            self.current.close("connect_timeout")
            self.back_off(st[1], "connect_timeout")
        elif st[0] == "running":
            fresh = max(st[1], self.last_frame or st[1])
            if t - fresh >= STALL_MS:
                self.current.close("stalled")
                self.back_off(0, "stalled")
        return True

    def frame(self, reader, t):
        if reader is self.current and not reader.closed:
            if self.last_frame is None or t - self.last_frame > 500:
                self.on_air.append({"t": t, "gen": self.gen})
            self.last_frame = t

    def connected(self, reader, t):
        if reader is self.current and self.state[0] == "connecting":
            self.state = ("running", t)
            emit("link", gen=self.gen, to="running", after_build_ms=round(t - reader.rec["built"], 1),
                 policy=self.policy)

    def lost(self, reader, reason):
        if reader is not self.current or reader.closed:
            return
        failed_before = 0 if self.state[0] == "running" else self.state[1] if self.state[0] == "connecting" else None
        reader.close("lost_" + reason)
        if failed_before is not None:
            self.back_off(failed_before, "lost_" + reason)

    def late_error(self, reader):
        """An error after the gate shut (an abandoned generation, still running in the back-off)."""
        if (self.policy == "close_rebuild" and reader is self.current and self.state[0] == "backoff"
                and reader.rec["handshake"] is not None):
            self.state = ("backoff", self.state[1], now_ms())
            emit("link", gen=self.gen, to="backoff", why="close_rebuild", attempt=self.state[1], wait_ms=0,
                 policy=self.policy)

    def running_fresh(self):
        return self.state[0] == "running" and self.last_frame is not None and now_ms() - self.last_frame < 1000

    def stop(self):
        GLib.source_remove(self.tick_id)
        if self.current is not None:
            self.current.teardown()
            self.log.append(self.current.rec)
            self.current = None


# ---------------------------------------------------------------------------------------------- a run

def run(a):
    if a.hls:
        with open(a.hls) as f:
            os.environ[HLS_ENV] = f.read().strip()
    load_forms()
    play = os.environ.get(PLAY_ENV, "")
    if not play or not os.environ.get(PUSH_ENV):
        emit("fatal", what=f"{PLAY_ENV} and {PUSH_ENV} must both be set")
        return 2
    if a.hls and not os.environ.get(HLS_ENV):
        emit("fatal", what=f"--hls needs {HLS_ENV}")
        return 2
    if os.environ.get("GST_DEBUG"):
        emit("fatal", what="GST_DEBUG is set: srtobject's debug prints both credentials (Owed 47)")
        return 2
    origin, sid, pw = split(play)
    uri = srt_uri(origin, sid, pw)
    del play
    Gst.init(None)
    gaps = [float(g) for g in a.gaps.split(",")]
    gaps = (gaps + gaps[-1:] * a.trials)[: a.trials]
    afters = [float(x) for x in a.after.split(",")]
    afters = (afters + afters[-1:] * a.trials)[: a.trials]
    emit("run_start", part=a.part, ingest=a.ingest, links=a.links, trials=a.trials, warm_s=a.warm, gaps_s=gaps, after_s=afters,
         drop=a.drop, hls=bool(a.hls), note=a.note,
         cooldown_s=a.cooldown, probe_life_s=a.probe_life, probe_every_s=a.probe_every, burst=a.burst,
         burst_life_s=a.burst_life, gst=Gst.version_string(),
         origin=urllib.parse.urlsplit(origin).hostname, streamid_chars=len(sid), passphrase_chars=len(pw),
         thresholds={"stall_ms": STALL_MS, "connect_timeout_ms": CONNECT_TIMEOUT_MS,
                     "backoff_base_ms": BACKOFF_BASE_MS, "backoff_cap_ms": BACKOFF_CAP_MS,
                     "srt_latency_ms": SRT_LATENCY_MS})
    loop = GLib.MainLoop()
    rc = {"code": 0}

    def trial(i):
        if i >= a.trials:
            emit("run_end", lines=STATE["lines"], withheld=STATE["withheld"])
            loop.quit()
            return False
        gap = gaps[i]
        tr = {"i": i + 1, "gap": gap, "t0": now_ms(), "readers": [], "probes": [], "bursts": [], "log": []}
        emit("trial_start", trial=i + 1, gap_s=gap, after_s=afters[i], drop=a.drop)
        if a.hls:
            tr["hls"] = HlsPoll(os.environ[HLS_ENV], i + 1)
        tr["p1"] = Publisher("P1", respawn=True, ingest=a.ingest)
        def start_r0():
            # R0 is never rebuilt after the kill; one that fails BEFORE the kill (the burst-style close of a
            # fresh reader) is rebuilt a second later, so the trial still has a warm reader at the kill.
            def lost(rd, why):
                if "kill" in tr:
                    return
                rd.close("lost_" + why)
                rd.teardown()
                emit("r0_rebuild", trial=i + 1, why=why)
                later(1000, start_r0)
            tr["r0"] = Reader("R0", uri, gaps=True, on_lost=lost)

        if a.part == "probes":
            later(15_000, start_r0)
        else:
            policies = a.links.split(",")
            later(15_000, lambda: tr.__setitem__("links", [Link(uri, tr["log"], pol) for pol in policies]))

        def kill():
            warm_ok = (tr["r0"].last_frame is not None and now_ms() - tr["r0"].last_frame < 1000) \
                if a.part == "probes" else all(lk.running_fresh() for lk in tr["links"])
            tr["kill"] = now_ms()
            tr["p1"].kill(clean=a.drop == "clean")
            emit("kill", trial=i + 1, warm_ok=warm_ok, how=a.drop)
            if not warm_ok:
                emit("trial_invalid", trial=i + 1, why="no frame in the last second at the kill")
                rc["code"] = 3
            if gap >= 0:
                GLib.timeout_add(int(gap * 1000), guarded(republish))
            GLib.timeout_add(int(afters[i] * 1000), guarded(end))
            return False

        def republish():
            tr["p2_spawn"] = now_ms()
            tr["p2"] = Publisher("P2", respawn=True, ingest=a.ingest)
            return False

        def probe(kind="probe", into="probes", life=None):
            if "ended" in tr:
                return False
            n = len(tr[into]) + 1
            r = Reader(f"{kind}{n}", uri, on_first=lambda rd, _t: rd.close("first_frame") or rd.teardown())
            tr[into].append(r)
            later((life or a.probe_life) * 1000, lambda: r.closed or (r.close("life"), r.teardown()))
            return True

        def burst():
            # The concurrency control: --burst readers at once, while P1 is still up. If Cloudflare
            # capped concurrent playback readers, some of these would get nothing.
            for _ in range(a.burst):
                probe("burst", "bursts", a.burst_life)

        def end():
            tr["ended"] = now_ms()
            for p in ("p1", "p2"):
                if p in tr:
                    tr[p].kill(clean=a.drop == "clean")
            if "hls" in tr:
                tr["hls"].stop()
            if a.part == "probes":
                tr["r0"].teardown()
                for r in tr["probes"] + tr["bursts"]:
                    if not r.closed:
                        r.close("trial_end")
                        r.teardown()
            else:
                tr["on_air"] = {lk.policy: lk.on_air for lk in tr["links"]}
                for lk in tr["links"]:
                    lk.stop()
            summarise(a.part, tr)
            GLib.timeout_add(int((a.cooldown if i + 1 < a.trials else 1) * 1000), guarded(lambda: trial(i + 1)))
            return False

        GLib.timeout_add(int(a.warm * 1000), guarded(kill))
        if a.part == "probes":
            if a.burst:
                later((a.warm - 5 - a.burst_life - 1) * 1000, burst)
            GLib.timeout_add(int((a.warm - 5) * 1000), guarded(
                lambda: GLib.timeout_add(int(a.probe_every * 1000), guarded(probe)) and probe() and False))
        return False

    GLib.idle_add(guarded(lambda: trial(0)))
    loop.run()
    return rc["code"]


def summarise(part, tr):
    kill = tr["kill"]
    p2 = tr.get("p2")
    out = {"trial": tr["i"], "gap_s": tr["gap"], "kill_ms": kill,
           "p2_spawn_rel_kill_ms": round(tr["p2_spawn"] - kill, 1) if "p2_spawn" in tr else None,
           "p2_spawns": p2.spawns if p2 else 0,
           "p2_connected_rel_kill_ms": round(p2.connected_at - kill, 1) if p2 and p2.connected_at else None}

    def rel(v):
        return None if v is None else round(v - kill, 1)

    def row(rec):
        return {"tag": rec["tag"], "built": rel(rec["built"]), "handshake": rel(rec["handshake"]),
                "first_byte": rel(rec["first_byte"]), "first_au": rel(rec["first_au"]),
                "first_frame": rel(rec["first_frame"]), "frames": rec["frames"], "aus": rec["aus"],
                "bytes": rec["bytes"], "ended": rec["ended"], "end": rel(rec["end"]),
                "errors": [e["text"] for e in rec["errors"]], "eos": rel(rec["eos"]),
                "resumes": [{"t": rel(x["t"]), "gap_ms": x["gap_ms"]} for x in rec["resumes"]]}

    if part == "probes":
        r0 = tr["r0"].rec
        out["r0"] = row(r0)
        rows = [row(r.rec) for r in tr["probes"]]
        out["probes"] = rows
        out["bursts"] = [row(r.rec) for r in tr["bursts"]]
        out["bursts_checked"] = len(out["bursts"])
        out["bursts_decoded"] = len([r for r in out["bursts"] if r["first_frame"] is not None])
        p2s = out["p2_spawn_rel_kill_ms"]
        p2s = float("inf") if p2s is None else p2s
        decoded = [r for r in rows if r["first_frame"] is not None and r["first_frame"] > p2s]
        out["probes_checked"] = len(rows)
        out["first_decoded_after_p2_rel_kill_ms"] = min((r["first_frame"] for r in decoded), default=None)
        out["first_decoding_probe_built_rel_kill_ms"] = min(
            (r["built"] for r in decoded if r["built"] >= p2s), default=None)
        out["r0_resumed_rel_kill_ms"] = min((x["t"] for x in out["r0"]["resumes"] if x["t"] > 0), default=None)
    else:
        rows = [row(rec) for rec in tr["log"]]
        out["generations"] = rows
        out["generations_checked"] = len(rows)
        out["on_air"] = {pol: [{"t": rel(x["t"]), "gen": x["gen"]} for x in xs] for pol, xs in tr["on_air"].items()}
        out["back_on_air_rel_kill_ms"] = {pol: min((x["t"] for x in xs if x["t"] > 0), default=None)
                                          for pol, xs in out["on_air"].items()}
        out["rebuilds_after_kill"] = {pol: len([r for r in rows if r["built"] > 0 and r["tag"].startswith(pol + ".")])
                                      for pol in tr["on_air"]}
    emit("summary", **out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["pub", "run"])
    ap.add_argument("--part", choices=["probes", "relay"], default="probes")
    ap.add_argument("--trials", type=int, default=3)
    ap.add_argument("--warm", type=float, default=45)
    ap.add_argument("--gaps", default="5.5")
    ap.add_argument("--after", default="60")
    ap.add_argument("--cooldown", type=float, default=240)
    ap.add_argument("--ingest", choices=["srt", "rtmps"], default="srt")
    ap.add_argument("--links", default="base")
    ap.add_argument("--probe-life", type=float, default=8)
    ap.add_argument("--probe-every", type=float, default=1)
    ap.add_argument("--burst", type=int, default=8)
    ap.add_argument("--burst-life", type=float, default=8)
    ap.add_argument("--drop", choices=["kill", "clean"], default="kill")
    ap.add_argument("--hls", default="")
    ap.add_argument("--note", default="")
    a = ap.parse_args()
    if a.mode == "pub":
        return pub(a.ingest)
    return run(a)


if __name__ == "__main__":
    sys.exit(main())
