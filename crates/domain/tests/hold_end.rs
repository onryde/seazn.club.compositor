//! Owed 108 fix (a), F-032: Cloudflare's hold-end close, through the supervisor. When the phone drops,
//! Cloudflare holds its SRT broadcast about 30 s and then closes every attached reader with an error;
//! the adapter reports it as `Event::InputError` with the build's `BuildFacts`, and the input link
//! rebuilds a base wait later with its attempt count reset, once an outage. Every expected instant is
//! derived from `Thresholds` and the tick grid (an effect comes on the first tick at or after the
//! instant its rule names), swept over four tick periods. These sweeps live here, not inline,
//! because the domain's crate root forbids `clippy::print_stderr`, and a sweep prints its count.
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

/// F-032's drop: the camera's last frame 1.9 s after the kill at 0, and Cloudflare's close 29 s
/// after that (the drop + 30.9; the finding's closes came 27.9 to 29.2 s after the last frame).
const LAST: Ms = 1_900;
const CLOSE: Ms = LAST + 29_000;

/// What Cloudflare's close finds on a relay build attached to its held broadcast.
const HELD: BuildFacts = BuildFacts {
    handshake_completed: true,
    carried_media: false,
};

/// A refused connect (a rotated passphrase, a deleted input): no handshake. Today's behaviour.
const REFUSED: BuildFacts = BuildFacts {
    handshake_completed: false,
    carried_media: false,
};

fn t() -> Thresholds {
    Thresholds::default()
}

/// The first tick at or after `at` on a grid of `step` from 0.
fn first_tick(at: Ms, step: Ms) -> Ms {
    at.div_ceil(step) * step
}

/// How the builds after the close end, for a phone that never returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    /// Each connect times out (F-032's schedule).
    TimesOut,
    /// Each build errors 100 ms in with `facts` (F-032 finding 6: an input with no publisher errors
    /// its readers too).
    Errors,
}

/// The supervisor driven on a tick grid: booted at 0, gen 1 connects and delivers frames through
/// `LAST`, then nothing. At the first tick at or after `close` the adapter reports an error on the
/// current generation with `facts`; with `After::Errors`, every build after it errors 100 ms in with
/// the same facts. Returns every `RebuildInput`, and whether any effect but a build or an abandon
/// came.
struct Run {
    s: Supervisor,
    now: Ms,
    step: Ms,
    builds: Vec<(Ms, u64)>,
    abandons: Vec<(Ms, u64)>,
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
        }
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

    /// The drop, the close at `close` with `facts`, then `after`, through `until`. Returns the
    /// instant the close was sent.
    fn outage(&mut self, close: Ms, facts: BuildFacts, after: After, until: Ms) -> Ms {
        self.drive(LAST, &[(close, facts)], after, until)[0]
    }

    /// Gen 1 on air until `last`, then each of `errors` on the current generation at the first tick
    /// at or after its instant, then `after` with the last one's facts, through `until`. Returns the
    /// instants the errors were sent.
    fn drive(&mut self, last: Ms, errors: &[(Ms, BuildFacts)], after: After, until: Ms) -> Vec<Ms> {
        let mut sent = Vec::new();
        let mut errored = 0;
        while self.now <= until {
            let gen = self.s.input_generation();
            if gen == 1 && self.now > 0 && self.now <= last {
                self.send(Event::InputConnected { gen: 1 });
                self.send(Event::InputFrame { gen: 1 });
            }
            if let Some(&(at, facts)) = errors.get(sent.len()) {
                if self.now >= at {
                    self.send(Event::InputError { gen, facts });
                    sent.push(self.now);
                    errored = gen;
                }
            }
            let done = sent.len() == errors.len();
            if let (true, After::Errors, Some(&(at, g))) = (done, after, self.builds.last()) {
                if g > errored && self.now >= at + 100 {
                    let facts = errors.last().expect("setup: an error").1;
                    self.send(Event::InputError { gen: g, facts });
                    errored = g;
                }
            }
            self.send(Event::Tick);
            self.now += self.step;
        }
        assert_eq!(sent.len(), errors.len(), "setup: every error was sent");
        sent
    }

    /// The builds at or after `from`.
    fn builds_from(&self, from: Ms) -> Vec<Ms> {
        self.builds
            .iter()
            .filter(|&&(at, _)| at >= from)
            .map(|&(at, _)| at)
            .collect()
    }
}

/// The close rebuilds on the first tick at or after a base wait from it, a fresh generation; today's
/// link (the same error, refused-shaped) waits its Backoff out, later. The close sends no abandon: the
/// adapter decided it.
#[test]
fn the_close_rebuilds_a_base_wait_later_on_every_tick_grid() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let until = CLOSE + th.input_backoff_cap_ms + th.input_connect_timeout_ms;
        let mut with = Run::booted(step);
        let sent = with.outage(CLOSE, HELD, After::TimesOut, until);
        let mut today = Run::booted(step);
        assert_eq!(today.outage(CLOSE, REFUSED, After::TimesOut, until), sent);
        let gen_at_close = with.builds.iter().filter(|&&(at, _)| at < sent).count() as u64;
        let want = first_tick(sent + th.input_backoff_base_ms, step);
        assert_eq!(
            with.builds.iter().find(|&&(at, _)| at >= sent),
            Some(&(want, gen_at_close + 1)),
            "step {step}: {:?}",
            with.builds
        );
        let late = today.builds_from(sent);
        assert!(
            late.first().is_some_and(|&b| b > want),
            "step {step}: today's next build {late:?} is later than {want}"
        );
        assert!(
            !with.abandons.iter().any(|&(at, _)| at == sent),
            "step {step}: the close owes no abandon: {:?}",
            with.abandons
        );
        checked += 1;
    }
    eprintln!("tick grids checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// The once-an-outage bound, for a phone that never returns: after the close, each build either
/// times out or errors at once with the close's own facts, out to web's 15-minute phone-lost end.
/// The extra connects against today's link are a constant (F-032: the reset re-runs the 2, 4 and 8 s
/// steps once), never growing with the horizon. Unbounded, an erroring build would be rebuilt a base
/// wait after each error, every 1.1 s.
#[test]
fn a_phone_that_never_returns_costs_a_constant_number_of_connects() {
    let end = CLOSE + 15 * 60_000;
    let settled = CLOSE + 5 * 60_000;
    let mut horizons = 0;
    for after in [After::TimesOut, After::Errors] {
        for step in [10, 250] {
            let mut with = Run::booted(step);
            let sent = with.outage(CLOSE, HELD, after, end);
            // Today's link: the same errors, refused-shaped, so the rule never reads one.
            let mut today = Run::booted(step);
            assert_eq!(today.outage(CLOSE, REFUSED, after, end), sent);
            let (w, d) = (with.builds_from(sent), today.builds_from(sent));
            let extra = |h: Ms| {
                let n = |b: &[Ms]| b.iter().filter(|&&at| at <= h).count() as i64;
                n(&w) - n(&d)
            };
            let (mut early, mut late) = (Vec::new(), Vec::new());
            let mut h = sent + 60_000;
            while h <= end {
                if h < settled {
                    early.push(extra(h))
                } else {
                    late.push(extra(h))
                }
                horizons += 1;
                h += 1_000;
            }
            let range = |v: &[i64]| (*v.iter().min().unwrap(), *v.iter().max().unwrap());
            let (e, l) = (range(&early), range(&late));
            eprintln!("{after:?}, step {step}: extra connects {e:?} to 5 min, {l:?} after");
            assert!(
                l.1 <= e.1,
                "{after:?}, step {step}: the extra grows: {e:?} then {l:?}"
            );
            assert!(e.1 <= 3, "{after:?}, step {step}: F-032 bounds it at 3: {e:?}");
            assert!(l.0 >= 1, "{after:?}, step {step}: the rule rebuilt: {l:?}");
            // The capped tail: both a capped step apart once the steps run out.
            let gap = |b: &[Ms]| b.windows(2).last().map(|x| x[1] - x[0]);
            assert_eq!(gap(&w), gap(&d), "{after:?}, step {step}: the same capped tail");
        }
    }
    eprintln!("horizons checked: {horizons}");
    assert!(horizons > 0, "horizons checked: {horizons}");
}

/// A refused connect is today's loss through the supervisor too: no handshake, no rebuild a base wait
/// later. So is a handshaken error before the 20 s gate. And a close before the link's first Running
/// (a session started into an idle input) changes nothing that today's link would not.
#[test]
fn a_refused_connect_and_a_session_never_on_camera_keep_todays_back_off() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let until = CLOSE + th.input_backoff_cap_ms + th.input_connect_timeout_ms;
        let mut refused = Run::booted(step);
        let sent = refused.outage(CLOSE, REFUSED, After::TimesOut, until);
        assert!(
            refused.builds_from(sent).first() > Some(&first_tick(sent + th.input_backoff_base_ms, step)),
            "step {step}: refused, {:?}",
            refused.builds
        );
        checked += 1;
    }
    // An early error, handshaken: F-032's errors at a new publisher's arrival came about 4 s after
    // the last frame, and none from the drop + 10 s to the close. At the last frame + 10 s the link
    // is connecting, and the error is today's loss there, the same builds as a refused one's.
    for step in STEPS {
        let early = LAST + 10_000;
        assert!(
            early < LAST + th.input_hold_end_after_ms,
            "setup: before the gate"
        );
        let until = early + th.input_backoff_cap_ms + th.input_connect_timeout_ms;
        let (mut with, mut today) = (Run::booted(step), Run::booted(step));
        let sent = with.outage(early, HELD, After::TimesOut, until);
        assert_eq!(today.outage(early, REFUSED, After::TimesOut, until), sent);
        assert!(
            with.builds_from(sent).first() > Some(&first_tick(sent + th.input_backoff_base_ms, step)),
            "step {step}: early, {:?}",
            with.builds
        );
        assert_eq!(with.builds, today.builds, "step {step}: early is today's");
        checked += 1;
    }
    // Never on camera: no InputConnected, no frame; every build errors at once, handshaken.
    for step in STEPS {
        let mut with = Run::booted(step);
        let mut today = Run::booted(step);
        let horizon = 4 * th.input_hold_end_after_ms;
        for (r, facts) in [(&mut with, HELD), (&mut today, REFUSED)] {
            let mut errored = 0;
            while r.now <= horizon {
                if let Some(&(at, g)) = r.builds.last() {
                    if g > errored && r.now >= at + 100 {
                        r.send(Event::InputError { gen: g, facts });
                        errored = g;
                    }
                }
                r.send(Event::Tick);
                r.now += r.step;
            }
        }
        assert!(with.builds.len() >= 3, "step {step}: setup: {:?}", with.builds);
        assert_eq!(
            with.builds, today.builds,
            "step {step}: never Running, never armed"
        );
        checked += 1;
    }
    eprintln!("cases checked: {checked}");
    assert_eq!(checked, 3 * STEPS.len());
}

/// The gate runs from the camera's LAST frame (`Programme::last_frame_at`, which the supervisor hands
/// the link), not from the boot or the first frame: here the camera was on air three gates before the
/// drop, so either wrong anchor would put every error past the gate. F-032's errors before the close
/// keep today's builds: the one at a new publisher's arrival, about 4 s after the last frame (on the
/// build still Running), and one on the fresh build being connected, inside the gate. Neither spends
/// the shot, so Cloudflare's close 29 s after the last frame still rebuilds a base wait later.
#[test]
fn the_gate_runs_from_the_last_frame_of_a_long_shot() {
    let th = t();
    let gate = th.input_hold_end_after_ms;
    let last = 3 * gate;
    let arrival = last + 4_000;
    assert!(
        arrival < last + th.input_stall_after_ms,
        "setup: the arrival's error is on the build still Running"
    );
    // A base wait after the arrival's loss, the fresh build connects for a connect timeout.
    let connecting = arrival + th.input_backoff_base_ms + th.input_connect_timeout_ms / 2;
    let close = last + 29_000;
    assert!(
        connecting < last + gate && close >= last + gate && arrival > gate,
        "setup: the early errors inside the gate, the close past it, all past one from boot"
    );
    let mut checked = 0;
    for step in STEPS {
        let until = close + th.input_backoff_cap_ms + th.input_connect_timeout_ms;
        let (mut with, mut today) = (Run::booted(step), Run::booted(step));
        let errors = |early| [(arrival, early), (connecting, early), (close, HELD)];
        let sent = with.drive(last, &errors(HELD), After::TimesOut, until);
        assert_eq!(today.drive(last, &errors(REFUSED), After::TimesOut, until), sent);
        let before = |r: &Run| -> Vec<(Ms, u64)> {
            r.builds.iter().copied().filter(|&(at, _)| at < sent[2]).collect()
        };
        assert!(
            with.builds
                .iter()
                .any(|&(at, g)| at > sent[0] && at < sent[1] && g == 2),
            "step {step}: setup: gen 2 is being connected at the second error: {:?}",
            with.builds
        );
        assert_eq!(
            before(&with),
            before(&today),
            "step {step}: the early errors are today's"
        );
        let gen_at_close = before(&with).len() as u64;
        let want = first_tick(sent[2] + th.input_backoff_base_ms, step);
        for (r, name) in [(&with, "handshaken early"), (&today, "refused early")] {
            assert_eq!(
                r.builds.iter().find(|&&(at, _)| at >= sent[2]),
                Some(&(want, gen_at_close + 1)),
                "step {step}, {name}: the close still rebuilds a base wait later: {:?}",
                r.builds
            );
        }
        checked += 1;
    }
    eprintln!("long-shot tick grids checked: {checked}");
    assert_eq!(checked, STEPS.len());
}

/// The hold routes the error to the link (it ticks no link, so it builds nothing), and a second
/// close in the same outage is today's loss: the rule's build errors the same way and waits the
/// count's next step.
#[test]
fn the_hold_routes_the_error_and_the_second_close_waits_its_step() {
    let th = t();
    let mut r = Run::booted(1);
    let sent = r.outage(
        CLOSE,
        HELD,
        After::TimesOut,
        CLOSE + th.input_backoff_base_ms + 100,
    );
    let rebuilt = first_tick(sent + th.input_backoff_base_ms, 1);
    assert_eq!(
        r.builds.last().map(|b| b.0),
        Some(rebuilt),
        "setup: {:?}",
        r.builds
    );
    let gen = r.s.input_generation();
    // A second close on the rule's build: today's loss from Connecting at attempt 1, so the next
    // build waits `delay(1)`, not the base again.
    let at = r.now;
    r.send(Event::InputError { gen, facts: HELD });
    let next = at + delay(1, th.input_backoff_base_ms, th.input_backoff_cap_ms);
    assert_ne!(next, at + th.input_backoff_base_ms, "setup: a witness");
    while r.now <= next {
        r.send(Event::Tick);
        r.now += 1;
    }
    assert_eq!(r.builds.last(), Some(&(next, gen + 1)), "{:?}", r.builds);

    // The hold: the error changes the supervisor (routed to the link), with no effect.
    let s = r.s.decide(Event::HeartbeatReply(Desired::Ending), r.now).next;
    assert!(
        matches!(s.lifecycle(), Lifecycle::Holding { .. }),
        "setup: {:?}",
        s.lifecycle()
    );
    let d = s.decide(
        Event::InputError {
            gen: gen + 1,
            facts: HELD,
        },
        r.now + 10,
    );
    assert!(d.effects.is_empty(), "{:?}", d.effects);
    assert_ne!(d.next, s, "the hold routed the error to the link");
}
