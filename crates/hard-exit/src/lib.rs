//! The compositor's one way out of the process (F-017; the PR #36 review's C1).
//!
//! `std::process::exit` is libc's `exit(3)`: it runs every `atexit` handler and C++ static
//! destructor, libcef's and GLib's among them, while CEF's and GStreamer's threads are still running
//! (the adapters' drops start their teardowns and join nothing, by design: C08 and C11 amendments of
//! 2026-10-19). On image.yml run 39410132377 that race ended a clean run with glibc's `double free or
//! corruption (!prev)` after its `exit code=0` line: status 134, which web counts as a crash.
//!
//! [`now`] is `_exit(2)`: the kernel ends every thread at once and no handler runs. It flushes
//! stderr and stdout first; the final beat has already been sent (`app::run::run` returns after
//! `flush_final_beats`).
//!
//! The workspace forbids `unsafe`. This crate is its one sanctioned exception, for the one call
//! below: no safe binding exists (rustix keeps its `exit_group` in an unstable, hidden module, and
//! nix has no `_exit`).
#![deny(unsafe_code)]

use std::io::Write;

/// Ends the process with `code` without running any exit handler or destructor.
pub fn now(code: i32) -> ! {
    let _ = std::io::stderr().flush();
    let _ = std::io::stdout().flush();
    // SAFETY: `_exit` accepts any status and never returns. Skipping the exit handlers is its
    // purpose; it reads and writes no Rust-owned memory.
    #[allow(unsafe_code)]
    unsafe {
        libc::_exit(code)
    }
}
