//! STUB (C08 red step).
use domain::config::Thresholds;
use domain::programme::ProgrammePhase;
use domain::supervisor::Supervisor;

use crate::ports::{Inbound, Ports};

#[derive(Debug)]
pub struct Driver {
    supervisor: Supervisor,
    ports: Ports,
}

impl Driver {
    pub fn new(t: Thresholds, ports: Ports, _deadline_ms: u64, _boot_ms: u64) -> Self {
        Self {
            supervisor: Supervisor::new(t),
            ports,
        }
    }
    pub fn step(&mut self, _msg: Inbound, _now: u64) -> Option<i32> {
        None
    }
    pub fn tick_due(&mut self, _now: u64) -> Option<i32> {
        None
    }
    pub fn next_tick_at(&self) -> u64 {
        u64::MAX
    }
    pub fn skipped_ticks(&self) -> u64 {
        0
    }
    pub fn programme_phase(&self) -> ProgrammePhase {
        let _ = &self.ports;
        self.supervisor.programme_phase()
    }
}
