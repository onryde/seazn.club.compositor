#!/usr/bin/env python3
"""Throwaway mutants for PR #62 fix round 1 (the review's m3); this branch is never merged.

`o82.py mutate <id>` applies one mutant to the PR head's tree (each replace must match exactly once,
and the result is re-read). `o82.py judge <id>` runs the killing tests (or image.yml's own check) and
exits 0 only when the named test or step FAILS with the expected text and the controls ran; `base`
exits 0 only when every targeted test passes unmutated. A build error is never a kill.
"""
import hashlib, os, re, subprocess, sys

CG = "crates/adapters/src/cef_graphics.rs"
WI = "crates/compositor/src/wiring.rs"
DF = "docker/Dockerfile"

HOLD = """    #[cfg(feature = "test-hooks")]
    if let Some(left) = shared.take_teardown_hold() {
        shared.log(&format!(
            "WARN test hook (Owed 82) gen={gen}: this CEF teardown never returns ({left} more to hold)"
        ));
        park_for_good();
    }
"""
NULL = """    if pipeline.set_state(gst::State::Null).is_err() {
        shared.log(&format!("gen={gen}: teardown to NULL failed"));
    }
"""

MUTANTS = {
    "C1-hold-deleted": [(CG, HOLD, "    /*MUT C1: the hold deleted*/\n")],
    "C2-hold-after-null": [(CG, HOLD, "    /*MUT C2: the hold moved below*/\n"),
                           (CG, NULL, NULL + "    /*MUT C2*/\n" + HOLD)],
    "C3-arm-deleted": [(WI, "    arm_test_hooks(&graphics, hooks);\n", "    let _ = hooks; /*MUT C3*/\n")],
    "C4-arm-ge-0": [(WI, "    if hooks.hold_teardowns > 0 {\n", "    if hooks.hold_teardowns >= 0 /*MUT C4*/ {\n")],
    "C5-relay-with-feature": [(DF, "RUN cargo build --release --locked -p compositor \\\n && install -m 0755 target/release/compositor /usr/local/bin/compositor \\\n",
                               "RUN cargo build --release --locked -p compositor --features test-hooks \\\n && install -m 0755 target/release/compositor /usr/local/bin/compositor \\\n")],
    "C6-final-from-deleted": [(DF, "\n# Last, so a build with no target is the relay image (harness.yml, chaos.yml, C16).\nFROM relay\n", "\n")],
}

ADAPTERS = ["cargo", "test", "-p", "adapters", "--lib", "--", "test_hook", "--show-output"]
FAKE_RUN = ["cargo", "test", "-p", "compositor", "--features", "test-hooks", "--test", "fake_run", "--", "test_hook", "--show-output"]
TWO = "cef_graphics::tests::test_hook_two_held_teardowns_leak_two_and_restart_nothing"
THREE = "cef_graphics::tests::test_hook_three_held_teardowns_restart_in_place_with_exit_75"
BUDGET = "cef_graphics::tests::test_hook_the_held_teardown_budget_is_taken_once_each"
LIVE = "test_hook_a_live_run_arms_the_held_teardowns_it_is_given"

# id -> (command, tests that must FAIL, texts that must appear, control tests that must pass)
UNIT = {
    "C1-hold-deleted": (ADAPTERS, [TWO, THREE], ["gen=1: its teardown was not held"], [BUDGET]),
    "C2-hold-after-null": (ADAPTERS, [TWO, THREE],
                           ["a held teardown parks before its set_state(NULL); a returned one ran it",
                            "left: Null", "right: Playing"], [BUDGET]),
    "C3-arm-deleted": (FAKE_RUN, [LIVE], ['Some("2"): []'], []),
    "C4-arm-ge-0": (FAKE_RUN, [LIVE], ["None: [", "the next 0 CEF teardowns never return"], []),
}
IMAGE_SHA = "7c75fd48a40c9105af32b7586a5730dbb6188645"  # PR #62 head 4a1925e's .github/workflows/image.yml
IMAGE_COPY = ".github/mutants-o82/image.yml.orig"
STEP = "The relay image carries no test hook (Owed 82)"
WANT = "::error::the relay image's binary carries the test hook ('COMPOSITOR_TEST_HOLD_TEARDOWNS')"
NOT_WANT = ["is not an ELF binary", "this check would read nothing"]


def mutate(mid):
    for path, old, new in MUTANTS[mid]:
        s = open(path).read()
        n = s.count(old)
        if n != 1:
            sys.exit(f"{mid}: {path}: the pin matches {n} times, not 1")
        open(path, "w").write(s.replace(old, new, 1))
        if new.strip() and open(path).read().count(new) < 1:
            sys.exit(f"{mid}: {path}: the mutant text is not in the file")
    print(f"{mid}: applied {len(MUTANTS[mid])} replace(s)")
    subprocess.run(["git", "--no-pager", "diff", "--stat"], check=True)


def run(cmd, log):
    print("+", " ".join(cmd), flush=True)
    p = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    open(log, "w").write(p.stdout)
    print(p.stdout[-6000:])
    return p.returncode, p.stdout


def line_for(out, name, verdict):
    return re.search(rf"^test {re.escape(name)} \.\.\. {verdict}$", out, re.M) is not None


def judge_unit(mid):
    cmd, must_fail, texts, controls = UNIT[mid]
    rc, out = run(cmd, f"{mid}.log")
    fail = []
    if re.search(r"^error\[E\d+\]", out, re.M):
        fail.append("a build error is not a kill")
    if rc == 0:
        fail.append("the run passed: the mutant SURVIVED")
    for t in must_fail:
        print(("FAILED as expected: " if line_for(out, t, "FAILED") else "NOT FAILED: ") + t)
        if not line_for(out, t, "FAILED"):
            fail.append(f"{t} did not fail")
    for x in texts:
        print(("seen: " if x in out else "NOT SEEN: ") + x)
        if x not in out:
            fail.append(f"expected text not seen: {x}")
    for t in controls:
        if not line_for(out, t, "ok"):
            fail.append(f"control {t} did not pass")
    print(f"{mid}: checked {len(must_fail)} killer test(s), {len(texts)} text(s), {len(controls)} control(s)")
    if len(must_fail) == 0:
        fail.append("anti-vacuity: no killer test named")
    return fail


def judge_base():
    fail = []
    checked = 0
    for cmd, names in [(ADAPTERS, [BUDGET, TWO, THREE]), (FAKE_RUN, [LIVE])]:
        rc, out = run(cmd, "base-%d.log" % checked)
        if rc != 0:
            fail.append("unmutated run failed: " + " ".join(cmd))
        for t in names:
            checked += 1
            ok = line_for(out, t, "ok")
            print(("passed: " if ok else "DID NOT PASS: ") + t)
            if not ok:
                fail.append(f"{t} did not pass unmutated")
    print(f"base: {checked} tests checked by name")
    if checked != 4:
        fail.append("anti-vacuity: 4 named tests expected")
    return fail


def judge_image(mid):
    blob = subprocess.run(["git", "hash-object", IMAGE_COPY], capture_output=True, text=True, check=True).stdout.strip()
    if blob != IMAGE_SHA:
        return [f"{IMAGE_COPY} is not PR #62's image.yml ({blob} != {IMAGE_SHA})"]
    script = subprocess.run(
        ["ruby", "-ryaml", "-e",
         'w = YAML.load_file(ARGV[0]); s = w["jobs"]["image"]["steps"].select { |x| x["name"] == ARGV[1] };'
         ' abort("steps named: #{s.size}") unless s.size == 1; print s[0]["run"]', IMAGE_COPY, STEP],
        capture_output=True, text=True, check=True).stdout
    open("step.sh", "w").write(script)
    rc, out = run(["bash", "--noprofile", "--norc", "-eo", "pipefail", "step.sh"], f"{mid}.log")
    fail = []
    if rc == 0:
        fail.append("image.yml's check passed: the mutant SURVIVED")
    if WANT not in out:
        fail.append("the expected ::error:: line is missing")
    for x in NOT_WANT:
        if x in out:
            fail.append(f"red for another reason: {x}")
    for b in ["relay.bin", "hooks.bin"]:
        if not re.search(rf"^{re.escape(b)}:\s+\d+ bytes, magic 7f454c46$", out, re.M):
            fail.append(f"control: {b} not read as an ELF binary")
    if not re.search(r"^marker 'COMPOSITOR_TEST_HOLD_TEARDOWNS': relay [1-9]\d*, relay-test-hooks [1-9]\d*$", out, re.M):
        fail.append("control: the marker line does not show the hook in both binaries")
    print(f"{mid}: expected red seen: {WANT in out}")
    return fail


def main():
    act, mid = sys.argv[1], sys.argv[2]
    if act == "mutate":
        if mid != "base":
            mutate(mid)
        return
    fail = judge_base() if mid == "base" else judge_image(mid) if mid.startswith(("C5", "C6")) else judge_unit(mid)
    for f in fail:
        print(f"::error::{mid}: {f}")
    print(f"{mid}: {'KILLED' if mid != 'base' and not fail else 'BASE GREEN' if not fail else 'NOT A KILL / NOT GREEN'}")
    sys.exit(1 if fail else 0)


main()
