//! STUB (C08 red step).
use crossbeam_channel::Receiver;

use crate::driver::Driver;
use crate::ports::Inbound;

pub fn run(_driver: Driver, _rx: Receiver<Inbound>, _clock: &dyn Fn() -> u64) -> i32 {
    -1
}
