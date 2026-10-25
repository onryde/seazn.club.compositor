//! The input pipeline's threads, kept for the life of the process and reused by every build (Owed 100,
//! `_FINDINGS.md` F-031).
//!
//! **Why.** glibc gives each thread an arena, and an arena keeps its high-water mark. A camera frame
//! (1.38 MB at 720p) is above glibc's first mmap threshold, but the threshold rises to the size of the
//! first such block freed, so from then on frame buffers come from the arena of the thread that
//! allocates them. When each build ran on new threads (its own build thread, and streaming threads
//! that GLib's pool does not keep), each build's frames landed in another arena and stayed there as
//! freed-but-held pages: +1.2 to +1.5 MB of anon memory per rebuild in CI (runs 40176756299,
//! 40178084471, 40179245528), the soak's camera-dropout growth. No object leaked: GStreamer's live
//! objects were the same after 30 more rebuilds, and capping glibc at two arenas held the same
//! rebuilds flat. On the same threads, the next build reuses what the last one freed.
//!
//! **What.** [`Workers`]: named threads that run a job, then wait for the next. A job goes to an
//! idle thread, the most recently idle first, and to a new thread only when none is idle, so a job
//! that hangs (a resolver inside a start, F-012; a decoder inside a teardown, F-001) never holds up
//! the next one: it runs on a thread of its own while it runs, as before. Threads never exit, so
//! their number is the most jobs ever in flight at once (two builds in the steady state: one tearing
//! down, the next starting), not the number of builds. [`BUILDS`] runs each build from its start to
//! its teardown (`gst_input::run_build`). The pipelines' streaming tasks run through [`task_pool`],
//! a `GstTaskPool` that `gst_input` hands each task as it is created, one pool and one set of
//! threads per [`TaskRole`]: from a single shared set, the video task landed on whichever thread was
//! idle, and the arenas it visited each kept a frame pool's worth (CI run 40191976912, after 40
//! rebuilds: 72 to 75 MiB of anon memory on one shared set, still climbing; 46 to 52 MiB on one set
//! a role, level).
use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::glib::subclass::prelude::ObjectSubclassIsExt;
use gstreamer::prelude::*;

type Job = Box<dyn FnOnce() + Send>;

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Threads that outlive the jobs they run (module doc).
#[derive(Debug)]
pub(crate) struct Workers {
    name: &'static str,
    /// Each thread's stack, in bytes.
    stack: usize,
    /// One sender a thread waiting for a job; a thread puts its own back once its job returns.
    idle: Mutex<Vec<mpsc::Sender<Job>>>,
    spawned: AtomicUsize,
    ran: AtomicU64,
}

/// One set's threads, as the test hooks read them.
#[cfg(any(test, feature = "test-hooks"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerCounts {
    /// Threads started so far (none ever ends).
    pub spawned: usize,
    /// Threads waiting for a job now.
    pub idle: usize,
    /// Jobs that ran to their end.
    pub ran: u64,
}

impl Workers {
    pub(crate) const fn new(name: &'static str, stack: usize) -> Self {
        Self {
            name,
            stack,
            idle: Mutex::new(Vec::new()),
            spawned: AtomicUsize::new(0),
            ran: AtomicU64::new(0),
        }
    }

    /// Runs `job` on an idle thread, or on a new one when none is idle. Never waits for a job. An
    /// error is a thread that could not be started; `job` is then dropped without running.
    pub(crate) fn run(&'static self, job: impl FnOnce() + Send + 'static) -> io::Result<()> {
        let mut job: Job = Box::new(job);
        loop {
            let Some(tx) = locked(&self.idle).pop() else {
                return self.spawn(job);
            };
            match tx.send(job) {
                Ok(()) => return Ok(()),
                // Its thread is gone: a thread lists itself only between two jobs, and leaves the list
                // only by taking one, so this needs a thread that ended while listed. Refused here, and
                // the job goes to the next one (`a_listed_thread_that_is_gone_is_passed_over`).
                Err(mpsc::SendError(back)) => job = back,
            }
        }
    }

    fn spawn(&'static self, first: Job) -> io::Result<()> {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name(self.name.to_owned())
            .stack_size(self.stack)
            .spawn(move || {
                let mut job = first;
                loop {
                    job();
                    self.ran.fetch_add(1, Ordering::SeqCst);
                    locked(&self.idle).push(tx.clone());
                    // This thread holds a sender of its own, so the channel never closes: it waits.
                    match rx.recv() {
                        Ok(next) => job = next,
                        Err(_) => return,
                    }
                }
            })?;
        self.spawned.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) fn counts(&self) -> WorkerCounts {
        WorkerCounts {
            spawned: self.spawned.load(Ordering::SeqCst),
            idle: locked(&self.idle).len(),
            ran: self.ran.load(Ordering::SeqCst),
        }
    }
}

/// A build thread's stack: Rust's default, 2 MiB, which each build's own thread always had.
const BUILD_STACK: usize = 2 << 20;
/// A streaming task's stack: 8 MiB, what GStreamer's default pool gave the tasks (GLib starts its
/// threads with glibc's default stack, which is `RLIMIT_STACK`, 8 MiB on ubuntu-24.04). The decoder
/// runs on the video queue's task.
const TASK_STACK: usize = 8 << 20;

/// Each input build, from its start to its teardown (`gst_input::run_build`).
pub(crate) static BUILDS: Workers = Workers::new("input-build", BUILD_STACK);

/// Which streaming task of an input pipeline a pool runs. Each has threads of its own, so a kind of
/// buffer is allocated on the same thread, in the same arena, build after build: from one shared
/// set, the task that decodes video lands on whichever thread is idle, and every arena it visits
/// keeps a frame pool's worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskRole {
    /// The SRT source's task, which also runs the demuxer.
    Source,
    /// The video queue's task, which runs the parser, the decoder and the converter.
    Video,
    /// The audio queue's task.
    Audio,
    /// Any other task.
    Other,
}

impl TaskRole {
    const ALL: [Self; 4] = [Self::Source, Self::Video, Self::Audio, Self::Other];

    fn workers(self) -> &'static Workers {
        static SOURCE: Workers = Workers::new("input-src", TASK_STACK);
        static VIDEO: Workers = Workers::new("input-video", TASK_STACK);
        static AUDIO: Workers = Workers::new("input-audio", TASK_STACK);
        static OTHER: Workers = Workers::new("input-task", TASK_STACK);
        match self {
            Self::Source => &SOURCE,
            Self::Video => &VIDEO,
            Self::Audio => &AUDIO,
            Self::Other => &OTHER,
        }
    }
}

/// The pool `role`'s streaming tasks run on, made once a process. `None`: GStreamer did not
/// initialise, or a pool did not prepare; the tasks then run on GStreamer's default pool, as they
/// did before Owed 100.
pub(crate) fn task_pool(role: TaskRole) -> Option<gst::TaskPool> {
    static POOLS: OnceLock<Option<[gst::TaskPool; 4]>> = OnceLock::new();
    let pools = POOLS.get_or_init(|| {
        gst::init().ok()?;
        let make = |role: TaskRole| {
            let pool = glib::Object::new::<InputTaskPool>();
            pool.imp().workers.set(role.workers()).ok()?;
            let pool = pool.upcast::<gst::TaskPool>();
            pool.prepare().ok()?;
            Some(pool)
        };
        let [source, video, audio, other] = TaskRole::ALL.map(make);
        Some([source?, video?, audio?, other?])
    });
    let [source, video, audio, other] = pools.as_ref()?;
    let pool = match role {
        TaskRole::Source => source,
        TaskRole::Video => video,
        TaskRole::Audio => audio,
        TaskRole::Other => other,
    };
    Some(pool.clone())
}

/// One role's streaming-task threads.
#[cfg(any(test, feature = "test-hooks"))]
pub(crate) fn role_counts(role: TaskRole) -> WorkerCounts {
    role.workers().counts()
}

/// Every streaming task's threads, all roles together.
#[cfg(any(test, feature = "test-hooks"))]
pub(crate) fn task_counts() -> WorkerCounts {
    TaskRole::ALL.iter().fold(
        WorkerCounts {
            spawned: 0,
            idle: 0,
            ran: 0,
        },
        |sum, role| {
            let c = role.workers().counts();
            WorkerCounts {
                spawned: sum.spawned + c.spawned,
                idle: sum.idle + c.idle,
                ran: sum.ran + c.ran,
            }
        },
    )
}

/// A task's end, for `GstTaskPool`'s join: set once its function has returned.
#[derive(Debug, Default)]
struct Done {
    done: Mutex<bool>,
    cv: Condvar,
}

impl Done {
    fn finish(&self) {
        *locked(&self.done) = true;
        self.cv.notify_all();
    }

    fn wait(&self) {
        let mut done = locked(&self.done);
        while !*done {
            done = self.cv.wait(done).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// What `push` hands GStreamer: joined when the task stops (`gst_task_join`).
#[derive(Debug)]
pub struct TaskJoin(Arc<Done>);

impl gst::TaskHandle for TaskJoin {
    fn join(self) {
        self.0.wait();
    }
}

mod imp {
    use std::sync::Arc;

    use std::sync::OnceLock;

    use gstreamer as gst;
    use gstreamer::glib;
    use gstreamer::subclass::prelude::*;
    use gstreamer::subclass::TaskPoolFunction;

    use super::{Done, TaskJoin, TaskRole, Workers};

    /// A `GstTaskPool` whose threads are one role's (`task_pool`).
    #[derive(Debug, Default)]
    pub struct InputTaskPool {
        /// Set once, as `task_pool` makes the pool. A pool made any other way runs its tasks with
        /// the tasks of no known role.
        pub(super) workers: OnceLock<&'static Workers>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InputTaskPool {
        const NAME: &'static str = "SeaznInputTaskPool";
        type Type = super::InputTaskPool;
        type ParentType = gst::TaskPool;
    }

    impl ObjectImpl for InputTaskPool {}
    impl GstObjectImpl for InputTaskPool {}

    impl TaskPoolImpl for InputTaskPool {
        type Handle = TaskJoin;

        fn push(&self, func: TaskPoolFunction) -> Result<Option<TaskJoin>, glib::Error> {
            let done = Arc::new(Done::default());
            let end = Arc::clone(&done);
            let workers = self
                .workers
                .get()
                .copied()
                .unwrap_or_else(|| TaskRole::Other.workers());
            workers
                .run(move || {
                    func.call();
                    end.finish();
                })
                .map_err(|e| {
                    glib::Error::new(gst::CoreError::Failed, &format!("no input task thread: {e}"))
                })?;
            Ok(Some(TaskJoin(done)))
        }
    }
}

glib::wrapper! {
    /// `task_pool`'s type.
    pub struct InputTaskPool(ObjectSubclass<imp::InputTaskPool>) @extends gst::TaskPool, gst::Object;
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread::{self, ThreadId};
    use std::time::{Duration, Instant};

    use super::Workers;

    /// Waits until `w` has `n` idle threads; false after 5 s.
    fn idle_reaches(w: &Workers, n: usize) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while w.counts().idle != n {
            if Instant::now() >= end {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
        }
        true
    }

    /// Runs one job on `w` and returns the id of the thread that ran it.
    fn ran_on(w: &'static Workers) -> ThreadId {
        let (tx, rx) = mpsc::channel();
        w.run(move || {
            let _ = tx.send(thread::current().id());
        })
        .expect("a thread");
        rx.recv_timeout(Duration::from_secs(5)).expect("the job ran")
    }

    /// The sequence: a first job starts a thread; each later job, once that thread is idle again,
    /// runs on it, so fifty jobs in a row start one thread.
    #[test]
    fn jobs_one_after_another_share_one_thread() {
        static W: Workers = Workers::new("t-sequence", 1 << 20);
        let first = ran_on(&W);
        let mut ids = vec![first];
        for _ in 0..50 {
            assert!(idle_reaches(&W, 1), "the thread went back to idle");
            ids.push(ran_on(&W));
        }
        assert!(idle_reaches(&W, 1));
        let c = W.counts();
        eprintln!("checked {} jobs: {c:?}", ids.len());
        assert_eq!(ids.len(), 51);
        assert!(
            ids.iter().all(|&id| id == first),
            "every job ran on the first thread"
        );
        assert_eq!(c.spawned, 1);
        assert_eq!(c.ran, 51);
    }

    /// A job that hangs never holds up the next: while the first is blocked, the second runs at once,
    /// on a second thread. Once both are idle, more jobs start no thread.
    #[test]
    fn a_hung_job_never_holds_up_the_next() {
        static W: Workers = Workers::new("t-hung", 1 << 20);
        let (release, held) = mpsc::channel::<()>();
        let (started, first_started) = mpsc::channel();
        W.run(move || {
            let _ = started.send(thread::current().id());
            let _ = held.recv();
        })
        .expect("a thread");
        let hung = first_started
            .recv_timeout(Duration::from_secs(5))
            .expect("the first job ran");
        let second = ran_on(&W);
        assert_ne!(second, hung, "the second job did not wait for the first");
        assert_eq!(W.counts().spawned, 2);
        release.send(()).expect("the first job is waiting");
        assert!(idle_reaches(&W, 2), "both threads idle");
        for _ in 0..10 {
            ran_on(&W);
            assert!(idle_reaches(&W, 2));
        }
        let c = W.counts();
        eprintln!("after the hang: {c:?}");
        assert_eq!(c.spawned, 2, "no thread after the hang ended");
        assert_eq!(c.ran, 12);
    }

    /// A job that panics ends its thread (as a build's own thread ended before Owed 100), and the next
    /// job still runs, on a new thread.
    #[test]
    fn a_job_after_one_that_panicked_still_runs() {
        static W: Workers = Workers::new("t-panic", 1 << 20);
        let first = ran_on(&W);
        assert!(idle_reaches(&W, 1));
        W.run(|| panic!("a job that fails (expected in this test)"))
            .expect("a thread");
        assert!(idle_reaches(&W, 0), "the failed job took the idle thread");
        let after = ran_on(&W);
        assert_ne!(after, first, "the panicked thread is gone");
        assert!(idle_reaches(&W, 1), "the new thread went back to idle");
        let c = W.counts();
        eprintln!("after the panic: {c:?}");
        assert_eq!(c.spawned, 2);
        assert_eq!(c.ran, 2, "the panicked job did not run to its end");
    }

    /// The guard in `run`: a listed thread whose receiver is gone is passed over, its sender dropped,
    /// and the job runs on the next thread (here, a new one).
    #[test]
    fn a_listed_thread_that_is_gone_is_passed_over() {
        static W: Workers = Workers::new("t-gone", 1 << 20);
        let (dead, gone) = mpsc::channel();
        drop(gone);
        super::locked(&W.idle).push(dead);
        assert_eq!(W.counts().idle, 1);
        ran_on(&W);
        assert!(idle_reaches(&W, 1), "the new thread is the one listed");
        let c = W.counts();
        assert_eq!(c.spawned, 1);
        assert_eq!(c.ran, 1);
    }
}
