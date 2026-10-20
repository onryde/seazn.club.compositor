//! C10b (Owed 50): the domain is the one authority on giving input A up. The input link's own two
//! rules, the connect timeout and the stall rule, each push `Effect::AbandonInput { gen }` once for the
//! generation they gave up, on the Running tick that decided it; a loss the adapter reported owes
//! none; the hold and the drain tick no link, so they abandon nothing. Every expected instant is
//! derived from `Thresholds`, the rules' own declaration, and the tick grid: an effect comes on the
//! first tick at or after the instant its rule names. Swept over four tick periods, from 1 ms (each
//! rule's exact instant) to 1 s. These sweeps live here, not inline, because the domain's crate root
//! forbids `clippy::print_stderr`, and a sweep must print the count it checked.
use domain::config::Thresholds;
use domain::input_link::LossReason;
use domain::lifecycle::Lifecycle;
use domain::supervisor::{Desired, Effect, Event, Supervisor};
use domain::time::Ms;

/// The tick periods swept: 1 ms lands on each rule's instant exactly, 33 ms is C08's frame period,
/// and the coarse two put the rule's instant between ticks.
const STEPS: [Ms; 4] = [1, 33, 250, 1_000];

const FAR: Ms = 10_000_000;

fn t() -> Thresholds {
    Thresholds::default()
}

/// A booted supervisor at 0: Running, nothing built yet (its first Running tick builds gen 1).
fn booted() -> Supervisor {
    let s = Supervisor::new(t()).decide(Event::Tick, 0).next;
    let d = s.decide(
        Event::BootstrapOk {
            heartbeat_interval_ms: t().heartbeat_interval_ms,
            deadline_at: FAR,
        },
        0,
    );
    assert_eq!(d.next.lifecycle(), Lifecycle::Running, "setup: booted");
    d.next
}

/// A frame of input generation `gen`.
fn frame(_gen: u64) -> Event {
    Event::InputFrame
}

/// The first tick at or after `at` on a grid of `step` from 0.
fn first_tick(at: Ms, step: Ms) -> Ms {
    at.div_ceil(step) * step
}

/// An input effect and when it came.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Did {
    Build(u64),
    Abandon(u64),
}

/// The supervisor, driven on a tick grid, with every input effect it pushed.
struct Run {
    s: Supervisor,
    now: Ms,
    step: Ms,
    log: Vec<(Ms, Did)>,
    /// The lifecycle at each instant an abandon came (all must be Running).
    abandoned_in: Vec<Lifecycle>,
}

impl Run {
    fn new(step: Ms) -> Self {
        Self {
            s: booted(),
            now: 0,
            step,
            log: Vec::new(),
            abandoned_in: Vec::new(),
        }
    }

    fn send(&mut self, e: Event) {
        let before = self.s.lifecycle();
        let d = self.s.decide(e, self.now);
        for fx in &d.effects {
            match *fx {
                Effect::RebuildInput { gen } => self.log.push((self.now, Did::Build(gen))),
                Effect::AbandonInput { gen } => {
                    self.log.push((self.now, Did::Abandon(gen)));
                    self.abandoned_in.push(before);
                }
                _ => {}
            }
        }
        self.s = d.next;
    }

    /// Each instant on the grid through `until`: `events` first (the adapter's reports of that
    /// instant), then the tick.
    fn until(&mut self, until: Ms, mut events: impl FnMut(Ms, &Supervisor) -> Vec<Event>) {
        while self.now <= until {
            for e in events(self.now, &self.s) {
                self.send(e);
            }
            self.send(Event::Tick);
            self.now += self.step;
        }
    }

    fn builds(&self) -> Vec<(Ms, u64)> {
        self.log
            .iter()
            .filter_map(|&(at, d)| match d {
                Did::Build(g) => Some((at, g)),
                Did::Abandon(_) => None,
            })
            .collect()
    }

    fn abandons(&self) -> Vec<(Ms, u64)> {
        self.log
            .iter()
            .filter_map(|&(at, d)| match d {
                Did::Abandon(g) => Some((at, g)),
                Did::Build(_) => None,
            })
            .collect()
    }

    /// Each built generation is abandoned at most once, only after its build, only by a Running
    /// tick, and before the next generation's build. Returns the abandons checked.
    fn check_once_each(&self, label: &str) -> usize {
        let (builds, abandons) = (self.builds(), self.abandons());
        for (i, &(at, g)) in abandons.iter().enumerate() {
            assert_eq!(
                abandons.iter().filter(|&&(_, h)| h == g).count(),
                1,
                "{label}: gen {g} abandoned more than once: {abandons:?}"
            );
            let built = builds.iter().find(|&&(_, h)| h == g).map(|&(b, _)| b);
            assert!(
                built.is_some_and(|b| b <= at),
                "{label}: gen {g} abandoned at {at} unbuilt"
            );
            if let Some(&(next, _)) = builds.iter().find(|&&(_, h)| h == g + 1) {
                assert!(
                    at <= next,
                    "{label}: gen {g} abandoned at {at}, after gen {} at {next}",
                    g + 1
                );
                let ordered = self.log.iter().position(|&(_, d)| d == Did::Abandon(g))
                    < self.log.iter().position(|&(_, d)| d == Did::Build(g + 1));
                assert!(
                    ordered,
                    "{label}: gen {g}'s abandon after the next build: {:?}",
                    self.log
                );
            }
            assert_eq!(self.abandoned_in[i], Lifecycle::Running, "{label}: gen {g}");
        }
        abandons.len()
    }
}

/// The stall rule: gen 1 connects, frames flow to `last`, then stop. Gen 1 is abandoned once, on the
/// first tick at or after `last + input_stall_after_ms`, before gen 2 is built; gen 2 never connects,
/// and each generation after is abandoned once by the connect timeout, on the first tick at or after
/// its build plus `input_connect_timeout_ms`.
#[test]
fn the_stall_rule_and_the_connect_timeout_each_abandon_a_generation_once() {
    let th = t();
    let mut checked = 0;
    let mut by_connect = 0;
    for step in STEPS {
        let mut r = Run::new(step);
        let stop = 2_000;
        let mut last = None;
        let run_to = stop + th.input_stall_after_ms + 4 * th.input_connect_timeout_ms;
        r.until(run_to, |now, s| {
            let mut e = Vec::new();
            if s.input_generation() == 1 && now > 0 && now <= stop {
                if now == step {
                    e.push(Event::InputConnected { gen: 1 });
                }
                e.push(frame(1));
                last = Some(now);
            }
            e
        });
        let label = format!("step {step}");
        let last = last.expect("setup: frames flowed");
        let abandons = r.abandons();
        assert_eq!(
            abandons.first(),
            Some(&(first_tick(last + th.input_stall_after_ms, step), 1)),
            "{label}: the stall rule's instant: {abandons:?}"
        );
        let builds = r.builds();
        for &(at, g) in abandons.iter().skip(1) {
            let built = builds.iter().find(|&&(_, h)| h == g).expect("built").0;
            assert_eq!(
                at,
                first_tick(built + th.input_connect_timeout_ms, step),
                "{label}: gen {g}'s connect timeout"
            );
            by_connect += 1;
        }
        let n = r.check_once_each(&label);
        assert!(
            n >= 3,
            "{label}: the stall and at least two connect timeouts: {abandons:?}"
        );
        checked += n;
    }
    eprintln!(
        "abandons checked: {checked} over {} tick periods ({by_connect} by the connect timeout)",
        STEPS.len()
    );
    assert!(checked >= 3 * STEPS.len());
    assert!(by_connect >= 2 * STEPS.len());
}

/// A loss the adapter reported (EOS, an error, or a stall it saw itself), from Connecting or from
/// Running, owes no abandon: the adapter decided it and its gate is shut. The next generation is
/// built (so the loss was real), connects and runs, and nothing is abandoned in the whole run.
#[test]
fn a_loss_the_adapter_reported_is_never_abandoned() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        for reason in [LossReason::Eos, LossReason::Error, LossReason::Stalled] {
            for running in [true, false] {
                let mut r = Run::new(step);
                let lose_at = 3 * step;
                let run_to = lose_at + 3 * th.input_stall_after_ms;
                r.until(run_to, |now, s| {
                    let gen = s.input_generation();
                    let mut e = Vec::new();
                    if now == lose_at && gen == 1 {
                        e.push(Event::InputLost { gen: 1, reason });
                        return e;
                    }
                    let connect = (gen == 1 && running && now == step) || (gen > 1 && now > lose_at);
                    if connect {
                        e.push(Event::InputConnected { gen });
                    }
                    if now > 0 && (gen > 1 || running) {
                        e.push(frame(gen));
                    }
                    e
                });
                let label = format!("step {step}, {reason:?}, from Running {running}");
                assert!(
                    r.builds().iter().any(|&(_, g)| g == 2),
                    "{label}: the loss was real"
                );
                assert_eq!(r.abandons(), vec![], "{label}");
                assert_eq!(r.s.input_generation(), 2, "{label}: gen 2 ran to the end");
                checked += 1;
            }
        }
    }
    eprintln!("adapter-reported losses checked: {checked}");
    assert_eq!(checked, STEPS.len() * 3 * 2);
}

/// The hold ticks no link: an input that would be given up on the tick after the end request is not,
/// through the whole hold, the drain and the exit. Its pair without the end request is abandoned on
/// that tick, so the hold is what changed it.
#[test]
fn the_hold_and_the_drain_abandon_nothing() {
    let th = t();
    let mut checked = 0;
    for step in STEPS {
        let due = first_tick(th.input_connect_timeout_ms, step);
        let end_at = due - step;
        for ending in [false, true] {
            let mut r = Run::new(step);
            let mut held_at_due = None;
            r.until(
                due + th.final_hold_ms + 2 * th.input_connect_timeout_ms,
                |now, s| {
                    if now == due {
                        held_at_due = Some(s.lifecycle());
                    }
                    if ending && now == end_at {
                        vec![Event::HeartbeatReply(Desired::Ending)]
                    } else {
                        vec![]
                    }
                },
            );
            let label = format!("step {step}, ending {ending}");
            if ending {
                assert!(
                    matches!(held_at_due, Some(Lifecycle::Holding { .. })),
                    "{label}: held at {due}: {held_at_due:?}"
                );
                assert!(
                    matches!(r.s.lifecycle(), Lifecycle::Exited(_)),
                    "{label}: {:?}",
                    r.s.lifecycle()
                );
                assert_eq!(r.abandons(), vec![], "{label}");
            } else {
                assert_eq!(
                    r.abandons().first(),
                    Some(&(due, 1)),
                    "{label}: the pair abandons"
                );
                r.check_once_each(&label);
            }
            checked += 1;
        }
    }
    eprintln!("hold pairs checked: {checked}");
    assert_eq!(checked, STEPS.len() * 2);
}
