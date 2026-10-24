//! The chaos suite's plan and table (C14): `harness/run-chaos.sh` over `harness/chaos.json`.
//!
//! The plan must name every chaos case file, expand each leg into its repeats, and refuse an
//! unknown leg. The table goes red on a FAIL, an ERROR, a MISSING verdict, a false-positive sweep
//! that some leg calling it did not run, and a NOT RUNNABLE leg (the PR #52 review's I6: an unrun leg
//! is never green; case 16's hold wave gate 3 BLOCKED until the owner rules Owed 82). Its last line
//! says whether the run counts toward wave gate 3. The expected legs come from chaos.json and the
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
/// calls it, and an outage on the restart cases. Case 16's legs are NOT RUNNABLE, as they are today,
/// unless `leaks_run` (a run after the owner's ruling on Owed 82, so that a red elsewhere is the only
/// red).
fn verdicts(dir: &Path, leaks_run: bool) {
    let _ = std::fs::remove_dir_all(dir);
    for (k, (name, case)) in expected_legs().into_iter().enumerate() {
        let d = dir.join(format!("verdict-{name}"));
        std::fs::create_dir_all(&d).unwrap();
        let v = if case == "cef-leaks" && !leaks_run {
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

fn not_runnable_legs() -> Vec<String> {
    expected_legs()
        .into_iter()
        .filter(|l| l.1 == "cef-leaks")
        .map(|l| l.0)
        .collect()
}

#[test]
fn the_table_holds_wave_gate_3_blocked_while_a_leg_is_not_runnable() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chaos-table-blocked");
    let d = dir.to_str().unwrap();
    let legs = expected_legs();
    let unrun = not_runnable_legs();
    assert_eq!(unrun.len(), 2, "case 16's two variants");

    verdicts(&dir, false);
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "NOT RUNNABLE is never green");
    let rows = s
        .lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| leg "))
        .count();
    eprintln!("table rows checked: {rows}");
    assert_eq!(rows, legs.len());
    assert_eq!(
        s.lines().filter(|l| l.contains("| NOT RUNNABLE |")).count(),
        unrun.len()
    );
    let passing = legs.len() - unrun.len();
    assert!(s.contains(&format!(
        "legs: {}, pass: {passing}, red: 0, not runnable: {}",
        legs.len(),
        unrun.len()
    )));
    assert!(
        s.contains(&format!(
            "WAVE GATE 3: BLOCKED: {} NOT RUNNABLE ({}; Owed 82",
            unrun.len(),
            unrun.join(" ")
        )),
        "the gate line names the unrun legs"
    );
    assert!(s.contains(&format!(
        "WAVE GATE 3: this run does not count ({passing} of {} legs PASS)",
        legs.len()
    )));
    assert!(!s.contains("this run counts"));

    // One NOT RUNNABLE leg alone is as red.
    let out = run(&["--table", d, &unrun[1]]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("WAVE GATE 3: BLOCKED: 1 NOT RUNNABLE"));
}

#[test]
fn the_table_reds_on_a_fail_an_error_a_missing_verdict_or_a_missed_sweep() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chaos-table");
    let d = dir.to_str().unwrap();
    let legs = expected_legs();
    let sweeps: Vec<&(String, String)> = legs.iter().filter(|l| calls_the_sweep(&l.1)).collect();
    eprintln!("legs that call the sweep: {}", sweeps.len());
    assert!(!sweeps.is_empty());

    // Every leg passes (case 16's too, as after the owner's ruling): the only run that counts.
    verdicts(&dir, true);
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(s.contains(&format!(
        "the false-positive sweep ran on {n} of the {n} legs",
        n = sweeps.len()
    )));
    assert!(s.contains(&format!(
        "WAVE GATE 3: this run counts ({n} of {n} legs PASS)",
        n = legs.len()
    )));
    // The worst of cef-killed's 3 runs: its third leg's outage, the highest (verdicts' 7 000 + k).
    let k = legs.iter().position(|l| l.0 == "cef-killed-r3").unwrap();
    assert!(s.contains(&format!("worst outage_ms, cef-killed: {} over 3 runs", 7_000 + k)));

    let mut reds = 0;
    edit(&dir, "dest-stall", |v| {
        v["status"] = json!("fail");
        v["failed"] = json!(["stall_swap"]);
    });
    let out = run(&["--table", d]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("| dest-stall | FAIL | 4/4 | outage_ms | failed: stall_swap (Owed 87) |"));
    assert!(stdout(&out).contains("this run does not count"));
    reds += 1;

    verdicts(&dir, true);
    std::fs::remove_dir_all(dir.join("verdict-cef-killed-r2")).unwrap();
    let out = run(&["--table", d]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("| cef-killed-r2 | MISSING |"));
    reds += 1;

    verdicts(&dir, true);
    edit(&dir, "camera-stop", |v| v["status"] = json!("error"));
    let out = run(&["--table", d]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("| camera-stop | ERROR (error) |"));
    reds += 1;

    // The sweep missed on ONE leg that calls it (the review's m5: on all, not on any).
    verdicts(&dir, true);
    edit(&dir, &sweeps[0].0, |v| v["fp_swept"] = json!(false));
    let out = run(&["--table", d]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains(&format!(
        "FAIL the false-positive sweep ran on {} of the {} legs that call it",
        sweeps.len() - 1,
        sweeps.len()
    )));
    reds += 1;
    eprintln!("table reds checked: {reds}");

    // A selection with no sweep (the restart cases call none) is not held to one.
    verdicts(&dir, false);
    let out = run(&["--table", d, "cef-killed"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("legs: 3, pass: 3, red: 0, not runnable: 0"));
    assert!(stdout(&out).contains("WAVE GATE 3: this run counts (3 of 3 legs PASS)"));
}

/// The checks a case's verdict can fail: every `require`/`require_run` name in its file, in the case
/// files it sources, and in run-case.sh's helpers it calls (the names those helpers declare).
fn declared_checks(case: &str) -> Vec<String> {
    let read = |p: &str| std::fs::read_to_string(repo().join(p)).unwrap();
    let mut text = read(&format!("harness/cases/{case}.sh"));
    if text.contains("cases/cef-gpu-zygote-killed.sh") {
        text.push_str(&read("harness/cases/cef-gpu-zygote-killed.sh"));
    }
    text.push_str(&read("harness/run-case.sh"));
    let mut names = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        for verb in ["require ", "require_run "] {
            if let Some(rest) = t.strip_prefix(verb) {
                if let Some(n) = rest.split_whitespace().next() {
                    names.push(n.to_owned());
                }
            }
        }
    }
    names
}

/// C14 fix round 3, item 4: chaos.json names the Owed row behind each check a leg is known to fail,
/// so a red table reads at a glance. Every name must be a check that leg can fail, and every row
/// must exist in docs/_INDEX.md with a title here: an owed entry for a check that no longer exists,
/// or a row that was never written, would make an unexplained red look explained.
#[test]
fn every_owed_entry_names_a_real_check_and_a_real_owed_row() {
    let m = manifest();
    let titles = m["owed_titles"].as_object().expect("chaos.json has owed_titles");
    let index = std::fs::read_to_string(repo().join("docs/_INDEX.md")).unwrap();
    let mut checked = 0;
    for l in m["legs"].as_array().unwrap() {
        let case = l["case"].as_str().unwrap();
        let Some(owed) = l.get("owed") else { continue };
        let checks = declared_checks(case);
        for (check, row) in owed.as_object().unwrap() {
            assert!(
                checks.contains(check),
                "{case}: owed check {check} is not a check the case declares"
            );
            let row = row.as_str().unwrap();
            assert!(titles.contains_key(row), "Owed {row} has no title in chaos.json");
            let n = row.split(' ').next().unwrap();
            assert!(
                index.contains(&format!("\n| {n} | ")),
                "Owed {n} is not a row of docs/_INDEX.md"
            );
            checked += 1;
        }
    }
    eprintln!("owed entries checked: {checked}");
    assert!(
        checked >= 10,
        "the known reds of round 2 and 3 are listed ({checked})"
    );
}

/// A red table names each failed check's Owed row, and says which reds no Owed row explains. No
/// verdict is softened: an owed red is as red as any other, and the run never counts.
#[test]
fn the_table_names_each_failed_checks_owed_row_and_stays_red() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chaos-table-owed");
    let d = dir.to_str().unwrap();
    let m = manifest();
    let title = |row: &str| m["owed_titles"][row].as_str().unwrap().to_owned();

    verdicts(&dir, true);
    // CI's dest-stall (rounds 2 and 3): every failed check is owed.
    edit(&dir, "dest-stall", |v| {
        v["status"] = json!("fail");
        v["failed"] = json!(["stall_swap", "output_lost"]);
    });
    // A check no Owed row names: the news in the run.
    edit(&dir, "camera-stop", |v| {
        v["status"] = json!("fail");
        v["failed"] = json!(["slate_ms"]);
    });
    // A leg with owed checks that failed on another one too.
    edit(&dir, "cef-killed-r1", |v| {
        v["status"] = json!("fail");
        v["failed"] = json!(["overlay_back", "same_session"]);
    });
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "an owed red is still red");
    assert!(s.contains(
        "| dest-stall | FAIL | 4/4 | outage_ms | failed: stall_swap (Owed 87), output_lost (Owed 84) |"
    ));
    assert!(s.contains("| camera-stop | FAIL | 4/4 | outage_ms | failed: slate_ms (NOT OWED) |"));
    assert!(s.contains("| cef-killed-r1 | FAIL | 4/4 | outage_ms | failed: overlay_back (Owed 85), same_session (NOT OWED) |"));
    assert!(s.contains("red, every failed check owed: dest-stall\n"), "{s}");
    assert!(s.contains(
        "red on a check no Owed row names: camera-stop (slate_ms); cef-killed-r1 (same_session)\n"
    ));
    for row in ["84", "85", "87"] {
        assert!(
            s.contains(&format!("Owed {row}: {}\n", title(row))),
            "Owed {row}'s title"
        );
    }
    assert!(!s.contains("Owed 86:"), "only the rows this run's reds name");
    assert!(s.contains("this run does not count"));
    assert!(!s.contains("this run counts"));

    // Only owed reds: still red, and the run still does not count.
    verdicts(&dir, true);
    edit(&dir, "dest-stall", |v| {
        v["status"] = json!("fail");
        v["failed"] = json!(["stall_swap"]);
    });
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(1));
    assert!(s.contains("red on a check no Owed row names: none\n"));
    assert!(s.contains("WAVE GATE 3: this run does not count"));

    // A green run prints neither line.
    verdicts(&dir, true);
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(!s.contains("every failed check owed") && !s.contains("no Owed row names"));
    eprintln!("owed table rows checked: 3, then 1, then 0");
}
