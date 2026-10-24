#![allow(clippy::print_stdout)] // harness tools report on stdout by design (C13)
//! `dump-thresholds` (C14): prints `harness/thresholds.json`, the numbers every chaos case's verdict
//! reads as `$t` (`harness::thresholds`). Regenerate after any threshold, budget or named adapter
//! constant changes:
//!
//! ```text
//! cargo run -p harness --bin dump-thresholds > harness/thresholds.json
//! ```
//!
//! The drift tests (`harness::thresholds`'s host test and `tests/thresholds.rs`) fail until it is.
use harness::thresholds::{render, AdapterConsts};

fn main() {
    print!(
        "{}",
        render(AdapterConsts {
            output_progress_poll_ms: adapters::gst_output::OUTPUT_PROGRESS_POLL_MS,
            cef_child_poll_ms: adapters::cef_graphics::CHILD_POLL_MS,
            encoded_ring_span_ms: adapters::gst_output::ENCODED_RING_SPAN.as_millis() as u64,
            reaper_kill_after_ms: adapters::cef_graphics::REAPER_KILL_AFTER_MS,
            teardown_leak_after_ms: adapters::cef_graphics::TEARDOWN_LEAK_AFTER_MS,
        })
    );
}
