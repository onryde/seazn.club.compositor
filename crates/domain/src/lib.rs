//! Pure domain of the Seazn relay compositor. No I/O, no clock, no env: time is passed in as `Ms`.
//! Purity is enforced by `tests/purity.rs` and `tests/deps.rs`.
#![forbid(unsafe_code)]
// The domain never writes to stderr; adapters and app own it (C12 logs there). Pinned by tests/purity.rs.
#![forbid(clippy::print_stderr)]
