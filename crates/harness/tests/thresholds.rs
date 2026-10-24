//! The drift gate's media half (C14): `harness/thresholds.json` against the adapters' own constants
//! (the host half, in `harness::thresholds`, checks the domain's and the budgets), and the budgets
//! that an adapter constant bounds.
use harness::thresholds::{
    render, AdapterConsts, BUDGET_OUTPUT_LOST_MAX_MS, BUDGET_ZYGOTE_VERDICT_MS, COMMITTED,
};

fn live() -> AdapterConsts {
    AdapterConsts {
        output_progress_poll_ms: adapters::gst_output::OUTPUT_PROGRESS_POLL_MS,
        cef_child_poll_ms: adapters::cef_graphics::CHILD_POLL_MS,
        encoded_ring_span_ms: adapters::gst_output::ENCODED_RING_SPAN.as_millis() as u64,
    }
}

#[test]
fn the_committed_thresholds_carry_the_adapters_constants() {
    let text = std::fs::read_to_string(COMMITTED).expect("harness/thresholds.json is committed");
    assert_eq!(
        text,
        render(live()),
        "harness/thresholds.json is stale: run `cargo run -p harness --bin dump-thresholds > harness/thresholds.json`"
    );
    eprintln!("thresholds.json bytes checked: {}", text.len());
    assert!(!text.is_empty());
}

#[test]
fn the_adapter_bounded_budgets_hold() {
    let a = live();
    let frame = domain::config::OutputProfile::P720.frame_period_ms();
    // Case 13: the 1 s child poll and a tick fit the zygote verdict's window.
    assert!(a.cef_child_poll_ms as f64 + frame <= BUDGET_ZYGOTE_VERDICT_MS as f64);
    // Spec §6: the 2 s leaky output queue is the most programme an outage may lose.
    assert!(a.encoded_ring_span_ms <= BUDGET_OUTPUT_LOST_MAX_MS);
}
