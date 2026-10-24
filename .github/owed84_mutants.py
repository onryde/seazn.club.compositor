"""DO NOT MERGE. Owed 84's gst-level mutants and red baseline (PR #57), one leg per argument.

Every leg first runs its target tests on the unmutated head, and each must pass. Then:
- a mutant leg applies one exact-text replace (it must match once; the new text carries a /*Gn*/
  marker) and runs the targets again: each must FAIL, with the expected panic text. A build error is
  not a kill.
- the red-baseline leg puts back main's gst_output.rs and gst_programme.rs (the code before the fix)
  and runs the four Owed 84 legs: each must FAIL, with the expected panic text. Main has no
  key-request hooks, so inert ones are appended for the round-2 tests to build. Round 2's own red
  (the review's I-1 and m-6) is G6 and G7: each takes back exactly one new guard.
"""
import os
import re
import subprocess
import sys

OUT = "crates/adapters/src/gst_output.rs"
PROG = "crates/adapters/src/gst_programme.rs"
BEFORE = "263b08ac6c65e60b03b509f52f5963e3ffbd6778"  # origin/main, the fix's merge base

PENDING = "a_sink_whose_connect_is_pending_when_the_destination_returns_sends_nothing_queued"
FAILED = "after_a_connect_that_failed_the_next_sink_sends_nothing_queued"
DRAIN = "a_drain_while_the_sink_is_still_connecting_ends_without_waiting_for_it"
LATE = "a_late_first_sink_starts_at_the_keyframe_after_its_swap_not_inside_the_ring"
BEFORE_KEY = "a_drain_before_the_asked_keyframe_ends_without_waiting_for_it"
NOT_TAKEN = "a_keyframe_request_never_taken_sends_nothing_and_the_drain_ends_with_the_encoders"
REFUSED_ONCE = "a_keyframe_request_refused_once_is_asked_again_at_the_next_frame"
VERSION = "the_gate_reads_gstreamer_1_24"
TESTS = "crates/adapters/tests/programme_output.rs"
HOOKS = """

// owed84_mutants.py red-baseline: the hooks the round-2 tests call, inert on the code before the fix.
#[cfg(feature = "test-hooks")]
impl GstProgramme {
    pub fn refuse_key_requests(&self, _n: u32) {}
    pub fn key_refusals_left(&self) -> u32 {
        0
    }
}
"""
NO_DRAIN = ["no matching event within"]

QUEUED = ["in front of a sink that cannot send"]
LEGS = {
    "G1-gate-always-open": (
        OUT,
        "Gate::Publish(sink) => rtmp_total(sink) > 0,",
        "Gate::Publish(sink) => rtmp_total(sink) > 0 || true /*G1*/,",
        [(PENDING, QUEUED), (FAILED, QUEUED)],
    ),
    "G2-no-gated-drain": (
        OUT,
        "if r.video_eos && r.audio_eos {\n                drop(r);\n                self.release_unsent();",
        "if false && r.video_eos && r.audio_eos /*G2*/ {\n                drop(r);\n                self.release_unsent();",
        [(DRAIN, NO_DRAIN)],
    ),
    "G3-no-keyframe-request": (
        OUT,
        "let taken = (self.request_key)();",
        "let taken = true /*G3*/;",
        [(PENDING, ["the keyframe came late"]), (LATE, ["nothing reached the file"])],
    ),
    "G4-connect-on-first-buffer": (
        OUT,
        's.set_property("async-connect", true);',
        's.set_property("async-connect", false /*G4*/);',
        [(PENDING, ["the sink connected to the destination"])],
    ),
    "G5-old-keyframe-may-start": (
        OUT,
        "if !e.key || e.seq < self.reconnect {",
        "if !e.key /*G5*/ {",
        [(LATE, ["is from before the reconnect", "the ring was replayed"])],
    ),
    "G6-empty-output-waits-at-eos": (
        OUT,
        "Next::Eos => {\n                    if !pick.started() {\n                        self.release_unsent();",
        "Next::Eos => {\n                    if false && !pick.started() /*G6*/ {\n                        self.release_unsent();",
        [(BEFORE_KEY, NO_DRAIN), (NOT_TAKEN, NO_DRAIN)],
    ),
    "G7-refused-key-not-asked-again": (
        OUT,
        "if !pick.started() {\n                        self.ask_key(&mut ask);",
        "if false && !pick.started() /*G7*/ {\n                        self.ask_key(&mut ask);",
        [(REFUSED_ONCE, ["was not asked again"]), (NOT_TAKEN, ["the request was asked"])],
    ),
    "G8-gstreamer-pin": (
        TESTS,
        'let want = "1.24.";',
        'let want = "1.26." /*G8*/;',
        [(VERSION, ["re-read its stats and EOS rules"])],
    ),
}
RED = [
    (PENDING, QUEUED),
    (FAILED, QUEUED),
    (DRAIN, ["no matching event within"]),
    (LATE, ["the ring was replayed", "is from before the reconnect"]),
]


def run(name):
    cmd = ["cargo", "test", "-p", "adapters", "--test", "programme_output", "--", "--exact", name, "--nocapture"]
    env = dict(os.environ, CARGO_TERM_COLOR="never")
    r = subprocess.run(cmd, capture_output=True, text=True, env=env)
    # Actions colours cargo's status words (`Running` arrives wrapped in escape codes).
    out = re.sub(r"\x1b\[[0-9;]*m", "", r.stdout + r.stderr)
    built = "Running tests/programme_output.rs" in out and "running 1 test" in out
    ok = f"test {name} ... ok" in out
    failed = f"test {name} ... FAILED" in out
    return built, ok, failed, out


def tail(out, n=40):
    return "\n".join(out.splitlines()[-n:])


def verdict(name, want, label):
    built, ok, failed, out = run(name)
    hit = [w for w in want if w in out]
    print(f"--- {label}: {name}: built={built} ok={ok} failed={failed} expected-text={hit}")
    print(tail(out))
    return built and failed and bool(hit)


def main(leg):
    if leg == "red-baseline":
        targets = RED
    else:
        targets = LEGS[leg][3]
    assert targets, "no targets"
    bad = 0
    for name, _ in targets:
        built, ok, _, out = run(name)
        print(f"--- baseline (head, unmutated): {name}: built={built} ok={ok}")
        if not (built and ok):
            print(tail(out, 80))
            bad += 1
    if bad:
        print(f"::error::{leg}: {bad} target(s) not green on the unmutated head")
        return 1
    if leg == "red-baseline":
        for path in (OUT, PROG):
            src = subprocess.run(["git", "show", f"{BEFORE}:{path}"], capture_output=True, text=True, check=True).stdout
            open(path, "w").write(src + (HOOKS if path == PROG else ""))
        print(f"put back {OUT} and {PROG} from {BEFORE}, with inert key-request hooks")
    else:
        path, old, new, _ = LEGS[leg]
        text = open(path).read()
        n = text.count(old)
        if n != 1:
            print(f"::error::{leg}: the mutant's text matches {n} times, not once")
            return 1
        open(path, "w").write(text.replace(old, new))
        print(f"applied {leg} to {path}")
    killed = 0
    for name, want in targets:
        if verdict(name, want, leg):
            killed += 1
        else:
            print(f"::error::{leg}: {name} was not failed with {want}")
    print(f"{leg}: {killed} of {len(targets)} targets failed as expected")
    return 0 if killed == len(targets) and killed > 0 else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
