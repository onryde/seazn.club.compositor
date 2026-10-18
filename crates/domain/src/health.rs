//! The heartbeat's report (spec §5.3; R2002's `heartbeat` def). Plain data: `Supervisor::health`
//! maps the sub-machines into it, and C07's `heartbeat_wire` writes it. Every optional field is
//! `None` until it is known, never defaulted (plan: "omitted when not measured, never sent as 0").
use crate::ladder::Rung;
use crate::lifecycle::ExitReason;
use crate::programme::ProgrammePhase;

/// The beat's `state`. Owner ruling 2026-10-14 (plan review F5, option A): `Playing` means the
/// programme is on air, slate or picture; the camera's state travels in `programme_phase` / `input`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeatState {
    Starting,
    Playing,
    Stalled,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkStatus {
    Connecting,
    Connected,
    Backoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphicsStatus {
    Starting,
    Ready,
    Restarting,
    Failed,
}

/// Overlay templates spec 2026-10-15 §4.3 (D10): the channel the page learns the phase from, as the
/// page reports it (C11 relays it): the Machine's loopback endpoint (A), or web (fallback B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhaseChannel {
    Loopback,
    Web,
}

/// Owner ruling 2026-10-15: the final beat carries the exit reason, because the Machine's logs go
/// with the Machine. `detail` is a fatal error's text, verbatim from `Event::Fatal { detail }`, and
/// `None` on every other exit (`docs/_INDEX.md` Owed 27, option a); C07 redacts it, caps it and
/// omits it when empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    pub reason: ExitReason,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    pub state: BeatState,
    pub programme_phase: ProgrammePhase,
    pub input: LinkStatus,
    pub output: LinkStatus,
    pub graphics: GraphicsStatus,
    pub input_kicks: u32,
    pub input_kick_alert: bool,
    /// Input builds after the first.
    pub pipeline_rebuilds: u64,
    /// Output builds after the first.
    pub sink_swaps: u64,
    /// Ladder overruns: frames dropped before the encoder.
    pub dropped_frames: u64,
    /// `Full` or `OverlayHalfRate` only (owner ruling 2026-10-18, Owed 20: no lite mode).
    pub rung: Rung,
    /// `None` until the page reports it (C11's `ScoreStale`).
    pub score_stale: Option<bool>,
    pub heartbeat_failures: u32,
    /// `None` until the page reports one: absent, never defaulted (C07 omits the key).
    pub phase_channel: Option<PhaseChannel>,
    /// `Some` on a `Stopped` beat only: the drain's or the exit's reason. `None` in every other state.
    pub exit: Option<ExitInfo>,
}
