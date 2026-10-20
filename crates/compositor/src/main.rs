//! The compositor (C12): reads the Machine's environment, builds every adapter and runs
//! `app::run::run`. The only place that reads `std::env`, the system clock or signals.
//!
//! 1. The stop signals' handlers, first (the PR #36 review's m1): a stop that arrives while the
//!    process boots is held for the loop, not left to SIGUSR1's and SIGUSR2's default action.
//! 2. No core, first (Owed 68, reopened by the PR #36 review's I1): Chromium's abort is a SIGTRAP
//!    with a core, and a 93–117 MB core of this process holds the page token (46 and 61 times in the
//!    rig), the stream key and the job token. The process is made non-dumpable, so the kernel writes
//!    no core of it under any `core_pattern`. RLIMIT_CORE is 1 byte, soft and hard, for CEF's
//!    children, which inherit the limit but not the dumpable flag (`execve` resets it): the kernel
//!    refuses a core to a piped `core_pattern` at exactly 1 ("RLIMIT_CORE is set to 1, aborting core",
//!    `fs/coredump.c`), and writes none to a file below the ELF minimum. A limit of 0 stops only the
//!    file case: for a pipe the kernel ignores the limit, which is how a core of this process
//!    reached the CI runner's crash helper (image.yml run 39410132377).
//! 3. The environment; a refusal exits 78 (EX_CONFIG) naming the key, never a value, and sends no
//!    beat (there is no URL or token to send one with yet).
//!    Once parsed, `JOB_TOKEN` leaves the process's environment, so CEF's children never inherit it.
//! 4. `oom_score_adj` -900 (spec §6); a non-root process may be refused, which is logged once.
//! 5. `wiring::run`, whose code is the process's: 0, 70 (fatal), 75 (C11b's in-place restart,
//!    EX_TEMPFAIL, which tini passes through so the host's `on-failure` restarts the process), or 78.
//!    `run()` returns only after the final beat posted or `FINAL_BEAT_TIMEOUT_MS` passed (Owed 28),
//!    so no wait is owed here.
//! 6. `hard_exit::now`, never `std::process::exit` (F-017): libc's `exit` runs libcef's and GLib's
//!    exit handlers while CEF's and GStreamer's threads still run, and one such race aborted a clean
//!    run with a double free after its `exit code=0` line (status 134).
use std::sync::Arc;

use compositor::clock::Clock;
use compositor::env;
use compositor::logged::Logger;
use compositor::wiring;
use compositor::CORE_LIMIT_BYTES;
use rustix::process::{set_dumpable_behavior, setrlimit, DumpableBehavior, Resource, Rlimit};

fn main() {
    let signals = wiring::arm_signals();
    /*MUT28*/ let dumpable: rustix::io::Result<()> = Ok(());
    let _ = (set_dumpable_behavior, DumpableBehavior::NotDumpable);
    let core = setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(CORE_LIMIT_BYTES),
            maximum: Some(CORE_LIMIT_BYTES),
        },
    );
    let clock = Arc::new(Clock::system());
    let log = Logger::stderr(Arc::clone(&clock));
    if let Err(e) = dumpable {
        log.warn(&format!("PR_SET_DUMPABLE 0 not set: {e}"));
    }
    if let Err(e) = core {
        log.warn(&format!("RLIMIT_CORE {CORE_LIMIT_BYTES} not set: {e}"));
    }
    let get = |k: &str| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned());
    if let Some(t) = get(env::JOB_TOKEN) {
        // Before anything can print it: a non-UTF-8 token is masked in its lossy form.
        log.learn_token(&t);
    }
    let env = match env::parse(get, !cfg!(debug_assertions)) {
        Ok(e) => e,
        Err(e) => {
            log.error(&e.to_string());
            log.info("exit code=78");
            hard_exit::now(78);
        }
    };
    // The plane has the token now (`env`). CEF's children start from this process's environment and
    // need none of it (the PR #36 review's m2). No thread runs yet (the signal handlers, the clock and
    // the logger start none), so removing it is sound; edition 2021 keeps `remove_var` safe.
    /*MUT29*/
    if let Err(e) = std::fs::write("/proc/self/oom_score_adj", "-900") {
        log.warn(&format!("oom_score_adj -900 not set: {}", e.kind()));
    }
    let code = match signals {
        Ok(signals) => wiring::run(&env, &clock, &log, signals),
        Err(e) => {
            log.error(&format!("signal handlers not installed: {e}"));
            70
        }
    };
    log.info(&format!("exit code={code}"));
    /*MUT27*/ std::process::exit(code);
}
