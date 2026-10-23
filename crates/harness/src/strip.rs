//! The camera's machine-readable strip (C13 brief, "Machine-readable strips"): the checker's ground
//! truth for which camera frame a programme frame shows.
//!
//! The bottom `cell` rows of a camera frame hold 48 square cells of side `cell = height / 30` (24 px
//! at 720p), each black or white, left to right:
//! - cells 0–31: the frame counter (u32, most significant bit first);
//! - cells 32–47: its checksum, `counter.wrapping_mul(2654435761) >> 16` (u16).
//!
//! A frame whose cells do not read, or whose checksum fails, has "no strip": a slate, a frame the
//! overlay covers, or a corrupted frame. **Counter 0 is never drawn**: its checksum is 0, so an
//! all-black strip, which is what a black slate shows, would read as counter 0. The camera counts
//! from 1 and `read` refuses 0.
//!
//! A cell reads only when the mean luma of its centre (the middle half in each direction, away from
//! the encoder's ringing at the edges) is clearly dark or clearly light; anything between (a grey
//! slate, a blend) reads as no strip. Luma uses limited range (16 black, 235 white), which is what
//! x264 and avdec_h264 carry.
use std::fmt;

/// Cells in the camera strip.
pub const CELLS: u32 = 48;
/// The counter's cells, most significant bit first.
pub const COUNTER_CELLS: u32 = 32;
/// The checksum's cells, after the counter's.
pub const CHECKSUM_CELLS: u32 = 16;
/// A cell's side is the frame height over this: 24 px at 720p (the brief's 24×24 cells), and the
/// strip scales with a camera that changes size (`POST /size`), so it reads at 24 px again once the
/// compositor scales the picture to its 720p output.
pub const HEIGHT_PER_CELL: u32 = 30;
/// Limited-range luma for a white cell.
pub const WHITE: u8 = 235;
/// Limited-range luma for a black cell.
pub const BLACK: u8 = 16;
/// A cell centre with a mean below this reads 0.
pub const DARK_BELOW: u32 = 80;
/// A cell centre with a mean above this reads 1; between the two, the cell does not read.
pub const LIGHT_ABOVE: u32 = 176;

/// The brief's checksum: the top 16 bits of `counter × 2654435761` (mod 2³²).
pub fn checksum(counter: u32) -> u16 {
    (counter.wrapping_mul(2_654_435_761) >> 16) as u16
}

/// The strip's cell side for a frame `height` rows high.
pub fn cell_size(height: u32) -> u32 {
    height / HEIGHT_PER_CELL
}

/// One read-only image plane (a luma plane, for reading).
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub width: u32,
    pub height: u32,
}

/// One writable image plane.
pub struct PlaneMut<'a> {
    pub data: &'a mut [u8],
    pub stride: usize,
    pub width: u32,
    pub height: u32,
}

impl fmt::Debug for Plane<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Plane({}x{}, stride {})", self.width, self.height, self.stride)
    }
}

impl fmt::Debug for PlaneMut<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PlaneMut({}x{}, stride {})",
            self.width, self.height, self.stride
        )
    }
}

impl PlaneMut<'_> {
    /// Fills a rectangle (clipped to the plane) with one value.
    pub fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, value: u8) {
        let x1 = (x + w).min(self.width) as usize;
        let y1 = (y + h).min(self.height) as usize;
        for row in y as usize..y1 {
            let start = row * self.stride;
            self.data[start + x as usize..start + x1].fill(value);
        }
    }

    pub fn as_plane(&self) -> Plane<'_> {
        Plane {
            data: self.data,
            stride: self.stride,
            width: self.width,
            height: self.height,
        }
    }
}

/// Paints `bits` (most significant first, `n` of them) as cells of side `cell` from `(x0, y0)`.
pub fn draw_cells(p: &mut PlaneMut<'_>, x0: u32, y0: u32, cell: u32, n: u32, bits: u64) {
    for i in 0..n {
        let on = (bits >> (n - 1 - i)) & 1 == 1;
        p.fill(x0 + i * cell, y0, cell, cell, if on { WHITE } else { BLACK });
    }
}

/// The mean luma of each of `n` cells' centres (the middle half in each direction), or `None` when a
/// cell is out of the plane or too small to sample.
pub fn cell_means(p: &Plane<'_>, x0: u32, y0: u32, cell: u32, n: u32) -> Option<Vec<u32>> {
    if cell < 4 || n == 0 || n > 64 || x0 + n * cell > p.width || y0 + cell > p.height {
        return None;
    }
    let (lo, hi) = (cell / 4, cell - cell / 4);
    (0..n)
        .map(|i| {
            let cx = x0 + i * cell;
            let mut sum = 0u32;
            for row in (y0 + lo)..(y0 + hi) {
                let start = row as usize * p.stride + (cx + lo) as usize;
                let end = row as usize * p.stride + (cx + hi) as usize;
                sum += p.data.get(start..end)?.iter().map(|&b| u32::from(b)).sum::<u32>();
            }
            Some(sum / ((hi - lo) * (hi - lo)))
        })
        .collect()
}

/// Reads `n` cells of side `cell` from `(x0, y0)` as bits, most significant first, or `None` when any
/// cell is out of the plane, too small to sample, or neither clearly dark nor clearly light.
pub fn read_cells(p: &Plane<'_>, x0: u32, y0: u32, cell: u32, n: u32) -> Option<u64> {
    let mut bits = 0u64;
    for mean in cell_means(p, x0, y0, cell, n)? {
        let bit = if mean < DARK_BELOW {
            0
        } else if mean > LIGHT_ABOVE {
            1
        } else {
            return None;
        };
        bits = (bits << 1) | bit;
    }
    Some(bits)
}

/// The camera strip's 48 cell means (for a failure's diagnosis: [`cell_means`]).
pub fn strip_cell_means(y: &Plane<'_>) -> Option<Vec<u32>> {
    let cell = cell_size(y.height);
    cell_means(y, 0, y.height.checked_sub(cell)?, cell, CELLS)
}

/// The strip's 48 bits for `counter`: the counter, then its checksum.
pub fn bits(counter: u32) -> u64 {
    (u64::from(counter) << CHECKSUM_CELLS) | u64::from(checksum(counter))
}

/// Draws `counter`'s strip along the bottom of a luma plane.
///
/// # Panics
/// On counter 0, which is reserved (it would read the same as a black slate), and on a frame too
/// narrow for 48 cells.
pub fn draw(y: &mut PlaneMut<'_>, counter: u32) {
    assert_ne!(
        counter, 0,
        "strip counter 0 is reserved: a black strip reads as it"
    );
    let cell = cell_size(y.height);
    assert!(
        CELLS * cell <= y.width && cell > 0,
        "a {}x{} frame cannot hold the strip",
        y.width,
        y.height
    );
    draw_cells(y, 0, y.height - cell, cell, CELLS, bits(counter));
}

/// The counter a frame's strip shows, or `None` (no strip).
pub fn read(y: &Plane<'_>) -> Option<u32> {
    let cell = cell_size(y.height);
    let all = read_cells(y, 0, y.height.checked_sub(cell)?, cell, CELLS)?;
    let counter = (all >> CHECKSUM_CELLS) as u32;
    let sum = (all & 0xffff) as u16;
    (counter != 0 && checksum(counter) == sum).then_some(counter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, fill: u8) -> Vec<u8> {
        vec![fill; (w * h) as usize]
    }

    #[test]
    fn the_checksum_is_the_briefs_formula() {
        // Worked by hand: 1 × 2654435761 = 0x9E3779B1, top 16 bits 0x9E37.
        assert_eq!(checksum(1), 0x9E37);
        // 2 × 2654435761 = 5308871522 = 0x1_3C6E_F362, mod 2^32 0x3C6EF362: 0x3C6E.
        assert_eq!(checksum(2), 0x3C6E);
        assert_eq!(checksum(0), 0, "why counter 0 is reserved");
    }

    #[test]
    fn a_drawn_strip_reads_back_at_both_profiles_and_a_smaller_camera() {
        let mut checked = 0;
        for (w, h) in [(1280, 720), (1920, 1080), (640, 360)] {
            for counter in [1u32, 2, 29, 30, 0x8000_0000, u32::MAX, 123_456_789] {
                let mut data = frame(w, h, 90);
                let mut p = PlaneMut {
                    data: &mut data,
                    stride: w as usize,
                    width: w,
                    height: h,
                };
                draw(&mut p, counter);
                assert_eq!(read(&p.as_plane()), Some(counter), "{w}x{h} counter {counter}");
                checked += 1;
            }
        }
        eprintln!("raw strips checked: {checked}");
        assert!(checked > 0);
    }

    #[test]
    fn solid_frames_have_no_strip() {
        for fill in [0u8, BLACK, 60, 128, 200, WHITE, 255] {
            let mut data = frame(1280, 720, fill);
            let p = PlaneMut {
                data: &mut data,
                stride: 1280,
                width: 1280,
                height: 720,
            };
            assert_eq!(read(&p.as_plane()), None, "solid luma {fill}");
        }
    }

    #[test]
    fn a_wrong_checksum_or_a_grey_cell_is_no_strip() {
        let mut data = frame(1280, 720, 90);
        let mut p = PlaneMut {
            data: &mut data,
            stride: 1280,
            width: 1280,
            height: 720,
        };
        draw(&mut p, 77);
        // Flip the last checksum cell.
        let last = if bits(77) & 1 == 1 { BLACK } else { WHITE };
        p.fill(47 * 24, 696, 24, 24, last);
        assert_eq!(read(&p.as_plane()), None, "a flipped checksum bit");
        draw(&mut p, 77);
        p.fill(5 * 24, 696, 24, 24, 128);
        assert_eq!(read(&p.as_plane()), None, "a grey cell");
    }

    #[test]
    #[should_panic(expected = "reserved")]
    fn counter_zero_is_refused() {
        let mut data = frame(1280, 720, 90);
        draw(
            &mut PlaneMut {
                data: &mut data,
                stride: 1280,
                width: 1280,
                height: 720,
            },
            0,
        );
    }

    #[test]
    fn a_frame_too_small_reads_none_rather_than_panicking() {
        let data = frame(100, 30, 128);
        assert_eq!(
            read(&Plane {
                data: &data,
                stride: 100,
                width: 100,
                height: 30
            }),
            None
        );
        let data = frame(10, 10, 128);
        assert_eq!(
            read(&Plane {
                data: &data,
                stride: 10,
                width: 10,
                height: 10
            }),
            None
        );
    }
}
