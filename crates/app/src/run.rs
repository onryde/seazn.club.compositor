//! The blocking loop (C08): a thin wrapper around the driver. It waits for a message until the next
//! tick is due, hands a message to `step` and runs `tick_due` on the clock, and returns the exit
//! code once the final beats have posted or `FINAL_BEAT_TIMEOUT_MS` has passed (Owed 28: the flush
//! runs on the queue's own clock, never the HTTP layer's).
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};

use crate::driver::Driver;
use crate::ports::Inbound;

pub fn run(mut driver: Driver, rx: Receiver<Inbound>, clock: &dyn Fn() -> u64) -> i32 {
    let code = loop {
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
