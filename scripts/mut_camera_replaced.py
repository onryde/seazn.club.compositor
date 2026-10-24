#!/usr/bin/env python3
"""DO NOT MERGE: the camera-frames-replaced wiring mutants (PR #60), one at a time.

Each mutant: an exact-text replace that must match once, marked /*MUTn*/; the seam test must FAIL
by its own assertion (a panic in programme_output.rs), never by a build refusal; then the original
is written back and its sha checked."""
import hashlib, subprocess, sys

TEST = "camera_frames_replaced_counts_the_frames_no_tick_showed"
CMD = ["cargo", "test", "-p", "adapters", "--test", "programme_output", "--", "--exact", TEST, "--show-output"]
MUTANTS = [
    ("MUT1 the slate arm reads no count", "crates/adapters/src/gst_programme.rs",
     "                self.shared\n                    .stats\n                    .on_camera_tick(self.frames.camera_written(), false);\n                None\n",
     "                /*MUT1*/\n                None\n"),
    ("MUT2 a camera tick is not on air", "crates/adapters/src/gst_programme.rs",
     "self.shared.stats.on_camera_tick(written, true);",
     "self.shared.stats.on_camera_tick(written, false /*MUT2*/);"),
    ("MUT3 the slot counts no write", "crates/adapters/src/frame_store.rs",
     "        slot.written += 1;\n",
     "        /*MUT3*/\n"),
]

def run():
    r = subprocess.run(CMD, capture_output=True, text=True)
    return r.returncode, r.stdout + r.stderr

rc, out = run()
print(f"BASELINE rc={rc}")
assert rc == 0 and "test result: ok. 1 passed" in out, out[-3000:]
killed = 0
for name, path, old, new in MUTANTS:
    orig = open(path).read()
    sha = hashlib.sha256(orig.encode()).hexdigest()
    assert orig.count(old) == 1, (name, orig.count(old))
    open(path, "w").write(orig.replace(old, new))
    try:
        rc, out = run()
    finally:
        open(path, "w").write(orig)
        assert hashlib.sha256(open(path).read().encode()).hexdigest() == sha
    built = "error[E" not in out and "could not compile" not in out
    failed = f"test {TEST} ... FAILED" in out
    own = "panicked at crates/adapters/tests/programme_output.rs" in out
    verdict = "KILLED" if (rc != 0 and built and failed and own) else "SURVIVED_OR_BUILD"
    print(f"{name}: rc={rc} built={built} failed={failed} own_panic={own} => {verdict}")
    for line in out.splitlines():
        if "panicked at" in line or "left:" in line or "right:" in line:
            print("   ", line)
    killed += verdict == "KILLED"
print(f"KILLED {killed} of {len(MUTANTS)}")
sys.exit(0 if killed == len(MUTANTS) else 1)
