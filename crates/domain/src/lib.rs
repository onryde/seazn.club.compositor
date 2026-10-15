//! Pure domain of the Seazn relay compositor. No I/O, no clock, no env: time is passed in as `Ms`.
//! Purity is enforced by `tests/purity.rs` and `tests/deps.rs`.
#![forbid(unsafe_code)]
