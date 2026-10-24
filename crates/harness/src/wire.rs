//! Owed 81's probe at the camera's sink: which PES packets each buffer the camera sends starts, read
//! as MPEG-TS (ISO/IEC 13818-1: the transport packet header §2.4.3.2, the adaptation field §2.4.3.4,
//! the PES header §2.4.3.6). fake-camera's `--wire` stamps every start with the wall time its buffer
//! reached `srtsink`, so a run can tell what the camera SENT, and when, from what it pushed
//! (`emit.jsonl`) and from what the compositor showed (the recordings).
//!
//! Only what the probe needs is read: the PID, the stream id (video `0xE0`–`0xEF`, audio
//! `0xC0`–`0xDF`) and the PTS of each PES that starts in a packet. Anything else (PSI, a packet
//! with no payload start, a PES of another stream) is skipped and counted only as a packet.

/// A transport packet's size.
pub const TS_PACKET: usize = 188;
/// Its first byte.
pub const SYNC: u8 = 0x47;

/// The elementary stream a PES belongs to, by its stream id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Video,
    Audio,
}

/// One PES packet that starts in a transport packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PesStart {
    pub pid: u16,
    pub stream: Stream,
    /// The PES's PTS, 90 kHz, when its header carries one.
    pub pts90k: Option<u64>,
}

/// What one buffer held.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Scan {
    pub starts: Vec<PesStart>,
    /// Whole packets read (good sync byte).
    pub packets: u64,
    /// 188-byte chunks whose first byte was not the sync byte (skipped).
    pub bad_sync: u64,
    /// Bytes past the last whole packet (a buffer that is not whole packets).
    pub partial_bytes: usize,
}

/// Reads `buf` as whole transport packets.
pub fn scan(buf: &[u8]) -> Scan {
    let mut out = Scan {
        partial_bytes: buf.len() % TS_PACKET,
        ..Scan::default()
    };
    for p in buf.chunks_exact(TS_PACKET) {
        if p[0] != SYNC {
            out.bad_sync += 1;
            continue;
        }
        out.packets += 1;
        if let Some(s) = pes_start(p) {
            out.starts.push(s);
        }
    }
    out
}

/// The PES that starts in packet `p` (188 bytes, good sync), if one does.
fn pes_start(p: &[u8]) -> Option<PesStart> {
    let pusi = p[1] & 0x40 != 0;
    let pid = (u16::from(p[1] & 0x1F) << 8) | u16::from(p[2]);
    let afc = (p[3] >> 4) & 0x03;
    if !pusi || afc & 0b01 == 0 {
        return None;
    }
    // §2.4.3.4: the adaptation field's first byte is its length (the bytes after it).
    let at = if afc & 0b10 != 0 { 5 + usize::from(p[4]) } else { 4 };
    let pes = p.get(at..)?;
    if pes.len() < 4 || pes[..3] != [0x00, 0x00, 0x01] {
        return None;
    }
    let stream = match pes[3] {
        0xE0..=0xEF => Stream::Video,
        0xC0..=0xDF => Stream::Audio,
        _ => return None,
    };
    // §2.4.3.6: PTS_DTS_flags are the top two bits of byte 7; '10' or '11' carries a PTS at 9.
    let pts90k = match pes.get(7..14) {
        Some(h) if h[0] & 0x80 != 0 => {
            let b = &h[2..7];
            Some(
                (u64::from((b[0] >> 1) & 0x07) << 30)
                    | (u64::from(b[1]) << 22)
                    | (u64::from(b[2] >> 1) << 15)
                    | (u64::from(b[3]) << 7)
                    | u64::from(b[4] >> 1),
            )
        }
        _ => None,
    };
    Some(PesStart { pid, stream, pts90k })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The PTS's five bytes as §2.4.3.7 lays them out ('0010' then PTS[32..30], a marker bit, PTS[29..15],
    /// a marker, PTS[14..0], a marker), written here from the standard, not from `scan`.
    fn pts_bytes(pts: u64) -> [u8; 5] {
        [
            0x20 | (((pts >> 30) & 0x07) as u8) << 1 | 1,
            ((pts >> 22) & 0xFF) as u8,
            ((((pts >> 15) & 0x7F) as u8) << 1) | 1,
            ((pts >> 7) & 0xFF) as u8,
            (((pts & 0x7F) as u8) << 1) | 1,
        ]
    }

    /// A PES header: start code, stream id, length 0, '10' flags byte, PTS_DTS_flags, header length, PTS.
    fn pes_header(stream_id: u8, pts: Option<u64>) -> Vec<u8> {
        let mut h = vec![0x00, 0x00, 0x01, stream_id, 0x00, 0x00, 0x80];
        match pts {
            Some(p) => {
                h.extend([0x80, 0x05]);
                h.extend(pts_bytes(p));
            }
            None => h.extend([0x00, 0x00]),
        }
        h
    }

    /// One transport packet: header (sync, PUSI, PID, AFC), an optional adaptation field of
    /// `af_len` stuffing bytes, then `payload`, padded with 0xFF to 188 bytes.
    fn packet(pid: u16, pusi: bool, af_len: Option<u8>, payload: &[u8]) -> Vec<u8> {
        let afc: u8 = match (af_len.is_some(), !payload.is_empty()) {
            (false, _) => 0b01,
            (true, true) => 0b11,
            (true, false) => 0b10,
        };
        let mut p = vec![
            SYNC,
            (u8::from(pusi) << 6) | ((pid >> 8) as u8 & 0x1F),
            (pid & 0xFF) as u8,
            afc << 4, // continuity counter 0
        ];
        if let Some(n) = af_len {
            p.push(n);
            if n > 0 {
                p.push(0x00); // the flags byte: no PCR, nothing else
                p.extend(std::iter::repeat_n(0xFF, usize::from(n) - 1));
            }
        }
        p.extend(payload);
        assert!(p.len() <= TS_PACKET, "a test packet that does not fit");
        p.resize(TS_PACKET, 0xFF);
        p
    }

    #[test]
    fn a_video_pes_start_reads_its_pid_and_pts() {
        let pts = 0x1_2345_6789 & ((1 << 33) - 1);
        let s = scan(&packet(0x41, true, None, &pes_header(0xE0, Some(pts))));
        assert_eq!(s.packets, 1);
        assert_eq!(
            s.starts,
            vec![PesStart {
                pid: 0x41,
                stream: Stream::Video,
                pts90k: Some(pts)
            }]
        );
    }

    #[test]
    fn every_pts_bit_is_read() {
        let mut checked = 0;
        for bit in 0..33 {
            let pts = 1u64 << bit;
            let s = scan(&packet(0x41, true, None, &pes_header(0xE0, Some(pts))));
            assert_eq!(s.starts.first().and_then(|p| p.pts90k), Some(pts), "bit {bit}");
            checked += 1;
        }
        eprintln!("pts bits checked: {checked}");
        assert_eq!(checked, 33);
    }

    #[test]
    fn an_adaptation_field_before_the_payload_is_skipped() {
        for af_len in [0u8, 1, 7, 20] {
            let s = scan(&packet(0x41, true, Some(af_len), &pes_header(0xE0, Some(90_000))));
            assert_eq!(
                s.starts.iter().map(|p| p.pts90k).collect::<Vec<_>>(),
                vec![Some(90_000)],
                "adaptation field of {af_len} bytes"
            );
        }
    }

    #[test]
    fn audio_is_told_from_video_and_other_streams_are_not_starts() {
        let mut buf = packet(0x42, true, None, &pes_header(0xC0, Some(3_000)));
        buf.extend(packet(0x43, true, None, &pes_header(0xBD, Some(6_000))));
        let s = scan(&buf);
        assert_eq!(s.packets, 2);
        assert_eq!(
            s.starts,
            vec![PesStart {
                pid: 0x42,
                stream: Stream::Audio,
                pts90k: Some(3_000)
            }],
            "private stream 1 (0xBD) is no camera stream"
        );
    }

    #[test]
    fn a_pes_without_a_pts_starts_with_none() {
        let s = scan(&packet(0x41, true, None, &pes_header(0xE0, None)));
        assert_eq!(s.starts.len(), 1);
        assert_eq!(s.starts[0].pts90k, None);
    }

    #[test]
    fn continuations_psi_and_adaptation_only_packets_start_nothing() {
        let pes = pes_header(0xE0, Some(1));
        let mut buf = packet(0x41, false, None, &pes); // no payload start: a continuation
        buf.extend(packet(0x00, true, None, &[0x00, 0x00, 0xB0, 0x0D, 0x00, 0x01])); // the PAT
        buf.extend(packet(0x41, true, Some(183), &[])); // adaptation field only
        let s = scan(&buf);
        assert_eq!(s.packets, 3);
        assert!(s.starts.is_empty(), "{:?}", s.starts);
    }

    #[test]
    fn bytes_after_a_short_adaptation_field_are_no_payload_when_the_control_says_none() {
        // adaptation_field_control '10' (§2.4.3.3): adaptation field only, no payload, whatever
        // the bytes after a short field look like.
        let mut p = packet(0x41, true, Some(7), &pes_header(0xE0, Some(42)));
        p[3] = (0b10 << 4) | (p[3] & 0x0F);
        let s = scan(&p);
        assert_eq!(s.packets, 1);
        assert!(s.starts.is_empty(), "{:?}", s.starts);
    }

    #[test]
    fn an_empty_buffer_reads_nothing() {
        assert_eq!(scan(&[]), Scan::default());
    }

    #[test]
    fn a_mux_buffer_of_seven_packets_keeps_its_starts_in_order() {
        // mpegtsmux alignment=7: 1 316 bytes a buffer (media.rs, C10).
        let mut buf = Vec::new();
        buf.extend(packet(0x42, true, None, &pes_header(0xC0, Some(100))));
        buf.extend(packet(0x41, true, Some(7), &pes_header(0xE0, Some(200))));
        for _ in 0..4 {
            buf.extend(packet(0x41, false, None, &[0xAB; 100]));
        }
        buf.extend(packet(0x42, true, None, &pes_header(0xC0, Some(300))));
        assert_eq!(buf.len(), 7 * TS_PACKET);
        let s = scan(&buf);
        assert_eq!(s.packets, 7);
        assert_eq!(
            s.starts
                .iter()
                .map(|p| (p.stream, p.pts90k.unwrap_or(0)))
                .collect::<Vec<_>>(),
            vec![(Stream::Audio, 100), (Stream::Video, 200), (Stream::Audio, 300)]
        );
    }

    #[test]
    fn a_bad_sync_byte_and_a_partial_tail_are_counted_not_read() {
        let mut bad = packet(0x41, true, None, &pes_header(0xE0, Some(5)));
        bad[0] = 0x00;
        let mut buf = bad;
        buf.extend(packet(0x41, true, None, &pes_header(0xE0, Some(7))));
        buf.extend([SYNC, 0x40, 0x41]);
        let s = scan(&buf);
        assert_eq!((s.packets, s.bad_sync, s.partial_bytes), (1, 1, 3));
        assert_eq!(
            s.starts.iter().map(|p| p.pts90k).collect::<Vec<_>>(),
            vec![Some(7)]
        );
    }

    #[test]
    fn a_header_cut_short_by_the_packet_end_is_no_start() {
        // A PES header whose PTS would run past the packet: an adaptation field leaves 10 bytes.
        let mut p = packet(0x41, true, Some(183 - 10), &[]);
        let start = TS_PACKET - 10;
        p[3] = (0b11 << 4) | (p[3] & 0x0F);
        p[start..].copy_from_slice(&pes_header(0xE0, Some(9))[..10]);
        let s = scan(&p);
        assert_eq!(s.packets, 1);
        assert_eq!(s.starts.len(), 1, "the start code and stream id are whole");
        assert_eq!(s.starts[0].pts90k, None, "the PTS is not");
    }
}
