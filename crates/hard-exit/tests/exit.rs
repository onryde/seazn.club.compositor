//! `now` runs no `atexit` handler, and `std::process::exit` does (the control): each child is this
//! test binary run again with `HARD_EXIT_CHILD` set, registering a handler that writes a marker to
//! stderr, then exiting with 7.
#![allow(unsafe_code)] // registers the `atexit` handler under test (libc::atexit has no safe binding)

use std::process::Command;

const MARKER: &str = "atexit handler ran";

extern "C" fn handler() {
    eprintln!("{MARKER}");
}

/// The child half: does nothing unless `HARD_EXIT_CHILD` names how to exit.
#[test]
fn child() {
    let Ok(how) = std::env::var("HARD_EXIT_CHILD") else {
        return;
    };
    // SAFETY: `handler` is an `extern "C" fn()` that lives for the whole process.
    let registered = unsafe { libc::atexit(handler) };
    assert_eq!(registered, 0, "atexit refused");
    eprintln!("child exiting through {how}");
    match how.as_str() {
        "now" => hard_exit::now(7),
        "std" => std::process::exit(7),
        other => panic!("unknown HARD_EXIT_CHILD {other}"),
    }
}

fn run(how: &str) -> (Option<i32>, String) {
    let out = Command::new(std::env::current_exe().expect("current_exe"))
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env("HARD_EXIT_CHILD", how)
        .output()
        .expect("spawn the child");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn now_exits_with_the_code_and_runs_no_atexit_handler() {
    let mut checked = 0;
    for (how, handler_runs) in [("std", true), ("now", false)] {
        let (code, err) = run(how);
        assert!(
            err.contains(&format!("child exiting through {how}")),
            "{how}: the child never ran:\n{err}"
        );
        assert_eq!(code, Some(7), "{how}: {err}");
        assert_eq!(err.contains(MARKER), handler_runs, "{how}: {err}");
        checked += 1;
    }
    eprintln!("exit paths checked: {checked}");
    assert_eq!(checked, 2);
}
