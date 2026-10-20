//! The compositor binary's composition root (C12). The binary (`main.rs`) is the only place that
//! reads `std::env`, the system clock or signals; these modules are its pieces, in a library so the
//! tests can drive them: `env` parses the Machine's environment, `clock` is the process's one clock,
//! `logged` holds the stderr log and the logging decorators, and `wiring` builds every port and runs
//! the loop.
pub mod clock;
pub mod env;
pub mod logged;
pub mod wiring;

/// R1's stop grace: `seazn.club` `apps/web/src/server/relay/config.ts` `RUNNER_STOP_GRACE_SECONDS`
/// (20 since the owner's ruling D15, 2026-10-16, spec 2026-10-15-overlay-templates-and-scenes, PR #969;
/// set on web main by `568b24cd6`, re-pinned at `dec7b95d9`), the time between the host's stop signal and
/// its SIGKILL. Here, not in `main.rs` as the brief placed it, so `tests/env.rs` can read it (a binary
/// target exports nothing).
pub const R1_STOP_GRACE_MS: u64 = 20_000;

/// The headroom `config.rs`'s `drain_timeout_ms` doc line keeps inside the grace ("3 s"), for EOS to
/// reach the sink and the process to exit.
pub const EXIT_HEADROOM_MS: u64 = 3_000;
