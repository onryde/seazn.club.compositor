//! A fake RTMP destination for the output tests (Owed 84): a TCP listener on 127.0.0.1 that takes a
//! publish the way an ingest does (the plain handshake, `connect`, `createStream`, `publish`), then
//! keeps every audio and video message it is sent, with the instant it arrived. `held()` builds one
//! that answers the `publish` only at `release()`, so rtmp2sink's connect stays pending for as long
//! as a test likes, as it does while a destination is out (C14 cases 4 and 12, F-023); `refuse()`
//! instead closes the held connection, so that connect fails. One connection at a time; a later one
//! is a new publish.
//!
//! rtmp2 never times a held `publish` answer out: its connection retries a read that hit the socket
//! timeout (`rtmpconnection.c`, `gst_rtmp_connection_input_ready`), so only writes and the TCP
//! connect itself fail at rtmp2sink's 5 s timeout. A test that wants the connect to fail calls
//! `refuse()`.
//!
//! A client's chunks are read by the RTMP 1.0 rules as rtmp2 writes them (GStreamer 1.24.2,
//! `gst/rtmp2/rtmp/rtmpchunkstream.c`): a type 0 header carries the absolute timestamp, types 1 and 2
//! a delta, and a type 3 header that starts a message repeats its chunk stream's last delta. The
//! replies are what rtmp2's client reads (`rtmpclient.c`): `_result` for `connect` (an info object
//! whose `code` is `NetConnection.Connect.Success`) and for `createStream` (a stream id that is not
//! 0), and `onStatus` on that stream for `publish` (`NetStream.Publish.Start`).
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// RTMP message type ids (RTMP 1.0 §5.4, §7.1).
const SET_CHUNK_SIZE: u8 = 1;
const AUDIO: u8 = 8;
const VIDEO: u8 = 9;
const COMMAND_AMF0: u8 = 20;
/// The chunk size both ends start at (RTMP 1.0 §5.4.1).
const DEFAULT_CHUNK: usize = 128;
/// The handshake's C1/S1/C2/S2 size (RTMP 1.0 §5.2).
const HANDSHAKE: usize = 1_536;
/// The stream id `createStream` answers.
const STREAM_ID: u32 = 1;
/// How often a blocked read or wait looks at the stop flag.
const POLL: Duration = Duration::from_millis(20);

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One audio or video message as it arrived.
#[derive(Debug, Clone)]
pub struct Media {
    /// 8 (audio) or 9 (video).
    pub kind: u8,
    /// The message's RTMP timestamp, ms.
    pub ts_ms: u32,
    pub at: Instant,
    /// The FLV tag body (RTMP media payloads are FLV tag bodies).
    pub data: Vec<u8>,
}

impl Media {
    /// An AVC NAL unit message, not the sequence header (FLV VideoTagHeader: codec 7, packet type 1).
    pub fn is_avc_frame(&self) -> bool {
        self.kind == VIDEO && self.data.first().is_some_and(|b| b & 0x0f == 7) && self.data.get(1) == Some(&1)
    }

    /// An AAC frame, not the sequence header (FLV AudioTagHeader: format 10, packet type 1).
    pub fn is_aac_frame(&self) -> bool {
        self.kind == AUDIO && self.data.first().is_some_and(|b| b >> 4 == 10) && self.data.get(1) == Some(&1)
    }

    /// FLV VideoTagHeader frame type 1: a keyframe.
    pub fn is_keyframe(&self) -> bool {
        self.kind == VIDEO && self.data.first().is_some_and(|b| b >> 4 == 1)
    }
}

/// What the destination has seen.
#[derive(Debug, Default)]
pub struct Record {
    /// TCP connections accepted.
    pub accepted: u32,
    /// Publishes answered with `NetStream.Publish.Start`.
    pub published: u32,
    /// When the last publish was answered.
    pub published_at: Option<Instant>,
    /// Every audio and video message of every publish, in arrival order.
    pub media: Vec<Media>,
    /// Why the last connection ended, if not by the client's close.
    pub ended: Option<String>,
}

struct Inner {
    hold: Mutex<bool>,
    refuse: AtomicBool,
    released: Condvar,
    stop: AtomicBool,
    record: Mutex<Record>,
}

/// The destination; dropping it stops it.
pub struct FakeRtmp {
    addr: SocketAddr,
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl FakeRtmp {
    /// Answers every publish at once.
    pub fn prompt() -> Self {
        Self::start(false)
    }

    /// Answers a publish only after `release()`.
    pub fn held() -> Self {
        Self::start(true)
    }

    fn start(hold: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        let addr = listener.local_addr().expect("local addr");
        let inner = Arc::new(Inner {
            hold: Mutex::new(hold),
            refuse: AtomicBool::new(false),
            released: Condvar::new(),
            stop: AtomicBool::new(false),
            record: Mutex::new(Record::default()),
        });
        let serving = Arc::clone(&inner);
        let thread = thread::Builder::new()
            .name("fake-rtmp".to_owned())
            .spawn(move || serve(&listener, &serving))
            .expect("fake-rtmp thread");
        Self {
            addr,
            inner,
            thread: Some(thread),
        }
    }

    /// `rtmp://127.0.0.1:<port>/live`: the output appends the stream key as the last segment.
    pub fn url(&self) -> String {
        format!("rtmp://{}/live", self.addr)
    }

    /// Answers the held publish (and every later one at once).
    pub fn release(&self) {
        *locked(&self.inner.hold) = false;
        self.inner.released.notify_all();
    }

    /// Closes the held connection instead of answering its publish (and any later held one), as a
    /// destination that gives a connect up does: rtmp2sink's connect fails at once.
    pub fn refuse(&self) {
        self.inner.refuse.store(true, Ordering::SeqCst);
        self.inner.released.notify_all();
    }

    pub fn record(&self) -> MutexGuard<'_, Record> {
        locked(&self.inner.record)
    }

    /// Waits until no media message has arrived for `quiet`, at most `limit`; returns the count.
    pub fn settle(&self, quiet: Duration, limit: Duration) -> usize {
        let end = Instant::now() + limit;
        let mut seen = self.record().media.len();
        let mut since = Instant::now();
        while Instant::now() < end {
            thread::sleep(POLL);
            let now = self.record().media.len();
            if now != seen {
                seen = now;
                since = Instant::now();
            } else if since.elapsed() >= quiet {
                break;
            }
        }
        seen
    }

    /// Writes the media received so far as an FLV file (header, then one tag per message, each with
    /// its RTMP timestamp), so the tests' own decoder reads it. Returns the tags written.
    pub fn write_flv(&self, path: &Path) -> usize {
        let rec = self.record();
        let mut out = Vec::new();
        out.extend_from_slice(b"FLV\x01\x05\x00\x00\x00\x09");
        out.extend_from_slice(&0u32.to_be_bytes());
        for m in &rec.media {
            let len = m.data.len() as u32;
            out.push(m.kind);
            out.extend_from_slice(&len.to_be_bytes()[1..]);
            out.extend_from_slice(&m.ts_ms.to_be_bytes()[1..]);
            out.push(m.ts_ms.to_be_bytes()[0]);
            out.extend_from_slice(&[0, 0, 0]);
            out.extend_from_slice(&m.data);
            out.extend_from_slice(&(11 + len).to_be_bytes());
        }
        std::fs::write(path, out).expect("write the received FLV");
        rec.media.len()
    }
}

impl Drop for FakeRtmp {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.released.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(listener: &TcpListener, inner: &Inner) {
    if let Err(e) = listener.set_nonblocking(true) {
        locked(&inner.record).ended = Some(format!("listener: {e}"));
        return;
    }
    while !inner.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                locked(&inner.record).accepted += 1;
                if let Err(e) = session(stream, inner) {
                    locked(&inner.record).ended = Some(e.to_string());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(e) => {
                locked(&inner.record).ended = Some(format!("accept: {e}"));
                return;
            }
        }
    }
}

/// Reads exactly what is asked, looking at the stop flag between timed-out reads.
struct Reader<'a> {
    s: TcpStream,
    stop: &'a AtomicBool,
}

impl Reader<'_> {
    /// `n` bytes; `Ok(None)` if the peer closed before the first of them.
    fn exact_or_close(&mut self, n: usize) -> io::Result<Option<Vec<u8>>> {
        let mut v = vec![0u8; n];
        let mut got = 0;
        while got < n {
            if self.stop.load(Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
            }
            match self.s.read(&mut v[got..]) {
                Ok(0) if got == 0 => return Ok(None),
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed mid-read")),
                Ok(k) => got += k,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Some(v))
    }

    fn exact(&mut self, n: usize) -> io::Result<Vec<u8>> {
        self.exact_or_close(n)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "closed"))
    }
}

/// One RTMP message.
struct Message {
    kind: u8,
    msid: u32,
    ts_ms: u32,
    payload: Vec<u8>,
}

/// One chunk stream's state across chunks (RTMP 1.0 §5.3.1.2).
#[derive(Default)]
struct ChunkStream {
    /// The last header's timestamp field: absolute after type 0, a delta after types 1 and 2.
    field: u32,
    ts_ms: u32,
    len: usize,
    kind: u8,
    msid: u32,
    buf: Vec<u8>,
    open: bool,
}

#[derive(Default)]
struct Chunks {
    size: usize,
    streams: BTreeMap<u32, ChunkStream>,
}

fn u24(b: &[u8]) -> u32 {
    u32::from_be_bytes([0, b[0], b[1], b[2]])
}

impl Chunks {
    /// The next whole message; `Ok(None)` when the peer closed between chunks.
    fn next(&mut self, rd: &mut Reader<'_>) -> io::Result<Option<Message>> {
        loop {
            let Some(b0) = rd.exact_or_close(1)? else {
                return Ok(None);
            };
            let fmt = b0[0] >> 6;
            let csid = match b0[0] & 0x3f {
                0 => 64 + u32::from(rd.exact(1)?[0]),
                1 => {
                    let b = rd.exact(2)?;
                    64 + u32::from(b[0]) + 256 * u32::from(b[1])
                }
                c => u32::from(c),
            };
            let size = self.size;
            let st = self.streams.entry(csid).or_default();
            match fmt {
                0 => {
                    let h = rd.exact(11)?;
                    st.field = u24(&h[0..3]);
                    st.len = u24(&h[3..6]) as usize;
                    st.kind = h[6];
                    st.msid = u32::from_le_bytes([h[7], h[8], h[9], h[10]]);
                }
                1 => {
                    let h = rd.exact(7)?;
                    st.field = u24(&h[0..3]);
                    st.len = u24(&h[3..6]) as usize;
                    st.kind = h[6];
                }
                2 => st.field = u24(&rd.exact(3)?),
                _ => {}
            }
            // An extended timestamp follows a field of 0xFFFFFF, and every type 3 chunk of a stream
            // whose field is that large (rtmp2 `serialize_next`).
            if (fmt < 3 && st.field == 0x00ff_ffff) || (fmt == 3 && st.field >= 0x00ff_ffff) {
                let ext = rd.exact(4)?;
                if fmt < 3 {
                    st.field = u32::from_be_bytes([ext[0], ext[1], ext[2], ext[3]]);
                }
            }
            if !st.open {
                st.ts_ms = if fmt == 0 {
                    st.field
                } else {
                    st.ts_ms.wrapping_add(st.field)
                };
                st.buf.clear();
                st.open = true;
            }
            let want = (st.len - st.buf.len()).min(size);
            let data = rd.exact(want)?;
            st.buf.extend_from_slice(&data);
            if st.buf.len() >= st.len {
                st.open = false;
                return Ok(Some(Message {
                    kind: st.kind,
                    msid: st.msid,
                    ts_ms: st.ts_ms,
                    payload: std::mem::take(&mut st.buf),
                }));
            }
        }
    }
}

// AMF0 (Action Message Format 0, §2): just what the replies need, and the command's name and
// transaction id.
fn amf_number(out: &mut Vec<u8>, n: f64) {
    out.push(0x00);
    out.extend_from_slice(&n.to_be_bytes());
}

fn amf_string(out: &mut Vec<u8>, s: &str) {
    out.push(0x02);
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn amf_null(out: &mut Vec<u8>) {
    out.push(0x05);
}

fn amf_object(out: &mut Vec<u8>, fields: &[(&str, &str)]) {
    out.push(0x03);
    for (k, v) in fields {
        out.extend_from_slice(&(k.len() as u16).to_be_bytes());
        out.extend_from_slice(k.as_bytes());
        amf_string(out, v);
    }
    out.extend_from_slice(&[0x00, 0x00, 0x09]);
}

/// A command's name and transaction id: an AMF0 string, then an AMF0 number.
fn command_head(p: &[u8]) -> Option<(String, f64)> {
    if p.first() != Some(&0x02) || p.len() < 3 {
        return None;
    }
    let n = usize::from(u16::from_be_bytes([p[1], p[2]]));
    let name = String::from_utf8(p.get(3..3 + n)?.to_vec()).ok()?;
    let rest = p.get(3 + n..)?;
    if rest.first() != Some(&0x00) || rest.len() < 9 {
        return None;
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&rest[1..9]);
    Some((name, f64::from_be_bytes(b)))
}

/// Writes one message on chunk stream 3, type 0 header, timestamp 0, in chunks of the client's
/// input size (it never changes it: we send no Set Chunk Size).
fn send(s: &mut TcpStream, kind: u8, msid: u32, payload: &[u8]) -> io::Result<()> {
    const CSID: u8 = 3;
    let mut out = vec![CSID, 0, 0, 0];
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
    out.push(kind);
    out.extend_from_slice(&msid.to_le_bytes());
    for (i, chunk) in payload.chunks(DEFAULT_CHUNK).enumerate() {
        if i > 0 {
            out.push(0xc0 | CSID);
        }
        out.extend_from_slice(chunk);
    }
    s.write_all(&out)
}

fn session(stream: TcpStream, inner: &Inner) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(POLL))?;
    stream.set_nodelay(true)?;
    let mut w = stream.try_clone()?;
    let mut rd = Reader {
        s: stream,
        stop: &inner.stop,
    };
    // C0 + C1, then S0 + S1 + S2 (S2 echoes C1; rtmp2's client is not strict), then C2.
    let c0c1 = rd.exact(1 + HANDSHAKE)?;
    if c0c1[0] != 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("C0 version {}", c0c1[0]),
        ));
    }
    let mut s = vec![3u8];
    s.extend(std::iter::repeat_n(0u8, 8));
    s.extend((0..HANDSHAKE - 8).map(|i| (i % 251) as u8));
    s.extend_from_slice(&c0c1[1..]);
    w.write_all(&s)?;
    rd.exact(HANDSHAKE)?;

    let mut chunks = Chunks {
        size: DEFAULT_CHUNK,
        ..Chunks::default()
    };
    while let Some(m) = chunks.next(&mut rd)? {
        match m.kind {
            SET_CHUNK_SIZE if m.payload.len() >= 4 => {
                let n = u32::from_be_bytes([m.payload[0], m.payload[1], m.payload[2], m.payload[3]]);
                chunks.size = (n & 0x7fff_ffff) as usize;
            }
            AUDIO | VIDEO => locked(&inner.record).media.push(Media {
                kind: m.kind,
                ts_ms: m.ts_ms,
                at: Instant::now(),
                data: m.payload,
            }),
            COMMAND_AMF0 => command(&mut w, &m, inner)?,
            _ => {}
        }
    }
    Ok(())
}

fn command(w: &mut TcpStream, m: &Message, inner: &Inner) -> io::Result<()> {
    let Some((name, txn)) = command_head(&m.payload) else {
        return Ok(());
    };
    let mut out = Vec::new();
    match name.as_str() {
        "connect" => {
            amf_string(&mut out, "_result");
            amf_number(&mut out, txn);
            amf_object(&mut out, &[("fmsVer", "FMS/3,0,1,123")]);
            amf_object(
                &mut out,
                &[
                    ("level", "status"),
                    ("code", "NetConnection.Connect.Success"),
                    ("description", "Connection succeeded."),
                ],
            );
            send(w, COMMAND_AMF0, 0, &out)
        }
        "createStream" => {
            amf_string(&mut out, "_result");
            amf_number(&mut out, txn);
            amf_null(&mut out);
            amf_number(&mut out, f64::from(STREAM_ID));
            send(w, COMMAND_AMF0, 0, &out)
        }
        "publish" => {
            let mut hold = locked(&inner.hold);
            while *hold {
                if inner.stop.load(Ordering::SeqCst) {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped while held"));
                }
                if inner.refuse.load(Ordering::SeqCst) {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "refused while held",
                    ));
                }
                hold = inner
                    .released
                    .wait_timeout(hold, POLL)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            drop(hold);
            amf_string(&mut out, "onStatus");
            amf_number(&mut out, 0.0);
            amf_null(&mut out);
            amf_object(
                &mut out,
                &[
                    ("level", "status"),
                    ("code", "NetStream.Publish.Start"),
                    ("description", "Start publishing."),
                ],
            );
            send(w, COMMAND_AMF0, m.msid, &out)?;
            let mut rec = locked(&inner.record);
            rec.published += 1;
            rec.published_at = Some(Instant::now());
            Ok(())
        }
        // releaseStream, FCPublish, FCUnpublish, deleteStream: nothing the client waits for.
        _ => Ok(()),
    }
}
