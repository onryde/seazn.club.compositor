//! Domain time: milliseconds on a monotonic clock that the app layer supplies. The domain never reads a clock.
pub type Ms = u64;

/// `now - since`, saturating at 0 so a reordered timestamp can never underflow into a huge age.
pub fn elapsed(now: Ms, since: Ms) -> Ms {
    now.saturating_sub(since)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_is_difference() {
        assert_eq!(elapsed(1_500, 1_000), 500);
    }

    #[test]
    fn elapsed_saturates_when_since_is_later() {
        assert_eq!(elapsed(1_000, 1_500), 0);
    }

    // Boundaries of the definition "now − since, saturating at 0": the whole u64 range, both ends.
    #[test]
    fn elapsed_holds_at_the_ends_of_the_range() {
        assert_eq!(elapsed(0, 0), 0, "same instant");
        assert_eq!(elapsed(Ms::MAX, Ms::MAX), 0, "same instant at the top");
        assert_eq!(elapsed(Ms::MAX, 0), Ms::MAX, "the largest age is representable");
        assert_eq!(
            elapsed(0, Ms::MAX),
            0,
            "the most reordered stamp is age 0, never a wrap"
        );
    }
}
