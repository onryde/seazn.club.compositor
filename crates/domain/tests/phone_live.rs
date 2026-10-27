//! Owed 108 fix (b), F-032: the phone-live nudge, through the supervisor. Web counts each time the
//! session's phone comes back live after an outage and serves the counter in every heartbeat reply
//! (`phoneLiveEdge`); C08 hands each value to the domain as `Event::PhoneLiveEdge`. The first value a
//! process sees is a baseline; each later increase is one nudge: a back-off builds now with its
//! attempt count reset, a connect with no media yet is discarded and built again now (once an outage),
//! and a camera still on air, or a connect already carrying bytes, is kept until it fails.
//!
//! Every expected instant is derived from `Thresholds`, `backoff::delay` and the tick grid (an effect
//! comes on the first tick at or after the instant its rule names), swept over four tick periods, and
//! each nudged run is held against the same run without the nudge ("today"). These sweeps live here,
//! not inline, because the domain's crate root forbids `clippy::print_stderr`, and a sweep prints its
//! count.
use domain::backoff::delay;
use domain::config::Thresholds;
use domain::input_link::BuildFacts;
use domain::lifecycle::Lifecycle;
use domain::supervisor::{Desired, Effect, Event, Supervisor};
use domain::time::Ms;

/// The tick periods swept: 1 ms lands on each rule's instant exactly, 33 ms is C08's frame period,
/// and the coarse two put the rule's instant between ticks.
const STEPS: [Ms; 4] = [1, 33, 250, 1_000];

const FAR: Ms = 10_000_000;

/// F-032's drop: the camera's last frame 1.9 s after the kill at 0.
const LAST: Ms = 1_900;

/// What Cloudflare's close finds on a relay build attached to its held broadcast (fix (a)).
const HELD: BuildFacts = BuildFacts {
    handshake_completed: true,
    carried_media: false,
};

fn t() -> Thresholds {
    Thresholds::default()
}

/// The first tick at or after `at` on a grid of `step` from 0.
fn first_tick(at: Ms, step: Ms) -> Ms {
    at.div_ceil(step) * step
}

fn edge(count: u64) -> Event {
    Event::PhoneLiveEdge { count }
}

/// The supervisor driven on a tick grid from a boot at 0, recording every input build and abandon.
#[derive(Debug, Clone)]
struct Run {
    s: Supervisor,
    /// The next tick's instant: events sent now arrive just before it.
    now: Ms,
    step: Ms,
    builds: Vec<(Ms, u64)>,
    abandons: Vec<(Ms, u64)>,
    /// The instant of the last camera frame sent.
    last_frame: Option<Ms>,
}

impl Run {
    fn booted(step: Ms) -> Self {
        let th = t();
        let s = Supervisor::new(th.clone()).decide(Event::Tick, 0).next;
        let d = s.decide(
            Event::BootstrapOk {
                heartbeat_interval_ms: th.heartbeat_interval_ms,
                deadline_at: FAR,
            },
            0,
        );
        assert_eq!(d.next.lifecycle(), Lifecycle::Running, "setup: booted");
        Self {
            s: d.next,
            now: 0,
            step,
            builds: Vec::new(),
            abandons: Vec::new(),
            last_frame: None,
        }
    }

    /// Gen 1 is built on the first tick, connects on the next, and delivers a frame on every tick
    /// through `last`; `events` go in at the first tick at or after each instant, before its frame.
    /// Stops at the first tick after `last`, not yet decided.
    fn on_air_with(step: Ms, last: Ms, events: &[(Ms, Event)]) -> Self {
        let mut r = Self::booted(step);
        let mut sent = 0;
        while r.now <= last {
            while let Some((at, e)) = events.get(sent) {
                if r.now < *at {
                    break;
                }
                r.send(e.clone());
                sent += 1;
            }
            if r.now > 0 {
                r.send(Event::InputConnected { gen: 1 });
                r.send(Event::InputFrame { gen: 1 });
                r.last_frame = Some(r.now);
            }
            r.tick();
        }
        assert_eq!(sent, events.len(), "setup: every event went in");
        assert_eq!(r.builds.first(), Some(&(0, 1)), "setup: gen 1 at once");
        r
    }

    /// `on_air_with`, the counter's baseline (`base`) sent on the camera's first frame.
    fn on_air(step: Ms, base: u64) -> Self {
        Self::on_air_with(step, LAST, &[(step, edge(base))])
    }

    fn send(&mut self, e: Event) {
        let d = self.s.decide(e, self.now);
        for fx in &d.effects {
            match *fx {
                Effect::RebuildInput { gen } => self.builds.push((self.now, gen)),
                Effect::AbandonInput { gen } => self.abandons.push((self.now, gen)),
                _ => {}
            }
        }
        self.s = d.next;
    }

    fn tick(&mut self) {
        self.send(Event::Tick);
        self.now += self.step;
    }

    /// A bound on the next build or abandon from `now`: a connect timeout, the capped back-off and
    /// two ticks. A run past it fails instead of hanging the suite.
    fn horizon(&self) -> Ms {
        let th = t();
        self.now + th.input_connect_timeout_ms + th.input_backoff_cap_ms + 2 * self.step
    }

    /// Ticks with no event until build number `n` (gen 1 is the first) is made; returns its instant.
    /// `now` is then the tick after it.
    fn idle_until_build(&mut self, n: usize) -> Ms {
        while self.builds.len() < n {
            let before = self.builds.len();
            let bound = self.horizon();
            while self.builds.len() == before {
                assert!(self.now <= bound, "no build after {before}: {:?}", self.builds);
                self.tick();
            }
        }
        self.builds[n - 1].0
    }

    /// Ticks with no event until abandon number `n` is made; returns its instant and generation.
    fn idle_until_abandon(&mut self, n: usize) -> (Ms, u64) {
        while self.abandons.len() < n {
            let before = self.abandons.len();
            let bound = self.horizon();
            while self.abandons.len() == before {
                assert!(
                    self.now <= bound,
                    "no abandon after {before}: {:?}",
                    self.abandons
                );
                self.tick();
            }
        }
        self.abandons[n - 1]
    }

    /// Ticks with no event until the first tick at or after `at` is due (not yet decided).
    fn idle_until(&mut self, at: Ms) {
        while self.now < at {
            self.tick();
        }
    }

    fn gen(&self) -> u64 {
        self.s.input_generation()
    }

    fn builds_from(&self, from: Ms) -> Vec<(Ms, u64)> {
        self.builds
            .iter()
            .copied()
            .filter(|&(at, _)| at >= from)
            .collect()
    }

    fn abandons_from(&self, from: Ms) -> Vec<(Ms, u64)> {
        self.abandons
            .iter()
            .copied()
            .filter(|&(at, _)| at >= from)
            .collect()
    }
}

/// The back-off after the stall: gen 1's stall is failure 1, and each timed-out connect after it
/// one more, so the wait before gen `g + 1` (g ≥ 2) is `delay(g - 1)`.
fn wait_after_gen(g: u64, th: &Thresholds) -> Ms {
    delay((g - 1) as u32, th.input_backoff_base_ms, th.input_backoff_cap_ms)
}

/// The count reset: the nudge's build is failure 1's retry, so when its connect times out the next
/// build waits `delay(1)`, as the connect after a stall's does.
fn reset_wait(th: &Thresholds) -> Ms {
    delay(1, th.input_backoff_base_ms, th.input_backoff_cap_ms)
}

/// A first value is a baseline, never an edge: 0 (the phone's first live) and 2 (a process that boots
/// or restarts in place after two edges). Sent in a long back-off, then repeated, it moves no build
/// and no abandon: the run is today's.
#[test]
fn a_first_value_is_a_baseline_and_a_repeat_is_not_an_edge() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        for first in [0, 2] {
            let mut today = Run::on_air_with(step, LAST, &[]);
            let mut with = today.clone();
            // Gen 4's connect timed out: the back-off before gen 5 is delay(3), longer than a tick.
            let (gave_up, gen) = today.idle_until_abandon(4);
            assert_eq!(with.idle_until_abandon(4), (gave_up, gen));
            assert!(wait_after_gen(gen, &th) > 2 * step, "setup: a long back-off");
            with.send(edge(first));
            with.tick();
            with.send(edge(first));
            today.tick();
            let until = gave_up + 3 * (th.input_connect_timeout_ms + th.input_backoff_cap_ms);
            today.idle_until(until);
            with.idle_until(until);
            assert_eq!(with.builds, today.builds, "step {step}, first {first}");
            assert_eq!(with.abandons, today.abandons, "step {step}, first {first}");
            let next = today.builds_from(gave_up)[0];
            assert_eq!(
                next,
                (first_tick(gave_up + wait_after_gen(gen, &th), step), gen + 1)
            );
            checked += 1;
        }
    }
    eprintln!("baselines checked: {checked}");
    assert_eq!(checked, STEPS.len() * 2);
}

/// An increase in a long back-off builds on the very next tick, and its count is reset: when that
/// connect times out, the next build waits `delay(1)`, not the `delay(4)` today's fifth failure would.
#[test]
fn an_increase_in_backoff_builds_now_with_the_count_reset() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let mut with = Run::on_air(step, 3);
        let mut today = with.clone();
        let (gave_up, gen) = with.idle_until_abandon(4);
        assert_eq!(today.idle_until_abandon(4), (gave_up, gen));
        let nudged = with.now;
        assert!(
            nudged < first_tick(gave_up + wait_after_gen(gen, &th), step),
            "setup: inside the back-off"
        );
        with.send(edge(4));
        with.tick();
        assert_eq!(
            with.builds.last(),
            Some(&(nudged, gen + 1)),
            "step {step}: built now"
        );
        // The nudge's build times out on its own connect timeout, and the next waits the reset step.
        let (out, g) = with.idle_until_abandon(5);
        assert_eq!(
            (out, g),
            (first_tick(nudged + th.input_connect_timeout_ms, step), gen + 1)
        );
        with.idle_until_build(gen as usize + 2);
        let after = with.builds_from(out)[0];
        assert_eq!(
            after,
            (first_tick(out + reset_wait(&th), step), gen + 2),
            "step {step}"
        );
        // Today: the same connects, each a failure more, so the waits keep growing.
        let (t_out, t_gen) = today.idle_until_abandon(5);
        today.idle_until_build(t_gen as usize + 1);
        let t_after = today.builds_from(t_out)[0].0;
        assert_eq!(t_after, first_tick(t_out + wait_after_gen(t_gen, &th), step));
        assert_ne!(
            reset_wait(&th),
            wait_after_gen(t_gen, &th),
            "setup: the reset is visible"
        );
        checked += 1;
    }
    eprintln!("back-off nudges checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// An increase while the camera is still on air tears nothing down: no abandon and no build while
/// frames keep coming, for more than two stall periods after it. The nudge stays latched and acts at
/// the stall: the stall's tick gives gen 1 up AND builds gen 2, where today builds a base wait later.
#[test]
fn an_increase_while_running_with_fresh_frames_acts_only_at_the_stall() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let nudge_at = 2_000;
        let last = nudge_at + 3 * th.input_stall_after_ms;
        let mut runs = Vec::new();
        for nudged in [true, false] {
            let mut events = vec![(1_000, edge(7))];
            if nudged {
                events.push((nudge_at, edge(8)));
            }
            let mut r = Run::on_air_with(step, last, &events);
            assert_eq!(r.builds, vec![(0, 1)], "step {step}: nothing rebuilt while fresh");
            assert!(r.abandons.is_empty(), "step {step}: nothing given up while fresh");
            let frame = r.last_frame.expect("setup: frames");
            let stall = first_tick(frame + th.input_stall_after_ms, step);
            r.idle_until_build(2);
            assert_eq!(r.abandons[0], (stall, 1), "step {step}: the stall");
            runs.push((r.builds[1], stall));
        }
        let ((with, stall), (today, _)) = (runs[0], runs[1]);
        assert_eq!(
            with,
            (stall, 2),
            "step {step}: the latched nudge builds on the stall's tick"
        );
        assert_eq!(today, (first_tick(stall + th.input_backoff_base_ms, step), 2));
        checked += 1;
    }
    eprintln!("running latches checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// An increase during a connect with no media yet gives that connect up and builds the next one on
/// the same tick. Once: the new connect, also without media, is kept to its own connect timeout, and
/// the build after it waits the reset step.
#[test]
fn an_increase_during_a_connect_with_no_media_discards_it_once() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let mut r = Run::on_air(step, 0);
        let built = r.idle_until_build(3);
        let at = r.now;
        assert!(
            at < built + th.input_connect_timeout_ms,
            "setup: still connecting"
        );
        r.send(edge(1));
        r.tick();
        assert_eq!(r.abandons_from(at), vec![(at, 3)], "step {step}: the discard");
        assert_eq!(
            r.builds_from(at),
            vec![(at, 4)],
            "step {step}: rebuilt on the same tick"
        );
        let out = r.idle_until_abandon(r.abandons.len() + 1);
        assert_eq!(
            out,
            (first_tick(at + th.input_connect_timeout_ms, step), 4),
            "step {step}"
        );
        r.idle_until_build(5);
        assert_eq!(r.builds[4], (first_tick(out.0 + reset_wait(&th), step), 5));
        checked += 1;
    }
    eprintln!("no-media discards checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// A connect that has carried bytes is kept: no abandon until its own connect timeout, and the nudge
/// acts there, building on the next tick with the count reset, where today waits the back-off. The
/// build after it starts with no media of its own, so a second increase during it discards it (the
/// outage's discard is still armed). If the kept connect decodes instead, the camera is back and the
/// nudge is cleared: the next stall waits today's base step.
#[test]
fn an_increase_during_a_connect_that_carries_media_keeps_it() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let mut ends = Vec::new();
        for nudged in [true, false] {
            let mut r = Run::on_air(step, 0);
            let built = r.idle_until_build(3);
            let at = r.now;
            r.send(Event::InputMedia { gen: 3 });
            if nudged {
                r.send(edge(1));
            }
            r.tick();
            let out = r.idle_until_abandon(r.abandons.len() + 1);
            assert_eq!(
                out,
                (first_tick(built + th.input_connect_timeout_ms, step), 3),
                "kept"
            );
            assert!(
                r.builds_from(at).is_empty(),
                "step {step}: nothing built before it"
            );
            r.idle_until_build(4);
            ends.push((out.0, r.builds[3]));
            if nudged {
                // The nudge's build, left alone, times out and waits the reset step.
                let mut alone = r.clone();
                let gave_up = alone.idle_until_abandon(alone.abandons.len() + 1);
                assert_eq!(
                    gave_up,
                    (first_tick(out.0 + step + th.input_connect_timeout_ms, step), 4)
                );
                alone.idle_until_build(5);
                assert_eq!(
                    alone.builds[4],
                    (first_tick(gave_up.0 + reset_wait(&th), step), 5)
                );
                // A second increase during it: no byte of its own yet, so it is discarded.
                let second = r.now;
                r.send(edge(2));
                r.tick();
                assert_eq!(r.abandons_from(second), vec![(second, 4)], "step {step}: gen 4");
                assert_eq!(r.builds.last(), Some(&(second, 5)));
            }
        }
        let ((out, with), (_, today)) = (ends[0], ends[1]);
        assert_eq!(
            with,
            (out + step, 4),
            "step {step}: the nudge acts at the timeout"
        );
        assert_eq!(today, (first_tick(out + wait_after_gen(3, &th), step), 4));

        // The kept connect decodes: Running clears the nudge, so the next stall waits the base step.
        let mut r = Run::on_air(step, 0);
        r.idle_until_build(3);
        r.send(Event::InputMedia { gen: 3 });
        r.send(edge(1));
        r.tick();
        let up = r.now;
        while r.now < up + 2 * th.input_stall_after_ms {
            r.send(Event::InputConnected { gen: 3 });
            r.send(Event::InputFrame { gen: 3 });
            r.last_frame = Some(r.now);
            r.tick();
        }
        assert_eq!(r.builds.len(), 3, "step {step}: on air, nothing rebuilt");
        let stall = first_tick(r.last_frame.unwrap() + th.input_stall_after_ms, step);
        r.idle_until_build(4);
        assert_eq!(r.abandons.last(), Some(&(stall, 3)));
        assert_eq!(
            r.builds[3],
            (first_tick(stall + th.input_backoff_base_ms, step), 4)
        );
        checked += 1;
    }
    eprintln!("media connects checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// Two increases in one outage discard one connect: the first gives gen 3 up, the second, during
/// gen 4's connect (no media either), keeps it to its connect timeout and acts there.
#[test]
fn two_increases_in_one_outage_give_one_discard() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let mut r = Run::on_air(step, 0);
        r.idle_until_build(3);
        let first = r.now;
        r.send(edge(1));
        r.tick();
        assert_eq!(
            r.builds.last(),
            Some(&(first, 4)),
            "setup: the first discards gen 3"
        );
        let second = r.now;
        r.send(edge(2));
        r.tick();
        let out = r.idle_until_abandon(r.abandons.len() + 1);
        assert_eq!(
            out,
            (first_tick(first + th.input_connect_timeout_ms, step), 4),
            "kept"
        );
        let discards = r.abandons_from(second).len() - 1;
        r.idle_until_build(5);
        assert_eq!(
            r.builds[4],
            (out.0 + step, 5),
            "step {step}: the second acts at the timeout"
        );
        assert_eq!(discards, 0, "step {step}: one discard an outage");
        checked += 1;
    }
    eprintln!("outages with two increases checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// Several increases read in one reply are one nudge: a jump from 3 to 6 is the run a step to 4 is.
#[test]
fn several_increases_in_one_reply_are_one_nudge() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let mut runs = Vec::new();
        for next in [4, 6] {
            let mut r = Run::on_air(step, 3);
            r.idle_until_build(3);
            r.send(edge(next));
            r.tick();
            let end = r.now + 3 * (th.input_connect_timeout_ms + th.input_backoff_cap_ms);
            r.idle_until(end);
            runs.push((r.builds, r.abandons));
        }
        assert_eq!(runs[0], runs[1], "step {step}");
        checked += 1;
    }
    eprintln!("jumps checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// Web's counter never goes down, so a lower value is not an edge: the run is today's. The next value
/// is read against it, so a rise from there is.
#[test]
fn a_lower_value_is_no_edge_and_the_next_is_read_against_it() {
    let mut checked = 0;
    for step in STEPS {
        let mut with = Run::on_air(step, 5);
        let mut today = with.clone();
        with.idle_until_build(3);
        today.idle_until_build(3);
        let lower = with.now;
        with.send(edge(3));
        with.tick();
        today.tick();
        assert_eq!(
            with.builds, today.builds,
            "step {step}: a lower value moved a build"
        );
        assert_eq!(with.abandons, today.abandons, "step {step}");
        let built = with.idle_until_build(4);
        assert!(built > lower, "setup");
        let at = with.now;
        with.send(edge(4));
        with.tick();
        assert_eq!(
            with.abandons_from(at),
            vec![(at, 4)],
            "step {step}: 4 rises from 3"
        );
        checked += 1;
    }
    eprintln!("lower values checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// Fix (a) and fix (b) in one outage build once. Cloudflare's close in a back-off makes a build due a
/// base wait later; a nudge in the same back-off, before or after the close on the same tick, moves
/// that one build to now, and none follows a base wait later. A close on the build a nudge discarded
/// (that generation is past) moves nothing.
#[test]
fn a_nudge_and_cloudflares_close_in_one_outage_build_once() {
    let th = t();
    let close_at = LAST + 29_000;
    let mut checked = 0;
    for step in STEPS {
        for nudge_first in [true, false] {
            let mut r = Run::on_air(step, 0);
            r.idle_until(close_at);
            let sent = r.now;
            let gen = r.gen();
            let close = Event::InputError { gen, facts: HELD };
            if nudge_first {
                r.send(edge(1));
                r.send(close);
            } else {
                r.send(close);
                r.send(edge(1));
            }
            r.tick();
            r.idle_until(sent + th.input_connect_timeout_ms);
            assert_eq!(
                r.builds_from(sent),
                vec![(sent, gen + 1)],
                "step {step}, {nudge_first}"
            );
            checked += 1;
        }
        // The close lands on the build the nudge gave up: that generation is past, so it moves nothing.
        let mut with = Run::on_air(step, 0);
        with.idle_until(LAST + th.input_hold_end_after_ms);
        let before = with.builds.len();
        with.idle_until_build(before + 1);
        let at = with.now;
        let discarded = with.gen();
        with.send(edge(1));
        with.tick();
        assert_eq!(
            with.abandons_from(at),
            vec![(at, discarded)],
            "setup: the discard"
        );
        let mut today = with.clone();
        with.send(Event::InputError {
            gen: discarded,
            facts: HELD,
        });
        let end = with.now + th.input_connect_timeout_ms + th.input_backoff_cap_ms;
        with.idle_until(end);
        today.idle_until(end);
        assert_eq!(
            with.builds, today.builds,
            "step {step}: the close on a past build"
        );
        checked += 1;
    }
    eprintln!("nudge and close pairs checked: {checked}");
    assert_eq!(checked, STEPS.len() * 3);
}

/// The sequence: an outage, a nudge (a discard), the camera back on air, a second outage, a second
/// nudge. Running re-arms the discard, so the second outage gets its own.
#[test]
fn outage_nudge_running_second_outage_nudge() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let mut r = Run::on_air(step, 0);
        r.idle_until_build(3);
        let first = r.now;
        r.send(edge(1));
        r.tick();
        assert_eq!(
            r.abandons_from(first),
            vec![(first, 3)],
            "step {step}: the first discard"
        );
        let up = r.now;
        while r.now < up + 2 * th.input_stall_after_ms {
            r.send(Event::InputConnected { gen: 4 });
            r.send(Event::InputFrame { gen: 4 });
            r.last_frame = Some(r.now);
            r.tick();
        }
        assert_eq!(r.builds.len(), 4, "step {step}: gen 4 on air");
        let stall = first_tick(r.last_frame.unwrap() + th.input_stall_after_ms, step);
        let built = r.idle_until_build(5);
        assert_eq!(
            built,
            first_tick(stall + th.input_backoff_base_ms, step),
            "the second outage"
        );
        let second = r.now;
        r.send(edge(2));
        r.tick();
        assert_eq!(
            r.abandons_from(second),
            vec![(second, 5)],
            "step {step}: the second discard"
        );
        assert_eq!(r.builds.last(), Some(&(second, 6)));
        checked += 1;
    }
    eprintln!("sequences checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// A process that has never had a camera (booted, or restarted in place, while the phone was away)
/// still acts on an increase after its baseline: web's word that the phone is back does not wait for
/// a first frame here. The pair, a baseline alone, keeps today's connects.
#[test]
fn a_process_with_no_camera_yet_acts_on_an_increase() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let mut today = Run::booted(step);
        today.tick();
        today.send(edge(4));
        let mut with = today.clone();
        let built = today.idle_until_build(2);
        assert_eq!(with.idle_until_build(2), built);
        let at = with.now;
        assert!(
            at < built + th.input_connect_timeout_ms,
            "setup: still connecting"
        );
        with.send(edge(5));
        with.tick();
        today.tick();
        assert_eq!(with.abandons_from(at), vec![(at, 2)], "step {step}: discarded");
        assert_eq!(with.builds.last(), Some(&(at, 3)));
        assert!(today.abandons_from(at).is_empty(), "step {step}: today keeps it");
        checked += 1;
    }
    eprintln!("cameraless processes checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// The hold ticks no link (C06): an increase in the final hold builds nothing.
#[test]
fn the_hold_builds_nothing_on_an_increase() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let mut r = Run::on_air(step, 0);
        r.idle_until_build(3);
        r.send(Event::HeartbeatReply(Desired::Ending));
        r.send(edge(1));
        let before = (r.builds.len(), r.abandons.len());
        r.idle_until(r.now + th.final_hold_ms / 2);
        assert!(
            matches!(r.s.lifecycle(), Lifecycle::Holding { .. }),
            "setup: in the hold"
        );
        assert_eq!((r.builds.len(), r.abandons.len()), before, "step {step}");
        checked += 1;
    }
    eprintln!("holds checked: {checked}");
    assert_eq!(checked, STEPS.len());
}
