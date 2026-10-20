//! The compositor (C12): reads the Machine's environment, builds every adapter and runs
//! `app::run::run`. The only place that reads `std::env`, the system clock or signals.
//!
//! 1. RLIMIT_CORE 0, first (Owed 68): Chromium's abort is a SIGTRAP with a core, and a 93–117 MB core
//!    of this process holds the page token (46 and 61 times in the rig), the stream key and the job
//!    token. CEF's children inherit the limit.
//! 2. The environment; a refusal exits 78 (EX_CONFIG) naming the key, never a value, and sends no
//!    beat (there is no URL or token to send one with yet).
//! 3. `oom_score_adj` -900 (spec §6); a non-root process may be refused, which is logged once.
//! 4. `wiring::run`, whose code is the process's: 0, 70 (fatal), 75 (C11b's in-place restart,
//!    EX_TEMPFAIL, which tini passes through so the host's `on-failure` restarts the process), or 78.
//!    `run()` returns only after the final beat posted or `FINAL_BEAT_TIMEOUT_MS` passed (Owed 28),
//!    so no wait is owed here.
use std::sync::Arc;

use compositor::clock::Clock;
use compositor::env;
use compositor::logged::Logger;
use compositor::wiring;
use rustix::process::{setrlimit, Resource, Rlimit};

fn main() {
    let core: Result<(), std::io::Error> = /*MUT*/ Ok(()); let _ = (
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    );
    let clock = Arc::new(Clock::system());
    let log = Logger::stderr(Arc::clone(&clock));
    if let Err(e) = core {
        log.warn(&format!("RLIMIT_CORE 0 not set: {e}"));
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
            std::process::exit(78);
        }
    };
    if let Err(e) = std::fs::write("/proc/self/oom_score_adj", "-900") {
        log.warn(&format!("oom_score_adj -900 not set: {}", e.kind()));
    }
    let code = wiring::run(&env, &clock, &log);
    log.info(&format!("exit code={code}"));
    std::process::exit(code);
}
