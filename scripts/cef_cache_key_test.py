#!/usr/bin/env python3
"""Tests `scripts/cef-cache-key.sh`, the tag key of image.yml's GHCR build cache (decision log
"CI: CEF layer from GHCR"). Stdlib only; run with `python3 -I` (check.yml).

The rule under test (the GHCR dispatch, 2026-10-23): the key changes when docker/GSTCEF_COMMIT or the
Dockerfile's cef-build stage changes, and cef-build's stage includes gst-base, the stage it is built
FROM. It holds still for the other stages, which change with every commit. A stage runs from its
FROM line to the line before the next FROM; this file reads those spans itself, never from the script.

  - the real files give 16 hex, the same twice, and the same for a copy in another directory;
  - one line inserted after each line of the Dockerfile: inside gst-base or cef-build, a new key;
    anywhere else (the header, rust-build, cef-test, the runtime stage), the same key;
  - gst-base's base image changed (its FROM line): a new key;
  - another 40-hex commit: a new key; the commit's surrounding whitespace: the same key;
  - refused, with exit 1, one `cef-cache-key:` line on stderr and nothing on stdout: a short, non-hex,
    upper-case or empty commit, a missing commit file or Dockerfile, and a Dockerfile with no
    gst-base, no cef-build, no rust-build, two gst-base stages, cef-build after rust-build, cef-build
    built FROM another stage, or no lines at all.

Each sweep prints how many cases it checked, and a sweep that checked none is a failure.

Usage: cef_cache_key_test.py [--script scripts/cef-cache-key.sh]
"""

import argparse
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCKERFILE = ROOT / "docker" / "Dockerfile"
COMMIT = ROOT / "docker" / "GSTCEF_COMMIT"
HASHED = ("gst-base", "cef-build")
FROM = re.compile(r"^FROM\s+(\S+)(?:\s+AS\s+(\S+))?\s*$", re.IGNORECASE)


def run(script, dockerfile, commit):
    p = subprocess.run([str(script), str(dockerfile), str(commit)], capture_output=True, text=True, timeout=30)
    return p.returncode, p.stdout, p.stderr


def stage_of_each_line(lines):
    """The stage name each line (1-based) belongs to, or None before the first FROM."""
    owner, current = {}, None
    for n, line in enumerate(lines, start=1):
        m = FROM.match(line)
        if m:
            current = m.group(2) or f"<unnamed {n}>"
        owner[n] = current
    return owner


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--script", default=str(ROOT / "scripts" / "cef-cache-key.sh"))
    a = ap.parse_args()
    script = Path(a.script).resolve()
    failures = []
    if not script.is_file():
        print(f"FAIL {script} does not exist", file=sys.stderr)
        return 1

    def check(ok, what):
        print(f"{'ok  ' if ok else 'FAIL'} {what}")
        if not ok:
            failures.append(what)

    lines = DOCKERFILE.read_text().splitlines()
    commit = COMMIT.read_text()
    owner = stage_of_each_line(lines)
    stages = [s for s in dict.fromkeys(owner.values()) if s]
    check(all(s in stages for s in HASHED), f"the Dockerfile has the stages {HASHED} (it has {stages})")

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        df, cf = tmp / "Dockerfile", tmp / "GSTCEF_COMMIT"

        def key_of(text, commit_text=commit):
            df.write_text(text)
            cf.write_text(commit_text)
            return run(script, df, cf)

        # The real files, twice, and a copy elsewhere.
        code, base, err = run(script, DOCKERFILE, COMMIT)
        base = base.strip()
        check(code == 0 and re.fullmatch(r"[0-9a-f]{16}", base) is not None,
              f"the real files give 16 hex (exit {code}, key '{base}', stderr '{err.strip()}')")
        again = run(script, DOCKERFILE, COMMIT)[1].strip()
        check(again == base, f"the same key twice ({base}, {again})")
        copy = key_of("\n".join(lines) + "\n")[1].strip()
        check(copy == base, f"a copy elsewhere gives the same key ({copy})")

        # One line inserted after each line (0 = at the top): it belongs to the stage of the line above.
        inside = outside = 0
        for i in range(0, len(lines) + 1):
            stage = owner.get(i)
            text = "\n".join(lines[:i] + ["# probe line"] + lines[i:]) + "\n"
            code, out, err = key_of(text)
            k = out.strip()
            if stage in HASHED:
                inside += 1
                ok = code == 0 and k != base and len(k) == 16
                want = "a new key"
            else:
                outside += 1
                ok = code == 0 and k == base
                want = "the same key"
            if not ok:
                check(False, f"a line after line {i} ({stage or 'header'}): {want} (exit {code}, key '{k}', "
                             f"stderr '{err.strip()}')")
        print(f"inserted lines checked: {inside} inside {'/'.join(HASHED)}, {outside} outside")
        check(inside > 0 and outside > 0, "both sides of the insert sweep checked at least one line")

        # gst-base's base image.
        gst_from = next(n for n, s in owner.items() if s == "gst-base" and FROM.match(lines[n - 1]))
        bumped = list(lines)
        bumped[gst_from - 1] = bumped[gst_from - 1].replace(FROM.match(lines[gst_from - 1]).group(1), "ubuntu:99.04", 1)
        check(bumped != lines, "the gst-base FROM edit changed a line")
        code, out, _ = key_of("\n".join(bumped) + "\n")
        check(code == 0 and out.strip() not in ("", base), f"another gst-base base image: a new key ({out.strip()})")

        # The commit file.
        other = "0" * 40 if commit.strip() != "0" * 40 else "1" * 40
        commits = [(other + "\n", False), ("  " + commit.strip() + "  \n\n", True), (commit.strip(), True)]
        for text, same in commits:
            code, out, _ = key_of("\n".join(lines) + "\n", text)
            k = out.strip()
            ok = code == 0 and ((k == base) if same else (k not in ("", base)))
            check(ok, f"commit {text.strip()[:12]}… ({'whitespace only' if same else 'another commit'}): "
                      f"{'the same key' if same else 'a new key'} ({k})")
        print(f"commit files checked: {len(commits)}")

        # Refusals.
        def without(stage):
            return [l for n, l in enumerate(lines, start=1) if owner[n] != stage]

        def span(stage):
            return [l for n, l in enumerate(lines, start=1) if owner[n] == stage]

        header = [l for n, l in enumerate(lines, start=1) if owner[n] is None]
        rest = [l for n, l in enumerate(lines, start=1) if owner[n] not in (None, "cef-build")]
        cef_from = span("cef-build")[0]
        refusals = [
            ("a short commit", "\n".join(lines), commit.strip()[:12]),
            ("a non-hex commit", "\n".join(lines), "g" * 40),
            ("an upper-case commit", "\n".join(lines), commit.strip().upper()),
            ("an empty commit file", "\n".join(lines), ""),
            ("no gst-base stage", "\n".join(without("gst-base")), commit),
            ("no cef-build stage", "\n".join(without("cef-build")), commit),
            ("no rust-build stage", "\n".join(without("rust-build")), commit),
            ("two gst-base stages", "\n".join(span("gst-base") + lines), commit),
            ("cef-build after rust-build", "\n".join(header + rest + span("cef-build")), commit),
            ("cef-build FROM another stage", "\n".join(l.replace(cef_from, "FROM ubuntu:24.04 AS cef-build")
                                                       if l == cef_from else l for l in lines), commit),
            ("an empty Dockerfile", "", commit),
        ]
        for name, text, commit_text in refusals:
            code, out, err = key_of(text + "\n" if text else "", commit_text)
            ok = code == 1 and out == "" and err.startswith("cef-cache-key: ") and err.count("\n") == 1
            check(ok, f"refused: {name} (exit {code}, stdout '{out.strip()}', stderr '{err.strip()}')")
        for name, missing in (("a missing commit file", "commit"), ("a missing Dockerfile", "dockerfile")):
            df.write_text("\n".join(lines) + "\n")
            cf.write_text(commit)
            gone = tmp / "absent"
            code, out, err = run(script, gone if missing == "dockerfile" else df, gone if missing == "commit" else cf)
            ok = code == 1 and out == "" and err.startswith("cef-cache-key: ") and err.count("\n") == 1
            check(ok, f"refused: {name} (exit {code}, stdout '{out.strip()}', stderr '{err.strip()}')")
        print(f"refusals checked: {len(refusals) + 2}")

    if failures:
        print(f"{len(failures)} failure(s)", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
