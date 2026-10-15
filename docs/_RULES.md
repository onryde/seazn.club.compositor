# Relay compositor: working rules

The rulebook is [`AGENTS.md`](../AGENTS.md); it is binding. This file restates the plan's wave
gates and lists the traps this repo has already paid for.

## Wave gates (plan, `plans/2026-10-14-r2-compositor-plan.md`)

1. **After C06:** domain review, plus all domain tests and properties.
2. **After C12:** the image builds, and a 60 s harness smoke passes.
3. **After C15:** three consecutive chaos runs are green, and the soak has passed.
4. **C17:** the owner watches the end-to-end run.

The orchestrator re-runs each wave's scoped gate itself and pastes the raw counts.

## Traps

- **SRT push from Fly never starts a CF broadcast** (shared register C14: `seazn.club`'s
  `R0-CORRECTIONS-FOR-R1.md`, R0 FP-4. Not this repo's task C14, the chaos suite). ffmpeg pushing SRT
  from a Fly Machine never started a broadcast while RTMPS did; a handset on SRT did publish. The
  output leg is RTMPS to Input B. Do not "simplify" it to SRT.
- **A secret in argv leaks through `ps`.** Pass secrets by env or file, never on a command line
  (`AGENTS.md` §7).
- **`cargo test` from the wrong cwd silently runs a different workspace.** The shell cwd resets to
  the main checkout between calls, so a run "from a worktree" can test `main` and come back green.
  Put `cd <abs worktree> &&` in the same call, and confirm the paths in cargo's `Running …` lines.
- **GitHub Actions' implicit shell has no `pipefail`** (found in C01). Under the default `bash -e`,
  `cargo test … | tee test.log` exits with tee's 0, so a FAILED suite passed the step, and the
  vacuous-run check still saw other suites' passes. `check.yml` sets `shell: bash`
  (`bash -eo pipefail`) at job level. Keep it.
- **libtest hides a passing test's output.** A guard's `checked N` line shows only on failure unless
  you run `cargo test -- --show-output`.
- **Cargo allows a dev-dependency cycle.** `adapters` as a normal dependency of `app` is refused by
  cargo as a cycle before any test runs; as a dev-dependency it builds, and only `boundary.rs`
  refuses it.
- **A manifest is not the dependency graph.** `clock = { workspace = true }` reads as `clock` in a
  member's manifest while the workspace table renames it to `tokio`, and a transitive dependency is
  in no manifest of ours at all. The dependency guards read `cargo metadata`'s resolved graph.
- **The dependency guards run `cargo metadata --locked`.** A cold cargo cache needs the network once
  to read the registry index; offline it fails safe (the guard goes red), and so does a stale
  `Cargo.lock`. Never drop `--locked` to make an offline run pass.
- **An `#[ignore]`d guard still passes `cargo test`.** CI fails on any ignored test and requires each
  layering guard's own `... ok` line.
- **`find -type f` never follows a symlink**, so a link under `crates/` or `docker/` would carry
  content past `check-portable.sh`. The script refuses every symlink there.
- **A denylist of std paths always has one more door** (`std::r#fs`, `use std::{self as s}`, a
  `macro_rules!` splice `std::$m`). `purity.rs` is an allowlist of std/core/alloc modules instead,
  over a lexer that knows `//` inside a string is not a comment.
