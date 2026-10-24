//! The chaos suite's plan and table (C14): `harness/run-chaos.sh` over `harness/chaos.json`.
//!
//! The plan must name every chaos case file, expand each leg into its repeats, and refuse an
//! unknown leg. The table goes red on a FAIL, a MISSING verdict, or a false-positive sweep that ran
//! nowhere. A NOT RUNNABLE leg is reported, never red. The expected legs come from chaos.json and the
//! case files, never from the script's output. Needs bash 4 or later (HARNESS_BASH, as in
//! case_verdicts.rs) and jq.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bash() -> String {
    let bash = std::env::var("HARNESS_BASH").unwrap_or_else(|_| "bash".into());
    let out = Command::new(&bash)
        .args(["-c", "echo ${BASH_VERSINFO[0]}"])
        .output()
        .expect("bash runs");
    let major: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    assert!(
        major >= 4,
        "run-chaos.sh needs bash 4 or later; {bash} is {major} (set HARNESS_BASH)"
    );
    bash
}

fn run(args: &[&str]) -> Output {
    let out = Command::new(bash())
        .arg(repo().join("harness/run-chaos.sh"))
        .args(args)
        .output()
        .expect("run-chaos.sh runs");
    eprintln!(
        "--- run-chaos.sh {args:?}: {:?}\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn manifest() -> Value {
    serde_json::from_str(&std::fs::read_to_string(repo().join("harness/chaos.json")).unwrap()).unwrap()
}

/// The legs chaos.json declares, with their repeats: (name, case).
fn expected_legs() -> Vec<(String, String)> {
    let mut v = Vec::new();
    for l in manifest()["legs"].as_array().unwrap() {
        let case = l["case"].as_str().unwrap();
        let leg = match l["variant"].as_str() {
            Some(variant) => format!("{case}-{variant}"),
            None => case.to_owned(),
        };
        let runs = l["runs"].as_u64().unwrap();
        if runs > 1 {
            v.extend((1..=runs).map(|r| (format!("{leg}-r{r}"), case.to_owned())));
        } else {
            v.push((leg, case.to_owned()));
        }
    }
    v
}

fn calls_the_sweep(case: &str) -> bool {
    std::fs::read_to_string(repo().join(format!("harness/cases/{case}.sh")))
        .unwrap()
        .contains("require_no_false_restart")
}

#[test]
fn the_plan_names_every_chaos_case_and_expands_the_repeats() {
    let legs = expected_legs();
    let mut cases: Vec<String> = legs.iter().map(|l| l.1.clone()).collect();
    cases.dedup();
    let mut files: Vec<String> = std::fs::read_dir(repo().join("harness/cases"))
        .unwrap()
        .filter_map(|e| {
            e.unwrap()
                .file_name()
                .into_string()
                .unwrap()
                .strip_suffix(".sh")
                .map(str::to_owned)
        })
        .filter(|c| c != "smoke") // C13's own end-to-end case (harness.yml), not a chaos case
        .collect();
    files.sort();
    let mut sorted = cases.clone();
    sorted.sort();
    eprintln!(
        "chaos cases: {} in chaos.json, {} case files",
        sorted.len(),
        files.len()
    );
    assert_eq!(sorted, files, "chaos.json and harness/cases/*.sh disagree");
    assert_eq!(files.len(), 16, "the brief's 16 cases (amended 2026-10-20)");

    let out = run(&["--legs", "all"]);
    assert_eq!(out.status.code(), Some(0));
    let plan: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = plan.iter().map(|l| l["name"].as_str().unwrap()).collect();
    let want: Vec<&str> = legs.iter().map(|l| l.0.as_str()).collect();
    eprintln!("legs planned: {}", names.len());
    assert_eq!(names, want);
    assert!(plan
        .iter()
        .all(|l| l["case"].is_string() && l["variant"].is_string() && l["run"].is_string()));

    let out = run(&["--legs", "camera-stop, cef-gpu-deaths-fast"]);
    let plan: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = plan.iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "camera-stop",
            "cef-gpu-deaths-fast-r1",
            "cef-gpu-deaths-fast-r2",
            "cef-gpu-deaths-fast-r3"
        ]
    );
    assert_eq!(plan[1]["variant"], "fast");
    assert_eq!(plan[1]["run"], "1");

    let out = run(&["--legs", "camera-stop,no-such-leg"]);
    assert_eq!(
        out.status.code(),
        Some(64),
        "an unknown leg is refused, never dropped"
    );
}

/// A verdict.json for every leg, as run-case.sh writes it: passing, with the sweep where the case
/// calls it, and an outage on the restart cases.
fn verdicts(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    for (k, (name, case)) in expected_legs().into_iter().enumerate() {
        let d = dir.join(format!("verdict-{name}"));
        std::fs::create_dir_all(&d).unwrap();
        let v = if case == "cef-leaks" {
            json!({"name": name, "case": case, "status": "not_runnable", "checks_declared": 1,
                "checks_run": 0, "failed": [], "measures": {}, "key": "", "fp_swept": false,
                "not_runnable_reason": "needs the hold_teardowns hook"})
        } else {
            json!({"name": name, "case": case, "status": "pass", "checks_declared": 4, "checks_run": 4,
                "failed": [], "measures": {"outage_ms": 7_000 + k as i64, "detect_ms": {"x": 1}},
                "key": "outage_ms", "fp_swept": calls_the_sweep(&case), "not_runnable_reason": null})
        };
        std::fs::write(d.join("verdict.json"), v.to_string()).unwrap();
    }
}

fn edit(dir: &Path, name: &str, f: impl Fn(&mut Value)) {
    let p = dir.join(format!("verdict-{name}/verdict.json"));
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    f(&mut v);
    std::fs::write(p, v.to_string()).unwrap();
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn the_table_reds_on_a_fail_a_missing_verdict_or_a_sweep_that_ran_nowhere() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chaos-table");
    let d = dir.to_str().unwrap();
    let legs = expected_legs();
    let sweeps = legs
        .iter()
        .filter(|l| l.1 != "cef-leaks" && calls_the_sweep(&l.1))
        .count();
    assert!(sweeps > 0);

    verdicts(&dir);
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(0));
    let rows = s
        .lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| leg "))
        .count();
    eprintln!("table rows checked: {rows}");
    assert_eq!(rows, legs.len());
    assert_eq!(s.lines().filter(|l| l.contains("| NOT RUNNABLE |")).count(), 2);
    assert!(s.contains(&format!(
        "the false-positive sweep ran on {sweeps} of the {sweeps} legs"
    )));
    // The worst of cef-killed's 3 runs: its third leg's outage, the highest (verdicts' 7 000 + k).
    let k = legs.iter().position(|l| l.0 == "cef-killed-r3").unwrap();
    assert!(s.contains(&format!("worst outage_ms, cef-killed: {} over 3 runs", 7_000 + k)));

    edit(&dir, "dest-stall", |v| {
        v["status"] = json!("fail");
        v["failed"] = json!(["stall_swap"]);
    });
    let out = run(&["--table", d]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("| dest-stall | FAIL | 4/4 | outage_ms | failed: stall_swap |"));

    verdicts(&dir);
    std::fs::remove_dir_all(dir.join("verdict-cef-killed-r2")).unwrap();
    let out = run(&["--table", d]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("| cef-killed-r2 | MISSING |"));

    verdicts(&dir);
    edit(&dir, "camera-stop", |v| v["status"] = json!("error"));
    assert_eq!(run(&["--table", d]).status.code(), Some(1));

    verdicts(&dir);
    for (name, case) in &legs {
        if case != "cef-leaks" {
            edit(&dir, name, |v| v["fp_swept"] = json!(false));
        }
    }
    let out = run(&["--table", d]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("FAIL the false-positive sweep ran on no leg"));

    // A selection with no sweep (the restart cases call none) is not held to one.
    verdicts(&dir);
    let out = run(&["--table", d, "cef-killed"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("legs: 3, red: 0"));
}
