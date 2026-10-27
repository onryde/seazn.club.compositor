//! The control worker (C08): two threads, so a slow request never blocks the frame clock and a
//! periodic beat never holds a final one up. The loop's side is a `ControlHandle` over three request
//! slots:
//!
//! - **final beats** (a report whose `exit` is `Some`): capacity 2, never dropped (Owed 28), served
//!   by their own lane (C08 review round 1, the coordinator's ruling): the thread `control-final`,
//!   over a second plane instance (`ControlPlane::final_lane`), so a periodic beat in flight, however
//!   long it takes, never delays or loses one. A run sends at most two, the drain's and then a fatal
//!   error's during the drain (C06 review m2), so its `send` never waits in practice; a third still
//!   posts, its `request` waiting for room.
//! - **the fetch**: capacity 1, never dropped. The domain has one fetch in flight at a time.
//! - **periodic beats**: capacity 1 behind the one in flight. A newer beat replaces the pending one
//!   (drop-oldest, AGENTS.md §4: the freshest state is the one worth sending), and the replaced beat
//!   is counted in `dropped`. A final beat retires a pending periodic beat too, so no periodic beat
//!   STARTS after a final one; one already in flight runs on, and may land after it.
//!
//! The fetch and the periodic beats share the thread `control`, the fetch first. Every result is
//! posted to the loop as an `Inbound` message, mapped below; no result's text is kept or logged (C07
//! has already redacted it, and the domain needs only its kind). A periodic beat's reply that carries
//! web's phone-live edge counter posts `Event::PhoneLiveEdge` right after its `HeartbeatReply` (Owed
//! 108 fix (b)); a final beat's does not, since nothing is rebuilt once the process is ending. A final beat is acknowledged, for
//! `flush`, as soon as its call returns, and its lane takes the next final beat while a result waits
//! for room in the inbound channel, so neither a full channel nor a stalled periodic beat holds up
//! the exit (review round 1, items 1 and 2).
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Select, Sender, TryRecvError};
use domain::health::{ExitDetail, HealthReport};
use domain::supervisor::{Event, HeartbeatRefusal};

use crate::ports::{
    ControlPlane, ControlQueue, ControlRequest, FetchError, HeartbeatError, Inbound, Measured, Reply,
    SessionConfig,
};

/// The final-beat slot: the drain's beat, then a fatal error's during the drain.
const FINAL_SLOT: usize = 2;

type Beat = (HealthReport, Measured);

/// The worker's count of final beats posted (their call returned), and the condvar `flush` waits on.
#[derive(Default)]
struct Posted {
    count: Mutex<u64>,
    cv: Condvar,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The loop's side of the worker (`ControlQueue`). Dropping it ends the worker.
pub struct ControlHandle {
    finals: Sender<Beat>,
    fetch: Sender<()>,
    periodic: Sender<Beat>,
    /// The periodic slot's receiving end, held so a newer beat can replace the pending one.
    periodic_pending: Receiver<Beat>,
    requested_finals: u64,
    /// A final beat the worker could not be handed (it has gone): `flush` cannot report it posted.
    lost_final: bool,
    posted: Arc<Posted>,
    dropped: u64,
}

// The counters only: a pending beat's report is not printed.
impl fmt::Debug for ControlHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlHandle")
            .field("requested_finals", &self.requested_finals)
            .field("posted_finals", &*locked(&self.posted.count))
            .field("lost_final", &self.lost_final)
            .field("dropped", &self.dropped)
            .finish()
    }
}

impl ControlHandle {
    /// Retires the pending periodic beat, if there is one, and counts it.
    fn retire_pending(&mut self) {
        if self.periodic_pending.try_recv().is_ok() {
            self.dropped += 1;
        }
    }
}

impl ControlQueue for ControlHandle {
    fn request(&mut self, req: ControlRequest) {
        match req {
            // A send error means the worker has gone; nothing can be posted then.
            ControlRequest::FetchSession => {
                let _ = self.fetch.send(());
            }
            ControlRequest::Heartbeat(h, m) if h.exit.is_some() => {
                self.retire_pending();
                if self.finals.send((h, m)).is_ok() {
                    self.requested_finals += 1;
                } else {
                    self.lost_final = true;
                }
            }
            ControlRequest::Heartbeat(h, m) => {
                if self.periodic.is_full() {
                    self.retire_pending();
                }
                // Only this handle sends on the slot, so after the retire it has room; a beat that
                // still does not fit is counted like any other beat not sent.
                if self.periodic.try_send((h, m)).is_err() {
                    self.dropped += 1;
                }
            }
        }
    }

    fn dropped(&self) -> u64 {
        self.dropped
    }

    fn flush(&mut self, timeout: Duration) -> bool {
        // A final beat its lane could not take never posts: say so at once, without waiting.
        if self.lost_final {
            return false;
        }
        let want = self.requested_finals;
        let guard = locked(&self.posted.count);
        let (posted, _) = self
            .posted
            .cv
            .wait_timeout_while(guard, timeout, |n| *n < want)
            .unwrap_or_else(PoisonError::into_inner);
        *posted >= want
    }
}

/// Starts the worker's two threads: `control` over `cp`, and `control-final` over
/// `cp.final_lane()`. Their results go to `tx`.
pub fn spawn(cp: Box<dyn ControlPlane>, tx: Sender<Inbound>) -> ControlHandle {
    let (finals, finals_rx) = bounded(FINAL_SLOT);
    let (fetch, fetch_rx) = bounded(1);
    let (periodic, periodic_rx) = bounded(1);
    let posted = Arc::new(Posted::default());
    let lane = FinalLane {
        cp: cp.final_lane(),
        tx: tx.clone(),
        finals: finals_rx,
        posted: Arc::clone(&posted),
    };
    let worker = Worker {
        cp,
        tx: tx.clone(),
        fetch: fetch_rx,
        periodic: periodic_rx.clone(),
    };
    for started in [
        thread::Builder::new()
            .name("control-final".into())
            .spawn(move || lane.run()),
        thread::Builder::new()
            .name("control".into())
            .spawn(move || worker.run()),
    ] {
        if let Err(e) = started {
            // No worker: no fetch could ever answer (the loop would wait for ever), or no final beat
            // could post. Exit instead.
            let detail = Some(ExitDetail::new(format!("control worker not started: {e}")));
            let _ = tx.send(Inbound::Event(Event::Fatal { detail }));
        }
    }
    ControlHandle {
        finals,
        fetch,
        periodic,
        periodic_pending: periodic_rx,
        requested_finals: 0,
        lost_final: false,
        posted,
        dropped: 0,
    }
}

/// The final beats' lane: its own plane instance and thread (review round 1, item 1).
struct FinalLane {
    /// Declared before `tx`, so a lane that ends (a panic included) closes its slot before its sender:
    /// once the inbound channel has closed, a final beat finds the lane gone.
    finals: Receiver<Beat>,
    tx: Sender<Inbound>,
    cp: Box<dyn ControlPlane>,
    posted: Arc<Posted>,
}

impl FinalLane {
    /// Ends when the handle has gone: the loop owns both the handle and the inbound channel's
    /// receiver, so nothing is left to read a result not yet posted.
    fn run(mut self) {
        // Results not yet posted, oldest first. The lane takes the next final beat whenever one is
        // queued, so none waits behind a result the loop is not reading (review round 1, item 2).
        let mut unposted: VecDeque<Inbound> = VecDeque::new();
        loop {
            let Some(msg) = unposted.pop_front() else {
                match self.finals.recv() {
                    Ok(beat) => self.post(beat, &mut unposted),
                    Err(_) => return,
                }
                continue;
            };
            let mut sel = Select::new();
            let take = sel.recv(&self.finals);
            sel.send(&self.tx);
            let op = sel.select();
            if op.index() == take {
                unposted.push_front(msg);
                match op.recv(&self.finals) {
                    Ok(beat) => self.post(beat, &mut unposted),
                    Err(_) => return,
                }
            } else if op.send(&self.tx, msg).is_err() {
                // The loop has gone (it returned): no result can be read, but a final beat may still
                // be queued.
                unposted.clear();
            }
        }
    }

    /// Posts one final beat, acknowledges it for `flush`, and queues its result.
    fn post(&mut self, (h, m): Beat, unposted: &mut VecDeque<Inbound>) {
        let r = self.cp.heartbeat(&h, &m);
        *locked(&self.posted.count) += 1;
        self.posted.cv.notify_all();
        unposted.push_back(replied(r));
    }
}

enum Job {
    Fetch,
    Periodic(Beat),
}

/// The fetch and the periodic beats' thread.
struct Worker {
    cp: Box<dyn ControlPlane>,
    tx: Sender<Inbound>,
    fetch: Receiver<()>,
    periodic: Receiver<Beat>,
}

impl Worker {
    fn run(mut self) {
        while let Some(job) = self.next() {
            let (msg, edge) = match job {
                Job::Fetch => (fetched(self.cp.fetch_session()), None),
                Job::Periodic((h, m)) => {
                    let r = self.cp.heartbeat(&h, &m);
                    let edge = live_edge(&r);
                    (replied(r), edge)
                }
            };
            // A full channel holds this thread alone: the final lane posts on its own. The loop may
            // have gone (it returned): the send then fails, and is ignored.
            for msg in std::iter::once(msg).chain(edge) {
                let _ = self.tx.send(msg);
            }
        }
    }

    /// The next request, the fetch first, waiting for one. `None` once the handle has gone and both
    /// slots are empty.
    fn next(&self) -> Option<Job> {
        loop {
            let mut sel = Select::new();
            let mut open = 0;
            match self.fetch.try_recv() {
                Ok(()) => return Some(Job::Fetch),
                Err(TryRecvError::Empty) => {
                    sel.recv(&self.fetch);
                    open += 1;
                }
                Err(TryRecvError::Disconnected) => {}
            }
            match self.periodic.try_recv() {
                Ok(b) => return Some(Job::Periodic(b)),
                Err(TryRecvError::Empty) => {
                    sel.recv(&self.periodic);
                    open += 1;
                }
                Err(TryRecvError::Disconnected) => {}
            }
            if open == 0 {
                return None;
            }
            // Wait for any open slot, then take by priority on the next pass.
            sel.ready();
        }
    }
}

fn fetched(r: Result<SessionConfig, FetchError>) -> Inbound {
    match r {
        Ok(cfg) => Inbound::Session(cfg),
        Err(FetchError::Refused(r)) => Inbound::Event(Event::BootstrapRefused(r)),
        Err(FetchError::Transient(_)) => Inbound::Event(Event::BootstrapFailed),
    }
}

/// Every heartbeat's result, the final beat's included (Owed 29: C07 returns `Gone` for every 410).
fn replied(r: Result<Reply, HeartbeatError>) -> Inbound {
    Inbound::Event(match r {
        Ok(reply) => Event::HeartbeatReply(reply.desired),
        Err(HeartbeatError::Gone) => Event::HeartbeatRefused(HeartbeatRefusal::Gone),
        Err(HeartbeatError::Transport(_)) => Event::HeartbeatFailed,
    })
}

/// A periodic reply's phone-live edge counter, when it carries one (Owed 108 fix (b)), for the
/// domain's input link: posted after the reply's own event, every time, since the domain reads a
/// repeat as no edge.
fn live_edge(r: &Result<Reply, HeartbeatError>) -> Option<Inbound> {
    let count = r.as_ref().ok()?.phone_live_edge?;
    Some(Inbound::Event(Event::PhoneLiveEdge { count }))
}
