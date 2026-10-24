#!/usr/bin/env python3
"""DO NOT MERGE: the PR #69 review's I1 mutant, "count pushes again", on the encoder-hang beat test.

The mutant moves the beat's frame count from the encoder's sink (`Stats::on_encoded_video_at`) to the
tick (`Stats::on_camera_tick`, which `GstProgramme::push` calls on every tick, slate or camera): the
first build's push count. The hang test must FAIL by its own assertion (a panic in
programme_output.rs), never by a build refusal; the baseline (unmutated) must pass first, three
times (the test is real-pace); the original is written back and its sha checked."""
import hashlib, subprocess, sys

TEST = "the_beats_fps_falls_while_the_encoder_hangs_though_every_push_keeps_its_pace"
CMD = ["cargo", "test", "-p", "adapters", "--test", "programme_output", "--", "--exact", TEST, "--show-output"]
PATH = "crates/adapters/src/stats.rs"
EDITS = [
    ("        *s.encoded_frames.get_or_insert(0) += 1;\n        let pts = pts.as_secs_f64();\n",
     "        /*MUT1a*/\n        let pts = pts.as_secs_f64();\n"),
    ("        locked(&self.inner).camera.on_tick(written, camera_on_air);\n",
     "        let mut s = locked(&self.inner);\n        *s.encoded_frames.get_or_insert(0) += 1; /*MUT1b*/\n        s.camera.on_tick(written, camera_on_air);\n"),
]

def run():
    r = subprocess.run(CMD, capture_output=True, text=True)
    return r.returncode, r.stdout + r.stderr

def show(out):
    for line in out.splitlines():
        if "beat fps over" in line or "panicked at" in line or "starved" in line:
            print("   ", line)

for i in range(3):
    rc, out = run()
    print(f"BASELINE {i + 1} rc={rc}")
    show(out)
    assert rc == 0 and "test result: ok. 1 passed" in out, out[-3000:]
orig = open(PATH).read()
sha = hashlib.sha256(orig.encode()).hexdigest()
mut = orig
for old, new in EDITS:
    assert mut.count(old) == 1, (old, mut.count(old))
    mut = mut.replace(old, new)
open(PATH, "w").write(mut)
try:
    rc, out = run()
finally:
    open(PATH, "w").write(orig)
    assert hashlib.sha256(open(PATH).read().encode()).hexdigest() == sha
built = "error[E" not in out and "could not compile" not in out
failed = f"test {TEST} ... FAILED" in out
own = "panicked at crates/adapters/tests/programme_output.rs" in out
verdict = "KILLED" if (rc != 0 and built and failed and own) else "SURVIVED_OR_BUILD"
print(f"MUT1 count pushes again: rc={rc} built={built} failed={failed} own_panic={own} => {verdict}")
show(out)
sys.exit(0 if verdict == "KILLED" else 1)
