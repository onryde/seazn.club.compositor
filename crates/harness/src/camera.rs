//! fake-camera's control and schedule (C13 brief, Files), apart from GStreamer.
//!
//! The camera's counter advances at 30 fps from its start, whatever it emits, so a gap shows in
//! `emit.jsonl` as missing counters and a jump in wall time. The controls on `:7001`:
//! - `POST /pause`: stop emitting (frames due are dropped); `POST /resume`: emit again;
//! - `POST /burst?ms=3000`: hold every frame due for `ms` (still counting), then emit the backlog as
//!   fast as the encoder takes it, with the frames' original PTS;
//! - `POST /size?w=&h=`: change the picture size (the strip scales with it: `strip::cell_size`).
//!
//! The pictures are drawn here too ([`frame`]): the pattern, then the strip, in an I420 buffer whose
//! [`Layout`] `media` checks against GStreamer's own before it pushes one.
use crate::strip::{self, PlaneMut};

/// The camera's frame rate.
pub const FPS: u64 = 30;
/// The longest burst accepted (its backlog lives in memory: counters only, frames drawn at push).
pub const MAX_BURST_MS: u64 = 10_000;
/// The largest picture (1080p, the compositor's largest profile).
pub const MAX_SIZE: (u32, u32) = (1920, 1080);

/// A control request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Pause,
    Resume,
    Burst { ms: u64 },
    Size { w: u32, h: u32 },
}

fn param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    url.split_once('?')?
        .1
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// Checks a picture size: even sides, no larger than 1080p, tall enough for 4-pixel strip cells and
/// wide enough for all 48.
pub fn check_size(w: u32, h: u32) -> Result<(), String> {
    let cell = strip::cell_size(h);
    if !w.is_multiple_of(2)
        || !h.is_multiple_of(2)
        || w > MAX_SIZE.0
        || h > MAX_SIZE.1
        || cell < 4
        || strip::CELLS * cell > w
    {
        return Err(format!(
            "{w}x{h} cannot carry the strip (even sides, at most 1920x1080, h ≥ 120)"
        ));
    }
    Ok(())
}

/// Parses a control request (`method`, request target).
pub fn parse_command(method: &str, url: &str) -> Result<Command, String> {
    if method != "POST" {
        return Err("controls are POST".into());
    }
    let path = url.split('?').next().unwrap_or("");
    let num = |k: &str| {
        param(url, k)
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or(format!("{k} must be a number"))
    };
    match path {
        "/pause" => Ok(Command::Pause),
        "/resume" => Ok(Command::Resume),
        "/burst" => match num("ms")? {
            ms @ 1..=MAX_BURST_MS => Ok(Command::Burst { ms }),
            _ => Err(format!("ms must be 1 to {MAX_BURST_MS}")),
        },
        "/size" => {
            let (w, h) = (num("w")?, num("h")?);
            let (w, h) = (
                u32::try_from(w).map_err(|_| "w is too large")?,
                u32::try_from(h).map_err(|_| "h is too large")?,
            );
            check_size(w, h)?;
            Ok(Command::Size { w, h })
        }
        _ => Err(format!("no control {path}")),
    }
}

/// The picture under the strip (`videotestsrc pattern=…`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// A moving ball: motion for the encoder, a mostly black picture.
    Ball,
    /// Noise in 16×16 blocks on the macroblock grid, new every frame, over a one-cell black guard
    /// above the strip (the 2026-10-18 amendment, for C15's cost legs; [`SNOW_BLOCK`] says why it is
    /// not per-pixel).
    Snow,
}

impl Pattern {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "ball" => Ok(Self::Ball),
            "snow" => Ok(Self::Snow),
            other => Err(format!("--pattern {other}: ball or snow")),
        }
    }
}

/// An I420 picture's layout: GStreamer's default for the size (rows padded to 4 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub y_stride: usize,
    pub c_stride: usize,
    pub u_offset: usize,
    pub v_offset: usize,
    pub size: usize,
}

fn round_up_4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

impl Layout {
    /// The layout of a `w`×`h` I420 picture (even sides: [`check_size`]).
    pub fn i420(w: u32, h: u32) -> Self {
        let (wu, hu) = (w as usize, h as usize);
        let y_stride = round_up_4(wu);
        let c_stride = round_up_4(wu / 2);
        let u_offset = y_stride * hu;
        let v_offset = u_offset + c_stride * (hu / 2);
        Self {
            width: w,
            height: h,
            y_stride,
            c_stride,
            u_offset,
            v_offset,
            size: v_offset + c_stride * (hu / 2),
        }
    }
}

/// The ball's side: a sixth of the height, so it moves well clear of the strip.
/// The snow's block side.
///
/// The amendment's "videotestsrc pattern=snow under the strip, which stays readable" is false at the
/// camera's 4 Mbps. Noise in every pixel starves x264 at every I-frame: the rate control's VBV goes
/// into emergency, and the strip at the frame's bottom comes out as flat grey. That happened in CI
/// run 39984287910, at frame 1020, the first snow keyframe. It reproduced locally through libx264 at
/// the camera's settings with sliced threads (as `tune=zerolatency` sets them) and again through a
/// second generation at the programme's settings.
///
/// Blocks of 16 on the macroblock grid, each new every frame, still defeat motion search and leave a
/// residual in every macroblock. With [`SNOW_GUARD_CELLS`], the strip read back on every frame
/// through both generations at 4 Mbps, and at 2 Mbps (twice the margin). Blocks of 8 failed at
/// 4 Mbps.
pub const SNOW_BLOCK: u32 = 16;
/// Black rows above the strip, in cells: the snow never shares a macroblock row with the strip's top.
pub const SNOW_GUARD_CELLS: u32 = 1;

fn ball_side(h: u32) -> u32 {
    h / 6
}

/// Frame `counter`'s picture: the pattern, then the strip (counter 0 is refused by `strip::draw`).
/// The ball is a white square moving 8 px a frame along the picture's middle third. The snow is new
/// noise in every [`SNOW_BLOCK`] block of every frame, seeded by the counter, so no two frames repeat.
pub fn frame(l: &Layout, pattern: Pattern, counter: u32) -> Vec<u8> {
    let mut data = vec![0u8; l.size];
    let (luma, chroma) = data.split_at_mut(l.u_offset);
    let mut y = PlaneMut {
        data: luma,
        stride: l.y_stride,
        width: l.width,
        height: l.height,
    };
    match pattern {
        Pattern::Ball => {
            y.fill(0, 0, l.width, l.height, strip::BLACK);
            let side = ball_side(l.height);
            let travel = l.width - side;
            let x = (u64::from(counter) * 8 % u64::from(travel)) as u32;
            y.fill(x, l.height / 3, side, side, strip::WHITE);
        }
        Pattern::Snow => {
            y.fill(0, 0, l.width, l.height, strip::BLACK);
            let guard = (1 + SNOW_GUARD_CELLS) * strip::cell_size(l.height);
            let noisy = l.height.saturating_sub(guard);
            // xorshift32, one draw per block.
            let mut state = counter.wrapping_mul(0x9E37_79B9) | 1;
            for by in (0..noisy).step_by(SNOW_BLOCK as usize) {
                for bx in (0..l.width).step_by(SNOW_BLOCK as usize) {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    let v = strip::BLACK + (state as u8 % (strip::WHITE - strip::BLACK + 1));
                    y.fill(bx, by, SNOW_BLOCK, SNOW_BLOCK.min(noisy - by), v);
                }
            }
        }
    }
    strip::draw(&mut y, counter);
    // Both patterns are grey: neutral chroma everywhere (row padding included), so nothing tints
    // the strip.
    chroma.fill(128);
    data
}

/// What the camera emits, frame by frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    paused: bool,
    burst_until_ms: Option<i64>,
    backlog: Vec<u32>,
    size: (u32, u32),
}

impl Schedule {
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            paused: false,
            burst_until_ms: None,
            backlog: Vec::new(),
            size: (w, h),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Frames held by a burst in progress.
    pub fn backlog(&self) -> usize {
        self.backlog.len()
    }

    /// Applies a control at `now_ms`. A second burst while one runs is refused.
    pub fn command(&mut self, c: Command, now_ms: i64) -> Result<(), String> {
        match c {
            Command::Pause => self.paused = true,
            Command::Resume => self.paused = false,
            Command::Burst { ms } => {
                if self.burst_until_ms.is_some() {
                    return Err("a burst is already holding frames".into());
                }
                self.burst_until_ms = Some(now_ms + ms as i64);
            }
            Command::Size { w, h } => {
                check_size(w, h)?;
                self.size = (w, h);
            }
        }
        Ok(())
    }

    /// Frame `counter` falls due at `now_ms`: the counters to push now, in order. A pause drops it; a
    /// burst holds it; the first frame due once a burst is over releases the backlog before itself.
    pub fn on_frame(&mut self, counter: u32, now_ms: i64) -> Vec<u32> {
        if let Some(until) = self.burst_until_ms {
            if now_ms < until {
                if !self.paused {
                    self.backlog.push(counter);
                }
                return Vec::new();
            }
            self.burst_until_ms = None;
            let mut out = std::mem::take(&mut self.backlog);
            if !self.paused {
                out.push(counter);
            }
            return out;
        }
        if self.paused {
            Vec::new()
        } else {
            vec![counter]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_parse_and_bad_ones_are_refused() {
        assert_eq!(parse_command("POST", "/pause"), Ok(Command::Pause));
        assert_eq!(parse_command("POST", "/resume"), Ok(Command::Resume));
        assert_eq!(
            parse_command("POST", "/burst?ms=3000"),
            Ok(Command::Burst { ms: 3_000 })
        );
        assert_eq!(
            parse_command("POST", "/size?w=640&h=360"),
            Ok(Command::Size { w: 640, h: 360 })
        );
        for (m, u) in [
            ("GET", "/pause"),
            ("POST", "/burst"),
            ("POST", "/burst?ms=0"),
            ("POST", "/burst?ms=10001"),
            ("POST", "/size?w=641&h=360"),
            ("POST", "/size?w=2560&h=1440"),
            ("POST", "/size?w=200&h=360"),
            ("POST", "/size?w=640&h=90"),
            ("POST", "/nope"),
        ] {
            assert!(parse_command(m, u).is_err(), "{m} {u}");
        }
    }

    #[test]
    fn a_pause_drops_frames_and_a_resume_emits_again() {
        let mut s = Schedule::new(1280, 720);
        assert_eq!(s.on_frame(1, 0), vec![1]);
        s.command(Command::Pause, 10).unwrap();
        assert!(s.on_frame(2, 33).is_empty());
        assert!(s.on_frame(3, 66).is_empty());
        s.command(Command::Resume, 70).unwrap();
        assert_eq!(
            s.on_frame(4, 100),
            vec![4],
            "2 and 3 are gone: the gap shows in emit.jsonl"
        );
        s.command(Command::Resume, 101).unwrap();
        assert_eq!(s.on_frame(5, 133), vec![5], "a second resume changes nothing");
    }

    #[test]
    fn a_burst_holds_frames_then_releases_them_in_order_before_the_next() {
        let mut s = Schedule::new(1280, 720);
        s.command(Command::Burst { ms: 100 }, 0).unwrap();
        assert!(
            s.command(Command::Burst { ms: 100 }, 1).is_err(),
            "one burst at a time"
        );
        assert!(s.on_frame(1, 0).is_empty());
        assert!(s.on_frame(2, 33).is_empty());
        assert!(s.on_frame(3, 66).is_empty());
        assert_eq!(s.backlog(), 3);
        assert_eq!(s.on_frame(4, 100), vec![1, 2, 3, 4]);
        assert_eq!(s.on_frame(5, 133), vec![5]);
        assert!(
            s.command(Command::Burst { ms: 50 }, 133).is_ok(),
            "a new burst once the last is over"
        );
    }

    #[test]
    fn an_empty_burst_releases_nothing_extra() {
        let mut s = Schedule::new(1280, 720);
        s.command(Command::Pause, 0).unwrap();
        s.command(Command::Burst { ms: 50 }, 0).unwrap();
        assert!(s.on_frame(1, 0).is_empty());
        assert!(
            s.on_frame(2, 60).is_empty(),
            "paused through the burst: nothing held, nothing sent"
        );
        assert_eq!(s.backlog(), 0);
    }

    #[test]
    fn a_size_change_is_checked() {
        let mut s = Schedule::new(1280, 720);
        s.command(Command::Size { w: 640, h: 360 }, 0).unwrap();
        assert_eq!(s.size(), (640, 360));
        assert!(s.command(Command::Size { w: 641, h: 360 }, 0).is_err());
        assert_eq!(s.size(), (640, 360));
        assert!(check_size(1280, 720).is_ok() && check_size(1920, 1080).is_ok());
    }

    #[test]
    fn patterns_are_ball_and_snow() {
        assert_eq!(Pattern::parse("snow"), Ok(Pattern::Snow));
        assert_eq!(Pattern::parse("ball"), Ok(Pattern::Ball));
        assert!(Pattern::parse("smpte").is_err());
    }

    #[test]
    fn the_layout_is_gstreamers_default_for_i420() {
        // GStreamer's I420 (video-info.c): Y stride RU4(w), U and V strides RU4(w/2), planes in a row.
        let l = Layout::i420(1280, 720);
        assert_eq!(
            (l.y_stride, l.c_stride, l.u_offset, l.v_offset, l.size),
            (1280, 640, 921_600, 1_152_000, 1_382_400)
        );
        let l = Layout::i420(642, 360);
        assert_eq!((l.y_stride, l.c_stride), (644, 324), "rows are padded to 4 bytes");
        assert_eq!(l.size, 644 * 360 + 2 * 324 * 180);
    }

    #[test]
    fn every_drawn_frame_reads_back_its_counter_on_both_patterns() {
        let mut checked = 0;
        for (w, h) in [(1280u32, 720u32), (640, 360), (1920, 1080), (642, 360)] {
            let l = Layout::i420(w, h);
            for pattern in [Pattern::Ball, Pattern::Snow] {
                for counter in [1u32, 2, 29, 30, 31, 1_000, 123_456, u32::MAX] {
                    let data = frame(&l, pattern, counter);
                    assert_eq!(data.len(), l.size);
                    let y = strip::Plane {
                        data: &data[..l.u_offset],
                        stride: l.y_stride,
                        width: w,
                        height: h,
                    };
                    assert_eq!(strip::read(&y), Some(counter), "{w}x{h} {pattern:?} {counter}");
                    assert!(data[l.u_offset..].iter().all(|&c| c == 128), "neutral chroma");
                    checked += 1;
                }
            }
        }
        eprintln!("camera frames checked: {checked}");
        assert_eq!(checked, 4 * 2 * 8);
    }

    #[test]
    fn the_snow_is_uniform_blocks_above_a_black_guard() {
        let mut checked = 0;
        for (w, h) in [(1280u32, 720u32), (1920, 1080), (642, 360)] {
            let l = Layout::i420(w, h);
            let f = frame(&l, Pattern::Snow, 99);
            let cell = strip::cell_size(h);
            let noisy = h - 2 * cell;
            let px = |x: u32, y: u32| f[y as usize * l.y_stride + x as usize];
            for y in 0..noisy {
                for x in 0..w {
                    let (bx, by) = (x - x % SNOW_BLOCK, y - y % SNOW_BLOCK);
                    assert_eq!(px(x, y), px(bx, by), "{w}x{h}: ({x}, {y}) is its block's value");
                    checked += 1;
                }
            }
            for y in noisy..h - cell {
                assert!((0..w).all(|x| px(x, y) == strip::BLACK), "{w}x{h}: guard row {y}");
            }
        }
        eprintln!("snow pixels checked: {checked}");
        assert!(checked > 0);
    }

    #[test]
    fn the_ball_moves_and_the_snow_changes_every_frame() {
        let l = Layout::i420(1280, 720);
        let above_strip = l.y_stride * (720 - strip::cell_size(720) as usize);
        for pattern in [Pattern::Ball, Pattern::Snow] {
            let (a, b) = (frame(&l, pattern, 10), frame(&l, pattern, 11));
            assert_ne!(
                a[..above_strip],
                b[..above_strip],
                "{pattern:?}: the picture moves"
            );
        }
        // Snow covers the luma range; the ball's picture is black and white only.
        let snow = frame(&l, Pattern::Snow, 7);
        let (lo, hi) = snow[..above_strip]
            .iter()
            .fold((255u8, 0u8), |(lo, hi), &p| (lo.min(p), hi.max(p)));
        assert_eq!((lo, hi), (strip::BLACK, strip::WHITE));
        let ball = frame(&l, Pattern::Ball, 7);
        assert!(ball[..above_strip]
            .iter()
            .all(|&p| p == strip::BLACK || p == strip::WHITE));
        assert!(ball[..above_strip].contains(&strip::WHITE), "the ball is drawn");
    }
}
