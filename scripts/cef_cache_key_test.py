#!/usr/bin/env python3
"""Tests image.yml's GHCR build-cache scripts (decision log "CI: CEF layer from GHCR"). Stdlib only;
run with `python3 -I` (check.yml).

`scripts/cef-cache-key.sh`, the tag key. The rule (the GHCR dispatch, 2026-10-23, and the PR #47
review's m1/m2): the key changes when docker/GSTCEF_COMMIT, the Dockerfile's cef-build stage, or the
digest of a base image it is built from changes; cef-build's stage includes gst-base, the stage it is
built FROM. It holds still for the other stages, which change with every commit. A stage runs from
its FROM line to the line before the next FROM; this file reads those spans itself, never from the
script. A fake resolver (CEF_CACHE_KEY_RESOLVE) stands in for the registry and logs what it is asked.

  - the real files give 16 hex, the same twice and from another directory, and the resolver is asked
    for exactly the external bases of gst-base and cef-build (this file's own parse);
  - one line inserted after each line, and ` # x` appended to each non-FROM line: inside gst-base or
    cef-build, a new key; anywhere else (the header, rust-build, cef-test, the runtime stage), the
    same key. The edit sweep covers the range's last line, which an insert cannot (it moves it);
  - each FROM line's base replaced: gst-base's, a new key; cef-build's, refused; the others, the same;
  - the base's digest moved under the same text: a new key; a base pinned `@sha256:` is not resolved;
  - another 40-hex commit: a new key; the commit's surrounding whitespace: the same key;
  - refused, with exit 1, one `cef-cache-key:` line on stderr and nothing on stdout: a short, non-hex,
    upper-case or empty commit, a missing commit file or Dockerfile, and a Dockerfile with no
    gst-base, no cef-build, no rust-build, two gst-base stages, cef-build after rust-build, cef-build
    before gst-base, cef-build built FROM another stage, no lines at all, a COPY, ADD, RUN --mount or
    other ARG in the hashed stages, a FROM naming a variable, a base the resolver cannot resolve, an
    answer that is no digest, or a digest from a resolver that then failed.

`scripts/cef-cache-holds-tree.sh`: a cache manifest holds the cef-build tree when a layer (never the
cache config) is 1 GB or more, in either manifest shape; anything else, or no manifest, is refused.

image.yml: the `cache` job's "Write the build cache" builds what "Build the image" and "Build the
cef-test stage" read (the same context, file and build args; the same target as "Build the image"),
or every warm read would miss (the review's m4).

Each sweep prints how many cases it checked, and a sweep that checked none is a failure.

Usage: cef_cache_key_test.py [--script scripts/cef-cache-key.sh] [--tree scripts/cef-cache-holds-tree.sh]
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCKERFILE = ROOT / "docker" / "Dockerfile"
COMMIT = ROOT / "docker" / "GSTCEF_COMMIT"
WORKFLOW = ROOT / ".github" / "workflows" / "image.yml"
HASHED = ("gst-base", "cef-build")
FROM = re.compile(r"^FROM\s+(?:--\S+\s+)*(\S+)(?:\s+AS\s+(\S+))?\s*$", re.IGNORECASE)
FAKE_RESOLVER = r"""#!/usr/bin/env bash
# The test's registry: logs each ref, answers from FAKE_MAP ("ref digest" lines), else sha256(ref).
echo "$1" >> "$FAKE_LOG"
if [ -n "${FAKE_FAIL:-}" ]; then echo "fake: $FAKE_FAIL" >&2; exit 1; fi
if [ -n "${FAKE_GARBAGE:-}" ]; then echo "$FAKE_GARBAGE"; exit 0; fi
if [ -n "${FAKE_DIGEST_THEN_FAIL:-}" ]; then printf 'sha256:%064d\n' 0; exit 1; fi
hit=$(awk -v r="$1" '$1 == r { print $2 }' "$FAKE_MAP")
if [ -n "$hit" ]; then echo "$hit"; else printf 'sha256:%s\n' "$(printf '%s' "$1" | sha256sum | cut -c1-64)"; fi
"""


def stage_of_each_line(lines):
    """The stage name each line (1-based) belongs to, or None before the first FROM."""
    owner, current = {}, None
    for n, line in enumerate(lines, start=1):
        m = FROM.match(line)
        if m:
            current = m.group(2) or f"<unnamed {n}>"
        owner[n] = current
    return owner


def external_bases(lines, owner):
    """The FROM bases of the hashed stages that name no stage defined above them, scratch or a pin."""
    named, out = set(), []
    for n, line in enumerate(lines, start=1):
        m = FROM.match(line)
        if not m:
            continue
        base, name = m.group(1), m.group(2)
        if owner[n] in HASHED and base not in named and base != "scratch" and "@sha256:" not in base:
            out.append(base)
        if name:
            named.add(name)
    return out


def step_with(text, name):
    """The `with:` keys of image.yml's step `name`, read as text (stdlib has no YAML)."""
    lines = text.splitlines()
    starts = [i for i, l in enumerate(lines) if re.fullmatch(r"\s*- name: " + re.escape(name) + r"\s*", l)]
    if len(starts) != 1:
        return None
    i = starts[0]
    indent = len(lines[i]) - len(lines[i].lstrip())
    keys = {}
    for l in lines[i + 1:]:
        if l.strip() and (len(l) - len(l.lstrip()) <= indent):
            break
        m = re.fullmatch(r"\s+(context|file|build-args|target):\s*(.*?)\s*", l)
        if m:
            keys[m.group(1)] = m.group(2)
    return keys


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--script", default=str(ROOT / "scripts" / "cef-cache-key.sh"))
    ap.add_argument("--tree", default=str(ROOT / "scripts" / "cef-cache-holds-tree.sh"))
    a = ap.parse_args()
    script, tree = Path(a.script).resolve(), Path(a.tree).resolve()
    failures = []
    for p in (script, tree):
        if not p.is_file():
            print(f"FAIL {p} does not exist", file=sys.stderr)
            return 1

    def check(ok, what, quiet=False):
        if not ok or not quiet:
            print(f"{'ok  ' if ok else 'FAIL'} {what}")
        if not ok:
            failures.append(what)

    lines = DOCKERFILE.read_text().splitlines()
    commit = COMMIT.read_text()
    owner = stage_of_each_line(lines)
    stages = [s for s in dict.fromkeys(owner.values()) if s]
    check(all(s in stages for s in HASHED), f"the Dockerfile has the stages {HASHED} (it has {stages})")
    want_bases = external_bases(lines, owner)
    check(len(want_bases) > 0, f"the hashed stages have an external base to resolve ({want_bases})")

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        df, cf = tmp / "Dockerfile", tmp / "GSTCEF_COMMIT"
        fake, fake_log, fake_map = tmp / "resolve", tmp / "resolved.log", tmp / "map"
        fake.write_text(FAKE_RESOLVER)
        fake.chmod(0o755)
        fake_map.write_text("")

        def run(dockerfile, commit_path, **env):
            fake_log.write_text("")
            e = dict(os.environ, CEF_CACHE_KEY_RESOLVE=str(fake), FAKE_LOG=str(fake_log), FAKE_MAP=str(fake_map))
            e.update(env)
            p = subprocess.run([str(script), str(dockerfile), str(commit_path)], capture_output=True, text=True,
                               timeout=30, env=e)
            return p.returncode, p.stdout, p.stderr

        def key_of(text_lines, commit_text=commit, **env):
            df.write_text("\n".join(text_lines) + "\n" if text_lines else "")
            cf.write_text(commit_text)
            return run(df, cf, **env)

        def asked():
            return fake_log.read_text().split()

        # The real files, twice, and a copy elsewhere; the resolver asked for exactly the external bases.
        code, base, err = run(DOCKERFILE, COMMIT)
        base = base.strip()
        check(code == 0 and re.fullmatch(r"[0-9a-f]{16}", base) is not None,
              f"the real files give 16 hex (exit {code}, key '{base}', stderr '{err.strip()}')")
        check(asked() == want_bases, f"the resolver was asked for {want_bases} (it was asked for {asked()})")
        check(run(DOCKERFILE, COMMIT)[1].strip() == base, "the same key twice")
        check(key_of(lines)[1].strip() == base, "a copy elsewhere gives the same key")

        # Two sweeps over the lines: an insert after each line (0 = the top; the new line belongs to the
        # stage of the line above it), and ` # x` appended to each non-FROM line.
        for sweep in ("insert", "edit"):
            inside = outside = 0
            for i in range(0 if sweep == "insert" else 1, len(lines) + 1):
                if sweep == "insert":
                    text = lines[:i] + ["# probe line"] + lines[i:]
                else:
                    if FROM.match(lines[i - 1]):
                        continue
                    text = list(lines)
                    text[i - 1] += " # x"
                stage = owner.get(i)
                code, out, err = key_of(text)
                k = out.strip()
                if stage in HASHED:
                    inside += 1
                    ok, want = code == 0 and k != base and len(k) == 16, "a new key"
                else:
                    outside += 1
                    ok, want = code == 0 and k == base, "the same key"
                check(ok, f"{sweep} at line {i} ({stage or 'header'}): {want} (exit {code}, key '{k}', "
                          f"stderr '{err.strip()}')", quiet=True)
            print(f"{sweep} sweep checked: {inside} lines inside {'/'.join(HASHED)}, {outside} outside")
            check(inside > 0 and outside > 0, f"both sides of the {sweep} sweep checked at least one line")

        # Each FROM line's base replaced by another image.
        froms = 0
        for n, line in enumerate(lines, start=1):
            m = FROM.match(line)
            if not m:
                continue
            froms += 1
            text = list(lines)
            text[n - 1] = line[:m.start(1)] + "ubuntu:99.04" + line[m.end(1):]
            code, out, err = key_of(text)
            k, stage = out.strip(), owner[n]
            if stage == "gst-base":
                ok, want = code == 0 and k not in ("", base), "a new key"
            elif stage == "cef-build":
                ok, want = code == 1 and out == "" and err.startswith("cef-cache-key: "), "refused"
            else:
                ok, want = code == 0 and k == base, "the same key"
            check(ok, f"line {n}'s FROM base replaced ({stage}): {want} (exit {code}, key '{k}', stderr '{err.strip()}')")
        print(f"FROM lines checked: {froms}")
        check(froms >= 3, "the FROM sweep checked at least three FROM lines")

        # The base's digest moves under the same text; a pinned base is not resolved.
        fake_map.write_text("".join(f"{b} sha256:{'a' * 64}\n" for b in want_bases))
        code, out, _ = key_of(lines)
        check(code == 0 and out.strip() not in ("", base), f"the base's digest moved, the text the same: a new key ({out.strip()})")
        fake_map.write_text("")
        pinned = [l.replace(want_bases[0], want_bases[0] + "@sha256:" + "b" * 64, 1) if FROM.match(l) and owner[n] == "gst-base" else l
                  for n, l in enumerate(lines, start=1)]
        code, out, err = key_of(pinned)
        check(code == 0 and asked() == want_bases[1:],
              f"a base pinned @sha256: is not resolved (exit {code}, asked {asked()}, stderr '{err.strip()}')")

        # The commit file.
        other = "0" * 40 if commit.strip() != "0" * 40 else "1" * 40
        commits = [(other + "\n", False), ("  " + commit.strip() + "  \n\n", True), (commit.strip(), True)]
        for text, same in commits:
            code, out, _ = key_of(lines, text)
            k = out.strip()
            ok = code == 0 and ((k == base) if same else (k not in ("", base)))
            check(ok, f"commit {text.strip()[:12]}… ({'whitespace only' if same else 'another commit'}): "
                      f"{'the same key' if same else 'a new key'} ({k})")
        print(f"commit files checked: {len(commits)}")

        # Refusals.
        def span(stage):
            return [l for n, l in enumerate(lines, start=1) if owner[n] == stage]

        def without(stage):
            return [l for n, l in enumerate(lines, start=1) if owner[n] != stage]

        def into(stage, extra):
            """The Dockerfile with `extra` added as the line after `stage`'s FROM."""
            n = next(n for n, l in enumerate(lines, start=1) if owner[n] == stage and FROM.match(l))
            return lines[:n] + [extra] + lines[n:]

        header = [l for n, l in enumerate(lines, start=1) if owner[n] is None]
        rest = [l for n, l in enumerate(lines, start=1) if owner[n] not in (None, "cef-build")]
        cef_from = span("cef-build")[0]
        gst_from = span("gst-base")[0]
        refusals = [
            ("a short commit", lines, commit.strip()[:12], {}),
            ("a non-hex commit", lines, "g" * 40, {}),
            ("an upper-case commit", lines, commit.strip().upper(), {}),
            ("an empty commit file", lines, "", {}),
            ("no gst-base stage", without("gst-base"), commit, {}),
            ("no cef-build stage", without("cef-build"), commit, {}),
            ("no rust-build stage", without("rust-build"), commit, {}),
            ("two gst-base stages", span("gst-base") + lines, commit, {}),
            ("cef-build after rust-build", header + rest + span("cef-build"), commit, {}),
            ("cef-build before gst-base", header + span("cef-build") + [l for n, l in enumerate(lines, start=1)
                                                                       if owner[n] not in (None, "cef-build")], commit, {}),
            ("cef-build FROM another stage", [("FROM ubuntu:24.04 AS cef-build" if l == cef_from else l) for l in lines], commit, {}),
            ("an empty Dockerfile", [], commit, {}),
            ("a COPY in cef-build", into("cef-build", "COPY docker/patches /patches"), commit, {}),
            ("an ADD in gst-base", into("gst-base", "ADD https://example.invalid/x.tar /x"), commit, {}),
            ("a RUN --mount in cef-build", into("cef-build", "RUN --mount=type=bind,source=x,target=/x true"), commit, {}),
            ("another ARG in cef-build", into("cef-build", "ARG CEF_EXTRA"), commit, {}),
            ("a FROM naming a variable", [("FROM ${BASE} AS gst-base" if l == gst_from else l) for l in lines], commit, {}),
            ("a base the resolver cannot resolve", lines, commit, {"FAKE_FAIL": "manifest unknown"}),
            ("a resolver answer that is no digest", lines, commit, {"FAKE_GARBAGE": "sha256:short"}),
            ("a resolver that prints a digest and fails", lines, commit, {"FAKE_DIGEST_THEN_FAIL": "1"}),
        ]
        for name, text, commit_text, env in refusals:
            code, out, err = key_of(text, commit_text, **env)
            ok = code == 1 and out == "" and err.startswith("cef-cache-key: ") and err.count("\n") == 1
            check(ok, f"refused: {name} (exit {code}, stdout '{out.strip()}', stderr '{err.strip()}')")
        for name, missing in (("a missing commit file", "commit"), ("a missing Dockerfile", "dockerfile")):
            key_of(lines)
            gone = tmp / "absent"
            code, out, err = run(gone if missing == "dockerfile" else df, gone if missing == "commit" else cf)
            ok = code == 1 and out == "" and err.startswith("cef-cache-key: ") and err.count("\n") == 1
            check(ok, f"refused: {name} (exit {code}, stdout '{out.strip()}', stderr '{err.strip()}')")
        print(f"refusals checked: {len(refusals) + 2}")

        # cef-cache-holds-tree.sh.
        layer = "application/vnd.oci.image.layer.v1.tar+gzip"
        config = "application/vnd.buildkit.cacheconfig.v0"
        manifests = [
            ("an index holding a 1.86 GB layer", {"manifests": [{"mediaType": layer, "size": 1861033202},
                                                              {"mediaType": layer, "size": 230780000},
                                                              {"mediaType": config, "size": 9000}]}, True),
            ("an image manifest holding it", {"config": {"mediaType": config, "size": 9000},
                                              "layers": [{"mediaType": layer, "size": 1861033202}]}, True),
            ("a layer of exactly 1 GB", {"manifests": [{"mediaType": layer, "size": 1000000000}]}, True),
            ("a layer one byte short of 1 GB", {"manifests": [{"mediaType": layer, "size": 999999999}]}, False),
            ("a big cache config and small layers", {"manifests": [{"mediaType": layer, "size": 230780000},
                                                                  {"mediaType": config, "size": 5000000000}]}, False),
            ("no layers", {"manifests": [{"mediaType": config, "size": 9000}]}, False),
            ("an empty object", {}, False),
            ("not JSON", None, False),
            ("an empty file", "", False),
        ]
        for name, doc, holds in manifests:
            f = tmp / "manifest.json"
            f.write_text("not json" if doc is None else (doc if isinstance(doc, str) else json.dumps(doc)))
            p = subprocess.run([str(tree), str(f)], capture_output=True, text=True, timeout=30)
            if holds:
                ok = p.returncode == 0 and p.stderr == "" and re.fullmatch(r"\d+ layers, the largest \d+ bytes\n", p.stdout)
            else:
                ok = p.returncode == 1 and p.stderr.startswith("cef-cache-holds-tree: ") and p.stderr.count("\n") == 1
            check(bool(ok), f"holds-tree: {name}: {'holds' if holds else 'refused'} (exit {p.returncode}, "
                            f"stdout '{p.stdout.strip()}', stderr '{p.stderr.strip()}')")
        print(f"cache manifests checked: {len(manifests)}")

    # image.yml: the writer builds what the readers read.
    text = WORKFLOW.read_text()
    steps = {n: step_with(text, n) for n in ("Build the image", "Build the cef-test stage", "Write the build cache")}
    check(all(v is not None for v in steps.values()), f"image.yml has each build step once ({list(steps)})")
    if all(v is not None for v in steps.values()):
        w = steps["Write the build cache"]
        check(all(w.get(k) for k in ("context", "file", "build-args")),
              f"the writer names its context, file and build args ({w})")
        for reader in ("Build the image", "Build the cef-test stage"):
            r = steps[reader]
            same = all(r.get(k) == w.get(k) for k in ("context", "file", "build-args"))
            check(same, f"\"{reader}\" reads what the writer builds: context, file, build args ({r} vs {w})")
        check(steps["Build the image"].get("target") == w.get("target"),
              f"the writer builds \"Build the image\"'s target ({w.get('target')} vs {steps['Build the image'].get('target')})")

    if failures:
        print(f"{len(failures)} failure(s)", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
