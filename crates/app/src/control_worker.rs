//! The control worker (C08): one thread owns the `ControlPlane`, so a slow request never blocks the
//! frame clock. The loop's side is a `ControlHandle` over three request slots:
//!
//! - **final beats** (a report whose `exit` is `Some`): capacity 2, never dropped (Owed 28). A run
//!   sends at most two, the drain's and then a fatal error's during the drain (C06 review m2), so its
//!   `send` never waits in practice; a third still posts, its `request` waiting for room.
//! - **the fetch**: capacity 1, never dropped. The domain has one fetch in flight at a time.
//! - **periodic beats**: capacity 1 behind the one in flight. A newer beat replaces the pending one
//!   (drop-oldest, AGENTS.md §4: the freshest state is the one worth sending), and the replaced beat
//!   is counted in `dropped`. A final beat retires a pending periodic beat too: no `playing` beat
//!   reaches web after a `stopped` one.
//!
//! The worker takes a final beat first, then the fetch, then a periodic beat. Every result is posted
//! to the loop as an `Inbound` message, mapped below; no result's text is kept or logged (C07 has
//! already redacted it, and the domain needs only its kind). A final beat is acknowledged, for
//! `flush`, before its result is posted, so a full inbound channel cannot hold up the exit.
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Select, Sender, TryRecvError};
use domain::health::{ExitDetail, HealthReport};
use domain::supervisor::{Desired, Event, HeartbeatRefusal};

use crate::ports::{
    ControlPlane, ControlQueue, ControlRequest, FetchError, HeartbeatError, Inbound, Measured, SessionConfig,
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
    /// Periodic beats replaced or retired before they were sent.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

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

    fn flush(&mut self, timeout: Duration) -> bool {
        let want = self.requested_finals;
        let guard = locked(&self.posted.count);
        let (posted, _) = self
            .posted
            .cv
            .wait_timeout_while(guard, timeout, |n| *n < want)
            .unwrap_or_else(PoisonError::into_inner);
        *posted >= want && !self.lost_final
    }
}

/// Starts the worker thread over `cp`; its results go to `tx`.
pub fn spawn(cp: Box<dyn ControlPlane>, tx: Sender<Inbound>) -> ControlHandle {
    let (finals, finals_rx) = bounded(FINAL_SLOT);
    let (fetch, fetch_rx) = bounded(1);
    let (periodic, periodic_rx) = bounded(1);
    let posted = Arc::new(Posted::default());
    let worker = Worker {
        cp,
        tx: tx.clone(),
        finals: finals_rx,
        fetch: fetch_rx,
        periodic: periodic_rx.clone(),
        posted: Arc::clone(&posted),
    };
    if let Err(e) = thread::Builder::new()
        .name("control".into())
        .spawn(move || worker.run())
    {
        // No worker: no fetch could ever answer, so the loop would wait for ever. Exit instead.
        let detail = Some(ExitDetail::new(format!("control worker not started: {e}")));
        let _ = tx.send(Inbound::Event(Event::Fatal { detail }));
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

enum Job {
    Final(Beat),
    Fetch,
    Periodic(Beat),
}

struct Worker {
    cp: Box<dyn ControlPlane>,
    tx: Sender<Inbound>,
    finals: Receiver<Beat>,
    fetch: Receiver<()>,
    periodic: Receiver<Beat>,
    posted: Arc<Posted>,
}

impl Worker {
    fn run(mut self) {
        while let Some(job) = self.next() {
            let msg = match job {
                Job::Final((h, m)) => {
                    let r = self.cp.heartbeat(&h, &m);
                    self.ack();
                    replied(r)
                }
                Job::Fetch => fetched(self.cp.fetch_session()),
                Job::Periodic((h, m)) => replied(self.cp.heartbeat(&h, &m)),
            };
            // The loop may have gone (it returned): keep serving, a final beat may still be queued.
            let _ = self.tx.send(msg);
        }
    }

    fn ack(&self) {
        *locked(&self.posted.count) += 1;
        self.posted.cv.notify_all();
    }

    /// The next request by priority, waiting for one. `None` once the handle has gone and every
    /// slot is empty.
    fn next(&self) -> Option<Job> {
        loop {
            let mut sel = Select::new();
            let mut open = 0;
            match self.finals.try_recv() {
                Ok(b) => return Some(Job::Final(b)),
                Err(TryRecvError::Empty) => {
                    sel.recv(&self.finals);
                    open += 1;
                }
                Err(TryRecvError::Disconnected) => {}
            }
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
fn replied(r: Result<Desired, HeartbeatError>) -> Inbound {
    Inbound::Event(match r {
        Ok(d) => Event::HeartbeatReply(d),
        Err(HeartbeatError::Gone) => Event::HeartbeatRefused(HeartbeatRefusal::Gone),
        Err(HeartbeatError::Transport(_)) => Event::HeartbeatFailed,
    })
}
