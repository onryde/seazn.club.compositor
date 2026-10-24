//! When a reader that decodes nothing says so (Owed 81's `input-tap`, the PR #59 review's m-4): on
//! the alignment-1 run its second build read for about 3 minutes with no frame, no error and no end,
//! and logged nothing. A `Silence` tells the caller once per silence, the first time it has lasted
//! `after_ms` since the build started or since its last frame, and again only after a frame ends it.

/// One build's silence watch, on any millisecond clock the caller reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Silence {
    after_ms: i64,
    since_ms: i64,
    told: bool,
}

impl Silence {
    /// A watch that starts silent at `now_ms` (a build that has decoded nothing yet).
    pub fn new(after_ms: i64, now_ms: i64) -> Self {
        Self {
            after_ms,
            since_ms: now_ms,
            told: false,
        }
    }

    /// A frame came: the silence, if any, is over.
    pub fn frame(&mut self, now_ms: i64) {
        self.since_ms = now_ms;
        self.told = false;
    }

    /// `Some(silent ms)` the first time the silence has lasted `after_ms`; `None` before that, and
    /// after it was told until a frame ends it.
    pub fn check(&mut self, now_ms: i64) -> Option<i64> {
        let silent = now_ms - self.since_ms;
        if self.told || silent < self.after_ms {
            return None;
        }
        self.told = true;
        Some(silent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_that_never_decodes_is_told_once_when_the_silence_reaches_the_bound() {
        let mut s = Silence::new(5_000, 1_000);
        assert_eq!(s.check(1_000), None, "no time has passed");
        assert_eq!(s.check(5_999), None, "4 999 ms is under the bound");
        assert_eq!(s.check(6_000), Some(5_000), "the bound itself is told");
        assert_eq!(s.check(9_000), None, "told once, not every poll");
        assert_eq!(s.check(60_000), None);
    }

    #[test]
    fn a_frame_ends_the_silence_and_the_next_one_is_told_again() {
        let mut s = Silence::new(5_000, 0);
        assert_eq!(s.check(5_000), Some(5_000));
        s.frame(7_000);
        assert_eq!(s.check(11_999), None, "4 999 ms since the frame");
        assert_eq!(
            s.check(12_500),
            Some(5_500),
            "a second silence, measured from the frame"
        );
        assert_eq!(s.check(13_000), None);
    }

    #[test]
    fn frames_that_keep_coming_are_never_a_silence() {
        let mut s = Silence::new(5_000, 0);
        let mut checked = 0;
        for t in (0..600_000).step_by(33) {
            s.frame(t);
            assert_eq!(s.check(t + 32), None, "at {t} ms");
            checked += 1;
        }
        eprintln!("polls checked: {checked}");
        assert_eq!(checked, 18_182);
    }
}
