"""DO NOT MERGE. Owed 84's gst-level mutants and red baseline (PR #57), one leg per argument.

Every leg first runs its target tests on the unmutated head, and each must pass. Then:
- a mutant leg applies one exact-text replace (it must match once; the new text carries a /*Gn*/
  marker) and runs the targets again: each must FAIL, with the expected panic text. A build error is
  not a kill.
- the red-baseline leg puts back main's gst_output.rs and gst_programme.rs (the code before the fix)
  and runs the four Owed 84 legs: each must FAIL, with the expected panic text.
"""
import subprocess
import sys

OUT = "crates/adapters/src/gst_output.rs"
PROG = "crates/adapters/src/gst_programme.rs"
BEFORE = "263b08ac6c65e60b03b509f52f5963e3ffbd6778"  # origin/main, the fix's merge base

PENDING = "a_sink_whose_connect_is_pending_when_the_destination_returns_sends_nothing_queued"
FAILED = "after_a_connect_that_failed_the_next_sink_sends_nothing_queued"
DRAIN = "a_drain_while_the_sink_is_still_connecting_ends_without_waiting_for_it"
LATE = "a_late_first_sink_starts_at_the_keyframe_after_its_swap_not_inside_the_ring"

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
        "if r.video_eos && r.audio_eos {\n                drop(r);\n                self.ring.release(self.gen);",
        "if false && r.video_eos && r.audio_eos /*G2*/ {\n                drop(r);\n                self.ring.release(self.gen);",
        [(DRAIN, ["no matching event within"])],
    ),
    "G3-no-keyframe-request": (
        OUT,
        "let taken = (self.request_key)();",
        "let taken = true /*G3*/;",
        [(PENDING, ["the keyframe came late"]), (LATE, ["the keyframe came more than", "keys ["])],
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
}
RED = [
    (PENDING, QUEUED),
    (FAILED, QUEUED),
    (DRAIN, ["no matching event within"]),
    (LATE, ["the ring was replayed", "is from before the reconnect"]),
]


def run(name):
    cmd = ["cargo", "test", "-p", "adapters", "--test", "programme_output", "--", "--exact", name, "--nocapture"]
    r = subprocess.run(cmd, capture_output=True, text=True)
    out = r.stdout + r.stderr
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
            open(path, "w").write(src)
        print(f"put back {OUT} and {PROG} from {BEFORE}")
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
