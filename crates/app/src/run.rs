//! The blocking loop (C08): a thin wrapper around the driver. It waits for a message until the next
//! tick is due, hands a message to `step` and runs `tick_due` on the clock, and returns the exit
//! code once the final beats have posted or `FINAL_BEAT_TIMEOUT_MS` has passed (Owed 28: the flush
//! runs on the queue's own clock, never the HTTP layer's).
//!
//! Each turn first steps the messages already queued, at most as many as were queued when it began
//! (so a full channel cannot starve the ticks), then ticks. A driver thread that stalled (a VM
//! pause, a blocking port call) so reads what arrived meanwhile before any timer judges its absence:
//! the egress backstop must not end a healthy sink on growth readings still in the queue (the PR #76
//! review's m-5).
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};

use crate::driver::Driver;
use crate::ports::Inbound;

pub fn run(mut driver: Driver, rx: Receiver<Inbound>, clock: &dyn Fn() -> u64) -> i32 {
    let code = 'turn: loop {
        for _ in 0..rx.len() {
            let Ok(msg) = rx.try_recv() else { break };
            if let Some(code) = driver.step(msg, clock()) {
                break 'turn code;
            }
        }
        if let Some(code) = driver.tick_due(clock()) {
            break code;
        }
        let wait = driver.next_tick_at().saturating_sub(clock());
        match rx.recv_timeout(Duration::from_millis(wait)) {
            Ok(msg) => {
                if let Some(code) = driver.step(msg, clock()) {
                    break code;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break driver.inbound_closed(clock()),
        }
    };
    driver.flush_final_beats();
    code
}
