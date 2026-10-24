//! `harness/netem.sh stats` (C14): the netem qdisc's counters, parsed from `tc -s qdisc show`
//! (`netem.sh parse-stats`, the same parser on stdin, so no tc and no stack are needed here).
//!
//! The PR #52 review's I1: CI's case 5 read `sent 0, dropped 0` under `rate 1kbit` while the stall
//! did apply (the GSO segments already queued take minutes at 1 kbit), so the witness that a rate
//! limit held the leg's packets is the qdisc's backlog. The fixtures are iproute2's text: a backlog in
//! bytes, or in its KiB / MiB form when near a whole one (`sprint_size`), and a leg with no netem.
//! Needs bash 4 or later (HARNESS_BASH, as in case_verdicts.rs).
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn parse(leg: &str, tc: &str) -> (Option<i32>, Value) {
    let bash = std::env::var("HARNESS_BASH").unwrap_or_else(|_| "bash".into());
    let mut child = Command::new(bash)
        .arg(repo().join("harness/netem.sh"))
        .args(["parse-stats", leg])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("netem.sh runs");
    child.stdin.take().unwrap().write_all(tc.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    eprintln!("--- parse-stats {leg}: {:?} {text}", out.status.code());
    (
        out.status.code(),
        serde_json::from_str(text.trim()).unwrap_or(Value::Null),
    )
}

const PRIO: &str = "qdisc prio 1: root refcnt 2 bands 4 priomap 1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1
 Sent 3176 bytes 22 pkt (dropped 0, overlimits 0 requeues 0)
 backlog 0b 0p requeues 0
";

#[test]
fn the_netem_qdiscs_counters_are_read_and_no_other_qdiscs() {
    let cases: [(&str, String, Value); 5] = [
        // Case 5 in CI: 1 kbit, nothing sent in 30 s, the queue holding the leg's packets.
        (
            "out",
            format!(
                "{PRIO}qdisc netem 40: parent 1:4 limit 1000 rate 1Kbit
 Sent 0 bytes 0 pkt (dropped 0, overlimits 0 requeues 0)
 backlog 130862b 45p requeues 0
"
            ),
            json!({"leg": "out", "present": true, "sent_bytes": 0, "sent_pkts": 0, "dropped": 0,
                "backlog_bytes": 130862, "backlog_pkts": 45}),
        ),
        // A backlog in iproute2's KiB form.
        (
            "out",
            format!(
                "{PRIO}qdisc netem 40: parent 1:4 limit 1000 rate 8Kbit
 Sent 19271 bytes 16 pkt (dropped 0, overlimits 0 requeues 0)
 backlog 64Kb 1p requeues 0
"
            ),
            json!({"leg": "out", "present": true, "sent_bytes": 19271, "sent_pkts": 16, "dropped": 0,
                "backlog_bytes": 65536, "backlog_pkts": 1}),
        ),
        // A loss leg, in the MiB form, and the netem qdisc first.
        (
            "in",
            format!(
                "qdisc netem 40: parent 1:4 limit 1000 delay 400ms 100ms loss 2%
 Sent 40123456 bytes 30001 pkt (dropped 221, overlimits 0 requeues 0)
 backlog 2Mb 3p requeues 0
{PRIO}"
            ),
            json!({"leg": "in", "present": true, "sent_bytes": 40123456, "sent_pkts": 30001,
                "dropped": 221, "backlog_bytes": 2097152, "backlog_pkts": 3}),
        ),
        // Cleared: only the default qdisc.
        (
            "out",
            "qdisc noqueue 0: root refcnt 2
 Sent 0 bytes 0 pkt (dropped 0, overlimits 0 requeues 0)
 backlog 0b 0p requeues 0
"
            .to_owned(),
            json!({"leg": "out", "present": false, "sent_bytes": 0, "sent_pkts": 0, "dropped": 0,
                "backlog_bytes": 0, "backlog_pkts": 0}),
        ),
        // The prio qdisc alone (netem's add failed): its own counters are never read as netem's.
        (
            "out",
            PRIO.to_owned(),
            json!({"leg": "out", "present": false, "sent_bytes": 0, "sent_pkts": 0, "dropped": 0,
                "backlog_bytes": 0, "backlog_pkts": 0}),
        ),
    ];
    let mut checked = 0;
    for (leg, tc, want) in &cases {
        let (code, got) = parse(leg, tc);
        assert_eq!(code, Some(0));
        assert_eq!(&got, want, "{tc}");
        checked += 1;
    }
    eprintln!("tc outputs checked: {checked}");
    assert_eq!(checked, cases.len());
}

#[test]
fn an_unknown_leg_is_refused() {
    let (code, _) = parse("sideways", PRIO);
    assert_eq!(code, Some(64));
}
