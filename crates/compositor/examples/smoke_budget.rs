#![allow(clippy::print_stdout)] // prints the smoke budget for image.yml (C12 Step 8)
//! The image smoke's budget in milliseconds, computed from the thresholds so a moved threshold moves
//! it (C12 Step 8, review m4; AGENTS.md class 20): image.yml runs this and never types the number.
use domain::config::Thresholds;

/// The smoke's deadline: the image runs with `RELAY_DEADLINE_AT` this far ahead (image.yml).
const SMOKE_DEADLINE_MS: u64 = 60_000;
/// Container start, process start and the session's fetch: Docker's own start plus the binary's
/// (F-005 measured a cold Machine start; a local `docker run` of a loaded image is far quicker).
const CONTAINER_START_MS: u64 = 8_000;

fn main() {
    let t = Thresholds::default();
    let budget = SMOKE_DEADLINE_MS // the deadline (image.yml's RELAY_DEADLINE_AT)
        + t.final_hold_ms // the Final scene's hold at the deadline (spec §4.2, D4)
        + t.drain_timeout_ms // the drain's bound (C06); the fake drains at once, so this is slack
        + CONTAINER_START_MS; // container and process start
    println!("{budget}");
}
