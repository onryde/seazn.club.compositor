//! The chaos suite's plan and table (C14): `harness/run-chaos.sh` over `harness/chaos.json`.
//!
//! The plan must name every chaos case file, expand each leg into its repeats, and refuse an
//! unknown leg. The table goes red on a FAIL, an ERROR, a MISSING verdict, a false-positive sweep
//! that some leg calling it did not run, and a NOT RUNNABLE leg (the PR #52 review's I6: an unrun leg
//! is never green, and blocks wave gate 3). Its last line
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

/// The head the table is told it judges (chaos.yml's CHAOS_HEAD_SHA).
const HEAD: &str = "8b71aef6b14436b80f30857adb31aafcb11654de";

/// run-chaos.sh as a run's first attempt (GITHUB_RUN_ATTEMPT 1).
fn run(args: &[&str]) -> Output {
    run_attempt(args, Some("1"))
}

/// run-chaos.sh with `attempt` as GITHUB_RUN_ATTEMPT (None: not a CI run). The runner's own
/// GITHUB_* values never leak in: check.yml runs this test inside a CI job.
fn run_attempt(args: &[&str], attempt: Option<&str>) -> Output {
    run_env(args, attempt, None)
}

/// run-chaos.sh over another manifest (CHAOS_MANIFEST), as a run's first attempt.
fn run_manifest(args: &[&str], manifest: &Path) -> Output {
    run_env(args, Some("1"), Some(manifest))
}

fn run_env(args: &[&str], attempt: Option<&str>, manifest: Option<&Path>) -> Output {
    let mut c = Command::new(bash());
    c.arg(repo().join("harness/run-chaos.sh"))
        .args(args)
        .env_remove("GITHUB_RUN_ATTEMPT")
        .env_remove("GITHUB_SHA")
        .env_remove("CHAOS_MANIFEST")
        .env("CHAOS_HEAD_SHA", HEAD);
    if let Some(a) = attempt {
        c.env("GITHUB_RUN_ATTEMPT", a);
    }
    if let Some(m) = manifest {
        c.env("CHAOS_MANIFEST", m);
    }
    let out = c.output().expect("run-chaos.sh runs");
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
        .collect();
    // The case files that are not chaos cases (case_verdicts.rs's NOT_CHAOS, the same list): C13's
    // smoke and Owed 81's two probe controls, each run by hand from harness.yml. Each must be a file.
    let not_chaos = ["smoke", "owed81-noaudio", "owed81-align1"];
    let before = files.len();
    files.retain(|c| !not_chaos.contains(&c.as_str()));
    assert_eq!(
        before - files.len(),
        not_chaos.len(),
        "a non-chaos name with no case file"
    );
    files.sort();
    let mut sorted = cases.clone();
    sorted.sort();
    eprintln!(
        "chaos cases: {} in chaos.json, {} case files",
        sorted.len(),
        files.len()
    );
    assert_eq!(sorted, files, "chaos.json and harness/cases/*.sh disagree");
    assert_eq!(
        files.len(),
        18,
        "the brief's 16 cases (amended 2026-10-20), case 17 (2026-10-24) and case 18 (Owed 94)"
    );

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
/// calls it, and an outage on the restart cases. Unless `all_run`, case 16's legs stand in as NOT
/// RUNNABLE legs (no case is since case 16 was wired to Owed 82's hook; the mechanism stays, and
/// case_verdicts.rs probes run-case.sh's side of it), so that a red elsewhere is the only red.
fn verdicts(dir: &Path, all_run: bool) {
    let _ = std::fs::remove_dir_all(dir);
    for (k, (name, case)) in expected_legs().into_iter().enumerate() {
        let d = dir.join(format!("verdict-{name}"));
        std::fs::create_dir_all(&d).unwrap();
        let v = if case == "cef-leaks" && !all_run {
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
    assert_eq!(unrun.len(), 2, "case 16's two variants, standing in");

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
            "WAVE GATE 3: BLOCKED: {} NOT RUNNABLE ({}; an unrun leg blocks the gate",
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

    // Every leg passes (case 16's too): the only run that counts.
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
    // Case 6 owes nothing since the owner's ruling (2026-10-25, "A"): a red there is news.
    edit(&dir, "cpu-starved", |v| {
        v["status"] = json!("fail");
        v["failed"] = json!(["alarm_raised"]);
    });
    let out = run(&["--table", d]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout(&out).contains("| cpu-starved | FAIL | 4/4 | outage_ms | failed: alarm_raised (NOT OWED) |")
    );
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

    // A selection with no sweep (the restart cases call none) is not held to one, and is green for
    // what it ran. It never counts (the re-review's I-2: it was pinned as "this run counts").
    verdicts(&dir, false);
    let out = run(&["--table", d, "cef-killed"]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(s.contains("legs: 3, pass: 3, red: 0, not runnable: 0"));
    assert!(s.contains(&format!(
        "WAVE GATE 3: a partial run (3 of {} legs) never counts (3 of 3 legs PASS)",
        legs.len()
    )));
    assert!(!s.contains("this run counts"), "{s}");
    // Every leg named one by one is the whole manifest: that is a full run.
    verdicts(&dir, true);
    let all: Vec<String> = manifest()["legs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| match l["variant"].as_str() {
            Some(v) => format!("{}-{v}", l["case"].as_str().unwrap()),
            None => l["case"].as_str().unwrap().to_owned(),
        })
        .collect();
    let out = run(&["--table", d, &all.join(",")]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("WAVE GATE 3: this run counts"));
}

/// The re-review's m-7: the table names the run it judges, and only a first attempt counts. A
/// "re-run failed jobs" attempt keeps the passing legs of the attempt before, so it is no fresh run.
#[test]
fn only_a_first_attempt_counts_and_the_table_names_its_run() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chaos-table-attempt");
    let d = dir.to_str().unwrap();
    let n = expected_legs().len();
    verdicts(&dir, true);
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        s.contains(&format!("run: attempt 1, head {HEAD}, legs {n} of {n}\n")),
        "{s}"
    );
    assert!(s.contains("WAVE GATE 3: this run counts"));
    let mut checked = 1;
    for (attempt, shown) in [(Some("2"), "2"), (None, "none")] {
        let out = run_attempt(&["--table", d], attempt);
        let s = stdout(&out);
        assert_eq!(out.status.code(), Some(0), "every leg passed");
        assert!(s.contains(&format!(
            "WAVE GATE 3: attempt {shown} never counts: only a run's first attempt does ({n} of {n} legs PASS)"
        )));
        assert!(!s.contains("this run counts"), "attempt {shown}: {s}");
        checked += 1;
    }
    eprintln!("attempts checked: {checked}");
}

/// The checks and measures a case's verdict records: every `require`/`require_run`/`measure` name in
/// its file, in the case files it sources, and in the run-case.sh helpers it calls, transitively
/// (a helper's names count only for a case that calls it: a signature over `clear_phase` is no
/// signature for a case that never calls `require_output_recovers`).
fn declared_checks(case: &str) -> Vec<String> {
    let read = |p: &str| std::fs::read_to_string(repo().join(p)).unwrap();
    let mut text = read(&format!("harness/cases/{case}.sh"));
    if text.contains("cases/cef-gpu-zygote-killed.sh") {
        text.push_str(&read("harness/cases/cef-gpu-zygote-killed.sh"));
    }
    // run-case.sh's `require_*` helpers: name -> body, each from `name() {` to its closing `}`.
    let lib = read("harness/run-case.sh");
    let mut helpers: Vec<(String, String)> = Vec::new();
    let mut open: Option<(String, String)> = None;
    for line in lib.lines() {
        if let Some((_, body)) = open.as_mut() {
            if line == "}" {
                helpers.push(open.take().unwrap());
            } else {
                body.push_str(line);
                body.push('\n');
            }
        } else if let Some(name) = line.strip_suffix("() {") {
            if name.starts_with("require_") {
                open = Some((name.to_owned(), String::new()));
            }
        }
    }
    assert!(
        helpers.len() >= 6,
        "run-case.sh's require_* helpers found: {}",
        helpers.len()
    );
    let calls = |t: &str, name: &str| {
        t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| w == name)
    };
    let mut included: Vec<String> = Vec::new();
    loop {
        let before = included.len();
        for (name, body) in &helpers {
            if !included.contains(name) && calls(&text, name) {
                included.push(name.clone());
                text.push_str(body);
            }
        }
        if included.len() == before {
            break;
        }
    }
    let mut names = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        for verb in ["require ", "require_run ", "measure "] {
            if let Some(rest) = t.strip_prefix(verb) {
                if let Some(n) = rest.split_whitespace().next() {
                    names.push(n.to_owned());
                }
            }
        }
    }
    names
}

/// The rows an `owed` value names: one row, or a list tried in order.
fn rows_of(v: &Value) -> Vec<String> {
    match v {
        Value::String(r) => vec![r.clone()],
        Value::Array(a) => a.iter().map(|r| r.as_str().unwrap().to_owned()).collect(),
        other => panic!("an owed value is a row or a list of rows: {other}"),
    }
}

/// The `.measures.<name>` a signature reads.
fn measures_read(sig: &str) -> Vec<String> {
    sig.split(".measures.")
        .skip(1)
        .map(|r| {
            r.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect()
        })
        .collect()
}

/// Whether `sig` holds on `verdict` under `jq -e`: Some(true/false), None when it does not compile.
fn signature_holds(sig: &str, verdict: &Value) -> Option<bool> {
    let mut c = Command::new("jq");
    c.args(["-e", sig])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = c.spawn().expect("jq runs");
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(verdict.to_string().as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    if String::from_utf8_lossy(&out.stderr).contains("compile error") || out.status.code() == Some(3) {
        return None;
    }
    Some(out.status.success())
}

/// C14 fix round 3, item 4: chaos.json names the Owed row behind each check a leg is known to fail,
/// so a red table reads at a glance. Every name must be a check that leg can fail, and every row
/// must exist in docs/_INDEX.md, still open, with a title here: an owed entry for a check that no
/// longer exists, a row that was never written, or a row since CLOSED (the re-review's m-6: had Owed
/// 85's tags stayed after #54 fixed it, a later regression of `overlay_back` would read "(Owed 85)")
/// would make an unexplained red look explained. And each row has a signature (the re-review's I-3) that compiles, is false
/// on a verdict that measured nothing (absent evidence is no cause), and reads only measures the
/// leg's case records (a signature over a measure no case writes could never hold).
/// These checks over manifest `m` and the Owed table `index`: (owed entries checked, signature
/// reads checked). Panics on the first entry that fails one.
fn owed_entries_checked(m: &Value, index: &str) -> (usize, usize) {
    let titles = m["owed_titles"].as_object().expect("chaos.json has owed_titles");
    let sigs = m["owed_signatures"]
        .as_object()
        .expect("chaos.json has owed_signatures");
    let (mut checked, mut sig_reads) = (0, 0);
    for l in m["legs"].as_array().unwrap() {
        let case = l["case"].as_str().unwrap();
        let Some(owed) = l.get("owed") else { continue };
        let checks = declared_checks(case);
        for (check, rows) in owed.as_object().unwrap() {
            assert!(
                checks.contains(check),
                "{case}: owed check {check} is not a check the case declares"
            );
            for row in rows_of(rows) {
                assert!(titles.contains_key(&row), "Owed {row} has no title in chaos.json");
                let n = row.split(' ').next().unwrap();
                let line = index
                    .lines()
                    .find(|x| x.starts_with(&format!("| {n} | ")))
                    .unwrap_or_else(|| panic!("Owed {n} is not a row of docs/_INDEX.md"));
                assert!(
                    !line.contains("CLOSED"),
                    "Owed {n} is CLOSED in docs/_INDEX.md: its tags leave chaos.json"
                );
                let sig = sigs
                    .get(&row)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("Owed {row} has no signature in chaos.json"));
                let read = measures_read(sig);
                assert!(!read.is_empty(), "Owed {row}'s signature reads no measure");
                for name in read {
                    assert!(
                        checks.contains(&name),
                        "{case}: Owed {row}'s signature reads .measures.{name}, which the case never records"
                    );
                    sig_reads += 1;
                }
                checked += 1;
            }
        }
    }
    for (row, sig) in sigs {
        let sig = sig.as_str().unwrap();
        for empty in [json!({"measures": {}}), json!({})] {
            assert_eq!(
                signature_holds(sig, &empty),
                Some(false),
                "Owed {row}'s signature compiles and holds on no evidence: {sig}"
            );
        }
        assert!(titles.contains_key(row), "Owed {row}'s signature has no title");
    }
    assert_eq!(sigs.len(), titles.len(), "a signature for every titled row");
    assert!(
        sig_reads >= checked,
        "every entry's signature reads a measure ({sig_reads})"
    );
    (checked, sig_reads)
}

#[test]
fn every_owed_entry_names_a_real_check_and_a_real_open_owed_row_with_a_signature() {
    // The committed manifest: no Owed row is open in chaos.json since Owed 86 left it by the owner's
    // ruling (2026-10-25, "A": case 6's clock measured, not judged), as Owed 87 did (2026-10-24, "A").
    // A new row's tags move this count.
    let m = manifest();
    let index = std::fs::read_to_string(repo().join("docs/_INDEX.md")).unwrap();
    let (checked, sig_reads) = owed_entries_checked(&m, &index);
    eprintln!("committed owed entries checked: {checked}; signature reads: {sig_reads}");
    assert_eq!(
        (checked, sig_reads),
        (0, 0),
        "an Owed row's tags are back in chaos.json: this test's committed count moves with them"
    );
    // The same checks over a probe entry, so the sweep is not read on nothing: case 6's alarm_raised
    // owed to an open test row whose signature reads the case's own measure.
    let mut probe = m.clone();
    probe["owed_signatures"]["98"] = json!(".measures.alarm_raised.raises >= 1");
    probe["owed_titles"]["98"] = json!("a probe row");
    let mut legs = 0;
    for l in probe["legs"].as_array_mut().unwrap() {
        if l["case"] == "cpu-starved" {
            l["owed"] = json!({"alarm_raised": "98"});
            legs += 1;
        }
    }
    assert_eq!(legs, 1, "case 6 is one leg of chaos.json");
    let (checked, sig_reads) =
        owed_entries_checked(&probe, "| 98 | a probe row, open | C14 | the coordinator |\n");
    eprintln!("probe owed entries checked: {checked}; signature reads: {sig_reads}");
    assert_eq!((checked, sig_reads), (1, 1));
}

/// The rows decided by a ruling left chaos.json with their tags, so a red on their old checks reads
/// NOT OWED: Owed 87 (the owner, 2026-10-24, "A": the stall rule retired) and Owed 86 (the owner,
/// 2026-10-25, "A": case 6's clock measured, not judged, under the induced starvation). No leg names
/// either, and neither has a signature or a title.
#[test]
fn the_decided_owed_rows_left_chaos_json_with_their_tags() {
    let m = manifest();
    let mut checked = 0;
    for row in ["86", "87"] {
        for map in ["owed_signatures", "owed_titles"] {
            assert!(
                m[map].get(row).is_none(),
                "Owed {row} is still in chaos.json's {map}"
            );
            checked += 1;
        }
        for l in m["legs"].as_array().unwrap() {
            let named = l
                .get("owed")
                .and_then(Value::as_object)
                .is_some_and(|o| o.values().any(|r| rows_of(r).contains(&row.to_owned())));
            assert!(!named, "{}: still owes Owed {row}", l["case"]);
            checked += 1;
        }
    }
    let legs = m["legs"].as_array().unwrap().len();
    eprintln!("decided-row absences checked: {checked} ({legs} legs)");
    assert!(legs >= 1);
    assert_eq!(checked, 2 * (2 + legs), "every map and leg read");
}

/// Owed 96 (2026-10-25): case 15 one-fewer's `scheduled_changes_shown` red reads owed only on its
/// cause's evidence: an off-air run that starts within a child poll of a GPU kill (`at_kill`), with
/// no deliberate hide after the first kill, that holds every missing change (CI's run 40148980537,
/// re-judged under the programme bound). A held page, a 2-frame blip at a kill beside one, a red
/// with nothing missing, an off-air run long after the last kill and a hidden one read NOT OWED.
/// case_verdicts.rs drives the same signature through the case's own measure.
#[test]
fn owed_96_names_the_one_fewer_off_air_red_and_nothing_else() {
    let m = manifest();
    let sig = m["owed_signatures"]["96"].as_str().unwrap().to_owned();
    // The CI red's off-air run; each shape below breaks one of the signature's three conjuncts.
    let off_with = |at_kill: bool, hidden: u32, covers: bool| {
        json!({"from_ms": 1_792_891_584_297_i64, "after_kill_ms": if at_kill { 77 } else { 30_077 },
            "at_kill": at_kill, "ms": 82_604, "frames": 2_478, "to_end": true,
            "hidden_after_kill": hidden, "covers_missing": covers})
    };
    let off = |covers: bool| off_with(true, 0, covers);
    let ci = json!({"due_after_last_kill": 22, "missing": (12..=33).collect::<Vec<u16>>(),
        "lag_ms": {"min": null, "max": null}, "off_air": off(true)});
    let missing = |off_air: Value| json!({"due_after_last_kill": 22, "missing": (12..=33).collect::<Vec<u16>>(), "off_air": off_air});
    let mut checked = 0;
    for (tag, holds, shown) in [
        ("ci-red", true, ci.clone()),
        (
            "held",
            false,
            json!({"due_after_last_kill": 22, "missing": [12, 13], "off_air": null}),
        ),
        (
            "blip-held",
            false,
            json!({"due_after_last_kill": 22, "missing": [12], "off_air": off(false)}),
        ),
        (
            "nothing-missing",
            false,
            json!({"due_after_last_kill": 0, "missing": [], "off_air": off(false)}),
        ),
        // Fix round 1's I-1: an off-air run that starts long after the last kill (no child poll ties
        // it to one), and one the graphics hid on purpose, hold the missing changes and are not
        // Owed 96's.
        ("later", false, missing(off_with(false, 0, true))),
        ("hidden", false, missing(off_with(true, 1, true))),
    ] {
        assert_eq!(
            signature_holds(&sig, &json!({"measures": {"scheduled_changes_shown": shown}})),
            Some(holds),
            "Owed 96 on {tag}: {shown}"
        );
        checked += 1;
    }
    eprintln!("Owed 96 signature cases checked: {checked}");
    assert_eq!(checked, 6);

    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chaos-table-owed-96");
    let d = dir.to_str().unwrap();
    let fail = |shown: Value| {
        move |v: &mut Value| {
            v["status"] = json!("fail");
            v["failed"] = json!(["scheduled_changes_shown"]);
            v["measures"]["scheduled_changes_shown"] = shown.clone();
        }
    };
    verdicts(&dir, true);
    edit(&dir, "cef-gpu-deaths-one-fewer", fail(ci));
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "an owed red is still red\n{s}");
    for line in [
        "| cef-gpu-deaths-one-fewer | FAIL | 4/4 | outage_ms | failed: scheduled_changes_shown (Owed 96) |",
        "red, every failed check owed: cef-gpu-deaths-one-fewer\n",
        "red on a check no Owed row names: none\n",
        &format!("Owed 96: {}\n", m["owed_titles"]["96"].as_str().unwrap()),
        "this run does not count",
    ] {
        assert!(s.contains(line), "{line}\n{s}");
    }
    verdicts(&dir, true);
    edit(
        &dir,
        "cef-gpu-deaths-one-fewer",
        fail(json!({"due_after_last_kill": 22, "missing": [12, 13], "off_air": null})),
    );
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(1));
    for line in [
        "| cef-gpu-deaths-one-fewer | FAIL | 4/4 | outage_ms | failed: scheduled_changes_shown (NOT OWED: Owed 96's signature absent) |",
        "red on a check no Owed row names: cef-gpu-deaths-one-fewer (scheduled_changes_shown)\n",
    ] {
        assert!(s.contains(line), "{line}\n{s}");
    }
    assert!(
        !s.contains("Owed 96:"),
        "a row whose evidence is absent is not titled\n{s}"
    );
}

/// Case 17's `stall_swap` measure as CI's run 40121220366 left it (PR #65), in this branch's shape:
/// the sink's own socket timeout 5 660 ms after the cut, the swap 515 ms later. Owed 87's evidence
/// once; since its ruling a red with it is as unexplained as any other.
fn ci_rule() -> Value {
    json!({"swaps_before": 0, "swap_gen": 2, "reason": "error", "reasons_paired": true,
        "error_ms": 1_000_005_660_i64, "error_to_swap_ms": 515, "error_from_cut_ms": 5660,
        "from_cut_ms": 6175, "from_growth_ms": 1497, "detect_ms": 982, "bound_ms": 5533.333333333333})
}

/// A red table names each failed check's Owed row when that row's signature holds in the run, and
/// says which reds no Owed row explains. No verdict is softened: an owed red is as red as any other,
/// and the run never counts. No row is open in chaos.json since Owed 86's ruling (the owner,
/// 2026-10-25, "A"), so a copy of the manifest gives case 6's leg a probe row, 98, on three of its
/// checks, its signature a measure of the run; the committed rows' reds read NOT OWED beside it.
#[test]
fn the_table_names_each_failed_checks_owed_row_and_stays_red() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chaos-table-owed");
    let d = dir.to_str().unwrap();
    let mut m = manifest();
    m["owed_signatures"]["98"] = json!(".measures.probe.evidence == true");
    m["owed_titles"]["98"] = json!("a probe row");
    let probed = json!(["beats_below_floor", "wire_below_floor", "alarm_raised"]);
    let mut legs = 0;
    for l in m["legs"].as_array_mut().unwrap() {
        if l["case"] == "cpu-starved" {
            l["owed"] = json!({"beats_below_floor": "98", "wire_below_floor": "98", "alarm_raised": "98"});
            legs += 1;
        }
    }
    assert_eq!(legs, 1, "case 6 is one leg of chaos.json");
    let path = dir.with_extension("json");
    let title = |row: &str| m["owed_titles"][row].as_str().unwrap().to_owned();
    let fail = |failed: Value, measures: Value| {
        move |v: &mut Value| {
            v["status"] = json!("fail");
            v["failed"] = failed.clone();
            for (k, x) in measures.as_object().unwrap() {
                v["measures"][k] = x.clone();
            }
        }
    };
    let evidence = |held: bool| json!({"probe": {"evidence": held}});

    verdicts(&dir, true);
    std::fs::write(&path, m.to_string()).unwrap();
    // Case 6's probed checks failed, each owed, the row's evidence present.
    edit(&dir, "cpu-starved", fail(probed.clone(), evidence(true)));
    // CI's case 17 (PR #65), its reds once Owed 87's: since the owner's ruling (2026-10-24, "A")
    // the row is DECIDED and its tags are gone, so both are news, its old evidence notwithstanding.
    edit(
        &dir,
        "dest-stall-rule",
        fail(
            json!(["buffer_capped", "stall_swap"]),
            json!({"stall_swap": ci_rule()}),
        ),
    );
    // CI's dest-stall of round 3, before the reason line, beside Owed 84's old evidence (the clear
    // inside a pending connect; its fix is in). Both reds are news.
    edit(
        &dir,
        "dest-stall",
        fail(
            json!(["stall_swap", "output_lost"]),
            json!({"stall_swap": {"reason": "unknown"}, "clear_phase_driven": {"connect_pending": true}}),
        ),
    );
    // A check no Owed row names: the news in the run.
    edit(&dir, "camera-stop", fail(json!(["slate_ms"]), json!({})));
    // A restart leg's overlay_back with Owed 85's old evidence, graphics given up after the start,
    // beside a check no Owed row ever named: PR #54 fixed that cause and its tags left chaos.json
    // (the rebase onto main 263b08a), so both reds are news.
    edit(
        &dir,
        "cef-killed-r1",
        fail(
            json!(["overlay_back", "same_session"]),
            json!({"graphics_failed_after_start": 1}),
        ),
    );
    let out = run_manifest(&["--table", d], &path);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "an owed red is still red");
    let mut owed_rows = 0;
    for row in [
        "| cpu-starved | FAIL | 4/4 | outage_ms | failed: beats_below_floor (Owed 98), wire_below_floor (Owed 98), alarm_raised (Owed 98) |",
        "| dest-stall | FAIL | 4/4 | outage_ms | failed: stall_swap (NOT OWED), output_lost (NOT OWED) |",
        "| dest-stall-rule | FAIL | 4/4 | outage_ms | failed: buffer_capped (NOT OWED), stall_swap (NOT OWED) |",
        "| camera-stop | FAIL | 4/4 | outage_ms | failed: slate_ms (NOT OWED) |",
        "| cef-killed-r1 | FAIL | 4/4 | outage_ms | failed: overlay_back (NOT OWED), same_session (NOT OWED) |",
    ] {
        assert!(s.contains(row), "{row}\n{s}");
        owed_rows += 1;
    }
    assert_eq!(owed_rows, 5);
    assert!(s.contains("red, every failed check owed: cpu-starved\n"), "{s}");
    assert!(s.contains(
        "red on a check no Owed row names: camera-stop (slate_ms); dest-stall (stall_swap, output_lost); cef-killed-r1 (overlay_back, same_session); dest-stall-rule (buffer_capped, stall_swap)\n"
    ), "{s}");
    assert!(
        s.contains(&format!("Owed 98: {}\n", title("98"))),
        "Owed 98's title"
    );
    for gone in ["Owed 70", "Owed 84", "Owed 85", "Owed 86", "Owed 87"] {
        assert!(
            !s.contains(gone),
            "only the rows this run's reds name, never a fixed or decided one: {gone}"
        );
    }
    assert!(s.contains("this run does not count"));
    assert!(!s.contains("this run counts"));

    // The re-review's I-3: an owed check that fails WITHOUT its row's evidence is not that row's:
    // case 6's beats_below_floor with the probe's evidence absent, beside a check the row never
    // named. And the rows fixed or decided read NOT OWED whatever their old evidence:
    // dest-disconnect's output_lost in a pending connect (Owed 84, PR #57), the fast leg's
    // overlay_back on a loop counted late (Owed 70 (a), PR #55), and Owed 87's checks with the
    // evidence its signature read before the owner's ruling (2026-10-24, "A"): dest-stall's and
    // dest-trickle's stall_swap on the sink's own error, and case 17's reds as CI left them.
    verdicts(&dir, true);
    edit(
        &dir,
        "dest-disconnect",
        fail(
            json!(["output_lost"]),
            json!({"clear_phase_driven": {"swap_to_clear_ms": 3_970, "connect_pending": true}}),
        ),
    );
    edit(
        &dir,
        "dest-stall",
        fail(json!(["stall_swap"]), json!({"stall_swap": ci_rule()})),
    );
    edit(
        &dir,
        "cpu-starved",
        fail(json!(["fps_beats_read", "beats_below_floor"]), evidence(false)),
    );
    edit(
        &dir,
        "dest-trickle",
        fail(json!(["stall_swap"]), json!({"stall_swap": ci_rule()})),
    );
    edit(
        &dir,
        "dest-stall-rule",
        fail(
            json!(["buffer_capped", "stall_swap"]),
            json!({"stall_swap": ci_rule()}),
        ),
    );
    for r in 1..=3 {
        edit(
            &dir,
            &format!("cef-gpu-deaths-fast-r{r}"),
            fail(
                json!(["overlay_back"]),
                json!({"deaths_logged": {"kills": 5, "counted_max": 1, "needs": 3}}),
            ),
        );
    }
    let out = run_manifest(&["--table", d], &path);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(1));
    let mut rows = 0;
    for row in [
        "| dest-disconnect | FAIL | 4/4 | outage_ms | failed: output_lost (NOT OWED) |",
        "| dest-stall | FAIL | 4/4 | outage_ms | failed: stall_swap (NOT OWED) |",
        "| cpu-starved | FAIL | 4/4 | outage_ms | failed: fps_beats_read (NOT OWED), beats_below_floor (NOT OWED: Owed 98's signature absent) |",
        "| dest-trickle | FAIL | 4/4 | outage_ms | failed: stall_swap (NOT OWED) |",
        "| dest-stall-rule | FAIL | 4/4 | outage_ms | failed: buffer_capped (NOT OWED), stall_swap (NOT OWED) |",
        "| cef-gpu-deaths-fast-r1 | FAIL | 4/4 | outage_ms | failed: overlay_back (NOT OWED) |",
        "| cef-gpu-deaths-fast-r2 | FAIL | 4/4 | outage_ms | failed: overlay_back (NOT OWED) |",
        "| cef-gpu-deaths-fast-r3 | FAIL | 4/4 | outage_ms | failed: overlay_back (NOT OWED) |",
    ] {
        assert!(s.contains(row), "{row}\n{s}");
        rows += 1;
    }
    assert!(s.contains("red, every failed check owed: none\n"), "{s}");
    assert!(s.contains(
        "red on a check no Owed row names: dest-disconnect (output_lost); dest-stall (stall_swap); cpu-starved (fps_beats_read, beats_below_floor); dest-trickle (stall_swap); cef-gpu-deaths-fast-r1 (overlay_back); cef-gpu-deaths-fast-r2 (overlay_back); cef-gpu-deaths-fast-r3 (overlay_back); dest-stall-rule (buffer_capped, stall_swap)\n"
    ), "{s}");
    for gone in [
        "Owed 70", "Owed 84", "Owed 85", "Owed 86:", "Owed 87:", "Owed 98:",
    ] {
        assert!(
            !s.contains(gone),
            "a row whose evidence is absent, or whose fix or ruling is in, is not named: {gone}"
        );
    }
    assert_eq!(rows, 8);

    // Only owed reds: still red, and the run still does not count.
    verdicts(&dir, true);
    edit(&dir, "cpu-starved", fail(probed, evidence(true)));
    let out = run_manifest(&["--table", d], &path);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(1));
    assert!(s.contains("red on a check no Owed row names: none\n"));
    assert!(s.contains("WAVE GATE 3: this run does not count"));

    // A green run prints neither line.
    verdicts(&dir, true);
    let out = run_manifest(&["--table", d], &path);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(!s.contains("every failed check owed") && !s.contains("no Owed row names"));
    eprintln!("owed table rows checked: {owed_rows}, then {rows}, then 1, then 0");
}

/// A check may name a list of Owed rows, tried in order (chaos.json's `about`): the first whose
/// signature holds is named, and with none holding the row reads every row's signature absent. No
/// leg of chaos.json carries a list now (Owed 85, 84 and 70 (a) have left it), so a copy of the
/// manifest gives the fast leg's overlay_back two test rows. And a table over that copy never counts
/// toward wave gate 3, even all green (the confirm pass's CHAOS_MANIFEST minor), while the same
/// verdicts over the committed manifest do.
#[test]
fn a_list_of_owed_rows_names_the_first_whose_signature_holds() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("chaos-table-owed-list");
    let d = dir.to_str().unwrap();
    let mut m = manifest();
    m["owed_signatures"]["98"] = json!(".measures.test_first == true");
    m["owed_titles"]["98"] = json!("a first test row");
    m["owed_signatures"]["99"] = json!(".measures.test_second == true");
    m["owed_titles"]["99"] = json!("a second test row");
    let mut lists = 0;
    for l in m["legs"].as_array_mut().unwrap() {
        if l["case"] == "cef-gpu-deaths" && l["variant"] == "fast" {
            l["owed"] = json!({"overlay_back": ["98", "99"]});
            lists += 1;
        }
    }
    assert_eq!(lists, 1, "the fast leg is in chaos.json");
    verdicts(&dir, true);
    let path = dir.with_extension("json");
    std::fs::write(&path, m.to_string()).unwrap();
    let fail = |first: bool, second: bool| {
        move |v: &mut Value| {
            v["status"] = json!("fail");
            v["failed"] = json!(["overlay_back"]);
            v["measures"]["test_first"] = json!(first);
            v["measures"]["test_second"] = json!(second);
        }
    };
    edit(&dir, "cef-gpu-deaths-fast-r1", fail(false, false));
    edit(&dir, "cef-gpu-deaths-fast-r2", fail(false, true));
    edit(&dir, "cef-gpu-deaths-fast-r3", fail(true, true));
    let out = run_manifest(&["--table", d], &path);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "an owed red is still red");
    let row = |r: u32, tag: &str| {
        format!("| cef-gpu-deaths-fast-r{r} | FAIL | 4/4 | outage_ms | failed: overlay_back ({tag}) |")
    };
    let mut checked = 0;
    for (r, tag) in [
        (1, "NOT OWED: Owed 98's and Owed 99's signature absent"),
        (2, "Owed 99"),
        (3, "Owed 98"),
    ] {
        assert!(s.contains(&row(r, tag)), "fast-r{r} reads {tag}: {s}");
        checked += 1;
    }
    assert!(
        s.contains("red, every failed check owed: cef-gpu-deaths-fast-r2, cef-gpu-deaths-fast-r3\n"),
        "{s}"
    );
    assert!(s.contains("red on a check no Owed row names: cef-gpu-deaths-fast-r1 (overlay_back)\n"));
    assert!(
        s.contains("Owed 98: a first test row\n") && s.contains("Owed 99: a second test row\n"),
        "{s}"
    );

    // All green over the copy: green, and never a gate run. The same verdicts over the committed
    // manifest count.
    verdicts(&dir, true);
    let out = run_manifest(&["--table", d], &path);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{s}");
    assert!(
        s.contains(&format!(
            "manifest: CHAOS_MANIFEST {}, not harness/chaos.json\n",
            path.display()
        )),
        "{s}"
    );
    // Every leg chaos.json expands to (37 since case 18), counted from the manifest itself.
    let n = expected_legs().len();
    assert_eq!(n, 37, "chaos.json's legs with their repeats");
    assert!(
        s.contains(&format!(
            "WAVE GATE 3: a run over another manifest (CHAOS_MANIFEST) never counts ({n} of {n} legs PASS)\n"
        )),
        "{s}"
    );
    assert!(!s.contains("this run counts"), "{s}");
    let out = run(&["--table", d]);
    let s = stdout(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        s.contains(&format!("WAVE GATE 3: this run counts ({n} of {n} legs PASS)\n")),
        "{s}"
    );
    assert!(!s.contains("manifest: CHAOS_MANIFEST"), "{s}");
    checked += 1;
    eprintln!("owed list rows checked: {checked} (3 rows, then the override's gate)");
    assert_eq!(checked, 4);
}
