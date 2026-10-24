//! The app event loop's core (C08): it folds every inbound message and every due tick through the
//! real `Supervisor`, and applies each `Decision`'s effects through the ports, in order. It reads no
//! clock: every call carries `now` (ms), so the scenario tests run it on a manual clock and `run()`
//! on the wall clock.
//!
//! - **The tick grid.** Tick `i` is due at `origin + ⌈i·1000 / rate⌉` ms, exact for every rate (a
//!   rounded period drifts: a 60 fps period rounded to the nanosecond puts its 50 ms tick at 51 ms).
//!   Before the session the rate is 10 per second from `boot_ms`; the session resets it to its
//!   profile's `fps`, with tick 0 at the session's instant. `tick_due` runs at most one second of ticks
//!   per call, oldest first; past that it jumps to the first index not yet due and counts the jump
//!   in `skipped_ticks`. Every tick in one call is decided at that call's `now`.
//! - **The session** arrives once, while booting; a second one changes nothing. A session with no
//!   frame rate cannot be paced, and is refused as one that is not ours (78). An inbound `Tick` is
//!   refused (the driver owns the tick), and a wiring effect with no session (an inbound
//!   `BootstrapOk`) exits as a fatal error.
//! - **The phase** is published after every `decide` that changed it, with a rising `seq` from
//!   `(Waiting, 0)` (amendment 2026-10-15).
//! - **The log** gets one line per hide (`overlay hidden reason=<r> gen=<g>`), one per sink swap
//!   before the port call (`sink swap reason=first|error|connect_timeout|stall gen=<g>`, Owed 87:
//!   what ended the sink before it; the port's own `sink swap gen=<g> target=…` line follows), and
//!   one per graphics event handed to `decide`, a stale one included (`cef ready|crashed|alive
//!   gen=<n>`; C12's amendment 2026-10-18). The skipped ticks and the queue's dropped beats reach it too
//!   (`ticks skipped total=<n>`, `heartbeats dropped total=<n>`; review round 1, item 4): a line when
//!   the count rises, at most one a minute, and whatever rose since the last line at the exit. No line
//!   carries a credential or a fatal error's text.
//! - **CEF's health** (C11b) is read from the graphics port once per `tick_due` that ran a tick, after
//!   its ticks, and handed to `decide` every time (the domain decides the in-place restart, never the
//!   driver), with one line when it changes (`cef health zygote_gone=<gen|-> leaked_teardowns=<n>
//!   gpu_process_deaths=<n>`). Never after `Exit`.
//! - **After `Exit`** nothing reaches a port from `step` or `tick_due`. `run()`'s flush follows, and
//!   logs `final beat not confirmed within 2000 ms` when it times out.
use std::time::Duration;

use domain::cef_health::CefHealth;
use domain::config::Thresholds;
use domain::graphics::HideReason;
use domain::health::ExitDetail;
use domain::lifecycle::{ExitReason, Lifecycle};
use domain::output_link::SwapReason;
use domain::programme::ProgrammePhase;
use domain::supervisor::{Decision, Effect, Event, Refusal, Supervisor};

use crate::ports::{ControlRequest, Inbound, Ports, SessionConfig, FINAL_BEAT_TIMEOUT_MS};

/// Ticks per second before the session gives a profile.
const BOOT_RATE: u64 = 10;

/// The least time between two lines of one counter (review round 1, item 4).
const TALLY_EVERY_MS: u64 = 60_000;

/// The fatal text for a wiring effect with no session to wire from.
pub(crate) const NO_SESSION: &str = "a session effect arrived before a session";
/// The fatal text for a loop whose every sender has gone.
pub(crate) const INBOUND_CLOSED: &str = "inbound channel closed";

/// Tick `i` is due at `origin + ⌈i·1000 / rate⌉` ms; `next` is the next index to run.
#[derive(Debug, Clone, Copy)]
struct Grid {
    origin: u64,
    rate: u64,
    next: u64,
}

impl Grid {
    fn due_at(&self, i: u64) -> u64 {
        self.origin
            .saturating_add(i.saturating_mul(1_000).div_ceil(self.rate))
    }

    /// The first index not yet due at `now` (`now ≥ origin`): one past the last `i` with
    /// `i·1000 ≤ (now − origin)·rate`.
    fn first_not_due(&self, now: u64) -> u64 {
        now.saturating_sub(self.origin).saturating_mul(self.rate) / 1_000 + 1
    }
}

/// A counter's log line: the total last logged, and when.
#[derive(Debug, Clone, Copy, Default)]
struct Tally {
    logged: u64,
    at: Option<u64>,
}

impl Tally {
    /// True (and the line counted as written) when `total` rose past the last line and that line is
    /// at least `TALLY_EVERY_MS` old, or there is none.
    fn due(&mut self, total: u64, now: u64) -> bool {
        if total <= self.logged || self.at.is_some_and(|at| now.saturating_sub(at) < TALLY_EVERY_MS) {
            return false;
        }
        self.logged = total;
        self.at = Some(now);
        true
    }

    /// At the exit: true when `total` rose past the last line, whenever that was.
    fn rest(&mut self, total: u64) -> bool {
        if total <= self.logged {
            return false;
        }
        self.logged = total;
        true
    }
}

#[derive(Debug)]
pub struct Driver {
    supervisor: Supervisor,
    ports: Ports,
    session: Option<SessionConfig>,
    deadline_ms: u64,
    grid: Grid,
    skipped_ticks: u64,
    skips: Tally,
    drops: Tally,
    phase: ProgrammePhase,
    phase_seq: u64,
    /// CEF's health as last read (C11b): its line is written when it changes.
    cef: CefHealth,
    exited: bool,
}

impl Driver {
    pub fn new(t: Thresholds, ports: Ports, deadline_ms: u64, boot_ms: u64) -> Self {
        Self {
            supervisor: Supervisor::new(t),
            ports,
            session: None,
            deadline_ms,
            grid: Grid {
                origin: boot_ms,
                rate: BOOT_RATE,
                next: 0,
            },
            skipped_ticks: 0,
            skips: Tally::default(),
            drops: Tally::default(),
            phase: ProgrammePhase::Waiting,
            phase_seq: 0,
            cef: CefHealth::default(),
            exited: false,
        }
    }

    /// One inbound message. `Some(code)` ⇒ exit.
    pub fn step(&mut self, msg: Inbound, now: u64) -> Option<i32> {
        if self.exited {
            return None;
        }
        match msg {
            Inbound::Session(cfg) => self.bootstrap(cfg, now),
            Inbound::Event(Event::Tick) => None,
            Inbound::Event(e) => {
                if let Some(line) = graphics_line(&e) {
                    self.ports.log.info(&line);
                }
                self.decide(e, now)
            }
        }
    }

    /// Every tick index due by `now`, at most one second of them. `Some(code)` ⇒ exit.
    pub fn tick_due(&mut self, now: u64) -> Option<i32> {
        let mut ran = 0;
        while !self.exited && self.grid.due_at(self.grid.next) <= now {
            if ran == self.grid.rate {
                let to = self.grid.first_not_due(now);
                self.skipped_ticks += to - self.grid.next;
                self.grid.next = to;
                if self.skips.due(self.skipped_ticks, now) {
                    let line = format!("ticks skipped total={}", self.skipped_ticks);
                    self.ports.log.info(&line);
                }
                break;
            }
            let index = self.grid.next;
            self.grid.next += 1;
            ran += 1;
            let d = self.supervisor.decide(Event::Tick, now);
            let frame = d.frame;
            if let Some(code) = self.apply(d, now) {
                return Some(code);
            }
            if let Some(choice) = frame {
                self.ports.programme.push(index, choice);
            }
        }
        // An exit returned above, so a tick that ran means the driver is live.
        if ran > 0 {
            return self.cef_health(now);
        }
        None
    }

    /// CEF's health, read once and handed to `decide` (C11b), with its line when it changed.
    fn cef_health(&mut self, now: u64) -> Option<i32> {
        let h = self.ports.graphics.health();
        if h != self.cef {
            self.cef = h;
            let zygote = h.unrecoverable.map_or_else(|| "-".to_owned(), |g| g.to_string());
            let line = format!(
                "cef health zygote_gone={zygote} leaked_teardowns={} gpu_process_deaths={}",
                h.leaked_teardowns, h.gpu_process_deaths
            );
            self.ports.log.info(&line);
        }
        self.decide(Event::CefHealth(h), now)
    }

    pub fn next_tick_at(&self) -> u64 {
        self.grid.due_at(self.grid.next)
    }

    pub fn skipped_ticks(&self) -> u64 {
        self.skipped_ticks
    }

    /// (Amended 2026-10-15, review I1) The supervisor's current programme phase, read-only.
    pub fn programme_phase(&self) -> ProgrammePhase {
        self.supervisor.programme_phase()
    }

    /// The loop's inbound channel has closed: no event can ever arrive, so exit as a fatal error.
    pub(crate) fn inbound_closed(&mut self, now: u64) -> i32 {
        let detail = Some(ExitDetail::new(INBOUND_CLOSED));
        self.step(Inbound::Event(Event::Fatal { detail }), now)
            .unwrap_or(ExitReason::Fatal.code())
    }

    /// Waits for the final beats to post, at most `FINAL_BEAT_TIMEOUT_MS` (Owed 28), and logs a
    /// beat it could not confirm (one that hung, as in DNS, or whose worker has gone). `run()` calls
    /// it once, after `Exit`: the one line after it.
    pub(crate) fn flush_final_beats(&mut self) -> bool {
        let posted = self
            .ports
            .control
            .flush(Duration::from_millis(FINAL_BEAT_TIMEOUT_MS));
        if !posted {
            let line = format!("final beat not confirmed within {FINAL_BEAT_TIMEOUT_MS} ms");
            self.ports.log.info(&line);
        }
        posted
    }

    fn bootstrap(&mut self, cfg: SessionConfig, now: u64) -> Option<i32> {
        if !matches!(self.supervisor.lifecycle(), Lifecycle::Booting { .. }) {
            return None;
        }
        if cfg.output.fps == 0 {
            return self.decide(Event::BootstrapRefused(Refusal::WrongMode), now);
        }
        let heartbeat_interval_ms = cfg.heartbeat_interval_ms;
        self.grid = Grid {
            origin: now,
            rate: u64::from(cfg.output.fps),
            next: 0,
        };
        self.session = Some(cfg);
        self.decide(
            Event::BootstrapOk {
                heartbeat_interval_ms,
                deadline_at: self.deadline_ms,
            },
            now,
        )
    }

    fn decide(&mut self, e: Event, now: u64) -> Option<i32> {
        let d = self.supervisor.decide(e, now);
        self.apply(d, now)
    }

    fn apply(&mut self, d: Decision, now: u64) -> Option<i32> {
        self.supervisor = d.next;
        let phase = self.supervisor.programme_phase();
        if phase != self.phase {
            self.phase = phase;
            self.phase_seq += 1;
            self.ports.phase.publish(phase, self.phase_seq);
        }
        for e in d.effects {
            if let Some(code) = self.effect(e, now) {
                return Some(code);
            }
        }
        None
    }

    fn effect(&mut self, e: Effect, now: u64) -> Option<i32> {
        let p = &mut self.ports;
        match e {
            Effect::FetchSession => p.control.request(ControlRequest::FetchSession),
            Effect::StartProgramme => {
                let Some(cfg) = &self.session else {
                    return self.no_session(now);
                };
                if let Err(text) = p.programme.start(&cfg.output) {
                    let detail = Some(ExitDetail::new(text));
                    return self.decide(Event::Fatal { detail }, now);
                }
            }
            Effect::RebuildInput { gen } => {
                let Some(cfg) = &self.session else {
                    return self.no_session(now);
                };
                p.input.rebuild(gen, &cfg.input);
            }
            // The domain gave the generation up (C10b): the adapter shuts it out. No session needed.
            Effect::AbandonInput { gen } => p.input.abandon(gen),
            Effect::SwapSink { gen, reason } => {
                let Some(cfg) = &self.session else {
                    return self.no_session(now);
                };
                p.log
                    .info(&format!("sink swap reason={} gen={gen}", swap_name(reason)));
                p.programme.swap_sink(gen, &cfg.target);
            }
            Effect::RestartGraphics { gen } => {
                let Some(cfg) = &self.session else {
                    return self.no_session(now);
                };
                p.graphics.restart(gen, &cfg.overlay_url);
            }
            Effect::HideOverlay { reason, gen } => {
                p.log
                    .info(&format!("overlay hidden reason={} gen={gen}", hide_name(reason)));
                // A hung or failed page is stopped (Owed 17), BEFORE the slot is cleared: the stop
                // closes the page's gate and waits out a sample being written, which would otherwise
                // land after the clear and stay on air (PR #32 review I2, Owed 57). A crashed or
                // recycled generation is already gone, and the reload now loading must survive (C05b
                // PR #22 review).
                if matches!(reason, HideReason::Hang | HideReason::Failed) {
                    p.graphics.stop();
                }
                p.programme.hide_overlay();
            }
            Effect::StepDown(r) => p.programme.step_down(r),
            Effect::SendHeartbeat(h) => {
                let m = p.metrics.sample();
                p.control.request(ControlRequest::Heartbeat(h, m));
                let dropped = p.control.dropped();
                if self.drops.due(dropped, now) {
                    p.log.info(&format!("heartbeats dropped total={dropped}"));
                }
            }
            Effect::DrainOutput => {
                p.programme.drain();
                p.input.stop();
                p.graphics.stop();
            }
            Effect::Exit(code) => {
                p.input.stop();
                p.graphics.stop();
                if self.skips.rest(self.skipped_ticks) {
                    p.log.info(&format!("ticks skipped total={}", self.skipped_ticks));
                }
                let dropped = p.control.dropped();
                if self.drops.rest(dropped) {
                    p.log.info(&format!("heartbeats dropped total={dropped}"));
                }
                self.exited = true;
                return Some(code);
            }
        }
        None
    }

    /// A wiring effect with no session to wire from (an inbound `BootstrapOk`): a fatal error.
    fn no_session(&mut self, now: u64) -> Option<i32> {
        let detail = Some(ExitDetail::new(NO_SESSION));
        self.decide(Event::Fatal { detail }, now)
    }
}

/// Owed 87: the domain's swap reason as the log line names it.
fn swap_name(r: SwapReason) -> &'static str {
    match r {
        SwapReason::First => "first",
        SwapReason::Error => "error",
        SwapReason::ConnectTimeout => "connect_timeout",
        SwapReason::Stall => "stall",
    }
}

fn hide_name(r: HideReason) -> &'static str {
    match r {
        HideReason::Hang => "hang",
        HideReason::Failed => "failed",
        HideReason::Crash => "crash",
        HideReason::Recycle => "recycle",
    }
}

fn graphics_line(e: &Event) -> Option<String> {
    match e {
        Event::CefReady { gen } => Some(format!("cef ready gen={gen}")),
        Event::CefCrashed { gen } => Some(format!("cef crashed gen={gen}")),
        Event::CefAlive { gen } => Some(format!("cef alive gen={gen}")),
        _ => None,
    }
}
