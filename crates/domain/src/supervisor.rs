//! The aggregate. `decide` is the only place compositor state changes (AGENTS.md §4): the app layer
//! applies `effects` through ports and pushes `frame` to the programme pipeline. It mirrors
//! `seazn.club`'s `session.ts` `decide()`.
//!
//! ```text
//! Booting --BootstrapOk--> Running --end request--> Holding --final_hold_ms--> Draining --> Exited
//!    |                        |                        |                          ^  (drained, or
//!    |                        +--------- SIGTERM ------+--------------------------+   drain_timeout_ms)
//!    +-- refused | signal | deadline already past --> Exited      (fatal: Exited from every state)
//! ```
//!
//! - **Booting** fetches the session, one fetch in flight at a time, retried on the bootstrap
//!   back-off. A refusal, a signal, a fatal error or a deadline already past exits at once: nothing is
//!   on air, so there is nothing to hold or drain.
//! - **Running** ticks every sub-machine and yields a frame on every tick. The beat's `Playing`,
//!   `Stalled` and `Starting` are derived from the output (owner ruling F5, option A), never stored.
//! - **Holding** is the final hold (overlay templates spec 2026-10-15 §4.2, D4), entered by the FIRST
//!   end request while Running: an `Ending` reply (`Ended`), `SIGINT` (web's own stop, `Signalled`),
//!   or the deadline tick (`Deadline`). The Final scene is on air for `final_hold_ms`, then the drain
//!   starts with the hold's reason; a second end request keeps the first hold's clock. Nothing is
//!   rebuilt in the hold: the links are not ticked, and graphics run only the bound (the frame on air
//!   comes off `cef_hang_after_ms` after its page's last proof of life; coordinator decision on the
//!   C05b PR #22 review), so a hang or a crash near Final still comes off air, and a restart or a
//!   recycle due in the hold waits for the drain, which stops the page (C08).
//! - **SIGTERM** is a stop the host started (coordinator decision I4, measured in F-008): it drains at
//!   once from Running or Holding, with no Final scene.
//! - **Draining** sends EOS to the output and exits once drained, or after `drain_timeout_ms`.
//! - **Exited** is terminal and ignores every later event.
//! - **The final beat** (owner ruling 2026-10-15): every path to `Exit` sends a `Stopped` beat that
//!   carries its reason first. A drain sends it when the drain starts; an immediate exit sends it in
//!   the same `Decision`, before `Exit`.
//! - **Generations.** Every adapter event carries the generation of the pipeline or page that sent
//!   it, and each sub-machine ignores any other (C04, C05).
use crate::backoff::delay;
use crate::config::Thresholds;
use crate::graphics::{Graphics, GraphicsAction, GraphicsState, HideReason};
use crate::health::{BeatState, ExitInfo, GraphicsStatus, HealthReport, LinkStatus, PhaseChannel};
use crate::input_link::{InputLink, InputState, LossReason};
use crate::ladder::{Ladder, Rung};
use crate::lifecycle::{ExitReason, Lifecycle};
use crate::output_link::{OutputLink, OutputState};
use crate::programme::{FrameChoice, Programme, ProgrammePhase};
use crate::time::{elapsed, Ms};

/// The heartbeat reply's `desired` (R1 contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desired {
    Live,
    Ending,
}

/// `Int` is web's own stop and its deadline stop; `Term` is a stop the host started (coordinator
/// decision I4; F-008 measured that web's explicit `SIGINT` wins over the Machine's stop signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    Term,
    Int,
}

/// Why the bootstrap was refused (C07 maps 410, 401, 404, and a 200 that is not a composed session).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Gone,
    Unauthorized,
    NotFound,
    WrongMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Tick,
    BootstrapOk {
        heartbeat_interval_ms: Ms,
        deadline_at: Ms,
    },
    BootstrapFailed,
    BootstrapRefused(Refusal),
    InputConnected {
        gen: u64,
    },
    InputFrame,
    InputLost {
        gen: u64,
        reason: LossReason,
    },
    /// The current sink's bytes-sent total, counted from 0 for each sink (C09: rtmp2sink
    /// `out-bytes-total`). It replaces the brief's `OutputConnected` and `OutputAck`: F-005 found CF
    /// Input B sends no RTMP acknowledgements, and C04's `OutputLink` reads bytes sent (Owed 13).
    OutputProgress {
        gen: u64,
        total_bytes: u64,
    },
    OutputError {
        gen: u64,
    },
    OutputDrained,
    EncoderOverrun,
    CefReady {
        gen: u64,
    },
    CefCrashed {
        gen: u64,
    },
    /// The page's `seazn:alive` beat (R2005; C11 relays it; Owed 18, owner 2026-10-18 "2 Ok").
    CefAlive {
        gen: u64,
    },
    ScoreStale(bool),
    HeartbeatReply(Desired),
    HeartbeatFailed,
    Signal(SignalKind),
    Fatal,
    /// The page's phase channel, as it reports it (overlay templates spec 2026-10-15 §4.3; C11).
    PhaseChannel(PhaseChannel),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    FetchSession,
    StartProgramme,
    RebuildInput {
        gen: u64,
    },
    SwapSink {
        gen: u64,
    },
    RestartGraphics {
        gen: u64,
    },
    /// The domain hide's reason and generation, unchanged (C05b PR #22 review I2): C12 logs
    /// `overlay hidden reason=<r> gen=<g>`, and C08 maps the reason.
    HideOverlay {
        reason: HideReason,
        gen: u64,
    },
    StepDown(Rung),
    SendHeartbeat(HealthReport),
    DrainOutput,
    Exit(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub next: Supervisor,
    pub effects: Vec<Effect>,
    pub frame: Option<FrameChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supervisor {
    t: Thresholds,
    lifecycle: Lifecycle,
    programme: Programme,
    input: InputLink,
    output: OutputLink,
    graphics: Graphics,
    ladder: Ladder,
    score_stale: Option<bool>,
    phase_channel: Option<PhaseChannel>,
    heartbeat_interval_ms: Ms,
    next_beat_at: Ms,
    deadline_at: Ms,
    output_ever_connected: bool,
    heartbeat_failures: u32,
}

impl Supervisor {
    pub fn new(t: Thresholds) -> Self {
        let hb = t.heartbeat_interval_ms;
        Self {
            t,
            lifecycle: Lifecycle::Booting {
                attempt: 0,
                retry_at: Some(0),
            },
            programme: Programme::default(),
            input: InputLink::new(),
            output: OutputLink::new(),
            graphics: Graphics::new(),
            ladder: Ladder::new(),
            score_stale: None,
            phase_channel: None,
            heartbeat_interval_ms: hb,
            next_beat_at: 0,
            deadline_at: Ms::MAX,
            output_ever_connected: false,
            heartbeat_failures: 0,
        }
    }

    pub fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }

    pub fn programme_phase(&self) -> ProgrammePhase {
        self.programme.phase()
    }

    pub fn input_generation(&self) -> u64 {
        self.input.generation()
    }

    pub fn output_generation(&self) -> u64 {
        self.output.generation()
    }

    pub fn graphics_generation(&self) -> u64 {
        self.graphics.generation()
    }

    pub fn thresholds(&self) -> &Thresholds {
        &self.t
    }

    /// The one place compositor state changes (AGENTS.md §4). Pure: the same state, event and instant
    /// always give the same decision.
    pub fn decide(&self, event: Event, now: Ms) -> Decision {
        let mut next = self.clone();
        let mut fx = Vec::new();
        let mut frame = None;
        match self.lifecycle {
            Lifecycle::Exited(_) => {}
            Lifecycle::Booting { attempt, retry_at } => next.booting(event, now, attempt, retry_at, &mut fx),
            Lifecycle::Running => frame = next.running(event, now, &mut fx),
            Lifecycle::Holding { since, reason } => frame = next.holding(event, now, since, reason, &mut fx),
            Lifecycle::Draining { since, reason } => next.draining(event, now, since, reason, &mut fx),
        }
        Decision {
            next,
            effects: fx,
            frame,
        }
    }

    /// An exit with nothing left to drain: the final beat, then `Exit`, in one decision.
    fn exit(&mut self, now: Ms, reason: ExitReason, fx: &mut Vec<Effect>) {
        self.lifecycle = Lifecycle::Exited(reason);
        fx.push(Effect::SendHeartbeat(self.health(now)));
        fx.push(Effect::Exit(reason.code()));
    }

    /// The drain's end. Its final beat went out when it started, so this is `Exit` alone.
    fn finish_drain(&mut self, reason: ExitReason, fx: &mut Vec<Effect>) {
        self.lifecycle = Lifecycle::Exited(reason);
        fx.push(Effect::Exit(reason.code()));
    }

    fn start_drain(&mut self, now: Ms, reason: ExitReason, fx: &mut Vec<Effect>) {
        self.lifecycle = Lifecycle::Draining { since: now, reason };
        fx.push(Effect::DrainOutput);
        fx.push(Effect::SendHeartbeat(self.health(now)));
    }

    fn start_hold(&mut self, now: Ms, reason: ExitReason) {
        self.lifecycle = Lifecycle::Holding { since: now, reason };
        self.programme.finish();
        // (Confirm pass N1, 2026-10-15) The first hold tick beats at once, so web learns `final` at
        // the start of the hold, not up to `heartbeat_interval_ms` later (`_INDEX.md` Owed 10).
        self.next_beat_at = now;
    }

    /// The beat, on the first tick at or after it is due.
    fn beat_if_due(&mut self, now: Ms, fx: &mut Vec<Effect>) {
        if now >= self.next_beat_at {
            fx.push(Effect::SendHeartbeat(self.health(now)));
            self.next_beat_at = now.saturating_add(self.heartbeat_interval_ms);
        }
    }

    fn booting(&mut self, event: Event, now: Ms, attempt: u32, retry_at: Option<Ms>, fx: &mut Vec<Effect>) {
        match event {
            Event::Tick => {
                if retry_at.is_some_and(|r| now >= r) {
                    self.lifecycle = Lifecycle::Booting {
                        attempt,
                        retry_at: None,
                    };
                    fx.push(Effect::FetchSession);
                }
            }
            // One fetch has one outcome: a failure counts only while a fetch is in flight, so a second
            // report for the same fetch neither moves the retry nor doubles the next wait.
            Event::BootstrapFailed if retry_at.is_none() => {
                let wait = delay(
                    attempt,
                    self.t.bootstrap_backoff_base_ms,
                    self.t.bootstrap_backoff_cap_ms,
                );
                self.lifecycle = Lifecycle::Booting {
                    attempt: attempt.saturating_add(1),
                    retry_at: Some(now.saturating_add(wait)),
                };
            }
            Event::BootstrapRefused(r) => {
                let reason = if r == Refusal::Gone {
                    ExitReason::SessionGone
                } else {
                    ExitReason::Refused
                };
                self.exit(now, reason, fx);
            }
            Event::BootstrapOk {
                heartbeat_interval_ms,
                deadline_at,
            } => {
                // Plan review focus 4: a deadline already past exits clean, and never starts pipelines.
                if deadline_at <= now {
                    return self.exit(now, ExitReason::Deadline, fx);
                }
                self.heartbeat_interval_ms = heartbeat_interval_ms;
                self.deadline_at = deadline_at;
                self.next_beat_at = now;
                self.lifecycle = Lifecycle::Running;
                fx.push(Effect::StartProgramme);
            }
            // Nothing is on air while booting, so a signal exits at once: nothing to hold or drain.
            Event::Signal(_) => self.exit(now, ExitReason::Signalled, fx),
            Event::Fatal => self.exit(now, ExitReason::Fatal, fx),
            _ => {}
        }
    }

    fn running(&mut self, event: Event, now: Ms, fx: &mut Vec<Effect>) -> Option<FrameChoice> {
        let t = self.t.clone();
        match event {
            Event::Tick => {
                if now >= self.deadline_at {
                    self.start_hold(now, ExitReason::Deadline);
                    return self.holding(Event::Tick, now, now, ExitReason::Deadline, fx);
                }
                self.input.check_stall(now, self.programme.last_frame_at(), &t);
                if self.input.on_tick(now, &t) {
                    fx.push(Effect::RebuildInput {
                        gen: self.input.generation(),
                    });
                }
                if self.output.on_tick(now, &t) {
                    fx.push(Effect::SwapSink {
                        gen: self.output.generation(),
                    });
                }
                match self.graphics.on_tick(now, &t) {
                    Some(GraphicsAction::Restart) => fx.push(Effect::RestartGraphics {
                        gen: self.graphics.generation(),
                    }),
                    Some(GraphicsAction::Hide { reason, gen }) => {
                        fx.push(Effect::HideOverlay { reason, gen })
                    }
                    None => {}
                }
                let choice = self.programme.on_tick(now, &t);
                self.beat_if_due(now, fx);
                return Some(choice);
            }
            Event::InputFrame => self.programme.on_frame(now, &t),
            Event::InputConnected { gen } => self.input.on_connected(now, gen),
            Event::InputLost { gen, reason } => self.input.on_lost(now, gen, reason, &t),
            Event::OutputProgress { gen, total_bytes } => self.on_output_progress(now, gen, total_bytes),
            Event::OutputError { gen } => self.output.on_error(now, gen, &t),
            Event::EncoderOverrun => {
                if let Some(r) = self.ladder.on_overrun(now, &t) {
                    fx.push(Effect::StepDown(r));
                }
            }
            Event::CefReady { gen } => self.graphics.on_ready(now, gen),
            Event::CefCrashed { gen } => self.on_cef_crashed(now, gen, fx),
            Event::CefAlive { gen } => self.graphics.on_alive(now, gen),
            Event::ScoreStale(b) => self.score_stale = Some(b),
            Event::PhaseChannel(c) => self.phase_channel = Some(c),
            Event::HeartbeatReply(Desired::Live) => self.heartbeat_failures = 0,
            Event::HeartbeatReply(Desired::Ending) => self.start_hold(now, ExitReason::Ended),
            Event::HeartbeatFailed => self.heartbeat_failures = self.heartbeat_failures.saturating_add(1),
            Event::Signal(SignalKind::Int) => self.start_hold(now, ExitReason::Signalled),
            Event::Signal(SignalKind::Term) => self.start_drain(now, ExitReason::Signalled, fx),
            Event::Fatal => self.exit(now, ExitReason::Fatal, fx),
            Event::OutputDrained
            | Event::BootstrapOk { .. }
            | Event::BootstrapFailed
            | Event::BootstrapRefused(_) => {}
        }
        None
    }

    fn holding(
        &mut self,
        event: Event,
        now: Ms,
        since: Ms,
        reason: ExitReason,
        fx: &mut Vec<Effect>,
    ) -> Option<FrameChoice> {
        let t = self.t.clone();
        match event {
            Event::Tick => {
                if elapsed(now, since) >= t.final_hold_ms {
                    self.start_drain(now, reason, fx);
                    return None;
                }
                // No link is ticked: nothing is rebuilt during the hold (no rebuild could finish inside
                // it). Graphics run the bound alone (coordinator decision on the C05b PR #22 review):
                // a page that hangs, or a crashed page's held frame, still comes off air at its bound,
                // and no restart, load timeout, recycle or cooldown's end runs, so the generation never
                // moves under a healthy page and its Final card stays on air.
                if let Some(GraphicsAction::Hide { reason, gen }) = self.graphics.on_bound_tick(now, &t) {
                    fx.push(Effect::HideOverlay { reason, gen });
                }
                let choice = self.programme.on_tick(now, &t); // Final ⇒ Slate(SlateKind::Final)
                self.beat_if_due(now, fx);
                return Some(choice);
            }
            Event::InputFrame => self.programme.on_frame(now, &t),
            Event::InputConnected { gen } => self.input.on_connected(now, gen),
            Event::InputLost { gen, reason: r } => self.input.on_lost(now, gen, r, &t),
            Event::OutputProgress { gen, total_bytes } => self.on_output_progress(now, gen, total_bytes),
            Event::OutputError { gen } => self.output.on_error(now, gen, &t),
            Event::CefReady { gen } => self.graphics.on_ready(now, gen),
            // (Amended 2026-10-18, C05b PR #22 review G2.) A crash in the hold is counted, so one at the
            // count hides at once, and one below it leaves the crashed page's frame on air to its
            // bound, which the bound tick above ends.
            Event::CefCrashed { gen } => self.on_cef_crashed(now, gen, fx),
            // A beat moves the bound of the page on air (Owed 18): without it a healthy page would
            // read as hung during the hold.
            Event::CefAlive { gen } => self.graphics.on_alive(now, gen),
            Event::ScoreStale(b) => self.score_stale = Some(b),
            Event::PhaseChannel(c) => self.phase_channel = Some(c),
            Event::HeartbeatReply(Desired::Live) => self.heartbeat_failures = 0,
            Event::HeartbeatFailed => self.heartbeat_failures = self.heartbeat_failures.saturating_add(1),
            Event::Signal(SignalKind::Term) => self.start_drain(now, ExitReason::Signalled, fx),
            Event::Fatal => self.exit(now, ExitReason::Fatal, fx),
            // A second end request (the reply after the SIGINT, or the reverse) keeps the FIRST hold's
            // clock. An overrun changes nothing: the hold is ≤ final_hold_ms.
            Event::HeartbeatReply(Desired::Ending)
            | Event::Signal(SignalKind::Int)
            | Event::EncoderOverrun
            | Event::OutputDrained
            | Event::BootstrapOk { .. }
            | Event::BootstrapFailed
            | Event::BootstrapRefused(_) => {}
        }
        None
    }

    fn draining(&mut self, event: Event, now: Ms, since: Ms, reason: ExitReason, fx: &mut Vec<Effect>) {
        match event {
            Event::OutputDrained => self.finish_drain(reason, fx),
            Event::Tick if elapsed(now, since) >= self.t.drain_timeout_ms => self.finish_drain(reason, fx),
            // The drain's beat named the drain's reason; a fatal error is a different one, so it beats.
            Event::Fatal => self.exit(now, ExitReason::Fatal, fx),
            _ => {}
        }
    }

    fn on_output_progress(&mut self, now: Ms, gen: u64, total_bytes: u64) {
        self.output.on_progress(now, gen, total_bytes);
        if self.output.is_connected() {
            self.output_ever_connected = true;
        }
    }

    fn on_cef_crashed(&mut self, now: Ms, gen: u64, fx: &mut Vec<Effect>) {
        // C05b PR #22 review I2: the hide carries its reason and generation.
        if let Some(GraphicsAction::Hide { reason, gen }) = self.graphics.on_crashed(now, gen, &self.t) {
            fx.push(Effect::HideOverlay { reason, gen });
        }
    }

    pub fn health(&self, now: Ms) -> HealthReport {
        let (state, exit) = match self.lifecycle {
            Lifecycle::Draining { reason, .. } | Lifecycle::Exited(reason) => {
                (BeatState::Stopped, Some(ExitInfo { reason, detail: None }))
            }
            Lifecycle::Booting { .. } => (BeatState::Starting, None),
            // Owner ruling 2026-10-14 (plan review F5, option A): `playing` = the programme is on air,
            // slate or picture, and the Final scene is on air in the hold. The camera's state travels
            // in programme_phase / input, never in `state`.
            Lifecycle::Running | Lifecycle::Holding { .. } if self.output.is_connected() => {
                (BeatState::Playing, None)
            }
            Lifecycle::Running | Lifecycle::Holding { .. } if self.output_ever_connected => {
                (BeatState::Stalled, None)
            }
            Lifecycle::Running | Lifecycle::Holding { .. } => (BeatState::Starting, None),
        };
        HealthReport {
            state,
            programme_phase: self.programme.phase(),
            input: match self.input.state() {
                InputState::Running { .. } => LinkStatus::Connected,
                InputState::Connecting { .. } => LinkStatus::Connecting,
                InputState::Backoff { .. } => LinkStatus::Backoff,
            },
            output: match self.output.state() {
                OutputState::Connected { .. } => LinkStatus::Connected,
                OutputState::Connecting { .. } => LinkStatus::Connecting,
                OutputState::Backoff { .. } => LinkStatus::Backoff,
            },
            graphics: match self.graphics.state() {
                GraphicsState::Ready { .. } => GraphicsStatus::Ready,
                GraphicsState::Starting { .. } => GraphicsStatus::Starting,
                GraphicsState::Backoff { .. } => GraphicsStatus::Restarting,
                GraphicsState::Failed { .. } => GraphicsStatus::Failed,
            },
            // At most KICK_HISTORY_CAP (32): the kick window is bounded, so the cast is exact.
            input_kicks: self.input.kicks(now, &self.t) as u32,
            input_kick_alert: self.input.kick_alert(now, &self.t),
            pipeline_rebuilds: self.input.generation().saturating_sub(1),
            sink_swaps: self.output.generation().saturating_sub(1),
            dropped_frames: self.ladder.overruns(),
            rung: self.ladder.rung(),
            score_stale: self.score_stale,
            heartbeat_failures: self.heartbeat_failures,
            phase_channel: self.phase_channel,
            exit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OutputProfile;
    use crate::programme::SlateKind;
    use ProgrammePhase::{Final, Frozen, Live, Reconnecting, Waiting};

    fn t() -> Thresholds {
        Thresholds::default()
    }
    const FAR: Ms = 10_000_000;

    /// C08 ticks on the frame period: 33 ms in both modes (ADR 0005, 30 fps).
    fn period() -> Ms {
        OutputProfile::P720.frame_period_ms().round() as Ms
    }

    fn booted_until(now: Ms, deadline_at: Ms) -> Supervisor {
        let s = Supervisor::new(t());
        let d = s.decide(Event::Tick, 0);
        assert_eq!(d.effects, vec![Effect::FetchSession]);
        let d = d.next.decide(
            Event::BootstrapOk {
                heartbeat_interval_ms: t().heartbeat_interval_ms,
                deadline_at,
            },
            now,
        );
        assert!(d.effects.contains(&Effect::StartProgramme));
        d.next
    }

    fn booted(now: Ms) -> Supervisor {
        booted_until(now, FAR)
    }

    fn exit_code(d: &Decision) -> Option<i32> {
        d.effects
            .iter()
            .find_map(|e| if let Effect::Exit(c) = e { Some(*c) } else { None })
    }

    fn beats(fx: &[Effect]) -> Vec<&HealthReport> {
        fx.iter()
            .filter_map(|e| {
                if let Effect::SendHeartbeat(h) = e {
                    Some(h)
                } else {
                    None
                }
            })
            .collect()
    }

    fn hides(fx: &[Effect]) -> Vec<(HideReason, u64)> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::HideOverlay { reason, gen } => Some((*reason, *gen)),
                _ => None,
            })
            .collect()
    }

    fn builds(fx: &[Effect]) -> bool {
        fx.iter().any(|e| {
            matches!(
                e,
                Effect::RebuildInput { .. } | Effect::SwapSink { .. } | Effect::RestartGraphics { .. }
            )
        })
    }

    /// The current sink's bytes-sent total (Owed 13: progress is a growing total).
    fn progress(s: &Supervisor, total_bytes: u64) -> Event {
        Event::OutputProgress {
            gen: s.output_generation(),
            total_bytes,
        }
    }

    /// One decided event: its instant, the event, and what came out (not `next`: long runs log many).
    #[derive(Debug)]
    struct Step {
        at: Ms,
        event: Event,
        effects: Vec<Effect>,
        frame: Option<FrameChoice>,
    }

    fn apply(s: &mut Supervisor, log: &mut Vec<Step>, event: Event, at: Ms) {
        let d = s.decide(event.clone(), at);
        *s = d.next;
        log.push(Step {
            at,
            event,
            effects: d.effects,
            frame: d.frame,
        });
    }

    /// From `booted(0)`: a tick every 1 000 ms before `fine_from`, then one every frame period, until
    /// `until` (not included). Each event of `events` is delivered at its instant, before any later
    /// tick and after a tick due at the same instant. The first tick (at 0) builds every generation 1.
    fn drive(mut events: Vec<(Ms, Event)>, until: Ms, fine_from: Ms) -> (Supervisor, Vec<Step>) {
        events.sort_by_key(|&(at, _)| at);
        let (mut s, mut log) = (booted(0), Vec::new());
        let (mut tick_at, mut i) = (0, 0);
        while tick_at < until {
            while i < events.len() && events[i].0 < tick_at {
                apply(&mut s, &mut log, events[i].1.clone(), events[i].0);
                i += 1;
            }
            apply(&mut s, &mut log, Event::Tick, tick_at);
            tick_at = if tick_at < fine_from {
                (tick_at + 1_000).min(fine_from)
            } else {
                tick_at + period()
            };
        }
        while i < events.len() && events[i].0 < until {
            apply(&mut s, &mut log, events[i].1.clone(), events[i].0);
            i += 1;
        }
        (s, log)
    }

    /// `booted(0)`, ticked on the frame period from 0 with camera frames, into `phase`, exactly as
    /// `tests/programme.rs`'s `final_is_terminal_against_every_feed` reaches it (Live after
    /// `resume_after_ms` + 100 of frames; Frozen after `frozen_after_ms` + 100 with no frame;
    /// Reconnecting after `slate_after_ms` + 100). Returns the supervisor and its last tick.
    fn reach(phase: ProgrammePhase) -> (Supervisor, Ms) {
        let (th, step) = (t(), period());
        let mut s = booted(0);
        let mut now = 0;
        if phase == Waiting {
            return (s.decide(Event::Tick, 0).next, 0);
        }
        while now <= th.resume_after_ms + 100 {
            s = s.decide(Event::InputFrame, now).next;
            s = s.decide(Event::Tick, now).next;
            now += step;
        }
        let last = now - step;
        let silence = match phase {
            Live => 0,
            Frozen => th.frozen_after_ms + 100,
            _ => th.slate_after_ms + 100,
        };
        while now <= last + silence {
            s = s.decide(Event::Tick, now).next;
            now += step;
        }
        assert_eq!(s.programme_phase(), phase, "setup: reach {phase:?}");
        (s, now - step)
    }

    // ---- the brief's scenarios (Step 3), with Owed 13's OutputProgress ----

    #[test]
    fn boot_retries_the_session_fetch_on_backoff() {
        let s = Supervisor::new(t());
        let d = s.decide(Event::Tick, 0);
        assert_eq!(d.effects, vec![Effect::FetchSession]);
        let d2 = d.next.decide(Event::Tick, 10);
        assert!(d2.effects.is_empty(), "no second fetch while one is in flight");
        let d3 = d2.next.decide(Event::BootstrapFailed, 100);
        let wait = t().bootstrap_backoff_base_ms;
        assert!(d3.next.decide(Event::Tick, 100 + wait - 1).effects.is_empty());
        assert_eq!(
            d3.next.decide(Event::Tick, 100 + wait).effects,
            vec![Effect::FetchSession]
        );
    }

    // A second call (AGENTS.md §5): one fetch has one outcome, so a failure reported while no fetch
    // is in flight changes nothing. The next real failure doubles the wait (the bootstrap back-off,
    // written here from its definition: base · 2^failures before it, capped).
    #[test]
    fn a_second_failure_report_for_one_fetch_changes_nothing() {
        let th = t();
        let s = Supervisor::new(th.clone()).decide(Event::Tick, 0).next;
        let s = s.decide(Event::BootstrapFailed, 100).next;
        let d = s.decide(Event::BootstrapFailed, 200);
        assert!(d.effects.is_empty() && d.frame.is_none());
        assert_eq!(d.next, s, "a second report moved the retry");
        let retry = 100 + th.bootstrap_backoff_base_ms;
        let d = s.decide(Event::Tick, retry);
        assert_eq!(d.effects, vec![Effect::FetchSession]);
        let s = d.next.decide(Event::BootstrapFailed, retry).next;
        let second = (th.bootstrap_backoff_base_ms * 2).min(th.bootstrap_backoff_cap_ms);
        assert!(s.decide(Event::Tick, retry + second - 1).effects.is_empty());
        assert_eq!(
            s.decide(Event::Tick, retry + second).effects,
            vec![Effect::FetchSession]
        );
    }

    #[test]
    fn refusals_exit_with_the_contract_codes() {
        let mut checked = 0;
        for (r, code, reason) in [
            (Refusal::Gone, 0, ExitReason::SessionGone),
            (Refusal::Unauthorized, 78, ExitReason::Refused),
            (Refusal::NotFound, 78, ExitReason::Refused),
            (Refusal::WrongMode, 78, ExitReason::Refused),
        ] {
            let d = Supervisor::new(t()).decide(Event::Tick, 0);
            let d = d.next.decide(Event::BootstrapRefused(r), 5);
            assert_eq!(exit_code(&d), Some(code), "{r:?}");
            assert_eq!(d.next.lifecycle(), Lifecycle::Exited(reason), "{r:?}");
            let b = beats(&d.effects);
            assert_eq!(b.len(), 1, "{r:?}: one final beat");
            assert_eq!(b[0].state, BeatState::Stopped);
            assert_eq!(b[0].exit.as_ref().map(|e| e.reason), Some(reason), "{r:?}");
            checked += 1;
        }
        assert_eq!(checked, 4, "refusals checked: {checked}");
    }

    #[test]
    fn deadline_in_past_at_bootstrap_exits_clean() {
        let d = Supervisor::new(t()).decide(Event::Tick, 0);
        let d = d.next.decide(
            Event::BootstrapOk {
                heartbeat_interval_ms: 5_000,
                deadline_at: 1_000,
            },
            2_000,
        );
        assert_eq!(exit_code(&d), Some(0));
        assert!(
            !d.effects.contains(&Effect::StartProgramme),
            "never start pipelines past the deadline"
        );
    }

    // Plan review focus 4, at its boundary: a deadline at the bootstrap's own instant has passed, and
    // one 1 ms later has not.
    #[test]
    fn a_deadline_at_the_bootstrap_instant_has_passed_and_one_later_has_not() {
        let at = |deadline_at| {
            let d = Supervisor::new(t()).decide(Event::Tick, 0);
            d.next.decide(
                Event::BootstrapOk {
                    heartbeat_interval_ms: 5_000,
                    deadline_at,
                },
                2_000,
            )
        };
        let d = at(2_000);
        assert_eq!(d.next.lifecycle(), Lifecycle::Exited(ExitReason::Deadline));
        assert_eq!(exit_code(&d), Some(0));
        let d = at(2_001);
        assert_eq!(d.next.lifecycle(), Lifecycle::Running);
        assert_eq!(d.effects, vec![Effect::StartProgramme]);
    }

    #[test]
    fn first_running_tick_builds_input_sink_and_graphics_and_beats() {
        let s = booted(1_000);
        let d = s.decide(Event::Tick, 1_000);
        assert!(d.effects.contains(&Effect::RebuildInput { gen: 1 }));
        assert!(d.effects.contains(&Effect::SwapSink { gen: 1 }));
        assert!(d.effects.contains(&Effect::RestartGraphics { gen: 1 }));
        assert!(d
            .effects
            .iter()
            .any(|e| matches!(e, Effect::SendHeartbeat(h) if h.state == BeatState::Starting)));
        assert_eq!(d.frame, Some(FrameChoice::Slate(SlateKind::Waiting)));
    }

    #[test]
    fn heartbeat_every_interval_not_every_tick() {
        let mut s = booted(0);
        let mut beats_at = Vec::new();
        // 33-ms ticks land each beat 16 ms late: 0, 5016, 10032, 15048, 20064
        for now in (0..=20_100).step_by(33) {
            let d = s.decide(Event::Tick, now);
            if d.effects.iter().any(|e| matches!(e, Effect::SendHeartbeat(_))) {
                beats_at.push(now);
            }
            s = d.next;
        }
        assert_eq!(beats_at.len(), 5, "beats at {beats_at:?}");
        for w in beats_at.windows(2) {
            assert!(w[1] - w[0] >= 5_000 && w[1] - w[0] < 5_033 + 33);
        }
    }

    #[test]
    fn beat_state_follows_output_and_programme() {
        let th = t();
        let mut s = booted(0).decide(Event::Tick, 0).next;
        assert_eq!(s.health(0).state, BeatState::Starting);
        let mut sent = 1_000;
        s = s.decide(progress(&s, sent), 10).next;
        // F5 option A: the slate is on air, so the session is playing; the camera shows as Waiting
        assert_eq!(
            s.health(10).state,
            BeatState::Playing,
            "slate on air counts as playing"
        );
        assert_eq!(s.health(10).programme_phase, ProgrammePhase::Waiting);
        let mut now = 10;
        while now < 10 + th.resume_after_ms + 100 {
            sent += 1_000;
            s = s.decide(Event::InputFrame, now).next;
            s = s.decide(progress(&s, sent), now).next;
            s = s.decide(Event::Tick, now).next;
            now += 33;
        }
        assert_eq!(s.health(now).state, BeatState::Playing);
        assert_eq!(s.health(now).programme_phase, ProgrammePhase::Live);
        // camera drops while the output stays up: still playing, phase says so
        let mut later = now;
        while later < now + th.slate_after_ms + 100 {
            sent += 1_000;
            s = s.decide(progress(&s, sent), later).next;
            s = s.decide(Event::Tick, later).next;
            later += 33;
        }
        assert_eq!(
            s.health(later).state,
            BeatState::Playing,
            "camera loss is not a stalled broadcast"
        );
        assert_eq!(s.health(later).programme_phase, ProgrammePhase::Reconnecting);
        let now = later;
        s = s.decide(Event::OutputError { gen: 1 }, now).next;
        assert_eq!(
            s.health(now).state,
            BeatState::Stalled,
            "output dropped after having connected"
        );
    }

    #[test]
    fn drain_that_never_completes_exits_at_the_timeout() {
        let th = t();
        let s = booted(0).decide(Event::Tick, 0).next;
        let d = s.decide(Event::Signal(SignalKind::Term), 100);
        assert!(d.effects.contains(&Effect::DrainOutput));
        let b = beats(&d.effects);
        assert_eq!(b.len(), 1, "the final beat goes out when the drain starts");
        assert_eq!(b[0].exit.as_ref().map(|e| e.reason), Some(ExitReason::Signalled));
        let s = d.next;
        assert_eq!(
            exit_code(&s.decide(Event::Tick, 100 + th.drain_timeout_ms - 1)),
            None
        );
        let d = s.decide(Event::Tick, 100 + th.drain_timeout_ms);
        assert_eq!(d.effects, vec![Effect::Exit(0)], "no second final beat");
    }

    #[test]
    fn ending_mid_reconnect_builds_nothing_more() {
        let s = booted(0).decide(Event::Tick, 0).next;
        let s = s
            .decide(
                Event::InputLost {
                    gen: 1,
                    reason: LossReason::Error,
                },
                50,
            )
            .next; // input now backing off
        let mut s = s.decide(Event::HeartbeatReply(Desired::Ending), 60).next;
        let (mut ticks, mut exited) = (0, false);
        for now in (60..20_000).step_by(33) {
            let d = s.decide(Event::Tick, now);
            assert!(
                !d.effects
                    .iter()
                    .any(|e| matches!(e, Effect::RebuildInput { .. } | Effect::SwapSink { .. })),
                "a build at {now}"
            );
            exited |= exit_code(&d).is_some();
            ticks += 1;
            s = d.next;
        }
        assert!(
            exited,
            "the hold, the drain and its timeout all fit the run ({ticks} ticks)"
        );
    }

    #[test]
    fn connected_input_with_no_frames_is_rebuilt_after_the_stall_window() {
        // CF holds the playback socket open after the phone drops: slate first, then a fresh input
        let th = t();
        let s = booted(0).decide(Event::Tick, 0).next; // builds input gen 1
        let mut s = s.decide(Event::InputConnected { gen: 1 }, 100).next;
        let mut rebuilt_at = None;
        let mut now = 100;
        while now < 100 + th.input_stall_after_ms + th.input_backoff_base_ms + 100 {
            let d = s.decide(Event::Tick, now);
            if d.effects.contains(&Effect::RebuildInput { gen: 2 }) {
                rebuilt_at = Some(now);
            }
            s = d.next;
            now += 33;
        }
        let at = rebuilt_at.expect("a connected-but-silent input was never rebuilt");
        assert!(
            at - 100 >= th.input_stall_after_ms,
            "rebuilt too early at +{}",
            at - 100
        );
    }

    #[test]
    fn signal_while_booting_exits_immediately() {
        for k in [SignalKind::Term, SignalKind::Int] {
            let d = Supervisor::new(t()).decide(Event::Signal(k), 0);
            assert_eq!(exit_code(&d), Some(0), "{k:?}");
            assert_eq!(d.next.lifecycle(), Lifecycle::Exited(ExitReason::Signalled));
            assert_eq!(
                beats(&d.effects)[0].exit.as_ref().map(|e| e.reason),
                Some(ExitReason::Signalled),
                "{k:?}: the final beat first"
            );
        }
    }

    #[test]
    fn crash_after_ending_exits_70_and_the_restart_then_sees_410() {
        let s = booted(0).decide(Event::Tick, 0).next;
        let s = s.decide(Event::HeartbeatReply(Desired::Ending), 10).next;
        assert_eq!(exit_code(&s.decide(Event::Fatal, 20)), Some(70));
        // the host-restarted process: a fresh supervisor whose bootstrap is refused as gone
        let r = Supervisor::new(t())
            .decide(Event::Tick, 0)
            .next
            .decide(Event::BootstrapRefused(Refusal::Gone), 5);
        assert_eq!(exit_code(&r), Some(0));
    }

    #[test]
    fn exited_ignores_every_event() {
        let s = Supervisor::new(t())
            .decide(Event::Signal(SignalKind::Term), 0)
            .next;
        for e in [
            Event::Tick,
            Event::Fatal,
            Event::InputFrame,
            Event::HeartbeatReply(Desired::Ending),
            Event::Signal(SignalKind::Int),
        ] {
            let d = s.decide(e.clone(), 10);
            assert!(
                d.effects.is_empty() && d.frame.is_none(),
                "{e:?} produced {:?}",
                d.effects
            );
            assert_eq!(d.next, s, "{e:?} changed an exited supervisor");
        }
    }

    #[test]
    fn running_ticks_always_yield_a_frame() {
        let mut s = booted(0);
        let mut n = 0;
        for now in (0..10_000).step_by(33) {
            let d = s.decide(Event::Tick, now);
            assert!(d.frame.is_some());
            n += 1;
            s = d.next;
        }
        assert!(n > 0);
    }

    // ---- the final hold (amendment 2026-10-15, overlay templates spec) ----

    #[test]
    fn final_hold_is_the_spec_number() {
        assert_eq!(t().final_hold_ms, 10_000, "overlay spec §4.2 FINAL_HOLD_MS");
    }

    #[test]
    fn ending_reply_holds_the_final_card_then_drains() {
        let th = t();
        let mut s = booted(0).decide(Event::Tick, 0).next;
        s = s.decide(progress(&s, 1_000), 10).next;
        assert_eq!(
            s.health(10).output,
            LinkStatus::Connected,
            "setup: output gen 1 connected"
        );
        // the input backs off, so its rebuild falls due inside the hold
        s = s
            .decide(
                Event::InputLost {
                    gen: 1,
                    reason: LossReason::Error,
                },
                20,
            )
            .next;
        let d = s.decide(Event::HeartbeatReply(Desired::Ending), 100);
        assert!(!d.effects.contains(&Effect::DrainOutput));
        assert_eq!(d.next.programme_phase(), Final);
        let mut s = d.next;
        let end = 100 + th.final_hold_ms;
        let (mut now, mut ticks) = (100, 0);
        while now < end {
            let d = s.decide(Event::Tick, now);
            assert_eq!(d.frame, Some(FrameChoice::Slate(SlateKind::Final)), "at {now}");
            assert!(!d.effects.contains(&Effect::DrainOutput), "drained at {now}");
            assert!(
                !builds(&d.effects),
                "a build in the hold at {now}: {:?}",
                d.effects
            );
            ticks += 1;
            s = d.next;
            now += period();
        }
        assert!(ticks > 0, "hold ticks checked: {ticks}");
        let d = s.decide(Event::Tick, end);
        assert!(
            d.effects.contains(&Effect::DrainOutput),
            "after {ticks} hold ticks"
        );
        let b = beats(&d.effects);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].state, BeatState::Stopped);
        assert_eq!(b[0].exit.as_ref().map(|e| e.reason), Some(ExitReason::Ended));
        assert_eq!(d.frame, None, "no frames pushed after EOS");
        assert_eq!(
            exit_code(&d.next.decide(Event::OutputDrained, end + 500)),
            Some(0)
        );
    }

    #[test]
    fn final_wins_over_every_camera_state() {
        let th = t();
        let step = period();
        let (mut starts, mut ticks) = (0, 0);
        let frames_for = th.resume_after_ms + 200;
        let gap = th.frozen_after_ms + 100;
        let long_gap = th.slate_after_ms + 100;
        assert!(
            frames_for + gap + long_gap < th.final_hold_ms,
            "all inside the hold"
        );
        for start in [Waiting, Live, Frozen, Reconnecting] {
            let (s, at) = reach(start);
            let mut s = s.decide(Event::HeartbeatReply(Desired::Ending), at).next;
            let mut now = at;
            let mut check = |s: &Supervisor, now: Ms| {
                let d = s.decide(Event::Tick, now);
                assert_eq!(d.next.programme_phase(), Final, "{start:?} at {now}");
                assert_eq!(
                    d.frame,
                    Some(FrameChoice::Slate(SlateKind::Final)),
                    "{start:?} at {now}"
                );
                ticks += 1;
                d.next
            };
            while now < at + frames_for {
                now += step;
                s = s.decide(Event::InputFrame, now).next;
                s = check(&s, now);
            }
            let last = now;
            while now < last + gap + long_gap {
                now += step;
                s = check(&s, now);
            }
            starts += 1;
        }
        assert_eq!(starts, 4, "starts checked: {starts} ({ticks} ticks)");
        assert!(ticks > 0);
    }

    #[test]
    fn a_second_end_request_during_the_hold_changes_nothing() {
        let th = t();
        let s = booted(0).decide(Event::Tick, 0).next;
        let s = s.decide(Event::HeartbeatReply(Desired::Ending), 100).next;
        let held = s.lifecycle();
        assert_eq!(
            held,
            Lifecycle::Holding {
                since: 100,
                reason: ExitReason::Ended
            }
        );
        for (e, at) in [
            (Event::HeartbeatReply(Desired::Ending), 2_000),
            (Event::Signal(SignalKind::Int), 3_000),
        ] {
            let d = s.decide(e.clone(), at);
            assert!(
                d.effects.is_empty() && d.frame.is_none(),
                "{e:?}: {:?}",
                d.effects
            );
            assert_eq!(d.next.lifecycle(), held, "{e:?}");
            assert_eq!(d.next, s, "{e:?} changed the hold");
        }
        let s = s.decide(Event::Signal(SignalKind::Int), 3_000).next;
        let d = s.decide(Event::Tick, 100 + th.final_hold_ms - 1);
        assert!(
            !d.effects.contains(&Effect::DrainOutput),
            "the hold kept its first clock"
        );
        let d = d.next.decide(Event::Tick, 100 + th.final_hold_ms);
        assert!(d.effects.contains(&Effect::DrainOutput));
        assert_eq!(
            d.next.lifecycle(),
            Lifecycle::Draining {
                since: 100 + th.final_hold_ms,
                reason: ExitReason::Ended
            }
        );
    }

    #[test]
    fn int_signal_holds_and_term_signal_drains_at_once() {
        let s = booted(0).decide(Event::Tick, 0).next;
        let d = s.decide(Event::Signal(SignalKind::Int), 100);
        assert!(!d.effects.contains(&Effect::DrainOutput));
        assert_eq!(
            d.next.lifecycle(),
            Lifecycle::Holding {
                since: 100,
                reason: ExitReason::Signalled
            }
        );
        assert_eq!(d.next.programme_phase(), Final);
        let held = d.next;
        let d = s.decide(Event::Signal(SignalKind::Term), 100);
        assert!(d.effects.contains(&Effect::DrainOutput));
        assert_eq!(
            d.next.lifecycle(),
            Lifecycle::Draining {
                since: 100,
                reason: ExitReason::Signalled
            }
        );
        assert_ne!(
            d.next.programme_phase(),
            Final,
            "a host stop never shows the Final scene"
        );
        let d = held.decide(Event::Signal(SignalKind::Term), 200);
        assert!(
            d.effects.contains(&Effect::DrainOutput),
            "SIGTERM during the hold drains"
        );
        assert_eq!(
            d.next.lifecycle(),
            Lifecycle::Draining {
                since: 200,
                reason: ExitReason::Signalled
            }
        );
    }

    #[test]
    fn deadline_holds_then_drains_with_reason_deadline() {
        let th = t();
        let deadline = 60_000;
        let s = booted_until(0, deadline).decide(Event::Tick, 0).next;
        let d = s.decide(Event::Tick, deadline - 1);
        assert_eq!(d.next.lifecycle(), Lifecycle::Running, "1 ms before the deadline");
        let d = d.next.decide(Event::Tick, deadline);
        assert_eq!(d.frame, Some(FrameChoice::Slate(SlateKind::Final)));
        assert!(!d.effects.contains(&Effect::DrainOutput));
        assert_eq!(
            d.next.lifecycle(),
            Lifecycle::Holding {
                since: deadline,
                reason: ExitReason::Deadline
            }
        );
        let d = d.next.decide(Event::Tick, deadline + th.final_hold_ms);
        assert!(d.effects.contains(&Effect::DrainOutput));
        assert_eq!(
            d.next.lifecycle(),
            Lifecycle::Draining {
                since: deadline + th.final_hold_ms,
                reason: ExitReason::Deadline
            }
        );
    }

    #[test]
    fn fatal_during_the_hold_exits_70() {
        let s = booted(0).decide(Event::Tick, 0).next;
        let s = s.decide(Event::HeartbeatReply(Desired::Ending), 100).next;
        let d = s.decide(Event::Fatal, 200);
        assert_eq!(exit_code(&d), Some(70));
        assert_eq!(d.next.lifecycle(), Lifecycle::Exited(ExitReason::Fatal));
        let at = |want: fn(&Effect) -> bool| d.effects.iter().position(want).expect("effect");
        assert!(
            at(
                |e| matches!(e, Effect::SendHeartbeat(h) if h.exit.as_ref().map(|x| x.reason) == Some(ExitReason::Fatal))
            ) < at(|e| matches!(e, Effect::Exit(_))),
            "the final beat comes before Exit: {:?}",
            d.effects
        );
    }

    #[test]
    fn health_during_the_hold_reports_final_and_stays_on_air() {
        let th = t();
        let mut s = booted(0).decide(Event::Tick, 0).next;
        let mut sent = 1_000;
        s = s.decide(progress(&s, sent), 10).next;
        s = s.decide(Event::HeartbeatReply(Desired::Ending), 100).next;
        let (mut now, mut hold_beats, mut ticks) = (100, Vec::new(), 0);
        while now < 100 + th.final_hold_ms {
            sent += 1_000;
            s = s.decide(progress(&s, sent), now).next;
            let d = s.decide(Event::Tick, now);
            let h = d.next.health(now);
            assert_eq!(h.state, BeatState::Playing, "at {now}");
            assert_eq!(h.programme_phase, Final, "at {now}");
            hold_beats.extend(beats(&d.effects).into_iter().cloned());
            ticks += 1;
            s = d.next;
            now += period();
        }
        assert!(!hold_beats.is_empty(), "no beat in {ticks} hold ticks");
        for b in &hold_beats {
            assert_eq!(b.programme_phase, Final);
            assert_eq!(b.state, BeatState::Playing);
            assert_eq!(b.exit, None, "no exit reason before the drain");
        }
    }

    #[test]
    fn phase_channel_reaches_the_report_and_is_absent_until_told() {
        let s = booted(0).decide(Event::Tick, 0).next;
        assert_eq!(s.health(0).phase_channel, None, "absent, never defaulted");
        let s = s.decide(Event::PhaseChannel(PhaseChannel::Web), 10).next;
        assert_eq!(s.health(10).phase_channel, Some(PhaseChannel::Web));
        let s = s.decide(Event::HeartbeatReply(Desired::Ending), 20).next;
        let s = s.decide(Event::PhaseChannel(PhaseChannel::Loopback), 30).next;
        assert_eq!(
            s.health(30).phase_channel,
            Some(PhaseChannel::Loopback),
            "the page's channel during the hold"
        );
    }

    // Confirm pass N1: the first hold tick beats `final` at once, for each end request. The expected
    // beat instant comes from the thresholds and the tick period, never from `decide`.
    #[test]
    fn the_first_hold_tick_beats_final_at_once() {
        let th = t();
        let step = period();
        // booted(0) beats on its first tick, at 0; the next is due `heartbeat_interval_ms` later, and
        // goes out on the first tick at or after it.
        let b = th.heartbeat_interval_ms.div_ceil(step) * step;
        let request = b + 100;
        let first_hold_tick = request.div_ceil(step) * step;
        assert!(
            first_hold_tick < b + th.heartbeat_interval_ms,
            "the scheduled beat would come anyway"
        );
        let mut checked = 0;
        for path in ["ending", "sigint", "deadline"] {
            let deadline = if path == "deadline" { request } else { FAR };
            let mut s = booted_until(0, deadline);
            let (mut now, mut sent, mut beat_at) = (0, 0, Vec::new());
            while now < request {
                sent += 1_000;
                if now > 0 {
                    s = s.decide(progress(&s, sent), now).next;
                }
                let d = s.decide(Event::Tick, now);
                if !beats(&d.effects).is_empty() {
                    beat_at.push(now);
                }
                s = d.next;
                now += step;
            }
            assert_eq!(beat_at, vec![0, b], "{path}: setup, the scheduled beats");
            assert_eq!(
                now, first_hold_tick,
                "{path}: setup, the first tick after the request"
            );
            match path {
                "ending" => s = s.decide(Event::HeartbeatReply(Desired::Ending), request).next,
                "sigint" => s = s.decide(Event::Signal(SignalKind::Int), request).next,
                _ => {}
            }
            let d = s.decide(Event::Tick, first_hold_tick);
            let bt = beats(&d.effects);
            assert_eq!(
                bt.len(),
                1,
                "{path}: no beat on the first hold tick {first_hold_tick}"
            );
            assert_eq!(bt[0].programme_phase, Final, "{path}");
            checked += 1;
        }
        assert_eq!(checked, 3, "end paths checked: {checked}");
    }

    // ---- the final hold's graphics (C05b PR #22 review G2; coordinator decision: the bound alone) ----

    /// The first tick at or after `at` in `log`, as its instant.
    fn first_tick_at_or_after(log: &[Step], at: Ms) -> Ms {
        log.iter()
            .find(|st| st.event == Event::Tick && st.at >= at)
            .map(|st| st.at)
            .expect("no tick at or after the instant")
    }

    /// The instant of the hold's `DrainOutput`, and every hide and graphics restart from the Ending
    /// reply at `f` to it. Every hold tick before the drain must put the Final card on air.
    fn hold_graphics(log: &[Step], f: Ms) -> (Ms, Vec<(Ms, HideReason, u64)>, usize) {
        let ending = Event::HeartbeatReply(Desired::Ending);
        let from = log
            .iter()
            .position(|st| st.at == f && st.event == ending)
            .expect("no Ending reply at the hold's start");
        let (mut hides_seen, mut restarts, mut ticks) = (Vec::new(), 0, 0);
        for st in &log[from..] {
            if st.event == Event::Tick && !st.effects.contains(&Effect::DrainOutput) {
                assert_eq!(
                    st.frame,
                    Some(FrameChoice::Slate(SlateKind::Final)),
                    "at {}",
                    st.at
                );
                ticks += 1;
            }
            for (reason, gen) in hides(&st.effects) {
                hides_seen.push((st.at, reason, gen));
            }
            restarts += st
                .effects
                .iter()
                .filter(|e| matches!(e, Effect::RestartGraphics { .. }))
                .count();
            if st.effects.contains(&Effect::DrainOutput) {
                assert!(ticks > 0, "hold ticks checked: {ticks}");
                return (st.at, hides_seen, restarts);
            }
        }
        panic!("the hold from {f} never drained");
    }

    /// Beats of generation 1 every `cef_alive_every_ms` from `first` up to `last`, both included.
    fn beats_of_1(first: Ms, last: Ms) -> Vec<(Ms, Event)> {
        let every = t().cef_alive_every_ms as usize;
        (first..=last)
            .step_by(every)
            .map(|at| (at, Event::CefAlive { gen: 1 }))
            .collect()
    }

    #[test]
    fn the_final_hold_still_hides_a_hang_or_a_crash() {
        let th = t();
        let (alive, hang, hold) = (th.cef_alive_every_ms, th.cef_hang_after_ms, th.final_hold_ms);
        let mut legs = 0;

        // A hang: the last beat 3 s before the Ending; the bound falls inside the hold.
        let last = 10 * alive;
        let f = last + 3_000;
        let mut ev = beats_of_1(alive, last);
        ev.push((0, Event::CefReady { gen: 1 }));
        ev.push((f, Event::HeartbeatReply(Desired::Ending)));
        let bound = last + hang;
        assert!(
            bound > f && bound < f + hold,
            "setup: the bound is inside the hold"
        );
        let (s, _) = drive(ev.clone(), bound - 1, 0);
        let early = s.decide(Event::Tick, bound - 1);
        assert!(
            hides(&early.effects).is_empty(),
            "hang: hidden 1 ms before the bound"
        );
        assert_eq!(
            hides(&s.decide(Event::Tick, bound).effects),
            vec![(HideReason::Hang, 1)]
        );
        let (_, log) = drive(ev, f + hold + 1_000, 0);
        let (drained, hidden, restarts) = hold_graphics(&log, f);
        assert_eq!(drained, first_tick_at_or_after(&log, f + hold), "hang: the drain");
        let at = first_tick_at_or_after(&log, bound);
        assert_eq!(hidden, vec![(at, HideReason::Hang, 1)], "hang");
        assert_eq!(restarts, 0, "hang: a restart in the hold");
        legs += 1;

        // A crash's held frame: the crash 5 s before the Ending, after the page's last beat; the reload
        // (generation 2) starts in Running and never signals ready.
        let last = 10 * alive;
        let crash = last + 500;
        let f = crash + 5_000;
        let mut ev = beats_of_1(alive, last);
        ev.push((0, Event::CefReady { gen: 1 }));
        ev.push((crash, Event::CefCrashed { gen: 1 }));
        ev.push((f, Event::HeartbeatReply(Desired::Ending)));
        let bound = last + hang;
        assert!(
            bound > f && bound < f + hold,
            "setup: the bound is inside the hold"
        );
        let (_, log) = drive(ev, f + hold + 1_000, 0);
        let reload = log
            .iter()
            .find(|st| st.effects.contains(&Effect::RestartGraphics { gen: 2 }))
            .map(|st| st.at)
            .expect("crash: the reload");
        assert!(reload < f, "setup: the reload starts in Running, at {reload}");
        let (_, hidden, restarts) = hold_graphics(&log, f);
        let at = first_tick_at_or_after(&log, bound);
        assert_eq!(hidden, vec![(at, HideReason::Crash, 1)], "crash");
        assert_eq!(restarts, 0, "crash: a restart in the hold");
        legs += 1;

        // A recycle due in the hold, the page silent since 1 s before the Ending: the hold runs no
        // recycle, so the page's own frame comes off at its bound, as a hang.
        let recycle_due = th.cef_recycle_every_ms;
        let f = recycle_due - 1_000;
        let last = f - 1_000;
        assert_eq!(last % alive, 0, "setup: the last beat is on the page's beat grid");
        let mut ev = beats_of_1(alive, last);
        ev.push((0, Event::CefReady { gen: 1 }));
        ev.push((f, Event::HeartbeatReply(Desired::Ending)));
        let bound = last + hang;
        assert!(recycle_due > f && bound < f + hold, "setup: both inside the hold");
        let (s, log) = drive(ev, f + hold + 1_000, f - 5_000);
        let (_, hidden, restarts) = hold_graphics(&log, f);
        let at = first_tick_at_or_after(&log, bound);
        assert_eq!(
            hidden,
            vec![(at, HideReason::Hang, 1)],
            "recycle due, page silent"
        );
        assert_eq!(restarts, 0, "recycle due: a restart in the hold");
        assert!(
            s.graphics_generation() <= 1,
            "recycle due: the generation moved in the hold"
        );
        legs += 1;

        // A crash at the count: two crashes in Running, each reload ready, then a crash of the
        // current generation 100 ms into the hold hides as Failed at once.
        let base = th.cef_backoff_base_ms;
        let (c1, c2) = (1_000, 1_000 + 2 * base + 1_000);
        // Each ready lands 100 ms after its back-off ends, past the restart's tick (the period is 33).
        let mut ev = vec![
            (0, Event::CefReady { gen: 1 }),
            (c1, Event::CefCrashed { gen: 1 }),
            (c1 + base + 100, Event::CefReady { gen: 2 }),
            (c2, Event::CefCrashed { gen: 2 }),
            (c2 + 2 * base + 100, Event::CefReady { gen: 3 }),
        ];
        let f = c2 + 2 * base + 1_000;
        let (s_f, _) = drive(ev.clone(), f, 0);
        assert_eq!(
            s_f.graphics_generation(),
            3,
            "setup: generation 3 built in Running"
        );
        assert_eq!(
            s_f.health(f).graphics,
            GraphicsStatus::Ready,
            "setup: generation 3 ready"
        );
        ev.push((f, Event::HeartbeatReply(Desired::Ending)));
        ev.push((f + 100, Event::CefCrashed { gen: 3 }));
        assert!(
            f + 100 - c1 <= th.cef_fail_window_ms,
            "setup: all three in the window"
        );
        let (s, log) = drive(ev, f + hold + 1_000, 0);
        let crash3 = log
            .iter()
            .find(|st| st.event == Event::CefCrashed { gen: 3 })
            .expect("the third crash");
        assert_eq!(
            hides(&crash3.effects),
            vec![(HideReason::Failed, 3)],
            "at the count"
        );
        let (_, hidden, restarts) = hold_graphics(&log, f);
        assert_eq!(hidden, vec![(f + 100, HideReason::Failed, 3)], "count: one hide");
        assert_eq!(restarts, 0, "count: a restart in the hold");
        assert_eq!(s.health(f + hold).graphics, GraphicsStatus::Failed);
        legs += 1;

        assert_eq!(legs, 4, "hold legs checked: {legs}");
    }

    // The coordinator's decision on the C05b PR #22 review, carried into C06: the hold runs the bound
    // alone. A recycle due in the hold of a healthy, beating page must not run: if it did, the
    // generation would move, the hold would drop the `Restart`, the page's beats would read as a stale
    // generation, and the held frame's bound would take the Final card off air before the hold ends.
    #[test]
    fn a_recycle_due_in_the_hold_keeps_the_final_card_shown() {
        let th = t();
        let (alive, hang, hold) = (th.cef_alive_every_ms, th.cef_hang_after_ms, th.final_hold_ms);
        let recycle_due = th.cef_recycle_every_ms;
        let f = recycle_due - 1_000;
        // A beat every `cef_alive_every_ms`, a quarter beat off the ready's grid, through the hold.
        let ev_beats = beats_of_1(alive / 4, f + hold + 1_000);
        let before_recycle = ev_beats
            .iter()
            .map(|&(at, _)| at)
            .filter(|&at| at < recycle_due)
            .max()
            .expect("a beat before the recycle");
        assert!(
            before_recycle + hang < f + hold,
            "setup: a recycle run in the hold would hide inside it, at {}",
            before_recycle + hang
        );
        let mut ev = ev_beats;
        ev.push((0, Event::CefReady { gen: 1 }));
        ev.push((f, Event::HeartbeatReply(Desired::Ending)));
        let (s, log) = drive(ev, f + hold + 1_000, f - 5_000);
        let (drained, hidden, restarts) = hold_graphics(&log, f);
        assert_eq!(hidden, vec![], "the Final card came off air");
        assert_eq!(restarts, 0, "a restart in the hold");
        let in_hold = log
            .iter()
            .filter(|st| st.at >= f && st.at < drained && st.event == Event::CefAlive { gen: 1 })
            .count();
        assert!(in_hold > 0, "beats delivered in the hold: {in_hold}");
        assert_eq!(s.graphics_generation(), 1, "the generation moved in the hold");
        for st in log.iter().filter(|st| st.at >= f && st.at < drained) {
            for b in beats(&st.effects) {
                assert_eq!(b.graphics, GraphicsStatus::Ready, "a hold beat at {}", st.at);
            }
        }
    }

    // ---- the hang beat (amendment 2026-10-18, Owed 18) ----

    #[test]
    fn a_ready_page_that_stops_beating_restarts() {
        let th = t();
        let (alive, hang, base) = (
            th.cef_alive_every_ms,
            th.cef_hang_after_ms,
            th.cef_backoff_base_ms,
        );
        let step = period();
        let mut s = booted(0).decide(Event::Tick, 0).next;
        assert_eq!(s.graphics_generation(), 1, "setup: generation 1 built");
        s = s.decide(Event::CefReady { gen: 1 }, 0).next;
        let beat_for = 30_000;
        let (mut now, mut next_beat, mut last_alive) = (step, alive, 0);
        while now <= beat_for {
            if next_beat <= now {
                s = s.decide(Event::CefAlive { gen: 1 }, next_beat).next;
                last_alive = next_beat;
                next_beat += alive;
            }
            let d = s.decide(Event::Tick, now);
            assert!(
                !d.effects
                    .iter()
                    .any(|e| matches!(e, Effect::RestartGraphics { .. })),
                "a beating page restarted at {now}"
            );
            assert!(hides(&d.effects).is_empty(), "a beating page hid at {now}");
            s = d.next;
            now += step;
        }
        let hung = last_alive + hang;
        while now < hung - 1 {
            let d = s.decide(Event::Tick, now);
            assert!(hides(&d.effects).is_empty(), "hid early at {now}");
            s = d.next;
            now += step;
        }
        let d = s.decide(Event::Tick, hung - 1);
        assert!(hides(&d.effects).is_empty(), "hid 1 ms before the bound");
        let d = d.next.decide(Event::Tick, hung);
        assert_eq!(hides(&d.effects), vec![(HideReason::Hang, 1)]);
        assert_eq!(d.next.health(hung).graphics, GraphicsStatus::Restarting);
        let d = d.next.decide(Event::Tick, hung + base - 1);
        assert!(
            !d.effects.contains(&Effect::RestartGraphics { gen: 2 }),
            "restarted 1 ms before the back-off ends"
        );
        let d = d.next.decide(Event::Tick, hung + base);
        assert!(
            d.effects.contains(&Effect::RestartGraphics { gen: 2 }),
            "{:?}",
            d.effects
        );
        let mut s = d.next.decide(Event::CefReady { gen: 2 }, hung + base).next;
        assert_eq!(s.health(hung + base).graphics, GraphicsStatus::Ready);
        // Generation 2 beats, so no hide follows (a Ready page that never beats hangs, correctly).
        let (mut now, mut next_beat) = (hung + base + step, hung + base + alive);
        while now <= hung + base + beat_for {
            if next_beat <= now {
                s = s.decide(Event::CefAlive { gen: 2 }, next_beat).next;
                next_beat += alive;
            }
            let d = s.decide(Event::Tick, now);
            assert!(hides(&d.effects).is_empty(), "generation 2 hid at {now}");
            s = d.next;
            now += step;
        }
        assert_eq!(s.health(now).graphics, GraphicsStatus::Ready);
    }

    // ---- sequences across sub-machines (AGENTS.md §5: after an interruption) ----

    // Input loss during Starting soon: the input rebuilds after its back-off, the waiting slate stays
    // on air, and the session reads Playing throughout; the camera's return then goes live.
    #[test]
    fn input_loss_during_starting_soon_rebuilds_and_keeps_the_waiting_slate() {
        let th = t();
        let step = period();
        let mut s = booted(0).decide(Event::Tick, 0).next;
        let mut sent = 1_000;
        s = s.decide(progress(&s, sent), 0).next;
        s = s.decide(Event::InputConnected { gen: 1 }, 10).next;
        let lost = 20;
        s = s
            .decide(
                Event::InputLost {
                    gen: 1,
                    reason: LossReason::Eos,
                },
                lost,
            )
            .next;
        assert_eq!(s.health(lost).input, LinkStatus::Backoff);
        let (mut now, mut rebuilt) = (step, None);
        while now < lost + th.input_backoff_base_ms + 2 * step {
            sent += 1_000;
            s = s.decide(progress(&s, sent), now).next;
            let d = s.decide(Event::Tick, now);
            assert_eq!(d.frame, Some(FrameChoice::Slate(SlateKind::Waiting)), "at {now}");
            if d.effects.contains(&Effect::RebuildInput { gen: 2 }) {
                rebuilt.get_or_insert(now);
            }
            s = d.next;
            assert_eq!(s.health(now).state, BeatState::Playing, "at {now}");
            now += step;
        }
        let at = rebuilt.expect("never rebuilt");
        assert!(at >= lost + th.input_backoff_base_ms, "rebuilt early at {at}");
        assert_eq!(s.health(now).pipeline_rebuilds, 1);
        assert_eq!(s.health(now).input_kicks, 1, "an EOS while Running is a kick");
        s = s.decide(Event::InputConnected { gen: 2 }, now).next;
        let back = now;
        let mut live_at = None;
        while now < back + th.resume_after_ms + 3 * step {
            sent += 1_000;
            s = s.decide(Event::InputFrame, now).next;
            s = s.decide(progress(&s, sent), now).next;
            let d = s.decide(Event::Tick, now);
            if d.frame == Some(FrameChoice::Emit) {
                live_at.get_or_insert(now);
            }
            s = d.next;
            now += step;
        }
        let live = live_at.expect("never live");
        assert!(live - back >= th.resume_after_ms, "live after {} ms", live - back);
    }

    // A hang during Reconnecting: graphics and the programme run independently. The overlay comes
    // off air and restarts while the reconnecting slate stays on, and the camera's return goes live.
    #[test]
    fn a_hang_during_reconnecting_hides_and_restarts_while_the_slate_holds() {
        let th = t();
        let step = period();
        let (s, at) = reach(Reconnecting);
        let mut s = s.decide(Event::CefReady { gen: 1 }, at).next;
        let hung = at + th.cef_hang_after_ms;
        // The hide lands up to a period after the bound, and the restart up to a period after its
        // back-off ends.
        let restart_by = hung + th.cef_backoff_base_ms + 2 * step;
        let (mut now, mut hid, mut restarted) = (at + step, None, None);
        while now <= restart_by {
            let d = s.decide(Event::Tick, now);
            assert_eq!(
                d.frame,
                Some(FrameChoice::Slate(SlateKind::Reconnecting)),
                "the slate at {now}"
            );
            if !hides(&d.effects).is_empty() {
                assert_eq!(hides(&d.effects), vec![(HideReason::Hang, 1)]);
                hid.get_or_insert(now);
            }
            if d.effects.contains(&Effect::RestartGraphics { gen: 2 }) {
                restarted.get_or_insert(now);
            }
            s = d.next;
            now += step;
        }
        let hid = hid.expect("the hang never hid");
        assert!(hid >= hung && hid < hung + step, "hid at {hid}, bound {hung}");
        let restarted = restarted.expect("never restarted");
        assert!(
            restarted >= hid + th.cef_backoff_base_ms,
            "restarted at {restarted}"
        );
        assert_eq!(s.programme_phase(), Reconnecting);
        let back = now;
        while now < back + th.resume_after_ms + 2 * step {
            s = s.decide(Event::InputFrame, now).next;
            s = s.decide(Event::Tick, now).next;
            now += step;
        }
        assert_eq!(s.programme_phase(), Live, "the camera's return after the hang");
    }

    // Final while the ladder is stepped: the one step down holds through the hold, an overrun in the
    // hold changes nothing, and the final beat still reports the rung.
    #[test]
    fn final_while_the_ladder_is_stepped_keeps_the_rung() {
        let th = t();
        let mut s = booted(0).decide(Event::Tick, 0).next;
        let mut steps = Vec::new();
        for k in 0..th.ladder_overruns_to_step as Ms {
            let d = s.decide(Event::EncoderOverrun, 100 + k);
            steps.extend(d.effects.into_iter().filter(|e| matches!(e, Effect::StepDown(_))));
            s = d.next;
        }
        assert_eq!(
            steps,
            vec![Effect::StepDown(Rung::OverlayHalfRate)],
            "setup: one step"
        );
        let dropped = s.health(200).dropped_frames;
        assert_eq!(dropped, th.ladder_overruns_to_step as u64);
        s = s.decide(Event::HeartbeatReply(Desired::Ending), 1_000).next;
        for k in 0..th.ladder_overruns_to_step as Ms {
            let d = s.decide(Event::EncoderOverrun, 1_100 + k);
            assert!(d.effects.is_empty(), "an overrun in the hold: {:?}", d.effects);
            assert_eq!(d.next, s, "an overrun in the hold changed the supervisor");
        }
        let d = s.decide(Event::Tick, 1_200);
        let b = beats(&d.effects);
        assert_eq!(b.len(), 1, "the first hold tick beats");
        assert_eq!((b[0].rung, b[0].programme_phase), (Rung::OverlayHalfRate, Final));
        let d = d.next.decide(Event::Tick, 1_000 + th.final_hold_ms);
        let b = beats(&d.effects);
        assert_eq!(b[0].rung, Rung::OverlayHalfRate, "the drain's final beat");
        assert_eq!(b[0].dropped_frames, dropped, "the hold dropped no counted frame");
    }

    // A crash at the count: the cef_fail_count-th crash in the window hides as Failed, graphics read
    // failed, and the page retries once the window has passed (C05: Failed is recoverable).
    #[test]
    fn a_crash_at_the_count_hides_as_failed_and_retries_after_the_window() {
        let th = t();
        let mut s = booted(0).decide(Event::Tick, 0).next;
        let mut now = 0;
        let mut hidden = Vec::new();
        for n in 1..=th.cef_fail_count as u64 {
            s = s.decide(Event::CefReady { gen: n }, now).next;
            now += 100;
            let d = s.decide(Event::CefCrashed { gen: n }, now);
            hidden.extend(hides(&d.effects));
            s = d.next;
            if n < th.cef_fail_count as u64 {
                let mut guard = 0;
                loop {
                    now += period();
                    let d = s.decide(Event::Tick, now);
                    let restarted = d.effects.contains(&Effect::RestartGraphics { gen: n + 1 });
                    s = d.next;
                    if restarted {
                        break;
                    }
                    guard += 1;
                    assert!(guard < 1_000, "no restart after crash {n}");
                }
            }
        }
        let count = th.cef_fail_count as u64;
        assert_eq!(hidden, vec![(HideReason::Failed, count)]);
        assert_eq!(s.health(now).graphics, GraphicsStatus::Failed);
        let retry = now + th.cef_fail_window_ms;
        let d = s.decide(Event::Tick, retry - 1);
        assert!(
            !d.effects.contains(&Effect::RestartGraphics { gen: count + 1 }),
            "retried 1 ms before the window"
        );
        let d = d.next.decide(Event::Tick, retry);
        assert!(
            d.effects.contains(&Effect::RestartGraphics { gen: count + 1 }),
            "{:?}",
            d.effects
        );
    }
}
