//! The overlay's score strip (C13 brief, "Overlay strip"): what `harness/pages/overlay-test.html`
//! draws so the checker can read the page's score timeline back out of the programme.
//!
//! In page space (CEF renders the page at 1920×1080, ADR 0005) the strip is 16 cells of 24×24 at the
//! top right, `x = 1920 − 16·24 = 1536`, `y = 0`, each black or white: the page's current **score
//! index** (u16, most significant bit first), the index of the latest timeline event whose `at +
//! delay` is not after the page's clock. After the compositor scales the page to the output, the
//! cells are `24 × width / 1920` wide (16 px at 720p, from `x = 1024`). The cells read with the
//! camera strip's rule (`strip::read_cells`); there is no checksum, so a reading is a fact about
//! the frame, and the checker only reports changes of index.
use crate::strip::{draw_cells, read_cells, Plane, PlaneMut};

/// Cells in the score strip.
pub const CELLS: u32 = 16;
/// The page's width (CEF's viewport, ADR 0005).
pub const PAGE_WIDTH: u32 = 1920;
/// A cell's side in page space.
pub const PAGE_CELL: u32 = 24;

/// A cell's side in a frame `width` pixels wide (16 at 1280, 24 at 1920).
pub fn cell_size(width: u32) -> u32 {
    width * PAGE_CELL / PAGE_WIDTH
}

/// The strip's left edge in a frame `width` pixels wide.
pub fn x0(width: u32) -> u32 {
    width - CELLS * cell_size(width)
}

/// The score index a frame's top-right cells show, or `None` when any cell does not read.
pub fn read(y: &Plane<'_>) -> Option<u16> {
    let cell = cell_size(y.width);
    read_cells(y, x0(y.width), 0, cell, CELLS).map(|b| b as u16)
}

/// Draws `index` the way the page does (the checker's test fixtures stand in for the page).
pub fn draw(y: &mut PlaneMut<'_>, index: u16) {
    let cell = cell_size(y.width);
    draw_cells(y, x0(y.width), 0, cell, CELLS, u64::from(index));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strip_sits_where_the_brief_puts_it() {
        // The brief: "After scaling to 720p that's 16 px cells at x = 1024".
        assert_eq!((cell_size(1280), x0(1280)), (16, 1024));
        assert_eq!((cell_size(1920), x0(1920)), (24, 1536));
    }

    #[test]
    fn indexes_round_trip_and_grey_does_not_read() {
        let mut checked = 0;
        for (w, h) in [(1280u32, 720u32), (1920, 1080)] {
            for index in [0u16, 1, 2, 19, 20, 0x8000, u16::MAX] {
                let mut data = vec![70u8; (w * h) as usize];
                let mut p = PlaneMut {
                    data: &mut data,
                    stride: w as usize,
                    width: w,
                    height: h,
                };
                draw(&mut p, index);
                assert_eq!(read(&p.as_plane()), Some(index), "{w}x{h} index {index}");
                checked += 1;
            }
            let data = vec![128u8; (w * h) as usize];
            assert_eq!(
                read(&Plane {
                    data: &data,
                    stride: w as usize,
                    width: w,
                    height: h
                }),
                None
            );
        }
        eprintln!("score strips checked: {checked}");
        assert!(checked > 0);
    }
}
