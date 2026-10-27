//! Camera pull link (SRT from CF Input A): reconnect forever on a capped back-off, count peer kicks,
//! and ignore every event from a generation other than the current one.
//!
//! ```text
//! Backoff --due tick: build the next generation--> Connecting --first decoded frame--> Running
//!    ^                                                  |                                 |
//!    +------------- connect timeout, or lost -----------+------ lost (EOS, error, stall) -+
//! ```
//!
//! - **Back-off.** `attempt` counts consecutive failures, and failure number f waits `delay(f - 1)`:
//!   1, 2, 4, 8, 10, 10 s (PDF failure table), retried forever. A first decoded frame resets it.
//! - **Generations.** Every build starts a new generation, and every adapter event carries the
//!   generation of the pipeline that sent it. An event from any other generation is ignored, so a late
//!   `InputLost` from a torn-down pipeline cannot knock down the healthy new one.
//! - **Kicks.** An EOS while Running is a peer kick: CF closed the playback (maintenance; F-002 saw no
//!   second-reader kick). More than `kick_alert_count` kicks in `kick_window_ms` raise the alert. A
//!   stall or an error is not a kick, and neither is an EOS before the first frame.
//! - **Stall.** A Running input with no frame for `input_stall_after_ms` is lost as `Stalled`, because
//!   CF holds a silent playback socket open (F-002). The connect has its own budget,
//!   `input_connect_timeout_ms`, so the stall rule reads Running only.
//! - **Give-ups (C10b, Owed 50).** The connect timeout and the stall rule are the link's own
//!   decisions, so it reports each one as `InputAction::Abandon` for the generation it gave up, and the
//!   adapter shuts that generation out: the domain is the one authority on them, and the adapter
//!   keeps no copy of the rules. Each generation is given up at most once, since the link then backs
//!   off and only a rebuild moves it on. A loss the adapter reported (`on_lost`) owes no abandon: the
//!   adapter decided it, and its gate is shut already.
//! - **Cloudflare's hold-end close (Owed 108 fix (a), F-032).** When the phone drops, Cloudflare holds
//!   its SRT broadcast about 30 s, then closes every attached reader with an error. The adapter reports
//!   an error on the current build, and the first one on the last build the link gave up while that
//!   build still runs (it is the current generation until the next rebuild), as `on_error`, with the
//!   build's `BuildFacts`. Between builds (Connecting or Backoff), an error on a build whose handshake
//!   completed and that never carried media, at least `input_hold_end_after_ms` after the last frame,
//!   is that close: the link rebuilds a base wait later, its attempt count reset, so a phone already
//!   back is not left waiting out the 8 or 10 s step. Bound: once an outage, re-armed only when a
//!   generation reaches Running, and disarmed until the link's first Running. Any other error is
//!   today's `on_lost(Error)`: a loss while Running or Connecting, nothing in Backoff.
//! - **The phone is live again (Owed 108 fix (b), F-032).** Web counts each time the session's phone
//!   comes back live after an outage (`phoneLiveEdge`, in a heartbeat's reply: the only way web can
//!   reach the relay) and the supervisor hands each value read to `on_phone_live_edge`. The first value
//!   the link sees is a baseline, never an edge, so a boot and an in-place restart act on nothing; each
//!   later increase latches ONE nudge, however much the value rose. The nudge acts on the next tick
//!   (`check_nudge`) or at the link's next failure (`back_off`), and either way the build is due at
//!   once with the attempt count reset, as after a first failure, never later than a build already
//!   due:
//!   - **Backoff:** the build is due now.
//!   - **Running:** nothing. A camera still delivering is never torn down (a web outage raises every
//!     live session's counter at once); the nudge stays latched and acts at the stall or the loss.
//!   - **Connecting with no media yet** (`on_media`: no byte has left the build's source): the connect
//!     is given up (`Abandon`) and built again now, once an outage (re-armed when a generation reaches
//!     Running): a reader attached to Cloudflare's held broadcast gets no bytes. A connect already
//!     carrying bytes, or one after the outage's discard, is kept, and the nudge acts at its connect
//!     timeout or its loss.
//!
//!   The nudge is cleared when it acts, or when a generation reaches Running.
use crate::backoff::delay;
use crate::config::Thresholds;
use crate::time::{elapsed, Ms};
use crate::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LossReason {
    Stalled,
    Eos,
    Error,
}

/// What a build's SRT connection had shown when its pipeline posted an error (Owed 108, F-032). The
/// adapter reads both off the build itself. Fix (b) reads `carried_media` too: a connect with media is
/// never discarded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct BuildFacts {
    /// The SRT handshake completed: libsrt had connected the socket. A refused connect (a rotated
    /// passphrase, a deleted input) errors about 20 ms in without one, which is what tells it from
    /// Cloudflare's hold-end close.
    pub handshake_completed: bool,
    /// Bytes reached the build past `srtsrc`, decoded or not.
    pub carried_media: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputState {
    /// The current generation is being built; `attempt` consecutive failures came before it.
    Connecting { attempt: u32, since: Ms },
    /// Frames flow; `since` is this generation's first decoded frame (C10).
    Running { since: Ms },
    /// `attempt` consecutive failures so far; the next build is at the first tick at or after `until`.
    Backoff { attempt: u32, until: Ms },
}

/// What the link asks of the input adapter (C10b): the effect a tick or a stall check decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// Build generation `gen` (`Effect::RebuildInput`).
    Rebuild { gen: u64 },
    /// The link gave generation `gen` up by its own rule, the connect timeout or the stall rule
    /// (`Effect::AbandonInput`).
    Abandon { gen: u64 },
}

/// What the driver's log reads of the phone-live nudge (Owed 108 fix (b), the PR #99 review's m1): a
/// copy of the link's own fields, taken before and after each decision. No rule reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NudgeView {
    pub state: InputState,
    pub generation: u64,
    /// Web's counter as last read; `None` until a reply carried one.
    pub counter: Option<u64>,
    /// A nudge is latched and has not acted yet.
    pub latched: bool,
    /// The outage's one discard of a connect with no byte is not spent yet.
    pub discard_armed: bool,
    /// The current generation's first byte has left its source.
    pub media: bool,
}

/// Kick history bound: the window never holds more than this many stamps (AGENTS.md §4, nothing
/// unbounded). C04 brief default. It must exceed `kick_alert_count`, or a kick storm could never read
/// as more than the alert count (pinned by `the_kick_history_bound_leaves_room_for_the_alert`).
pub const KICK_HISTORY_CAP: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputLink {
    state: InputState,
    generation: u64,
    kicks: Window,
    /// The hold-end rebuild's one shot an outage (Owed 108): set when a generation reaches Running,
    /// spent by the rebuild. False until the link's first Running.
    hold_end_armed: bool,
    /// Web's phone-live edge counter as last read (Owed 108 fix (b)): `None` until a reply carries
    /// one, so the first value is a baseline.
    phone_live_edge: Option<u64>,
    /// A nudge latched by an increase of that counter and not acted on yet.
    nudged: bool,
    /// The nudge's one discard of a connect an outage: spent by the discard, re-armed when a
    /// generation reaches Running. Armed from the start: web's counter says the phone is back whether
    /// or not this process has had a camera yet (an in-place restart has not).
    discard_armed: bool,
    /// The current generation's first byte has left its source (`on_media`). False at each build.
    media: bool,
}

impl Default for InputLink {
    fn default() -> Self {
        Self::new()
    }
}

impl InputLink {
    /// `Backoff { attempt: 0, until: 0 }`: the first tick builds generation 1.
    pub fn new() -> Self {
        Self {
            state: InputState::Backoff { attempt: 0, until: 0 },
            generation: 0,
            kicks: Window::new(KICK_HISTORY_CAP),
            hold_end_armed: false,
            phone_live_edge: None,
            nudged: false,
            discard_armed: true,
            media: false,
        }
    }

    pub fn state(&self) -> InputState {
        self.state
    }

    /// The number of builds so far, and the generation an adapter event must carry to count.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The phone-live nudge's fields as they stand, for the log (`NudgeView`).
    pub fn nudge_view(&self) -> NudgeView {
        NudgeView {
            state: self.state,
            generation: self.generation,
            counter: self.phone_live_edge,
            latched: self.nudged,
            discard_armed: self.discard_armed,
            media: self.media,
        }
    }

    /// Whether a frame of generation `gen` counts (C10b): it is the current generation, and the link
    /// has not given it up or lost it (Backoff). A frame from a generation rebuilt past, or one queued
    /// before the give-up, changes nothing. While Connecting it counts. The adapter never sends a
    /// build's frames ahead of its `InputConnected` (one streaming thread sends both, that one first,
    /// on a FIFO channel), save when the `InputConnected` was refused for a full channel and is tried
    /// again on the next sample; such a frame is the build's own (the PR #35 review's m3).
    pub fn takes_frames_from(&self, gen: u64) -> bool {
        gen == self.generation && !matches!(self.state, InputState::Backoff { .. })
    }

    /// The current generation's first decoded frame (C10 times "connected" to it, F-002). It counts
    /// only while Connecting: a second one while Running must not restart the stall clock.
    pub fn on_connected(&mut self, now: Ms, gen: u64) {
        if gen == self.generation && matches!(self.state, InputState::Connecting { .. }) {
            self.state = InputState::Running { since: now };
            self.hold_end_armed = true;
            // Owed 108 fix (b): the camera is back, so a nudge not yet acted on has nothing left to
            // do, and the next outage gets its own discard.
            self.nudged = false;
            self.discard_armed = true;
        }
    }

    /// The current generation's first byte out of its source (Owed 108 fix (b)), decoded or not: a
    /// nudge then keeps the connect instead of discarding it. Another generation's changes nothing.
    /// One that comes after the link gave its generation up needs no guard: the flag is read only
    /// while Connecting, and every build clears it.
    pub fn on_media(&mut self, gen: u64) {
        if gen == self.generation {
            self.media = true;
        }
    }

    /// Web's phone-live edge counter from a heartbeat reply (Owed 108 fix (b)). The first value is a
    /// baseline; a value above the last one read latches one nudge (`check_nudge`). A value at or
    /// below it latches nothing: web's counter never goes down, so a lower one is not an edge, and
    /// it becomes the value the next one is read against.
    pub fn on_phone_live_edge(&mut self, count: u64) {
        if self.phone_live_edge.is_some_and(|seen| count > seen) {
            self.nudged = true;
        }
        self.phone_live_edge = Some(count);
    }

    /// The latched nudge, on a tick (Owed 108 fix (b)), before `on_tick`: in Backoff the build is due
    /// now, its count reset, so the tick builds it; Connecting with no media and the outage's discard
    /// armed, the connect is given up (`Abandon`) and its rebuild is due now. Running, or a connect
    /// with media, or the discard spent, it stays latched for `back_off`: the stall, the loss or the
    /// connect timeout.
    pub fn check_nudge(&mut self, now: Ms, t: &Thresholds) -> Option<InputAction> {
        if !self.nudged {
            return None;
        }
        match self.state {
            // Due now, so never later than the build it replaces: one due sooner is due by now too,
            // and this tick's `on_tick` builds either.
            InputState::Backoff { .. } => {
                self.back_off(now, 0, t);
                None
            }
            InputState::Connecting { .. } if !self.media && self.discard_armed => {
                self.discard_armed = false;
                let gen = self.generation;
                self.back_off(now, 0, t);
                Some(InputAction::Abandon { gen })
            }
            InputState::Connecting { .. } | InputState::Running { .. } => None,
        }
    }

    /// An error on generation `gen`'s pipeline (Owed 108, F-032): the current build's, or the first one
    /// of the last build the link gave up while that build still runs. Cloudflare's hold-end close
    /// (`is_hold_end`) rebuilds a base wait later with the attempt count reset; any other error is
    /// `on_lost(Error)`. `last_frame_at` is the programme's latest camera frame.
    pub fn on_error(
        &mut self,
        now: Ms,
        gen: u64,
        facts: BuildFacts,
        last_frame_at: Option<Ms>,
        t: &Thresholds,
    ) {
        if gen != self.generation {
            return;
        }
        if !self.is_hold_end(now, facts, last_frame_at, t) {
            self.on_lost(now, gen, LossReason::Error, t);
            return;
        }
        self.hold_end_armed = false;
        // A base wait with the count reset, unless a build is already due sooner: the rule cuts a
        // wait short and never makes one longer.
        let due = match self.state {
            InputState::Backoff { until, .. } => Some(until),
            InputState::Connecting { .. } | InputState::Running { .. } => None,
        };
        self.back_off(now, 0, t);
        if let (Some(due), InputState::Backoff { until, .. }) = (due, &mut self.state) {
            *until = (*until).min(due);
        }
    }

    /// Cloudflare's hold-end close (F-032): between builds, armed, on a build whose handshake
    /// completed and that never carried media, at least `input_hold_end_after_ms` after the last
    /// frame. The handshake is the condition that matters most: a refused connect errors without one.
    /// With no frame to measure from it is never the close (an armed link has had a Running, so has a
    /// frame; this is the guard on that).
    fn is_hold_end(&self, now: Ms, facts: BuildFacts, last_frame_at: Option<Ms>, t: &Thresholds) -> bool {
        let between_builds = !matches!(self.state, InputState::Running { .. });
        between_builds
            && self.hold_end_armed
            && facts.handshake_completed
            && !facts.carried_media
            && last_frame_at.is_some_and(|f| elapsed(now, f) >= t.input_hold_end_after_ms)
    }

    /// The current generation's pipeline ended: EOS, an error, or the stall rule.
    pub fn on_lost(&mut self, now: Ms, gen: u64, reason: LossReason, t: &Thresholds) {
        if gen != self.generation {
            return;
        }
        let failed_before = match self.state {
            InputState::Running { .. } => {
                if reason == LossReason::Eos {
                    self.kicks.record(now, t.kick_window_ms);
                }
                0
            }
            InputState::Connecting { attempt, .. } => attempt,
            // Already lost: a second EOS or an error from the same pipeline changes nothing.
            InputState::Backoff { .. } => return,
        };
        self.back_off(now, failed_before, t);
    }

    /// Running but no frame since max(running since, last frame) for `input_stall_after_ms`: lost as
    /// `Stalled`, and given up (`Abandon`). A frame from before this connection does not count.
    pub fn check_stall(&mut self, now: Ms, last_frame_at: Option<Ms>, t: &Thresholds) -> Option<InputAction> {
        let InputState::Running { since } = self.state else {
            return None;
        };
        let fresh = last_frame_at.map_or(since, |f| f.max(since));
        if elapsed(now, fresh) < t.input_stall_after_ms {
            return None;
        }
        let gen = self.generation;
        self.on_lost(now, gen, LossReason::Stalled, t);
        Some(InputAction::Abandon { gen })
    }

    /// A due back-off builds the next generation (`Rebuild`); a connect past
    /// `input_connect_timeout_ms` is given up (`Abandon`) and backs off.
    pub fn on_tick(&mut self, now: Ms, t: &Thresholds) -> Option<InputAction> {
        match self.state {
            InputState::Backoff { attempt, until } if now >= until => {
                self.generation += 1;
                self.state = InputState::Connecting { attempt, since: now };
                self.media = false;
                Some(InputAction::Rebuild { gen: self.generation })
            }
            InputState::Connecting { attempt, since }
                if elapsed(now, since) >= t.input_connect_timeout_ms =>
            {
                self.back_off(now, attempt, t);
                Some(InputAction::Abandon { gen: self.generation })
            }
            _ => None,
        }
    }

    pub fn kicks(&self, now: Ms, t: &Thresholds) -> usize {
        self.kicks.count(now, t.kick_window_ms)
    }

    /// PDF failure table: "alert if it repeats > 3 times in 5 min".
    pub fn kick_alert(&self, now: Ms, t: &Thresholds) -> bool {
        self.kicks(now, t) > t.kick_alert_count
    }

    /// Failure number `failed_before + 1` waits `delay(failed_before)`. Both additions saturate: the
    /// clock is a u64 (time.rs), and "retried forever" has no last attempt. A latched nudge (Owed 108
    /// fix (b)) acts here and is spent: the count starts again as after a first failure, and the build
    /// is due now.
    fn back_off(&mut self, now: Ms, failed_before: u32, t: &Thresholds) {
        let (failed_before, wait) = if self.nudged {
            self.nudged = false;
            (0, 0)
        } else {
            let wait = delay(failed_before, t.input_backoff_base_ms, t.input_backoff_cap_ms);
            (failed_before, wait)
        };
        self.state = InputState::Backoff {
            attempt: failed_before.saturating_add(1),
            until: now.saturating_add(wait),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Thresholds {
        Thresholds::default()
    }

    /// A tick that built (`Rebuild`), as against one that gave a generation up or did nothing.
    fn built(a: Option<InputAction>) -> bool {
        matches!(a, Some(InputAction::Rebuild { .. }))
    }

    /// Build, connect, run: returns a link in Running at generation 1.
    fn running(now: Ms) -> InputLink {
        let mut l = InputLink::new();
        assert!(built(l.on_tick(now, &t())));
        l.on_connected(now, 1);
        l
    }

    /// Ticks forward from `now` in 10 ms steps until the next build; returns its time.
    fn next_build(l: &mut InputLink, now: Ms, th: &Thresholds) -> Ms {
        next_build_every(l, now, 10, th)
    }

    /// Ticks forward from `now` in `step` ms steps until the next build; returns its time. From
    /// Backoff or Connecting the next build is due within one connect timeout (Connecting gives up)
    /// plus the capped back-off, so a link that never builds fails here instead of hanging the suite.
    fn next_build_every(l: &mut InputLink, mut now: Ms, step: Ms, th: &Thresholds) -> Ms {
        let horizon = now + th.input_connect_timeout_ms + th.input_backoff_cap_ms + step;
        while !built(l.on_tick(now, th)) {
            now += step;
            assert!(now <= horizon, "no build by {horizon} ms: {:?}", l.state());
        }
        now
    }

    /// An EOS from Running at `now`, then the rebuild and its first frame: one kick, recovered.
    fn kick_and_recover(l: &mut InputLink, now: Ms, th: &Thresholds) -> Ms {
        let gen = l.generation();
        l.on_lost(now, gen, LossReason::Eos, th);
        let built = next_build(l, now, th);
        l.on_connected(built, l.generation());
        assert!(matches!(l.state(), InputState::Running { .. }));
        built
    }

    // ---- the brief's 12 (C04 Step 1) ----

    #[test]
    fn first_tick_builds_generation_one() {
        let mut l = InputLink::new();
        assert_eq!(l.on_tick(0, &t()), Some(InputAction::Rebuild { gen: 1 }));
        assert_eq!(l.generation(), 1);
        assert!(matches!(l.state(), InputState::Connecting { attempt: 0, .. }));
        assert_eq!(l.on_tick(1, &t()), None, "no second build while connecting");
    }

    #[test]
    fn consecutive_failures_follow_the_pdf_backoff_1_2_4_8_10_10() {
        let th = t();
        let mut l = InputLink::new();
        let mut now = 0;
        let mut gaps = Vec::new();
        let mut last_build = None;
        for _ in 0..7 {
            // wait for the next build (bounded, so a link that never builds fails rather than hangs)
            now = next_build(&mut l, now, &th);
            if let Some(b) = last_build {
                gaps.push(now - b);
            }
            last_build = Some(now);
            let gen = l.generation();
            l.on_lost(now, gen, LossReason::Error, &th); // fails instantly
        }
        // The PDF's sequence, from the config's numbers: base, doubling, capped.
        let (base, cap) = (th.input_backoff_base_ms, th.input_backoff_cap_ms);
        let want: Vec<Ms> = (0..6).map(|a| (base << a).min(cap)).collect();
        assert_eq!(
            want,
            vec![1_000, 2_000, 4_000, 8_000, 10_000, 10_000],
            "PDF: 1 s → 10 s"
        );
        let rounded: Vec<Ms> = gaps.iter().map(|g| g / 10 * 10).collect();
        assert_eq!(rounded, want);
    }

    #[test]
    fn loss_from_running_restarts_backoff_at_one_second() {
        let th = t();
        let mut l = running(0);
        l.on_lost(5_000, 1, LossReason::Stalled, &th);
        assert_eq!(
            l.state(),
            InputState::Backoff {
                attempt: 1,
                until: 5_000 + th.input_backoff_base_ms
            }
        );
    }

    #[test]
    fn connect_that_hangs_times_out_into_backoff() {
        let th = t();
        let mut l = InputLink::new();
        l.on_tick(0, &th);
        assert_eq!(l.on_tick(th.input_connect_timeout_ms - 1, &th), None);
        assert!(matches!(l.state(), InputState::Connecting { .. }), "1 ms early");
        assert_eq!(
            l.on_tick(th.input_connect_timeout_ms, &th),
            Some(InputAction::Abandon { gen: 1 }),
            "the link gives up the generation it timed out"
        );
        assert!(matches!(l.state(), InputState::Backoff { attempt: 1, .. }));
    }

    #[test]
    fn stale_generation_loss_is_ignored() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Eos, &th);
        // A wait loop on a fixed `now` hangs when the back-off is wrong; the assertion fails instead.
        assert!(built(l.on_tick(2_000, &th)), "the 1 s back-off ends at 2 s");
        l.on_connected(2_000, 2);
        l.on_lost(2_100, 1, LossReason::Error, &th); // late event from the torn-down gen-1 pipeline
        assert_eq!(l.state(), InputState::Running { since: 2_000 });
    }

    #[test]
    fn stale_generation_connected_is_ignored() {
        let mut l = InputLink::new();
        l.on_tick(0, &t());
        l.on_connected(0, 0);
        assert!(matches!(l.state(), InputState::Connecting { .. }));
    }

    #[test]
    fn second_loss_while_backing_off_is_ignored() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Eos, &th);
        let s = l.state();
        l.on_lost(1_010, 1, LossReason::Error, &th);
        assert_eq!(l.state(), s);
    }

    #[test]
    fn kick_alert_needs_more_than_three_eos_in_five_minutes() {
        let th = t();
        let mut l = running(0);
        let mut now = 0;
        for k in 1..=th.kick_alert_count + 1 {
            now += 20_000; // PDF chaos: relay kicks the reader every 20 s
            let gen = l.generation();
            l.on_lost(now, gen, LossReason::Eos, &th);
            now = next_build_every(&mut l, now, 100, &th);
            l.on_connected(now, l.generation());
            assert_eq!(l.kick_alert(now, &th), k > th.kick_alert_count, "after {k} kicks");
        }
        assert!(
            !l.kick_alert(now + th.kick_window_ms + 1, &th),
            "alert clears once kicks age out"
        );
    }

    // The window's edge, from C02's `Window` rule rather than this module: a stamp exactly `span` old
    // still counts, and one 1 ms older does not (window::tests::stamp_exactly_span_old_still_counts,
    // counts_only_inside_span). So `kick_alert_count + 1` kicks whose first and last lie exactly
    // `kick_window_ms` apart raise the alert at the last kick's instant, and it clears 1 ms later.
    #[test]
    fn kicks_exactly_the_window_apart_raise_the_alert_and_clear_1_ms_later() {
        let th = t();
        let (n, w) = (th.kick_alert_count as Ms, th.kick_window_ms);
        let t0 = 10_000;
        let mut l = running(0);
        for i in 0..=n {
            let at = t0 + w * i / n;
            assert!(
                matches!(l.state(), InputState::Running { .. }),
                "kick {i} must come from Running"
            );
            if i == n {
                assert!(!l.kick_alert(at, &th), "{n} kicks are not yet more than {n}");
            }
            l.on_lost(at, l.generation(), LossReason::Eos, &th);
            if i < n {
                // The 1 s back-off after a kick, then the rebuild's first frame: long before the next.
                let rebuilt = at + th.input_backoff_base_ms;
                assert!(rebuilt < t0 + w * (i + 1) / n);
                assert!(
                    built(l.on_tick(rebuilt, &th)),
                    "the kick's back-off ends at {rebuilt}"
                );
                l.on_connected(rebuilt, l.generation());
            }
        }
        let last = t0 + w;
        assert_eq!(
            l.kicks(last, &th),
            th.kick_alert_count + 1,
            "the first kick is exactly the window old, so it still counts"
        );
        assert!(l.kick_alert(last, &th));
        assert_eq!(
            l.kicks(last + 1, &th),
            th.kick_alert_count,
            "1 ms later it is out"
        );
        assert!(!l.kick_alert(last + 1, &th));
    }

    #[test]
    fn camera_stall_is_not_a_kick() {
        let th = t();
        let mut l = running(0);
        let mut now = 0;
        for _ in 0..10 {
            now += 1_000;
            let gen = l.generation();
            l.on_lost(now, gen, LossReason::Stalled, &th);
            now = next_build_every(&mut l, now, 100, &th);
            l.on_connected(now, l.generation());
        }
        assert_eq!(l.generation(), 11, "ten rebuilds happened");
        assert_eq!(l.kicks(now, &th), 0, "a camera stall is not a peer kick");
    }

    #[test]
    fn running_without_frames_stalls_into_a_rebuild() {
        // CF can keep the playback socket open with nothing flowing (phone backgrounded)
        let th = t();
        let mut l = running(0);
        assert_eq!(l.check_stall(th.input_stall_after_ms - 1, None, &th), None);
        assert!(matches!(l.state(), InputState::Running { .. }));
        assert_eq!(
            l.check_stall(th.input_stall_after_ms, None, &th),
            Some(InputAction::Abandon { gen: 1 }),
            "the link gives up the generation that stalled"
        );
        assert!(matches!(l.state(), InputState::Backoff { attempt: 1, .. }));
    }

    #[test]
    fn stall_clock_runs_from_the_later_of_connect_and_last_frame() {
        let th = t();
        let mut l = running(0);
        l.check_stall(10_000, Some(9_000), &th); // a fresh frame 1 s ago: healthy
        assert!(matches!(l.state(), InputState::Running { .. }));
        l.check_stall(9_000 + th.input_stall_after_ms, Some(9_000), &th);
        assert!(matches!(l.state(), InputState::Backoff { .. }));
        // a frame from BEFORE this connection must not count: reconnect at 20 s, old frame at 9 s
        let mut l = running(20_000);
        l.check_stall(20_000 + th.input_stall_after_ms - 1, Some(9_000), &th);
        assert!(
            matches!(l.state(), InputState::Running { .. }),
            "an old frame must not stall a fresh connection"
        );
    }

    #[test]
    fn stall_is_not_a_kick_and_is_ignored_unless_running() {
        let th = t();
        let mut l = InputLink::new();
        l.check_stall(1_000_000, None, &th); // backing off: no effect
        assert!(matches!(l.state(), InputState::Backoff { attempt: 0, until: 0 }));
        let mut l = running(0);
        l.check_stall(th.input_stall_after_ms, None, &th);
        assert_eq!(l.kicks(th.input_stall_after_ms, &th), 0);
    }

    // ---- sequences: a second call, an interruption, both generation directions ----

    // A second call: the pipeline that was kicked can report EOS twice (EOS, then an error on teardown).
    // One kick is one kick.
    #[test]
    fn a_second_eos_from_the_kicked_pipeline_counts_once() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Eos, &th);
        l.on_lost(1_005, 1, LossReason::Eos, &th);
        l.on_lost(1_010, 1, LossReason::Error, &th);
        assert_eq!(l.kicks(1_010, &th), 1);
    }

    // kick → recover → kick: the second kick lands on generation 2 and counts as the second.
    #[test]
    fn kick_recover_kick_counts_two() {
        let th = t();
        let mut l = running(0);
        let back = kick_and_recover(&mut l, 1_000, &th);
        assert_eq!((l.generation(), l.kicks(back, &th)), (2, 1));
        let again = kick_and_recover(&mut l, back + 5_000, &th);
        assert_eq!((l.generation(), l.kicks(again, &th)), (3, 2));
        assert_eq!(
            l.state(),
            InputState::Running { since: again },
            "recovered after the second kick"
        );
    }

    #[test]
    fn eos_before_the_first_frame_is_a_failure_not_a_kick() {
        let th = t();
        let mut l = InputLink::new();
        l.on_tick(0, &th);
        l.on_lost(500, 1, LossReason::Eos, &th);
        assert_eq!(
            l.state(),
            InputState::Backoff {
                attempt: 1,
                until: 500 + th.input_backoff_base_ms
            }
        );
        assert_eq!(l.kicks(500, &th), 0);
    }

    // A second call: a duplicate "connected" while Running must not move `since`, or the stall clock
    // (which runs from it when no frame came) would restart on a link that never delivered a frame.
    #[test]
    fn a_second_connected_does_not_restart_the_run() {
        let th = t();
        let mut l = running(0);
        l.on_connected(4_000, 1);
        assert_eq!(l.state(), InputState::Running { since: 0 });
        l.check_stall(th.input_stall_after_ms, None, &th);
        assert!(
            matches!(l.state(), InputState::Backoff { .. }),
            "stall clock from the first frame"
        );
    }

    // Old-gen connect, the other direction from the brief's gen 0: generation 1's first frame,
    // delivered late after generation 2 was built, must not mark generation 2 Running.
    #[test]
    fn old_generation_connected_after_a_rebuild_is_ignored() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Error, &th);
        let built = next_build(&mut l, 1_000, &th);
        assert_eq!(l.generation(), 2);
        l.on_connected(built + 100, 1);
        assert!(
            matches!(l.state(), InputState::Connecting { .. }),
            "gen 1 is gone"
        );
        l.on_connected(built + 200, 2);
        assert_eq!(l.state(), InputState::Running { since: built + 200 });
    }

    // A generation the domain never built cannot be the current pipeline's.
    #[test]
    fn a_generation_not_yet_built_is_ignored() {
        let th = t();
        let mut l = InputLink::new();
        l.on_tick(0, &th);
        l.on_connected(10, 2);
        assert!(matches!(l.state(), InputState::Connecting { .. }));
        let mut l = running(0);
        l.on_lost(1_000, 2, LossReason::Eos, &th);
        assert_eq!(l.state(), InputState::Running { since: 0 });
        assert_eq!(l.kicks(1_000, &th), 0);
    }

    // The stall rule reads Running only: backing off after a loss, and connecting (which has its own
    // timeout), it changes nothing, however old the last frame.
    #[test]
    fn stall_check_while_backing_off_or_connecting_changes_nothing() {
        let th = t();
        let mut l = running(0);
        l.on_lost(1_000, 1, LossReason::Error, &th);
        let backing_off = l.state();
        l.check_stall(1_000 + th.input_stall_after_ms * 10, None, &th);
        assert_eq!(l.state(), backing_off);
        let built = next_build(&mut l, 1_000, &th);
        let connecting = l.state();
        l.check_stall(built + th.input_stall_after_ms, Some(0), &th);
        assert_eq!(l.state(), connecting, "the connect timeout, not the stall rule");
        assert_eq!(l.generation(), 2);
    }

    // After an interruption: three failed connects build up the back-off; a first frame resets it, so
    // the next loss waits the base again.
    #[test]
    fn a_first_frame_resets_the_backoff_to_its_base() {
        let th = t();
        let mut l = InputLink::new();
        let mut now = 0;
        for _ in 0..3 {
            now = next_build(&mut l, now, &th);
            let gen = l.generation();
            l.on_lost(now, gen, LossReason::Error, &th);
        }
        assert!(matches!(l.state(), InputState::Backoff { attempt: 3, .. }));
        now = next_build(&mut l, now, &th);
        l.on_connected(now, l.generation());
        l.on_lost(now + 1, l.generation(), LossReason::Error, &th);
        assert_eq!(
            l.state(),
            InputState::Backoff {
                attempt: 1,
                until: now + 1 + th.input_backoff_base_ms
            }
        );
    }

    #[test]
    fn the_kick_history_bound_leaves_room_for_the_alert() {
        let th = t();
        assert!(
            KICK_HISTORY_CAP > th.kick_alert_count,
            "{KICK_HISTORY_CAP} ≤ {}",
            th.kick_alert_count
        );
        // a kick storm: more kicks inside one window than the history holds
        let mut l = running(0);
        let mut now = 0;
        for _ in 0..KICK_HISTORY_CAP + 8 {
            now = kick_and_recover(&mut l, now + 100, &th);
        }
        assert!(now < th.kick_window_ms, "the storm fits one window ({now} ms)");
        assert_eq!(l.kicks(now, &th), KICK_HISTORY_CAP, "bounded");
        assert!(l.kick_alert(now, &th), "the alert holds through the storm");
    }

    // The clock is a u64 (time.rs): a back-off near its top saturates instead of overflowing.
    #[test]
    fn backoff_from_the_top_of_the_clock_range_saturates() {
        let th = t();
        let top = Ms::MAX - 10;
        let mut l = running(top);
        l.on_lost(top + 5, 1, LossReason::Error, &th);
        assert_eq!(
            l.state(),
            InputState::Backoff {
                attempt: 1,
                until: Ms::MAX
            }
        );
        assert!(
            built(l.on_tick(Ms::MAX, &th)),
            "the build still comes, at the top"
        );
        assert_eq!(l.on_tick(Ms::MAX, &th), None);
        assert!(matches!(l.state(), InputState::Connecting { attempt: 1, .. }));
    }

    // ---- C10b: the link's own give-ups (Owed 50) ----

    // A second call: once a generation is given up, the link backs off, and neither rule gives it up
    // again, however long nothing comes; the next tick that acts is the rebuild, for the next one.
    #[test]
    fn a_generation_is_given_up_once_by_either_rule() {
        let th = t();
        let mut checked = 0;
        // The connect timeout.
        let mut l = InputLink::new();
        l.on_tick(0, &th);
        let at = th.input_connect_timeout_ms;
        assert_eq!(l.on_tick(at, &th), Some(InputAction::Abandon { gen: 1 }));
        for later in [at, at + 1, at + th.input_backoff_base_ms - 1] {
            assert_eq!(l.on_tick(later, &th), None, "connect: tick at {later}");
            assert_eq!(l.check_stall(later, None, &th), None, "connect: stall at {later}");
            checked += 2;
        }
        assert_eq!(
            l.on_tick(at + th.input_backoff_base_ms, &th),
            Some(InputAction::Rebuild { gen: 2 })
        );
        // The stall rule.
        let mut l = running(0);
        let at = th.input_stall_after_ms;
        assert_eq!(
            l.check_stall(at, None, &th),
            Some(InputAction::Abandon { gen: 1 })
        );
        for later in [at, at + 1, at + th.input_backoff_base_ms - 1] {
            assert_eq!(l.check_stall(later, None, &th), None, "stall: stall at {later}");
            assert_eq!(l.on_tick(later, &th), None, "stall: tick at {later}");
            checked += 2;
        }
        assert_eq!(
            l.on_tick(at + th.input_backoff_base_ms, &th),
            Some(InputAction::Rebuild { gen: 2 })
        );
        assert_eq!(checked, 12);
    }

    // A loss the adapter reported (EOS, error, or its own stall report) is the adapter's decision: no
    // abandon is owed, from Running or from Connecting, at the loss or on any tick of the back-off.
    #[test]
    fn a_loss_the_adapter_reported_is_never_abandoned() {
        let th = t();
        let mut checked = 0;
        for reason in [LossReason::Eos, LossReason::Error, LossReason::Stalled] {
            for from_running in [true, false] {
                let mut l = if from_running {
                    running(0)
                } else {
                    InputLink::new()
                };
                if !from_running {
                    l.on_tick(0, &th);
                }
                l.on_lost(100, 1, reason, &th);
                let mut now = 100;
                loop {
                    let a = l.on_tick(now, &th).or(l.check_stall(now, None, &th));
                    match a {
                        Some(InputAction::Rebuild { gen: 2 }) => break,
                        None => {}
                        other => panic!("({reason:?}, running {from_running}) at {now}: {other:?}"),
                    }
                    now += 10;
                    assert!(now < 100 + th.input_backoff_cap_ms + 20, "no rebuild");
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 6);
    }

    // The two rules name the generation they gave up, never a later one: after rebuilds, the
    // abandon carries the current generation, and a rebuild in the same tick (a back-off of 0)
    // follows it with the next.
    #[test]
    fn an_abandon_names_the_generation_it_gave_up() {
        let th = Thresholds {
            input_backoff_base_ms: 0,
            ..t()
        };
        let mut l = InputLink::new();
        assert_eq!(l.on_tick(0, &th), Some(InputAction::Rebuild { gen: 1 }));
        let at = th.input_connect_timeout_ms;
        assert_eq!(l.on_tick(at, &th), Some(InputAction::Abandon { gen: 1 }));
        assert_eq!(l.on_tick(at, &th), Some(InputAction::Rebuild { gen: 2 }));
        l.on_connected(at, 2);
        let stall = at + th.input_stall_after_ms;
        assert_eq!(
            l.check_stall(stall, None, &th),
            Some(InputAction::Abandon { gen: 2 })
        );
        assert_eq!(l.on_tick(stall, &th), Some(InputAction::Rebuild { gen: 3 }));
    }

    // C10b: frames count only from the current generation while it is not given up or lost.
    #[test]
    fn frames_count_only_from_the_current_live_generation() {
        let th = t();
        let mut checked = 0;
        let mut l = InputLink::new();
        assert!(!l.takes_frames_from(0), "nothing built");
        assert!(!l.takes_frames_from(1), "nothing built");
        l.on_tick(0, &th);
        for (gen, want) in [(0, false), (1, true), (2, false)] {
            assert_eq!(l.takes_frames_from(gen), want, "connecting, gen {gen}");
            checked += 1;
        }
        l.on_connected(10, 1);
        for (gen, want) in [(0, false), (1, true), (2, false)] {
            assert_eq!(l.takes_frames_from(gen), want, "running, gen {gen}");
            checked += 1;
        }
        // Given up by the stall rule: none, the given-up generation included.
        assert!(l.check_stall(10 + th.input_stall_after_ms, None, &th).is_some());
        for gen in 0..=2 {
            assert!(!l.takes_frames_from(gen), "given up, gen {gen}");
            checked += 1;
        }
        // Rebuilt: the next generation's, never the given-up one's.
        let at = next_build(&mut l, 10 + th.input_stall_after_ms, &th);
        assert_eq!(l.generation(), 2, "rebuilt at {at}");
        for (gen, want) in [(1, false), (2, true), (3, false)] {
            assert_eq!(l.takes_frames_from(gen), want, "rebuilt, gen {gen}");
            checked += 1;
        }
        // Lost as the adapter reported it: none.
        l.on_lost(at + 1, 2, LossReason::Error, &th);
        assert!(!l.takes_frames_from(2), "lost");
        checked += 1;
        assert_eq!(checked, 13);
    }

    // ---- Owed 108 fix (a): Cloudflare's hold-end close (F-032) ----

    /// A build whose handshake completed and that carried nothing: what Cloudflare's close finds on a
    /// relay build attached to its held broadcast (F-032).
    const HELD: BuildFacts = BuildFacts {
        handshake_completed: true,
        carried_media: false,
    };

    /// F-032's drop: the camera's last frame 1.9 s after the kill at 0.
    const LAST: Ms = 1_900;

    /// Ticks `l` every 10 ms from `from` through `to`, reading the stall rule against `last` first, as
    /// the supervisor's Running tick does; returns the instants it built at.
    fn tick_through(l: &mut InputLink, from: Ms, to: Ms, last: Option<Ms>, th: &Thresholds) -> Vec<Ms> {
        let mut builds = Vec::new();
        let mut now = from;
        while now <= to {
            l.check_stall(now, last, th);
            if built(l.on_tick(now, th)) {
                builds.push(now);
            }
            now += 10;
        }
        builds
    }

    /// Running from 0 (so armed), the last frame at `LAST`, then the stall and the rebuilds that each
    /// time out, ticked through `to`.
    fn dropped(to: Ms, th: &Thresholds) -> InputLink {
        let mut l = running(0);
        tick_through(&mut l, 10, to, Some(LAST), th);
        l
    }

    /// The rule's outcome for an error at `at` on `l`: a base wait from `at` with the count reset
    /// (`backoff::delay(0)`), or a build already due sooner.
    fn fired_from(l: &InputLink, at: Ms, th: &Thresholds) -> InputState {
        let wait = delay(0, th.input_backoff_base_ms, th.input_backoff_cap_ms);
        let until = match l.state() {
            InputState::Backoff { until, .. } => until.min(at + wait),
            _ => at + wait,
        };
        InputState::Backoff { attempt: 1, until }
    }

    /// What today's link does with the same error: `on_lost(Error)`.
    fn today(l: &InputLink, at: Ms, th: &Thresholds) -> InputState {
        let mut twin = l.clone();
        twin.on_lost(at, twin.generation(), LossReason::Error, th);
        twin.state()
    }

    /// F-032's unlucky schedule: the close, 29 s after the last frame (the drop + 30.9), lands in the
    /// Backoff after the build from +23.9 was given up at +28.9, its next build due at +36.9.
    #[test]
    fn cloudflares_close_on_the_given_up_build_rebuilds_a_base_wait_later_with_the_count_reset() {
        let th = t();
        let close = LAST + 29_000;
        let mut l = dropped(close - 10, &th);
        let gen = l.generation();
        let InputState::Backoff { attempt, until } = l.state() else {
            panic!("setup: backing off at the close, {:?}", l.state());
        };
        assert!(
            attempt > 1 && until > close + th.input_backoff_base_ms,
            "setup: a long wait to cut"
        );
        let want = fired_from(&l, close, &th);
        assert_ne!(today(&l, close, &th), want, "setup: today's link waits it out");

        l.on_error(close, gen, HELD, Some(LAST), &th);
        assert_eq!(l.state(), want);
        assert_eq!(l.generation(), gen, "no build until the tick");
        let InputState::Backoff { until, .. } = want else {
            unreachable!()
        };
        assert_eq!(tick_through(&mut l, close, until, Some(LAST), &th), vec![until]);
        assert_eq!(
            l.state(),
            InputState::Connecting {
                attempt: 1,
                since: until
            }
        );
    }

    /// The close on the build being connected: the same rule, from the Connecting arm.
    #[test]
    fn cloudflares_close_on_the_build_being_connected_rebuilds_a_base_wait_later() {
        let th = t();
        let at = LAST + th.input_hold_end_after_ms + 3_100;
        let mut l = dropped(at - 10, &th);
        let gen = l.generation();
        assert!(
            matches!(l.state(), InputState::Connecting { attempt, .. } if attempt > 1),
            "setup: connecting a later attempt at {at}, {:?}",
            l.state()
        );
        let want = fired_from(&l, at, &th);
        assert_ne!(
            today(&l, at, &th),
            want,
            "setup: today's link waits its attempt's step"
        );
        l.on_error(at, gen, HELD, Some(LAST), &th);
        assert_eq!(l.state(), want);
    }

    /// Each error the rule must not read as the close, from Backoff and from Connecting: the outcome
    /// is today's `on_lost(Error)`, and in every case that differs from the rule's, so a deleted guard
    /// shows.
    #[test]
    fn an_error_the_rule_does_not_read_as_the_close_is_todays_loss() {
        let th = t();
        let backing_off = LAST + 29_000;
        let connecting = LAST + th.input_hold_end_after_ms + 3_100;
        let mut checked = 0;
        for at in [backing_off, connecting] {
            let base = dropped(at - 10, &th);
            assert!(
                !matches!(base.state(), InputState::Running { .. }),
                "setup at {at}"
            );
            let cases: [(&str, BuildFacts, Option<Ms>); 4] = [
                (
                    "no handshake: a refused connect",
                    BuildFacts::default(),
                    Some(LAST),
                ),
                (
                    "no handshake, with bytes",
                    BuildFacts {
                        handshake_completed: false,
                        carried_media: true,
                    },
                    Some(LAST),
                ),
                (
                    "the build carried media",
                    BuildFacts {
                        handshake_completed: true,
                        carried_media: true,
                    },
                    Some(LAST),
                ),
                ("no frame to measure from", HELD, None),
            ];
            for (name, facts, last) in cases {
                let mut l = base.clone();
                let (was, fired) = (today(&l, at, &th), fired_from(&l, at, &th));
                assert_ne!(was, fired, "setup: {name} at {at} is a witness");
                l.on_error(at, l.generation(), facts, last, &th);
                assert_eq!(l.state(), was, "{name} at {at}");
                checked += 1;
            }
        }
        assert_eq!(checked, 8, "cases checked: {checked}");
    }

    /// The 20 s gate's edge: 1 ms before it the error is today's (nothing, in Backoff); at it, the
    /// rule fires. Read in a Backoff long enough that the two outcomes differ at both instants.
    #[test]
    fn the_rule_fires_from_the_gate_and_not_1_ms_before() {
        let th = t();
        let at = LAST + th.input_hold_end_after_ms;
        let base = dropped(at - 10, &th);
        assert!(
            matches!(base.state(), InputState::Backoff { .. }),
            "setup: {:?}",
            base.state()
        );
        let gen = base.generation();

        let mut early = base.clone();
        assert_ne!(
            today(&early, at - 1, &th),
            fired_from(&early, at - 1, &th),
            "setup: a witness"
        );
        early.on_error(at - 1, gen, HELD, Some(LAST), &th);
        assert_eq!(early.state(), base.state(), "1 ms before the gate");

        let mut on = base.clone();
        on.on_error(at, gen, HELD, Some(LAST), &th);
        assert_eq!(on.state(), fired_from(&base, at, &th), "at the gate");
    }

    /// Once an outage: the build the rule made errors the same way, and that is today's loss, both
    /// while it connects and once it is given up.
    #[test]
    fn a_second_close_in_the_same_outage_is_todays_loss() {
        let th = t();
        let close = LAST + 29_000;
        let mut l = dropped(close - 10, &th);
        l.on_error(close, l.generation(), HELD, Some(LAST), &th);
        let InputState::Backoff { until, .. } = l.state() else {
            panic!("setup: fired, {:?}", l.state());
        };
        tick_through(&mut l, close, until, Some(LAST), &th);
        assert!(
            matches!(l.state(), InputState::Connecting { .. }),
            "setup: rebuilt"
        );

        let at = until + 1_000;
        let mut c = l.clone();
        let want = today(&c, at, &th);
        assert_ne!(want, fired_from(&c, at, &th), "setup: a witness");
        c.on_error(at, c.generation(), HELD, Some(LAST), &th);
        assert_eq!(c.state(), want, "connecting: the shot is spent");

        let to = until + th.input_connect_timeout_ms + 500;
        tick_through(&mut l, until + 10, to, Some(LAST), &th);
        let parked = l.state();
        assert!(matches!(parked, InputState::Backoff { .. }), "setup: {parked:?}");
        assert_ne!(parked, fired_from(&l, to, &th), "setup: a witness");
        l.on_error(to, l.generation(), HELD, Some(LAST), &th);
        assert_eq!(l.state(), parked, "given up: the shot is spent");
    }

    /// Disarmed until the link's first Running: a session started into an idle input (F-032 finding
    /// 6's no-publisher errors) keeps today's back-off, with no frame or with a frame stamp to measure
    /// from (the arm alone).
    #[test]
    fn before_the_first_running_the_rule_never_fires() {
        let th = t();
        let mut checked = 0;
        for last in [None, Some(0)] {
            let mut l = InputLink::new();
            let mut now = 0;
            let mut errors = 0;
            // Each build past the gate errors 100 ms in.
            while now <= 4 * th.input_hold_end_after_ms {
                if built(l.on_tick(now, &th)) && now >= th.input_hold_end_after_ms {
                    let at = now + 100;
                    let want = today(&l, at, &th);
                    assert_ne!(want, fired_from(&l, at, &th), "setup: a witness at {at}");
                    l.on_error(at, l.generation(), HELD, last, &th);
                    assert_eq!(l.state(), want, "never Running, last frame {last:?}, at {at}");
                    errors += 1;
                    now = at;
                }
                now += 10;
            }
            assert!(errors >= 3, "errors checked: {errors}");
            checked += errors;
        }
        assert!(checked >= 6, "errors checked: {checked}");
    }

    /// An error while Running is today's loss and leaves the shot armed: the builds that follow never
    /// reach Running, and the close later in that outage still fires. An error from another generation
    /// changes nothing.
    #[test]
    fn a_running_loss_and_another_generation_leave_the_shot_alone() {
        let th = t();
        let mut l = running(0);
        let at = LAST + th.input_hold_end_after_ms;
        let want = today(&l, at, &th);
        l.on_error(at, 1, HELD, Some(LAST), &th);
        assert_eq!(l.state(), want, "a Running build's error is a loss");

        let close = at + 14_100;
        tick_through(&mut l, at + 10, close - 10, Some(LAST), &th);
        assert!(
            matches!(l.state(), InputState::Backoff { attempt, .. } if attempt > 1),
            "setup: {:?}",
            l.state()
        );
        let parked = l.clone();
        for gen in [l.generation() - 1, l.generation() + 1] {
            l.on_error(close, gen, HELD, Some(LAST), &th);
            assert_eq!(l, parked, "gen {gen}");
        }
        assert_ne!(
            today(&l, close, &th),
            fired_from(&l, close, &th),
            "setup: a witness"
        );
        l.on_error(close, l.generation(), HELD, Some(LAST), &th);
        assert_eq!(l.state(), fired_from(&parked, close, &th), "still armed");
    }

    /// The rule never delays a build: a close in the last base wait of a back-off keeps its due
    /// instant, the count reset.
    #[test]
    fn the_rule_never_delays_a_build_already_due() {
        let th = t();
        let l0 = dropped(LAST + 29_000 - 10, &th);
        let InputState::Backoff { until, .. } = l0.state() else {
            panic!("setup: {:?}", l0.state())
        };
        let at = until - th.input_backoff_base_ms / 2;
        let mut l = dropped(at - 10, &th);
        assert_eq!(l.state(), l0.state(), "setup: the same back-off");
        l.on_error(at, l.generation(), HELD, Some(LAST), &th);
        assert_eq!(l.state(), InputState::Backoff { attempt: 1, until });
    }

    /// Re-armed only by Running: Running, a drop, the close fires, the phone back (Running), the next
    /// drop, and its close fires again, three outages over.
    #[test]
    fn running_rearms_the_rule_for_the_next_outage() {
        let th = t();
        let mut l = running(0);
        let mut last = LAST;
        let mut fired = 0;
        for outage in 0..3 {
            let close = last + 29_000;
            tick_through(&mut l, last + 10, close - 10, Some(last), &th);
            let want = fired_from(&l, close, &th);
            assert_ne!(today(&l, close, &th), want, "setup: outage {outage} is a witness");
            l.on_error(close, l.generation(), HELD, Some(last), &th);
            assert_eq!(l.state(), want, "outage {outage}'s close");
            fired += 1;
            // The phone is back: the rule's build delivers its first frame.
            let InputState::Backoff { until, .. } = l.state() else {
                unreachable!()
            };
            assert!(built(l.on_tick(until, &th)));
            l.on_connected(until + 500, l.generation());
            assert!(matches!(l.state(), InputState::Running { .. }));
            last = until + 2_000;
        }
        assert_eq!(fired, 3, "outages checked: {fired}");
    }

    /// A phone that never returns: after the rule's one rebuild each build times out, and the builds
    /// re-run the back-off's steps from the base. Derived from `backoff::delay` and the connect
    /// timeout, then held against F-032's own schedule (the drop + 31.9, 38.9, 47.9, 60.9, 75.9;
    /// today's +37, +52, +67, +82). The bound over many horizons is swept in `tests/hold_end.rs`.
    #[test]
    fn a_phone_that_never_returns_gets_the_backoffs_steps_once_more() {
        let th = t();
        let (base, cap, timeout) = (
            th.input_backoff_base_ms,
            th.input_backoff_cap_ms,
            th.input_connect_timeout_ms,
        );
        let close = LAST + 29_000;
        let horizon = close + 60_000;
        let mut without = dropped(close - 10, &th);
        let mut with = without.clone();
        // Today's: the Backoff the close found runs out, then each step from its count.
        let InputState::Backoff { attempt, until } = without.state() else {
            panic!("setup: {:?}", without.state())
        };
        let mut want_today = vec![until];
        for k in attempt.. {
            let next = want_today[want_today.len() - 1] + timeout + delay(k, base, cap);
            if next > horizon {
                break;
            }
            want_today.push(next);
        }
        let mut want_with = vec![close + delay(0, base, cap)];
        for k in 1.. {
            let next = want_with[want_with.len() - 1] + timeout + delay(k, base, cap);
            if next > horizon {
                break;
            }
            want_with.push(next);
        }
        assert_eq!(
            tick_through(&mut without, close, horizon, Some(LAST), &th),
            want_today
        );
        with.on_error(close, with.generation(), HELD, Some(LAST), &th);
        assert_eq!(
            tick_through(&mut with, close, horizon, Some(LAST), &th),
            want_with
        );
        // F-032's numbers, the finding's own.
        assert_eq!(&want_with[..5], &[31_900, 38_900, 47_900, 60_900, 75_900]);
        let rounded: Vec<Ms> = want_today.iter().take(4).map(|b| (b + 100) / 1_000).collect();
        assert_eq!(rounded, vec![37, 52, 67, 82]);
        // Both then run a capped step apart.
        let last_gap = |b: &[Ms]| b.windows(2).last().map(|w| w[1] - w[0]);
        assert_eq!(last_gap(&want_with), Some(timeout + cap));
        assert_eq!(last_gap(&want_today), Some(timeout + cap));
    }
}
