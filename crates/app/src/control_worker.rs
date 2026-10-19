//! STUB (C08 red step).
use std::time::Duration;

use crossbeam_channel::Sender;

use crate::ports::{ControlPlane, ControlQueue, ControlRequest, Inbound};

#[derive(Debug)]
pub struct ControlHandle {
    dropped: u64,
}

impl ControlHandle {
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

impl ControlQueue for ControlHandle {
    fn request(&mut self, _req: ControlRequest) {}
    fn flush(&mut self, _timeout: Duration) -> bool {
        false
    }
}

pub fn spawn(_cp: Box<dyn ControlPlane>, _tx: Sender<Inbound>) -> ControlHandle {
    ControlHandle { dropped: 0 }
}
