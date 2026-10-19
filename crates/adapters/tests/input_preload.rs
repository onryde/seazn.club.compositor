//! M4 (PR #30 review): the input's plugins are loaded when the adapter is built, so the first
//! `rebuild`, on the driver's thread, loads none. Its own test binary, so this process has loaded no
//! plugin before the test: in `tests/input.rs` an earlier test would have loaded them all already.
use std::collections::BTreeSet;

use adapters::frame_store::FrameStore;
use adapters::gst_input::GstInput;
use app::ports::{InputSource, InputSpec};
use domain::config::{OutputProfile, Thresholds};
use gstreamer as gst;
use gstreamer::prelude::*;

fn loaded_plugins() -> BTreeSet<String> {
    gst::Registry::get()
        .plugins()
        .into_iter()
        .filter(|p| p.is_loaded())
        .map(|p| p.plugin_name().to_string())
        .collect()
}

#[test]
fn the_first_rebuild_loads_no_plugin() {
    gst::init().expect("gstreamer");
    let at_init = loaded_plugins();
    let (tx, _rx) = crossbeam_channel::bounded(64);
    let out = OutputProfile::P720;
    let mut input = GstInput::new(
        FrameStore::default(),
        tx,
        &Thresholds::default(),
        out.width,
        out.height,
    );
    let at_new = loaded_plugins();
    // Nothing listens here: the build is what is checked, not a connect.
    input.rebuild(
        1,
        &InputSpec {
            url: "srt://127.0.0.1:9".to_owned(),
            stream_id: String::new(),
            passphrase: String::new(),
        },
    );
    let pipeline = input.pipeline().expect("the build's pipeline");
    let used: BTreeSet<String> = pipeline
        .iterate_elements()
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|e| e.factory())
        .filter_map(|f| f.plugin_name().map(|n| n.to_string()))
        .collect();
    let unloaded_at_init: Vec<&String> = used.difference(&at_init).collect();
    let missing: Vec<&String> = used.difference(&at_new).collect();
    eprintln!(
        "plugins the pipeline uses: {} {used:?}; not loaded at init: {unloaded_at_init:?}; \
         not loaded before the first rebuild: {missing:?}",
        used.len()
    );
    assert!(used.len() >= 5, "the pipeline's plugins were read: {used:?}");
    assert!(
        !unloaded_at_init.is_empty(),
        "this process had them unloaded before the adapter, so the check can see a load"
    );
    assert!(missing.is_empty(), "the first rebuild loaded {missing:?}");
    input.stop();
    // A pipeline still tearing down at process exit aborts the binary (SIGABRT): wait it out.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while input.teardowns_in_flight() > 0 && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(input.teardowns_in_flight(), 0, "the build's teardown returned");
}
