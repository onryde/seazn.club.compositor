//! Capped exponential backoff: base · 2^attempt, clamped to `cap`.
use crate::time::Ms;

pub fn delay(attempt: u32, base: Ms, cap: Ms) -> Ms {
    // 2^attempt is exact below 64; from 64 up it does not fit, and u64::MAX stands in for it so the
    // product saturates (and a zero base stays zero).
    let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    base.saturating_mul(factor).min(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    // PDF failure table: "reconnects forever with back-off (1 s → 10 s)".
    #[test]
    fn input_sequence_is_1_2_4_8_then_capped_at_10_s() {
        let got: Vec<Ms> = (0..7).map(|a| delay(a, 1_000, 10_000)).collect();
        assert_eq!(got, vec![1_000, 2_000, 4_000, 8_000, 10_000, 10_000, 10_000]);
    }

    #[test]
    fn huge_attempt_never_overflows() {
        assert_eq!(delay(u32::MAX, 1_000, 10_000), 10_000);
    }

    #[test]
    fn zero_base_is_zero() {
        assert_eq!(delay(5, 0, 10_000), 0);
    }

    // PDF budget "output reconnect ≤ 5 s": back-off 0.5 s → 2 s. A success resets the caller's attempt
    // to 0, and the sequence starts over at the base: the function holds no state of its own.
    #[test]
    fn output_sequence_is_half_1_2_then_capped_and_restarts_at_base_after_a_reset() {
        let run = || (0..5).map(|a| delay(a, 500, 2_000)).collect::<Vec<Ms>>();
        assert_eq!(run(), vec![500, 1_000, 2_000, 2_000, 2_000]);
        assert_eq!(
            run(),
            vec![500, 1_000, 2_000, 2_000, 2_000],
            "a second run after a reset"
        );
    }

    // The contract is base · 2^attempt until the cap binds. With a cap that never binds, the exact
    // power of two must come back, all the way to the top bit.
    #[test]
    fn exponent_is_exact_until_the_cap_binds() {
        assert_eq!(delay(40, 1, Ms::MAX), 1_099_511_627_776, "2^40");
        assert_eq!(
            delay(63, 1, Ms::MAX),
            9_223_372_036_854_775_808,
            "2^63, the top bit"
        );
    }

    // 2^64 does not fit a u64: the product saturates instead of wrapping to a tiny (or zero) delay.
    #[test]
    fn attempt_past_the_word_saturates() {
        assert_eq!(delay(64, 1, Ms::MAX), Ms::MAX);
        assert_eq!(delay(u32::MAX, 1, Ms::MAX), Ms::MAX);
        assert_eq!(
            delay(64, 0, Ms::MAX),
            0,
            "zero base stays zero even at saturation"
        );
    }

    // 1_000 · 2^62 = 2^65 · 125: wrapping arithmetic reads it as 0 ms, i.e. a hot retry loop.
    #[test]
    fn product_overflow_saturates_instead_of_wrapping() {
        assert_eq!(delay(62, 1_000, 10_000), 10_000);
        assert_eq!(delay(63, 2, Ms::MAX), Ms::MAX);
    }

    #[test]
    fn cap_wins_even_over_the_base() {
        assert_eq!(delay(0, 1_000, 0), 0, "zero cap");
        assert_eq!(
            delay(0, 5_000, 2_000),
            2_000,
            "a base above its cap is clamped on the first try"
        );
    }
}
